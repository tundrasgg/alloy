//! The protection of the data directory and its files (ARFC 0002 sections
//! 3.12 and 3.14): created through the `disk-io` seam with
//! `Protection::PrivateRoot` and `Protection::Private`, on the simulator's
//! disk, which follows the Linux rules, and on this host's `os` disk.

mod common;

use std::sync::Arc;

use alloy_id::datadir::{UnixEntry, UnixKind, check_at_start};
use alloy_id::store::{AT_REST_KEY_FILE, DataDir, HOST_FILE, NODE_FILE, SECRETS_FILE};
use alloy_id::{DataDirFacts, proto::NodeSecrets, proto::NodeState};
use alloy_rt::{
    Access, Create, DiskError, DiskIo, EntryKind, OpenOptions, RtPath, Runtime, Security,
};
use common::{Then, err, on_host, run_starts, start};

const HOST: &[u8] = b"alloy-host 1\njoin joining\n";

fn dir_path() -> RtPath {
    RtPath::new("/alloyd").expect("a valid path")
}

/// What `security` reports for `path` as a [`UnixEntry`].
async fn unix_entry(disk: &dyn DiskIo, path: &RtPath) -> Result<UnixEntry, String> {
    match disk.security(path).await.map_err(err("security"))? {
        Security::Unix {
            kind,
            symlink,
            uid,
            mode,
        } => Ok(UnixEntry {
            path: path.as_str().to_owned(),
            kind: match (symlink, kind) {
                (true, _) => UnixKind::Symlink,
                (false, EntryKind::Dir) => UnixKind::Dir,
                (false, EntryKind::File) => UnixKind::File,
                (false, _) => UnixKind::Other,
            },
            uid,
            mode,
        }),
        other => Err(format!("{path:?}: not a Unix entry: {other:?}")),
    }
}

fn expect_mode(entry: &UnixEntry, mode: u32) -> Result<(), String> {
    if entry.mode == mode {
        Ok(())
    } else {
        Err(format!(
            "{} has mode {:o}, not {mode:o}",
            entry.path, entry.mode
        ))
    }
}

/// The data directory has mode `0700` and every file written in it `0600`,
/// a `.new` left by an interrupted write with another mode included, and the
/// result is an Alloy data directory.
#[test]
fn the_data_directory_and_its_files_are_created_private() {
    on_host(1, |runtime: Runtime| async move {
        let disk = Arc::clone(runtime.disk_io());
        let dir = DataDir::create(Arc::clone(&disk), dir_path())
            .await
            .map_err(err("create"))?;
        let entry = unix_entry(disk.as_ref(), &dir_path()).await?;
        expect_mode(&entry, 0o700)?;
        if DataDir::create(Arc::clone(&disk), dir_path()).await.is_ok() {
            return Err("a second create of the data directory succeeded".to_owned());
        }
        dir.write(HOST_FILE, HOST).await.map_err(err("host"))?;

        // A `.new` that an interrupted write left, with the mode `open`
        // gives (`0644` on `mem`).
        let stale = dir_path().join("secrets.new").map_err(err("path"))?;
        let options = OpenOptions {
            access: Access::ReadWrite,
            create: Create::New,
            direct: false,
            exclusive: false,
        };
        drop(disk.open(&stale, options).await.map_err(err("stale"))?);
        expect_mode(&unix_entry(disk.as_ref(), &stale).await?, 0o644)?;

        let rng = runtime.rng().as_ref();
        let mut secrets = NodeSecrets::default();
        secrets.identity_key = vec![0x11; 32];
        dir.create_secrets(rng, &secrets)
            .await
            .map_err(err("secrets"))?;
        dir.write_handshake_counter(7)
            .await
            .map_err(err("counter"))?;
        dir.write_node_state(&NodeState::default())
            .await
            .map_err(err("node"))?;
        for name in [
            HOST_FILE,
            AT_REST_KEY_FILE,
            SECRETS_FILE,
            "handshake-counter",
            NODE_FILE,
        ] {
            let path = dir_path().join(name).map_err(err("path"))?;
            expect_mode(&unix_entry(disk.as_ref(), &path).await?, 0o600)?;
        }
        if disk.metadata(&stale).await != Err(DiskError::NotFound) {
            return Err("the stale secrets.new is still there".to_owned());
        }

        let host = dir
            .read(HOST_FILE)
            .await
            .map_err(err("read host"))?
            .ok_or("no host file")?;
        let facts = DataDirFacts::Linux {
            ancestors: vec![unix_entry(disk.as_ref(), &RtPath::root()).await?],
            data_dir: Some(unix_entry(disk.as_ref(), &dir_path()).await?),
            data_dir_empty: false,
            host_file: Some(
                unix_entry(
                    disk.as_ref(),
                    &dir_path().join(HOST_FILE).map_err(err("path"))?,
                )
                .await?,
            ),
            host_file_first_line_ok: host.starts_with(b"alloy-host 1\n"),
        };
        check_at_start(&facts).map_err(err("check_at_start"))
    });
}

/// The create of the data directory is durable when `create` returns: a
/// crash right after it, with every unsynced change lost, keeps it.
#[test]
fn a_created_data_directory_survives_a_crash() {
    let create = start(|runtime: Runtime| async move {
        DataDir::create(Arc::clone(runtime.disk_io()), dir_path())
            .await
            .map(drop)
            .map_err(err("create"))
    });
    let check = start(|runtime: Runtime| async move {
        let entry = unix_entry(runtime.disk_io().as_ref(), &dir_path()).await?;
        expect_mode(&entry, 0o700)
    });
    run_starts(3, vec![(create, Then::FINISH), (check, Then::FINISH)]);
}

/// An adopted directory, such as a mount point, gets mode `0700`.
#[test]
fn an_adopted_directory_gets_mode_0700() {
    on_host(2, |runtime: Runtime| async move {
        let disk = Arc::clone(runtime.disk_io());
        disk.create_dir(&dir_path()).await.map_err(err("mkdir"))?;
        expect_mode(&unix_entry(disk.as_ref(), &dir_path()).await?, 0o755)?;
        let dir = DataDir::adopt(Arc::clone(&disk), dir_path())
            .await
            .map_err(err("adopt"))?;
        expect_mode(&unix_entry(disk.as_ref(), &dir_path()).await?, 0o700)?;
        dir.write(HOST_FILE, HOST).await.map_err(err("host"))?;
        let host = dir_path().join(HOST_FILE).map_err(err("path"))?;
        expect_mode(&unix_entry(disk.as_ref(), &host).await?, 0o600)
    });
}

/// On this host's `os` disk, in a fresh directory under the temporary
/// directory: on Linux and macOS the data directory gets mode `0700` and its
/// files `0600`; on Windows the data directory gets owner `Administrators`
/// and a protected list granting full control to `SYSTEM` and
/// `Administrators` only, and each file that owner and only that list,
/// inherited, so a host file written through `DataDir` passes the Windows
/// data directory rule. A Windows process that may not name `Administrators`
/// as owner gets `PermissionDenied`, and nothing is created.
#[test]
fn on_this_host_the_data_directory_and_its_files_are_private() {
    use alloy_id::datadir::{Ace as RuleAce, Principal, WindowsEntry};
    use alloy_rt::{OsHost, RuntimeConfig, register_os};
    use alloy_seam::{Binding, ConfigValue, ImplName, RegistryBuilder};

    let host = OsHost::start(2).expect("the os host starts");
    let mut builder = RegistryBuilder::new();
    register_os(&mut builder, &host).expect("os registers");
    let registry = builder.build();
    let base = std::env::temp_dir();
    let root = base.to_str().expect("a UTF-8 temporary directory");
    assert!(root.len() < 0x80);
    let mut payload = vec![0x0A, u8::try_from(root.len()).expect("short")];
    payload.extend_from_slice(root.as_bytes());
    let mut config = RuntimeConfig::all(ImplName::new("os").expect("a name"));
    config.disk_io = Binding {
        name: ImplName::new("os").expect("a name"),
        config: ConfigValue::new(1, payload).expect("a value"),
    };
    let runtime = Runtime::from_registry(&registry, &config).expect("a runtime");
    let disk = Arc::clone(runtime.disk_io());
    let mut bytes = [0; 8];
    runtime.rng().fill(&mut bytes).expect("random bytes");
    let name = format!("alloy-id-test-{:016x}", u64::from_le_bytes(bytes));
    let path = RtPath::root().join(&name).expect("a path");
    let host_path = path.join(HOST_FILE).expect("a path");

    let created = host.block_on(DataDir::create(Arc::clone(&disk), path.clone()));
    if cfg!(windows)
        && matches!(
            created,
            Err(alloy_id::StoreError::Disk(DiskError::PermissionDenied))
        )
    {
        // The token may not name `Administrators` as owner: it is not
        // elevated. Nothing is left behind.
        assert_eq!(
            host.block_on(disk.metadata(&path)),
            Err(DiskError::NotFound)
        );
        return;
    }
    let dir = created.expect("the data directory is created");
    host.block_on(dir.write(HOST_FILE, HOST))
        .expect("the host file");
    let dir_security = host.block_on(disk.security(&path)).expect("security");
    let host_security = host.block_on(disk.security(&host_path)).expect("security");
    host.block_on(disk.remove(&host_path)).expect("cleanup");
    host.block_on(disk.remove(&path)).expect("cleanup");

    match (dir_security, host_security) {
        (
            Security::Unix { mode: dir_mode, .. },
            Security::Unix {
                mode: file_mode, ..
            },
        ) => {
            assert_eq!((dir_mode, file_mode), (0o700, 0o600));
        }
        (dir_security, host_security) => {
            let entry = |security: Security, at: &str| match security {
                Security::Windows {
                    kind,
                    reparse_point,
                    owner,
                    dacl_protected,
                    dacl,
                } => WindowsEntry {
                    path: at.to_owned(),
                    is_dir: kind == EntryKind::Dir,
                    reparse_point,
                    owner: Principal {
                        sid: owner.sid,
                        name: owner.name,
                    },
                    dacl_protected,
                    dacl: dacl.map(|list| {
                        list.into_iter()
                            .map(|ace| RuleAce {
                                allow: ace.allow,
                                principal: Principal {
                                    sid: ace.trustee.sid,
                                    name: ace.trustee.name,
                                },
                                mask: ace.mask,
                                inherit_only: ace.inherit_only,
                                inherited: ace.inherited,
                            })
                            .collect()
                    }),
                },
                other => panic!("{at}: not a Windows entry: {other:?}"),
            };
            let data_dir = entry(dir_security, "data directory");
            let host_file = entry(host_security, "host file");
            assert_eq!(data_dir.owner.sid, "S-1-5-32-544");
            assert_eq!(host_file.owner.sid, "S-1-5-32-544");
            let sids = |e: &WindowsEntry| -> Vec<(String, bool)> {
                e.dacl
                    .iter()
                    .flatten()
                    .map(|ace| (ace.principal.sid.clone(), ace.inherited))
                    .collect()
            };
            let admins = |inherited| {
                vec![
                    ("S-1-5-18".to_owned(), inherited),
                    ("S-1-5-32-544".to_owned(), inherited),
                ]
            };
            assert!(data_dir.dacl_protected && !host_file.dacl_protected);
            assert_eq!(sids(&data_dir), admins(false));
            assert_eq!(sids(&host_file), admins(true));
            let facts = DataDirFacts::Windows {
                ancestors: Vec::new(),
                data_dir: Some(data_dir),
                host_file: Some(host_file),
                host_file_first_line_ok: true,
            };
            assert_eq!(check_at_start(&facts), Ok(()));
        }
    }
}
