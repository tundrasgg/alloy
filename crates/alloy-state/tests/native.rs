//! The `native` stores on the `os` runtime, in a fresh directory under the
//! system's temporary directory: store files created private, durability
//! across a reopen, exclusive use of a directory, exports that match `mem`'s,
//! and a group that restarts on them.
//!
//! On Windows a store file is created with owner `Administrators`, which only
//! an elevated process may name (ARFC 0005 clause 3.1.15). A test run by a
//! process that is not elevated checks that the build then fails and leaves
//! no file, and checks nothing more.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use alloy_rt::{
    DiskError, EntryKind, OsHost, Protection, RtPath, Runtime, RuntimeConfig, Security, register_os,
};
use alloy_seam::{
    Binding, BuildError, ConfigError, ConfigValue, ImplName, Registry, RegistryBuilder, SeamConfig,
};
use alloy_state::{
    ApplyBatch, Entry, Group, LogId, LogStore, LogStoreSeam, MemStoreConfig, NativeStoreConfig,
    NodeId, Op, Scope, StateStore, StateStoreSeam, Vote, register_mem, register_native,
};

const ME: NodeId = NodeId([0x22; 16]);

/// An `os` runtime with `disk-io` rooted at the temporary directory, and a
/// fresh directory there that is removed with its files on drop.
struct Fixture {
    host: OsHost,
    runtime: Runtime,
    registry: Registry,
    name: String,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let host = OsHost::start(2).unwrap();
        let mut builder = RegistryBuilder::new();
        register_os(&mut builder, &host).unwrap();
        register_native(&mut builder).unwrap();
        register_mem(&mut builder).unwrap();
        let registry = builder.build();
        let base = std::env::temp_dir();
        let root = base.to_str().unwrap();
        let mut payload = vec![0x0A, root.len() as u8];
        assert!(root.len() < 0x80);
        payload.extend_from_slice(root.as_bytes());
        let mut config = RuntimeConfig::all(ImplName::new("os").unwrap());
        config.disk_io = Binding {
            name: ImplName::new("os").unwrap(),
            config: ConfigValue::new(1, payload).unwrap(),
        };
        let runtime = Runtime::from_registry(&registry, &config).unwrap();
        let mut bytes = [0; 8];
        runtime.rng().fill(&mut bytes).unwrap();
        let name = format!("alloy-state-test-{:016x}", u64::from_le_bytes(bytes));
        let path = RtPath::root().join(&name).unwrap();
        host.block_on(runtime.disk_io().create_dir(&path)).unwrap();
        let dir = base.join(&name);
        Fixture {
            host,
            runtime,
            registry,
            name,
            dir,
        }
    }

    fn config(&self) -> ConfigValue {
        let dir = self.dir.to_str().unwrap().to_owned();
        NativeStoreConfig { dir }.to_value(1).unwrap()
    }

    fn log(&self) -> Result<Arc<dyn LogStore>, BuildError> {
        let native = ImplName::new("native").unwrap();
        self.registry
            .instantiate::<LogStoreSeam>(&self.runtime, &native, &self.config())
    }

    fn state(&self) -> Result<Arc<dyn StateStore>, BuildError> {
        let native = ImplName::new("native").unwrap();
        self.registry
            .instantiate::<StateStoreSeam>(&self.runtime, &native, &self.config())
    }

    /// The `disk-io` path of `file` in the fixture's directory.
    fn path(&self, file: &str) -> RtPath {
        RtPath::root().join(&self.name).unwrap().join(file).unwrap()
    }

    /// A fixture in which this process may create private files: always on
    /// Linux and macOS, and on Windows when it may name `Administrators` as
    /// owner. Otherwise it checks that each store fails to build and leaves
    /// no file, and returns `None`.
    fn private() -> Option<Fixture> {
        let fixture = Fixture::new();
        let disk = fixture.runtime.disk_io();
        let probe = fixture.path("probe");
        match fixture
            .host
            .block_on(disk.create_dir_protected(&probe, Protection::Private))
        {
            Ok(()) => {
                fixture.host.block_on(disk.remove(&probe)).unwrap();
                Some(fixture)
            }
            Err(DiskError::PermissionDenied) if cfg!(windows) => {
                let log = fixture.log().map(drop);
                let state = fixture.state().map(drop);
                for (built, file) in [(log, "log.redb"), (state, "state.redb")] {
                    assert!(
                        matches!(&built, Err(BuildError::Failed(d)) if d.as_str().contains("permission denied")),
                        "{file}: {built:?}"
                    );
                    let left = fixture.host.block_on(disk.metadata(&fixture.path(file)));
                    assert_eq!(left.map(drop), Err(DiskError::NotFound), "{file}");
                }
                None
            }
            Err(other) => panic!("probe: {other:?}"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let disk = self.runtime.disk_io();
        let dir = RtPath::root().join(&self.name).unwrap();
        for file in ["log.redb", "state.redb"] {
            let _ = self.host.block_on(disk.remove(&dir.join(file).unwrap()));
        }
        let _ = self.host.block_on(disk.remove(&dir));
    }
}

fn id(index: u64) -> LogId {
    LogId {
        term: 1,
        leader: ME,
        index,
    }
}

#[test]
fn a_missing_directory_is_invalid_config() {
    let fixture = Fixture::new();
    let mut dir = fixture.dir.clone();
    dir.push("missing");
    let config = NativeStoreConfig {
        dir: dir.to_str().unwrap().to_owned(),
    }
    .to_value(1)
    .unwrap();
    let native = ImplName::new("native").unwrap();
    let error = fixture
        .registry
        .instantiate::<LogStoreSeam>(&fixture.runtime, &native, &config)
        .unwrap_err();
    assert!(
        matches!(&error, BuildError::Config(ConfigError::Invalid { field, .. }) if field.as_str() == "dir"),
        "{error:?}"
    );
}

/// ARFC 0006 clause 5.2: `native` creates its file with mode `0600` on
/// Linux, whatever the umask, and on Windows with owner `Administrators` and
/// only the list it inherits from `dir`. A file that exists is opened as it
/// is.
#[test]
fn store_files_are_created_private() {
    let Some(fixture) = Fixture::private() else {
        return;
    };
    let disk = fixture.runtime.disk_io();
    let stores = (fixture.log().unwrap(), fixture.state().unwrap());
    for file in ["log.redb", "state.redb"] {
        let path = fixture.path(file);
        match fixture.host.block_on(disk.security(&path)).unwrap() {
            Security::Unix {
                kind,
                symlink,
                mode,
                ..
            } => {
                assert_eq!(
                    (kind, symlink, mode),
                    (EntryKind::File, false, 0o600),
                    "{file}"
                );
            }
            Security::Windows {
                kind,
                reparse_point,
                owner,
                dacl_protected,
                dacl,
            } => {
                assert_eq!((kind, reparse_point), (EntryKind::File, false), "{file}");
                assert_eq!(owner.sid, "S-1-5-32-544", "{file}");
                assert!(!dacl_protected, "{file}");
                let dacl = dacl.expect("a list");
                assert!(
                    !dacl.is_empty() && dacl.iter().all(|ace| ace.inherited),
                    "{file}"
                );
            }
        }
    }
    drop(stores);
    if cfg!(unix) {
        let path = fixture.path("log.redb");
        fixture.host.block_on(disk.set_mode(&path, 0o640)).unwrap();
        let log = fixture.log().unwrap();
        let Security::Unix { mode, .. } = fixture.host.block_on(disk.security(&path)).unwrap()
        else {
            panic!("a Unix entry");
        };
        assert_eq!(mode, 0o640, "an existing file is opened as it is");
        drop(log);
    }
}

#[test]
fn native_stores_keep_their_contents_across_a_reopen() {
    let Some(fixture) = Fixture::private() else {
        return;
    };
    let vote = Vote {
        term: 3,
        node: ME,
        committed: true,
    };
    let log = fixture.log().unwrap();
    assert!(
        matches!(fixture.log(), Err(BuildError::Failed(_))),
        "one instance per dir"
    );
    let entries: Vec<Entry> = (1..=5)
        .map(|i| Entry {
            log_id: id(i),
            payload: vec![i as u8; 9],
        })
        .collect();
    fixture.host.block_on(async {
        log.save_vote(vote).await.unwrap();
        log.append(entries.clone()).await.unwrap();
        log.purge_through(id(2)).await.unwrap();
        log.truncate_from(5).await.unwrap();
    });
    drop(log);

    let state = fixture.state().unwrap();
    fixture.host.block_on(async {
        let ops = vec![
            Op::Put {
                key: b"a".to_vec(),
                value: b"1".to_vec(),
            },
            Op::Put {
                key: b"b".to_vec(),
                value: b"2".to_vec(),
            },
        ];
        state
            .apply(ApplyBatch { log_id: id(4), ops })
            .await
            .unwrap();
        let ops = vec![Op::Delete { key: b"a".to_vec() }];
        state
            .apply(ApplyBatch { log_id: id(5), ops })
            .await
            .unwrap();
    });
    drop(state);

    let log = fixture.log().unwrap();
    let state = fixture.state().unwrap();
    fixture.host.block_on(async {
        assert_eq!(log.read_vote().await.unwrap(), Some(vote));
        let held = log.state().await.unwrap();
        assert_eq!((held.last_purged, held.last), (Some(id(2)), Some(id(4))));
        assert_eq!(log.read(0, 10).await.unwrap(), entries[2..4].to_vec());
        assert_eq!(state.applied().await.unwrap(), Some(id(5)));
        let pairs = state.scan(b"", None, 10).await.unwrap();
        assert_eq!(pairs, vec![(b"b".to_vec(), b"2".to_vec())]);
    });
}

async fn export(store: &dyn StateStore) -> Vec<u8> {
    let snapshot = store.snapshot().await.unwrap();
    let mut out = Vec::new();
    while let Some(chunk) = snapshot.next_chunk().await.unwrap() {
        out.extend(chunk);
    }
    out
}

#[test]
fn exports_install_across_implementations() {
    let Some(fixture) = Fixture::private() else {
        return;
    };
    let mem_config = MemStoreConfig {
        name: "cross".to_owned(),
        fail_after: 0,
    }
    .to_value(1)
    .unwrap();
    let mem = fixture
        .registry
        .instantiate::<StateStoreSeam>(
            &fixture.runtime,
            &ImplName::new("mem").unwrap(),
            &mem_config,
        )
        .unwrap();
    let native = fixture.state().unwrap();
    fixture.host.block_on(async {
        let ops = (0u16..3000)
            .map(|i| Op::Put {
                key: i.to_be_bytes().to_vec(),
                value: vec![7; usize::from(i % 500)],
            })
            .collect();
        mem.apply(ApplyBatch { log_id: id(9), ops }).await.unwrap();
        let bytes = export(mem.as_ref()).await;
        let install = native.begin_install().await.unwrap();
        for chunk in bytes.chunks(70_000) {
            install.write(chunk.to_vec()).await.unwrap();
        }
        assert_eq!(install.finish().await.unwrap(), Some(id(9)));
        assert_eq!(export(native.as_ref()).await, bytes);

        let empty_mem = MemStoreConfig {
            name: "empty".to_owned(),
            fail_after: 0,
        }
        .to_value(1)
        .unwrap();
        let empty = fixture
            .registry
            .instantiate::<StateStoreSeam>(
                &fixture.runtime,
                &ImplName::new("mem").unwrap(),
                &empty_mem,
            )
            .unwrap();
        let install = empty.begin_install().await.unwrap();
        install.write(export(native.as_ref()).await).await.unwrap();
        install.finish().await.unwrap();
        assert_eq!(export(empty.as_ref()).await, bytes);
    });
}

#[test]
fn a_group_on_native_stores_survives_a_restart() {
    let Some(fixture) = Fixture::private() else {
        return;
    };
    let created = fixture.host.block_on(async {
        let group = Group::open(
            &fixture.runtime,
            ME,
            fixture.log().unwrap(),
            fixture.state().unwrap(),
        )
        .await
        .unwrap();
        group.initialize().await.unwrap();
        let labels = BTreeMap::from([("role".to_owned(), "control".to_owned())]);
        let created = group
            .create("Item", Scope::Cluster, "first", b"spec".to_vec(), labels)
            .await
            .unwrap();
        group.shutdown().await.unwrap();
        created
    });
    fixture.host.block_on(async {
        let group = Group::open(
            &fixture.runtime,
            ME,
            fixture.log().unwrap(),
            fixture.state().unwrap(),
        )
        .await
        .unwrap();
        let read = group.get("Item", &Scope::Cluster, "first").await.unwrap();
        assert_eq!(read, Some(created));
        group.shutdown().await.unwrap();
    });
}
