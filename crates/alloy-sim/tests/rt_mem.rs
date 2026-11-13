//! Runs the runtime seam conformance suites of `tests/conformance/rt`
//! against the `mem` implementations of a simulated host, for the seams
//! `alloy-sim` implements in full: `clock`, `rng`, `spawn`, `disk-io`, and
//! the `Runtime` built on them (ARFC 0005 section 5). The `mem` `net` lists
//! only `lo`; T-0077 builds it and adds its suite here.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_rt::{RtPath, RuntimeConfig};
use alloy_seam::{Detail, ImplName};
use alloy_sim::{Boot, DeviceConfig, Failure, HostBoot, HostConfig, RunConfig, RunResult, run};
use conformance_rt::{Check, Env, Failure as CheckFailure, Harness, Implementation, Outcome};

/// The clauses of the `clock`, `rng`, `spawn`, `disk-io`, and `Runtime`
/// checks. No check of these clauses needs more of `net` than `lo`.
const CLAUSES: [&str; 12] = [
    "arfc-0005#2.1",
    "arfc-0005#2.2",
    "arfc-0005#2.4",
    "arfc-0005#2.5",
    "arfc-0005#3.1.3",
    "arfc-0005#3.1.4",
    "arfc-0005#3.1.5",
    "arfc-0005#3.1.6",
    "arfc-0005#3.1.10",
    "arfc-0005#3.1.11",
    "arfc-0005#3.1.13",
    "arfc-0005#3.1.14",
];

/// The block device the harness attaches: 1 MiB of 512-byte blocks.
const DEVICE: &str = "/dev/sim0";

/// Runs each check as the `boot` task of the one host of a fresh world,
/// with an empty scratch directory and a block device.
struct MemHarness;

impl Harness for MemHarness {
    fn implementation(&self) -> Implementation {
        Implementation::Mem
    }

    fn run(&self, check: &Check) -> Outcome {
        let seed = check.name.len() as u64;
        let run_check = check.run;
        let slot: Arc<Mutex<Option<Outcome>>> = Arc::default();
        let out = Arc::clone(&slot);
        let device = RtPath::new(DEVICE).expect("a valid path");
        let summary = run(RunConfig::new(seed), move |world| async move {
            let filled = Arc::clone(&out);
            let attached = device.clone();
            let boot: Boot = Arc::new(move |boot: HostBoot| {
                let slot = Arc::clone(&filled);
                let device = attached.clone();
                Box::pin(async move {
                    let mem = ImplName::new("mem").expect("a valid name");
                    let scratch = RtPath::new("/scratch").expect("a valid path");
                    let made = boot.runtime.disk_io().create_dir(&scratch).await;
                    let env = Env {
                        implementation: Implementation::Mem,
                        runtime: boot.runtime,
                        registry: boot.registry.build(),
                        config: RuntimeConfig::all(mem),
                        seed,
                        scratch,
                        unix_dir: None,
                        block_device: Some(device),
                        file_block_size: Some(4_096),
                    };
                    let outcome = match made {
                        Ok(()) => run_check(env).await,
                        Err(error) => Err(CheckFailure::Failed(format!("scratch: {error}"))),
                    };
                    *slot.lock().expect("slot") = Some(outcome);
                })
            });
            let mut config = HostConfig::new(boot);
            config.devices.push(DeviceConfig {
                path: device,
                len: 1 << 20,
                block_size: 512,
            });
            world
                .add_host(config)
                .map_err(|error| Failure(Detail::new(&error.to_string())))?;
            while out.lock().expect("slot").is_none() {
                world.sleep(Duration::from_secs(1)).await;
            }
            Ok(())
        });
        let outcome = slot.lock().expect("slot").take();
        match outcome {
            None => Err(CheckFailure::Failed(format!(
                "the check did not complete: {}",
                summary.line()
            ))),
            Some(outcome) if summary.result == RunResult::Pass || check.panics_a_task => outcome,
            Some(_) => Err(CheckFailure::Failed(format!(
                "the run failed: {:?}",
                summary.failure
            ))),
        }
    }
}

#[test]
fn mem_clock_rng_spawn_disk_io_and_runtime_conform() {
    let checks: Vec<Check> = conformance_rt::checks()
        .into_iter()
        .filter(|check| check.applies.includes(Implementation::Mem))
        .filter(|check| CLAUSES.contains(&check.clause))
        .collect();
    let disk = checks
        .iter()
        .filter(|check| check.name.contains("disk") || check.clause.contains("3.1.1"))
        .count();
    println!("{} checks, {disk} of the disk", checks.len());
    let failures: Vec<String> = checks
        .iter()
        .filter_map(|check| {
            MemHarness
                .run(check)
                .err()
                .map(|failure| format!("{}: {failure}", check.name))
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
}
