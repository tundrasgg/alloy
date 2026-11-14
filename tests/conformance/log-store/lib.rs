//! Conformance suite for the `log-store` seam (ARFC 0006 sections 5.2, 5.3,
//! and 5.5).
//!
//! The suite is generic over the `LogStore` trait. Every implementation runs
//! the same checks: `mem` on the simulator and `native` on the CI host in a
//! temporary directory (ARFC 0006 section 5.5). A [`Harness`] for one
//! implementation builds an [`Env`] for each [`Check`], drives the check's
//! future to the end, and returns its [`Outcome`]. [`run_suite`] runs every
//! check that applies to the harness's implementation. A check of a clause
//! that names one implementation's configuration runs only on that
//! implementation.
//!
//! Each check is named after the clause it checks, for example
//! `arfc0006_s5_3_4_gap_fails_not_contiguous` for ARFC 0006 clause 5.3.4.
//! Checks build every instance from [`Env::registry`] with a configuration
//! the suite encodes itself from section 5.2, so the configuration format is
//! checked too.
//!
//! The kill of a `native` child process during a stream of appends (section
//! 5.5) needs a second process, which only the harness can start. [`crash`]
//! holds both halves: what the child runs and what the parent checks after
//! the kill.

#![forbid(unsafe_code)]

use std::fmt;

use alloy_rt::{BoxFuture, Runtime};
use alloy_seam::Registry;

/// Returns a failure with the formatted text when `cond` is false.
macro_rules! ensure {
    ($cond:expr, $($fmt:tt)+) => {
        if !$cond {
            return Err($crate::Failure(format!($($fmt)+)));
        }
    };
}

/// Returns a failure naming both values when they differ.
macro_rules! ensure_eq {
    ($left:expr, $right:expr, $($fmt:tt)+) => {{
        let left = &$left;
        let right = &$right;
        if left != right {
            return Err($crate::Failure(format!(
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
            run: |env| -> alloy_rt::BoxFuture<'static, $crate::Outcome> { Box::pin($name(env)) },
        }
    };
}

/// The seam items under test, from `alloy-state` (ARFC 0006 section 5.1).
pub use alloy_state as state;

mod config;
pub mod crash;
mod log;
mod model;
mod support;

/// The implementation a harness runs the suite against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Implementation {
    /// The `redb` implementation, `native`, on the CI host.
    Native,
    /// The in-memory implementation, `mem`.
    Mem,
}

/// The implementations a check runs on.
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
/// stores. The requirements below are part of the suite's contract: a check
/// fails when the harness breaks them.
#[derive(Clone, Debug)]
pub struct Env {
    /// The implementation under test.
    pub implementation: Implementation,
    /// The context every factory receives. For `mem` it is the runtime of a
    /// fresh simulated host, and the check runs as one of its tasks.
    pub runtime: Runtime,
    /// A registry built for this check alone, holding the implementation
    /// under test for kind `log-store`, registered with `register_native`
    /// or `register_mem`. For `mem`, the registry comes from one fresh
    /// `register_mem` call, so no name is in use when the check starts.
    pub registry: Registry,
    /// The seed the check derives its data from.
    pub seed: u64,
    /// For `native`: at least two absolute native paths of existing, empty
    /// directories, used by this check alone. Empty for `mem`.
    pub dirs: Vec<String>,
}

/// Why a check did not pass: the implementation broke the clause the check
/// is named after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure(pub String);

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed: {}", self.0)
    }
}

/// The result of one check.
pub type Outcome = Result<(), Failure>;

/// One conformance check.
#[derive(Clone, Copy, Debug)]
pub struct Check {
    /// The check's name, which names its clause:
    /// `arfc0006_s<clause>_<behavior>`.
    pub name: &'static str,
    /// The clause checked, as `arfc-0006#<clause>`.
    pub clause: &'static str,
    /// The implementations the check runs on.
    pub applies: Applies,
    /// Runs the check in `env`.
    pub run: fn(Env) -> BoxFuture<'static, Outcome>,
}

/// Runs checks against one implementation.
pub trait Harness {
    /// The implementation this harness runs.
    fn implementation(&self) -> Implementation;
    /// Builds a fresh [`Env`], runs `check` in it to the end, and returns its
    /// outcome.
    fn run(&self, check: &Check) -> Outcome;
}

/// Every check of the suite, sorted by name.
pub fn checks() -> Vec<Check> {
    let mut all = Vec::new();
    all.extend(log::checks());
    all.extend(config::checks());
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

    /// The name prefix a clause gives: `arfc-0006#5.3.4` is
    /// `arfc0006_s5_3_4_`.
    fn prefix(clause: &str) -> String {
        let section = clause
            .strip_prefix("arfc-0006#")
            .expect("every clause is in ARFC 0006");
        format!("arfc0006_s{}_", section.replace('.', "_"))
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
    fn every_clause_of_section_5_3_has_a_check() {
        let clauses: BTreeSet<&str> = checks().iter().map(|check| check.clause).collect();
        for clause in [
            "arfc-0006#5.3.1",
            "arfc-0006#5.3.2",
            "arfc-0006#5.3.3",
            "arfc-0006#5.3.4",
            "arfc-0006#5.3.5",
            "arfc-0006#5.3.6",
            "arfc-0006#5.3.7",
            "arfc-0006#5.3.8",
            "arfc-0006#5.2",
        ] {
            assert!(clauses.contains(clause), "no check for {clause}");
        }
    }

    #[test]
    fn applicability_filters_by_implementation() {
        assert!(Applies::All.includes(Implementation::Native));
        assert!(Applies::All.includes(Implementation::Mem));
        assert!(Applies::Only(Implementation::Mem).includes(Implementation::Mem));
        assert!(!Applies::Only(Implementation::Mem).includes(Implementation::Native));
        assert!(!Applies::Only(Implementation::Native).includes(Implementation::Mem));
    }
}
