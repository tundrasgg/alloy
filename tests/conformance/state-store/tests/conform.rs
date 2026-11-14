//! Runs the `state-store` suite against both implementations (ARFC 0006 section
//! 5.5): `mem` on a simulated host, and `native` on this host in a temporary
//! directory. Each check gets a fresh [`Env`] and a seed derived from its
//! name, and a failure prints the seed.
//!
//! On Windows, `native` creates its store file `Private` through `disk-io`,
//! with owner `Administrators` (ARFC 0005 clause 3.1.15), and a process that
//! may not name that owner gets `BuildError::Failed` (D-0062). The `native`
//! test first asks whether this process can create a `Private` file. When it
//! cannot, it checks that refusal instead of the suite, and prints one line
//! saying the `native` checks did not run and why.
//!
//! On `native`, the registry also holds `mem`, from one fresh `register_mem`
//! call, for the checks that move an export between the two.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_rt::{DiskError, OsHost, Protection, RtPath, Runtime, RuntimeConfig, register_os};
use alloy_seam::{Binding, BuildError, ConfigValue, Detail, ImplName, Registry, RegistryBuilder};
use alloy_sim::{Boot, HostBoot, HostConfig, RunConfig, RunResult, run};
use conformance_state_store::state::StateStoreSeam;
use conformance_state_store::{
    Check, Env, Failure, Harness, Implementation, Outcome, checks, run_suite,
};

/// The suite's name, which prefixes each check's directory.
const SUITE: &str = "state-store";

/// The seed of a check: FNV-1a of its name, so it is fixed per check.
fn seed_of(name: &str) -> u64 {
    name.bytes().fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3)
    })
}

/// Runs each check as the `boot` task of the one host of a fresh simulated
/// world, with a registry holding `mem` from one fresh `register_mem` call.
struct MemHarness;

impl Harness for MemHarness {
    fn implementation(&self) -> Implementation {
        Implementation::Mem
    }

    fn run(&self, check: &Check) -> Outcome {
        let seed = seed_of(check.name);
        let run_check = check.run;
        let slot: Arc<Mutex<Option<Outcome>>> = Arc::default();
        let out = Arc::clone(&slot);
        let summary = run(RunConfig::new(seed), move |world| async move {
            let filled = Arc::clone(&out);
            let boot: Boot = Arc::new(move |boot: HostBoot| {
                let slot = Arc::clone(&filled);
                Box::pin(async move {
                    let mut builder = boot.registry;
                    let outcome = match alloy_state::register_mem(&mut builder) {
                        Ok(()) => {
                            run_check(Env {
                                implementation: Implementation::Mem,
                                runtime: boot.runtime,
                                registry: builder.build(),
                                seed,
                                dirs: Vec::new(),
                            })
                            .await
                        }
                        Err(error) => Err(Failure(format!("register_mem: {error}"))),
                    };
                    *slot.lock().expect("slot") = Some(outcome);
                })
            });
            world
                .add_host(HostConfig::new(boot))
                .map_err(|error| alloy_sim::Failure(Detail::new(&error.to_string())))?;
            while out.lock().expect("slot").is_none() {
                world.sleep(Duration::from_secs(1)).await;
            }
            Ok(())
        });
        let outcome = slot.lock().expect("slot").take();
        match outcome {
            None => Err(Failure(format!(
                "the check did not complete: {}",
                summary.line()
            ))),
            Some(outcome) if summary.result == RunResult::Pass => outcome,
            Some(_) => Err(Failure(format!("the run failed: {:?}", summary.failure))),
        }
    }
}

/// Runs each check on an `os` runtime of this host, in a directory of its
/// own under the temporary directory, with a registry holding `native` and
/// `mem`.
struct NativeHarness {
    host: OsHost,
    /// The temporary directory: the `disk-io` root of every check's runtime.
    base: PathBuf,
}

impl NativeHarness {
    fn new() -> NativeHarness {
        NativeHarness {
            host: OsHost::start(2).expect("the os host starts"),
            base: std::env::temp_dir(),
        }
    }

    /// The `os` bindings, with `disk-io` rooted at the temporary directory:
    /// `OsDiskIoConfig { root }` (ARFC 0005 section 4.2).
    fn runtime_config(&self) -> Result<RuntimeConfig, Failure> {
        let os = ImplName::new("os").map_err(|e| Failure(format!("{e}")))?;
        let root = self
            .base
            .to_str()
            .ok_or_else(|| Failure("the temporary directory is not UTF-8".into()))?;
        let mut payload = vec![0x0A];
        let mut len = root.len() as u64;
        while len >= 0x80 {
            payload.push((len as u8) | 0x80);
            len >>= 7;
        }
        payload.push(len as u8);
        payload.extend_from_slice(root.as_bytes());
        let mut config = RuntimeConfig::all(os.clone());
        config.disk_io = Binding {
            name: os,
            config: ConfigValue::new(1, payload).map_err(|e| Failure(format!("{e}")))?,
        };
        Ok(config)
    }

    /// A fresh `os` runtime and registry, and a directory of its own under
    /// the temporary directory holding two empty directories, `a` and `b`.
    fn prepare(&self, seed: u64) -> Result<Place, Failure> {
        let mut builder = RegistryBuilder::new();
        register_os(&mut builder, &self.host).map_err(|e| Failure(format!("register_os: {e}")))?;
        alloy_state::register_native(&mut builder)
            .map_err(|e| Failure(format!("register_native: {e}")))?;
        alloy_state::register_mem(&mut builder)
            .map_err(|e| Failure(format!("register_mem: {e}")))?;
        let registry = builder.build();
        let runtime = Runtime::from_registry(&registry, &self.runtime_config()?)
            .map_err(|e| Failure(format!("the os runtime: {e}")))?;

        let mut unique = [0; 8];
        runtime
            .rng()
            .fill(&mut unique)
            .map_err(|e| Failure(format!("rng: {e}")))?;
        let name = format!(
            "alloy-conformance-{SUITE}-{seed:016x}-{:016x}",
            u64::from_le_bytes(unique)
        );
        let top = RtPath::root()
            .join(&name)
            .map_err(|e| Failure(format!("{e}")))?;
        let mut dirs = Vec::new();
        let made = self.host.block_on(async {
            runtime.disk_io().create_dir(&top).await?;
            for place in ["a", "b"] {
                runtime.disk_io().create_dir(&top.join(place)?).await?;
            }
            Ok::<(), DiskError>(())
        });
        made.map_err(|e| Failure(format!("the check's directories: {e}")))?;
        for place in ["a", "b"] {
            let dir = self.base.join(&name).join(place);
            let dir = dir
                .to_str()
                .ok_or_else(|| Failure("a directory is not UTF-8".into()))?;
            dirs.push(dir.to_owned());
        }

        Ok(Place {
            runtime,
            registry,
            top,
            dirs,
        })
    }

    fn try_run(&self, check: &Check, seed: u64) -> Outcome {
        let Place {
            runtime,
            registry,
            top,
            dirs,
        } = self.prepare(seed)?;
        let outcome = self.host.block_on((check.run)(Env {
            implementation: Implementation::Native,
            runtime: runtime.clone(),
            registry,
            seed,
            dirs,
        }));
        // Removing the directory is housekeeping, not part of the check: a
        // directory left behind changes no outcome.
        let _ = self.host.block_on(remove_tree(&runtime, top));
        outcome
    }
}

/// What [`NativeHarness::prepare`] makes for one use.
struct Place {
    runtime: Runtime,
    registry: Registry,
    /// The directory of its own, as `disk-io` names it.
    top: RtPath,
    /// The absolute native paths of `top/a` and `top/b`.
    dirs: Vec<String>,
}

impl NativeHarness {
    /// Whether this process can create a `Private` file through `disk-io`
    /// (ARFC 0005 clause 3.1.15). Only a Windows process whose token may not
    /// name `Administrators` as owner cannot: the create returns
    /// `PermissionDenied` and leaves nothing at the path. Any other result
    /// is a failure.
    fn can_create_private(&self) -> Result<bool, Failure> {
        let Place { runtime, top, .. } = self.prepare(seed_of("can_create_private"))?;
        let file = top
            .join("a")
            .and_then(|dir| dir.join("private"))
            .map_err(|e| Failure(format!("{e}")))?;
        let created = self
            .host
            .block_on(runtime.disk_io().create_file(&file, Protection::Private));
        let answer = match created {
            Ok(handle) => {
                drop(handle);
                Ok(true)
            }
            Err(DiskError::PermissionDenied) if cfg!(windows) => {
                match self.host.block_on(runtime.disk_io().metadata(&file)) {
                    Err(DiskError::NotFound) => Ok(false),
                    found => Err(Failure(format!(
                        "a refused Private create left an entry at its path: {found:?}"
                    ))),
                }
            }
            Err(e) => Err(Failure(format!("creating a Private file: {e}"))),
        };
        // Housekeeping, as in `try_run`.
        let _ = self.host.block_on(remove_tree(&runtime, top));
        answer
    }

    /// For a process that cannot create a `Private` file: building `native`
    /// on an existing, empty directory fails with `BuildError::Failed`, and
    /// the directory is still empty afterwards (ARFC 0005 clause 3.1.15,
    /// ARFC 0006 clause 5.2).
    fn check_refusal(&self) -> Outcome {
        let Place {
            runtime,
            registry,
            top,
            dirs,
        } = self.prepare(seed_of("check_refusal"))?;
        let outcome = self.host.block_on(async {
            let dir = dirs.first().ok_or_else(|| Failure("no directory".into()))?;
            // `NativeStoreConfig { dir }` in version 1 (ARFC 0006 clause 5.2).
            let mut payload = vec![0x0A];
            let mut len = dir.len() as u64;
            while len >= 0x80 {
                payload.push((len as u8) | 0x80);
                len >>= 7;
            }
            payload.push(len as u8);
            payload.extend_from_slice(dir.as_bytes());
            let config = ConfigValue::new(1, payload).map_err(|e| Failure(format!("{e}")))?;
            let native = ImplName::new("native").map_err(|e| Failure(format!("{e}")))?;
            match registry.instantiate::<StateStoreSeam>(&runtime, &native, &config) {
                Err(BuildError::Failed(_)) => {}
                Err(other) => {
                    return Err(Failure(format!(
                        "native without Private: got {other:?}, expected Failed"
                    )));
                }
                Ok(_) => {
                    return Err(Failure(
                        "native built although this process cannot create a Private file".into(),
                    ));
                }
            }
            let a = top.join("a").map_err(|e| Failure(format!("{e}")))?;
            let left = runtime
                .disk_io()
                .list(&a)
                .await
                .map_err(|e| Failure(format!("listing the directory: {e}")))?;
            if left.is_empty() {
                Ok(())
            } else {
                let names: Vec<_> = left.into_iter().map(|entry| entry.name).collect();
                Err(Failure(format!(
                    "a refused native build left entries in its directory: {names:?}"
                )))
            }
        });
        // Housekeeping, as in `try_run`.
        let _ = self.host.block_on(remove_tree(&runtime, top));
        outcome
    }
}

impl Harness for NativeHarness {
    fn implementation(&self) -> Implementation {
        Implementation::Native
    }

    fn run(&self, check: &Check) -> Outcome {
        self.try_run(check, seed_of(check.name))
    }
}

/// Removes `path` and everything under it.
async fn remove_tree(runtime: &Runtime, path: RtPath) -> Result<(), DiskError> {
    let mut stack = vec![(path, false)];
    while let Some((path, listed)) = stack.pop() {
        if listed {
            runtime.disk_io().remove(&path).await?;
            continue;
        }
        let entries = runtime.disk_io().list(&path).await?;
        stack.push((path.clone(), true));
        for entry in entries {
            let child = path.join(&entry.name)?;
            if entry.kind == alloy_rt::EntryKind::Dir {
                stack.push((child, false));
            } else {
                runtime.disk_io().remove(&child).await?;
            }
        }
    }
    Ok(())
}

/// Runs the suite on `harness` and returns each failure with its seed.
fn failures(harness: &dyn Harness) -> Vec<String> {
    let results = run_suite(harness);
    assert!(!results.is_empty(), "no check applies");
    println!("{} checks on {:?}", results.len(), harness.implementation());
    results
        .into_iter()
        .filter_map(|(name, outcome)| {
            outcome
                .err()
                .map(|failure| format!("{name} (seed {:#x}): {failure}", seed_of(name)))
        })
        .collect()
}

#[test]
fn mem_conforms_to_the_state_store_suite() {
    let failures = failures(&MemHarness);
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn native_conforms_to_the_state_store_suite() {
    let harness = NativeHarness::new();
    let private = harness.can_create_private().expect("the Private probe");
    if private {
        let failures = failures(&harness);
        assert!(failures.is_empty(), "{failures:#?}");
        return;
    }
    // ARFC 0005 clause 3.1.15 and D-0062: `native` refuses to build here.
    if let Err(failure) = harness.check_refusal() {
        panic!("{failure}");
    }
    let skipped = checks()
        .into_iter()
        .filter(|check| check.applies.includes(Implementation::Native))
        .count();
    // Straight to the handle, so the test harness does not capture the line.
    writeln!(
        std::io::stderr(),
        "state-store: the {skipped} native checks did not run: this process may not name \
         Administrators as owner, so native refuses to build (ARFC 0005 clause 3.1.15, \
         D-0062); that refusal was checked instead"
    )
    .expect("stderr takes the skip line");
}
