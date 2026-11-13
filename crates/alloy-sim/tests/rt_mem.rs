//! Runs the runtime seam conformance suites of `tests/conformance/rt`
//! against the `mem` implementations of a simulated host, for the seams
//! `alloy-sim` implements in full: `clock`, `rng`, `spawn`, and the `Runtime`
//! built on them (ARFC 0005 section 5). The `mem` `net` lists only `lo` and
//! the `mem` `disk-io` refuses every call; T-0077 and T-0094 build them and
//! add their suites here.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_rt::{RtPath, RuntimeConfig};
use alloy_seam::{Detail, ImplName};
use alloy_sim::{Boot, Failure, HostBoot, HostConfig, RunConfig, RunResult, run};
use conformance_rt::{Check, Env, Failure as CheckFailure, Harness, Implementation, Outcome};

/// The clauses of the `clock`, `rng`, `spawn`, and `Runtime` checks.
const CLAUSES: [&str; 8] = [
    "arfc-0005#2.1",
    "arfc-0005#2.2",
    "arfc-0005#2.5",
    "arfc-0005#3.1.3",
    "arfc-0005#3.1.4",
    "arfc-0005#3.1.5",
    "arfc-0005#3.1.6",
    "arfc-0005#3.1.14",
];

/// The checks of those clauses that need the `disk-io` model, by full name.
/// No check of those clauses needs more of `net` than `lo`.
const NEEDS_DISK: [&str; 3] = [
    "arfc0005_s3_1_3_disk_methods_may_be_called_concurrently",
    "arfc0005_s3_1_3_hostile_arguments_return_without_panic",
    "arfc0005_s3_1_3_mem_never_returns_an_os_error",
];

/// Runs each check as the `boot` task of the one host of a fresh world.
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
        let summary = run(RunConfig::new(seed), move |world| async move {
            let filled = Arc::clone(&out);
            let boot: Boot = Arc::new(move |boot: HostBoot| {
                let slot = Arc::clone(&filled);
                Box::pin(async move {
                    let mem = ImplName::new("mem").expect("a valid name");
                    let env = Env {
                        implementation: Implementation::Mem,
                        runtime: boot.runtime,
                        registry: boot.registry.build(),
                        config: RuntimeConfig::all(mem),
                        seed,
                        scratch: RtPath::root(),
                        unix_dir: None,
                        block_device: None,
                        file_block_size: Some(4_096),
                    };
                    let outcome = run_check(env).await;
                    *slot.lock().expect("slot") = Some(outcome);
                })
            });
            world
                .add_host(HostConfig::new(boot))
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
fn mem_clock_rng_spawn_and_runtime_conform() {
    let all = conformance_rt::checks();
    for name in NEEDS_DISK {
        assert!(
            all.iter().any(|check| check.name == name),
            "no check named {name}"
        );
    }
    let checks: Vec<Check> = all
        .into_iter()
        .filter(|check| check.applies.includes(Implementation::Mem))
        .filter(|check| CLAUSES.contains(&check.clause) && !NEEDS_DISK.contains(&check.name))
        .collect();
    assert!(checks.len() >= 38, "{} checks", checks.len());
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
