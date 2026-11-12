//! Repository automation for Alloy, run as `cargo xtask <task>`.
//!
//! `cargo xtask ci` is the single quality gate: nothing merges unless it exits 0.
//! It runs formatting, lints, tests, the dependency allowlist, and the repository
//! policy checks, in that order, and stops at the first step that fails.

#![forbid(unsafe_code)]

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::process::{Command, ExitCode};

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  ci          run every check of the quality gate, stopping at the first failure
  check-repo  run the repository policy checks
  help        print this message";

/// Exit code for a command line that names no task or an unknown task.
const EXIT_USAGE: u8 = 2;

/// A task that `cargo xtask` can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Task {
    Ci,
    CheckRepo,
    Help,
}

/// Why a command line could not be parsed into a [`Task`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum UsageError {
    /// No task was given.
    Missing,
    /// The first argument is not a known task.
    Unknown(String),
    /// A task was given more arguments than it accepts.
    Extra(String),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "no task given"),
            Self::Unknown(task) => write!(f, "unknown task `{task}`"),
            Self::Extra(arg) => write!(f, "unexpected argument `{arg}`"),
        }
    }
}

/// Parses the arguments that follow the program name.
///
/// Total over any input, including arguments that are not valid UTF-8.
fn parse<I>(args: I) -> Result<Task, UsageError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let first = args.next().ok_or(UsageError::Missing)?;
    let task = match first.to_str() {
        Some("ci") => Task::Ci,
        Some("check-repo") => Task::CheckRepo,
        Some("help" | "-h" | "--help") => Task::Help,
        _ => return Err(UsageError::Unknown(first.to_string_lossy().into_owned())),
    };
    match args.next() {
        Some(extra) => Err(UsageError::Extra(extra.to_string_lossy().into_owned())),
        None => Ok(task),
    }
}

/// One step of the quality gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Runs `cargo` with the given arguments. The step passes when cargo exits 0.
    Cargo {
        name: &'static str,
        args: &'static [&'static str],
        /// Printed when the step fails, to point at the usual cause.
        hint: Option<&'static str>,
    },
    /// Runs the repository policy checks in this process.
    CheckRepo,
}

impl Step {
    fn name(&self) -> &'static str {
        match self {
            Self::Cargo { name, .. } => name,
            Self::CheckRepo => "check-repo",
        }
    }
}

/// The quality gate, in the order the steps run.
const CI_STEPS: [Step; 5] = [
    Step::Cargo {
        name: "fmt",
        args: &["fmt", "--all", "--", "--check"],
        hint: Some("run `cargo fmt --all` to format the workspace"),
    },
    Step::Cargo {
        name: "clippy",
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        hint: None,
    },
    Step::Cargo {
        name: "test",
        args: &["test", "--workspace", "--all-features", "--locked"],
        hint: None,
    },
    Step::Cargo {
        name: "deny",
        args: &["deny", "--locked", "check"],
        hint: Some("this step needs cargo-deny: `cargo install --locked cargo-deny`"),
    },
    Step::CheckRepo,
];

/// The result of running one step.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed(String),
}

/// The first step of a run that did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failure {
    step: &'static str,
    detail: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "step `{}` failed: {}", self.step, self.detail)
    }
}

/// Runs `steps` in order through `exec` and stops at the first one that fails.
///
/// Steps after a failing step are not run.
fn run_steps<F>(steps: &[Step], mut exec: F) -> Result<(), Failure>
where
    F: FnMut(&Step) -> Outcome,
{
    for step in steps {
        eprintln!("xtask: ==> {}", step.name());
        if let Outcome::Failed(detail) = exec(step) {
            return Err(Failure {
                step: step.name(),
                detail,
            });
        }
    }
    Ok(())
}

/// Runs one step for real: cargo steps as child processes, check-repo in process.
fn exec_step(step: &Step) -> Outcome {
    match step {
        Step::Cargo { args, hint, .. } => run_cargo(args, *hint),
        Step::CheckRepo => check_repo(),
    }
}

/// Runs the cargo that invoked this program, so that every step uses the
/// toolchain pinned in `rust-toolchain.toml`.
fn run_cargo(args: &[&str], hint: Option<&str>) -> Outcome {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let command = format!("cargo {}", args.join(" "));
    let detail = match Command::new(&cargo).args(args).status() {
        Ok(status) if status.success() => return Outcome::Passed,
        Ok(status) => format!("`{command}` exited with {status}"),
        Err(err) => format!("could not start `{command}`: {err}"),
    };
    match hint {
        Some(hint) => Outcome::Failed(format!("{detail}; {hint}")),
        None => Outcome::Failed(detail),
    }
}

/// The repository policy checks.
///
/// This is the stub T-0001 specifies: it defines no rules and passes. T-0002
/// replaces it with the rules of `agent/core/4-evidence.md` and the path lock.
fn check_repo() -> Outcome {
    eprintln!("xtask: check-repo defines no rules yet (T-0002 adds them)");
    Outcome::Passed
}

fn main() -> ExitCode {
    let task = match parse(env::args_os().skip(1)) {
        Ok(task) => task,
        Err(err) => {
            eprintln!("xtask: {err}\n\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let result = match task {
        Task::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Task::Ci => run_steps(&CI_STEPS, exec_step),
        Task::CheckRepo => run_steps(&[Step::CheckRepo], exec_step),
    };
    match result {
        Ok(()) => {
            eprintln!("xtask: all steps passed");
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("xtask: {failure}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_each_task() {
        assert_eq!(parse(args(&["ci"])), Ok(Task::Ci));
        assert_eq!(parse(args(&["check-repo"])), Ok(Task::CheckRepo));
        assert_eq!(parse(args(&["help"])), Ok(Task::Help));
        assert_eq!(parse(args(&["--help"])), Ok(Task::Help));
        assert_eq!(parse(args(&["-h"])), Ok(Task::Help));
    }

    #[test]
    fn rejects_missing_unknown_and_extra_arguments() {
        assert_eq!(parse(args(&[])), Err(UsageError::Missing));
        assert_eq!(
            parse(args(&["build"])),
            Err(UsageError::Unknown("build".to_owned()))
        );
        assert_eq!(
            parse(args(&["ci", "--fast"])),
            Err(UsageError::Extra("--fast".to_owned()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_is_total_over_non_utf8_arguments() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0x66, 0xff, 0x6f]);
        assert!(matches!(parse(vec![bad]), Err(UsageError::Unknown(_))));
    }

    #[cfg(windows)]
    #[test]
    fn parse_is_total_over_non_utf8_arguments() {
        use std::os::windows::ffi::OsStringExt;
        let bad = OsString::from_wide(&[0x66, 0xd800, 0x6f]);
        assert!(matches!(parse(vec![bad]), Err(UsageError::Unknown(_))));
    }

    #[test]
    fn ci_runs_the_gate_in_the_specified_order() {
        let names: Vec<_> = CI_STEPS.iter().map(Step::name).collect();
        assert_eq!(names, ["fmt", "clippy", "test", "deny", "check-repo"]);
    }

    #[test]
    fn ci_cargo_steps_use_the_specified_commands() {
        let cargo_args: Vec<_> = CI_STEPS
            .iter()
            .filter_map(|step| match step {
                Step::Cargo { args, .. } => Some(*args),
                Step::CheckRepo => None,
            })
            .collect();
        assert_eq!(cargo_args[0], ["fmt", "--all", "--", "--check"]);
        assert_eq!(cargo_args[1][0], "clippy");
        assert!(cargo_args[1].ends_with(&["--", "-D", "warnings"]));
        assert_eq!(cargo_args[2][..2], ["test", "--workspace"]);
        assert_eq!(cargo_args[3], ["deny", "--locked", "check"]);
    }

    #[test]
    fn run_steps_runs_every_step_when_all_pass() {
        let mut ran = Vec::new();
        let result = run_steps(&CI_STEPS, |step| {
            ran.push(step.name());
            Outcome::Passed
        });
        assert_eq!(result, Ok(()));
        assert_eq!(ran, ["fmt", "clippy", "test", "deny", "check-repo"]);
    }

    #[test]
    fn run_steps_stops_at_the_first_failure() {
        let mut ran = Vec::new();
        let result = run_steps(&CI_STEPS, |step| {
            ran.push(step.name());
            if step.name() == "clippy" {
                Outcome::Failed("lint".to_owned())
            } else {
                Outcome::Passed
            }
        });
        assert_eq!(
            result,
            Err(Failure {
                step: "clippy",
                detail: "lint".to_owned()
            })
        );
        assert_eq!(ran, ["fmt", "clippy"]);
    }

    #[test]
    fn check_repo_passes() {
        assert_eq!(check_repo(), Outcome::Passed);
    }
}
