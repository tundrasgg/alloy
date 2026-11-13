//! Conformance suites for the five runtime seams of `alloy-rt`: `clock`,
//! `net`, `disk-io`, `rng`, and `spawn` (ARFC 0005 sections 2, 3.1, and 5).
//!
//! The suites are generic over the seam traits. Every implementation runs the
//! same checks: `mem` on the simulator and `os` on the CI host (ARFC 0005
//! section 5, ARFC 0001 clause 3.4). A [`Harness`] for one implementation
//! builds an [`Env`] for each [`Check`], drives the check's future to the
//! end, and returns its [`Outcome`]. [`run_suite`] runs every check that
//! applies to the harness's implementation. A check of a clause that names
//! one implementation runs only on that implementation.
//!
//! Each check is named after the clause it checks, for example
//! `arfc0005_s3_1_4_now_never_decreases` for ARFC 0005 clause 3.1.4. Items of
//! `alloy-rt` that need no seam instance (paths, buffers, task names,
//! defaults, error text, and `Debug` output) are checked by this crate's unit
//! tests, which run without a harness.
//!
//! Clauses of section 3.1 that depend on the simulated world (crashes, clock
//! faults, links, partitions, and clause 3.1.9.1) belong to the simulator's
//! suite, `tests/conformance/sim/` (ARFC 0005 section 5).

#![forbid(unsafe_code)]

use std::fmt;

use alloy_rt::{BoxFuture, RtPath, Runtime, RuntimeConfig};
use alloy_seam::Registry;

/// Returns a `Failed` outcome with the formatted text when `cond` is false.
macro_rules! ensure {
    ($cond:expr, $($fmt:tt)+) => {
        if !$cond {
            return Err($crate::Failure::Failed(format!($($fmt)+)));
        }
    };
}

/// Returns a `Failed` outcome naming both values when they differ.
macro_rules! ensure_eq {
    ($left:expr, $right:expr, $($fmt:tt)+) => {{
        let left = &$left;
        let right = &$right;
        if left != right {
            return Err($crate::Failure::Failed(format!(
                "{}: got {:?}, expected {:?}",
                format!($($fmt)+),
                left,
                right
            )));
        }
    }};
}

/// Builds the [`Check`] for an `async fn(Env) -> Outcome` named after its
/// clause.
macro_rules! check {
    ($clause:literal, $applies:expr, $name:ident) => {
        $crate::Check {
            name: stringify!($name),
            clause: $clause,
            applies: $applies,
            panics_a_task: false,
            run: |env| -> alloy_rt::BoxFuture<'static, $crate::Outcome> { Box::pin($name(env)) },
        }
    };
    ($clause:literal, $applies:expr, $name:ident, panics_a_task) => {
        $crate::Check {
            panics_a_task: true,
            ..check!($clause, $applies, $name)
        }
    };
}

mod support;

mod clock;
#[path = "disk-io/mod.rs"]
mod disk_io;
mod net;
mod rng;
mod runtime;
mod spawn;

/// The implementation a harness runs the suites against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Implementation {
    /// The operating system implementation, `os`.
    Os,
    /// The simulator's in-memory implementation, `mem`.
    Mem,
}

/// The implementations a check runs on (ARFC 0005 section 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applies {
    /// Every implementation.
    All,
    /// Only the named implementation, because its clause names it.
    Only(Implementation),
}

impl Applies {
    /// Whether a check with this applicability runs on `implementation`.
    pub fn includes(self, implementation: Implementation) -> bool {
        match self {
            Applies::All => true,
            Applies::Only(only) => only == implementation,
        }
    }
}

/// What a harness gives one check.
///
/// A harness builds a new `Env` for every check, so no check sees another's
/// sockets, files, or tasks. The requirements below are part of the suite's
/// contract: a check fails when the harness breaks them.
#[derive(Clone, Debug)]
pub struct Env {
    /// The implementation under test.
    pub implementation: Implementation,
    /// The runtime the check runs on, built from `registry` with `config`.
    /// For `mem` it is the runtime of a fresh simulated host with the default
    /// `HostConfig` and no faults, and the check runs as one of its tasks.
    pub runtime: Runtime,
    /// A registry that holds the implementation under test for the five
    /// runtime kinds. Checks build further instances from it.
    pub registry: Registry,
    /// Binds each of the five kinds to the implementation under test, with a
    /// configuration its factory accepts.
    pub config: RuntimeConfig,
    /// The seed the check derives its data from.
    pub seed: u64,
    /// An existing, empty directory in the `disk-io` namespace, used by this
    /// check alone.
    pub scratch: RtPath,
    /// An absolute native directory, used by this check alone, in which it
    /// may create Unix domain sockets. Every path the check makes is this
    /// directory, a `/`, and at most 8 more bytes, so it must stay within
    /// the 107 bytes of `UnixPath`. `None` when the platform has no Unix
    /// domain sockets.
    pub unix_dir: Option<String>,
    /// A block device of at least 1 MiB in the `disk-io` namespace that
    /// nothing else holds, all zeros. Required for `mem`, where the harness
    /// attaches one; `None` for `os` when the host has none to spare.
    pub block_device: Option<RtPath>,
    /// The `block_size` the harness configured for the file namespace, which
    /// is 4,096 for a default `mem` host. `None` when the harness does not
    /// set it, as for `os`.
    pub file_block_size: Option<u32>,
}

/// Why a check did not pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The implementation broke the clause the check is named after.
    Failed(String),
    /// The environment lacks what the check needs, such as a block device or
    /// an IPv6 loopback address. Never returned for `mem` with a harness that
    /// meets the requirements of [`Env`].
    Skipped(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Failed(text) => write!(f, "failed: {text}"),
            Failure::Skipped(text) => write!(f, "skipped: {text}"),
        }
    }
}

/// The result of one check.
pub type Outcome = Result<(), Failure>;

/// One conformance check.
#[derive(Clone, Copy, Debug)]
pub struct Check {
    /// The check's name, which names its clause:
    /// `arfc0005_s<clause>_<behavior>`.
    pub name: &'static str,
    /// The clause checked, as `arfc-0005#<clause>`.
    pub clause: &'static str,
    /// The implementations the check runs on.
    pub applies: Applies,
    /// The check panics one task on purpose to test clause 3.1.6. On `mem`
    /// a task panic makes the run's result `fail` (ARFC 0005 section 6), so a
    /// harness must not count that panic against the check; it still counts
    /// any other failure.
    pub panics_a_task: bool,
    /// Runs the check in `env`.
    pub run: fn(Env) -> BoxFuture<'static, Outcome>,
}

/// Runs checks against one implementation.
pub trait Harness {
    /// The implementation this harness runs.
    fn implementation(&self) -> Implementation;
    /// Builds a fresh [`Env`], runs `check` in it to the end, and returns its
    /// outcome. A check whose future never completes is a failure, which a
    /// `mem` harness reports through the run's virtual time budget.
    fn run(&self, check: &Check) -> Outcome;
}

/// Every check of the five suites, sorted by name.
pub fn checks() -> Vec<Check> {
    let mut all = Vec::new();
    all.extend(clock::checks());
    all.extend(rng::checks());
    all.extend(spawn::checks());
    all.extend(net::checks());
    all.extend(disk_io::checks());
    all.extend(runtime::checks());
    all.sort_by_key(|check| check.name);
    all
}

/// Runs every check that applies to `harness`'s implementation, in name
/// order, and returns each check's name with its outcome.
pub fn run_suite(harness: &dyn Harness) -> Vec<(&'static str, Outcome)> {
    let implementation = harness.implementation();
    checks()
        .into_iter()
        .filter(|check| check.applies.includes(implementation))
        .map(|check| (check.name, harness.run(&check)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The name prefix a clause gives: `arfc-0005#3.1.4` is
    /// `arfc0005_s3_1_4_`.
    fn prefix(clause: &str) -> String {
        let section = clause
            .strip_prefix("arfc-0005#")
            .expect("every clause is in ARFC 0005");
        format!("arfc0005_s{}_", section.replace('.', "_"))
    }

    #[test]
    fn every_check_is_named_after_its_clause() {
        let all = checks();
        assert!(!all.is_empty());
        for check in &all {
            assert!(
                check.name.starts_with(&prefix(check.clause)),
                "{} does not name its clause {}",
                check.name,
                check.clause
            );
        }
    }

    #[test]
    fn check_names_are_unique() {
        let all = checks();
        let names: BTreeSet<&str> = all.iter().map(|check| check.name).collect();
        assert_eq!(names.len(), all.len());
    }

    #[test]
    fn only_the_panic_checks_panic_a_task() {
        let panicking: Vec<&str> = checks()
            .iter()
            .filter(|check| check.panics_a_task)
            .map(|check| check.name)
            .collect();
        assert_eq!(
            panicking,
            [
                "arfc0005_s3_1_14_join_handle_returns_panicked",
                "arfc0005_s3_1_6_panicking_task_joins_panicked_and_others_run",
            ]
        );
    }

    #[test]
    fn applicability_filters_by_implementation() {
        assert!(Applies::All.includes(Implementation::Os));
        assert!(Applies::All.includes(Implementation::Mem));
        assert!(Applies::Only(Implementation::Mem).includes(Implementation::Mem));
        assert!(!Applies::Only(Implementation::Mem).includes(Implementation::Os));
        assert!(!Applies::Only(Implementation::Os).includes(Implementation::Mem));
    }
}
