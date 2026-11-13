//! Repository automation for Alloy, run as `cargo xtask <task>`.
//!
//! `cargo xtask ci` is the single quality gate: nothing merges unless it exits 0.
//! It runs formatting, lints, tests, the dependency allowlist, and the repository
//! policy checks, in that order, and stops at the first step that fails.

#![forbid(unsafe_code)]
// xtask is the repository's build tool, not part of Alloy: it runs cargo and
// git and reads the tree, so it calls the operating system directly. The
// `direct-io` ban covers the crates under `crates/` (ARFC 0005 clause 3.1.1).
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "xtask is the build tool, outside the runtime seams; it runs cargo and git"
)]

#[cfg(test)]
mod clippy_fixture;
mod commits;
mod rules;
mod text;
mod tree;

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use rules::BranchDiff;
use tree::Tree;

const USAGE: &str = "\
usage: cargo xtask <task> [options]

tasks:
  ci          run every check of the quality gate, stopping at the first failure
  check-repo  run the repository policy checks
  check-commits <rev-range>
              check the messages of the commits in <rev-range>, along the first
              parent, against the format for commits on main
  help        print this message

options for ci and check-repo:
  --base <ref>     also run the path lock on the changes since <ref>
  --branch <name>  the branch the path lock judges, when HEAD is detached (needs --base)";

/// Exit code for a command line that names no task or an unknown task.
const EXIT_USAGE: u8 = 2;

/// Files under this path are rule fixtures that break the rules on purpose.
const FIXTURES: &str = "xtask/tests/fixtures/";

/// A task that `cargo xtask` can run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Task {
    Ci(Options),
    CheckRepo(Options),
    /// Checks the commits in a git revision range.
    CheckCommits(String),
    Help,
}

/// Options shared by `ci` and `check-repo`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Options {
    /// The ref the path lock compares against. Without it the path lock is skipped.
    base: Option<String>,
    /// The branch the path lock judges. Defaults to the branch HEAD is on.
    branch: Option<String>,
}

/// Why a command line could not be parsed into a [`Task`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum UsageError {
    /// No task was given.
    Missing,
    /// The first argument is not a known task.
    Unknown(String),
    /// A task was given an argument it does not accept, or an option twice.
    Extra(String),
    /// An option was given without its value.
    MissingValue(&'static str),
    /// An option value that is empty or that git would read as an option.
    BadValue(&'static str, String),
    /// `--branch` was given without `--base`.
    BranchWithoutBase,
    /// `check-commits` was given no revision range.
    MissingRange,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "no task given"),
            Self::Unknown(task) => write!(f, "unknown task `{task}`"),
            Self::Extra(arg) => write!(f, "unexpected argument `{arg}`"),
            Self::MissingValue(flag) => write!(f, "`{flag}` needs a value"),
            Self::BadValue(flag, value) => write!(f, "`{flag}` cannot be `{value}`"),
            Self::BranchWithoutBase => write!(f, "`--branch` needs `--base`"),
            Self::MissingRange => write!(f, "`check-commits` needs a revision range"),
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
    match first.to_str() {
        Some("ci") => parse_options(args).map(Task::Ci),
        Some("check-repo") => parse_options(args).map(Task::CheckRepo),
        Some("check-commits") => parse_range(args).map(Task::CheckCommits),
        Some("help" | "-h" | "--help") => match args.next() {
            Some(extra) => Err(UsageError::Extra(extra.to_string_lossy().into_owned())),
            None => Ok(Task::Help),
        },
        _ => Err(UsageError::Unknown(first.to_string_lossy().into_owned())),
    }
}

/// Parses `--base <ref>` and `--branch <name>`, each at most once.
fn parse_options<I>(mut args: I) -> Result<Options, UsageError>
where
    I: Iterator<Item = OsString>,
{
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        let (flag, slot) = match arg.to_str() {
            Some("--base") if options.base.is_none() => ("--base", &mut options.base),
            Some("--branch") if options.branch.is_none() => ("--branch", &mut options.branch),
            _ => return Err(UsageError::Extra(arg.to_string_lossy().into_owned())),
        };
        let value = args.next().ok_or(UsageError::MissingValue(flag))?;
        let value = value.to_string_lossy().into_owned();
        if value.is_empty() || value.starts_with('-') {
            return Err(UsageError::BadValue(flag, value));
        }
        *slot = Some(value);
    }
    if options.branch.is_some() && options.base.is_none() {
        return Err(UsageError::BranchWithoutBase);
    }
    Ok(options)
}

/// Parses the one revision range `check-commits` takes.
fn parse_range<I>(mut args: I) -> Result<String, UsageError>
where
    I: Iterator<Item = OsString>,
{
    let range = args.next().ok_or(UsageError::MissingRange)?;
    let range = range.to_string_lossy().into_owned();
    if range.is_empty() || range.starts_with('-') {
        return Err(UsageError::BadValue("check-commits", range));
    }
    match args.next() {
        Some(extra) => Err(UsageError::Extra(extra.to_string_lossy().into_owned())),
        None => Ok(range),
    }
}

/// A cargo subcommand that is installed separately from cargo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tool {
    /// Arguments to cargo that succeed exactly when the tool can be started.
    probe: &'static [&'static str],
    /// Printed when the step fails and the probe fails too.
    install: &'static str,
}

/// One step of the quality gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Runs `cargo` with the given arguments from the repository root. The step
    /// passes when cargo exits 0.
    Cargo {
        name: &'static str,
        args: &'static [&'static str],
        /// Printed when the step fails, to point at the usual cause.
        hint: Option<&'static str>,
        /// The separately installed tool the step runs, if any.
        tool: Option<Tool>,
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
const CI_STEPS: [Step; 6] = [
    Step::Cargo {
        name: "fmt",
        args: &["fmt", "--all", "--", "--check"],
        hint: Some("run `cargo fmt --all` to format the workspace"),
        tool: None,
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
            "-D",
            "clippy::disallowed_methods",
            "-D",
            "clippy::disallowed_types",
        ],
        hint: None,
        tool: None,
    },
    // The direct-io ban at forbid level, which no attribute in source can lower
    // (rustc rejects such an attribute with E0453), in every crate but the two
    // runtime crates and this one, which opt out with an `allow`. `--no-deps`
    // keeps the forbid off the excluded crates when they build as dependencies;
    // the `clippy` step lints them.
    Step::Cargo {
        name: "clippy-forbid",
        args: &[
            "clippy",
            "--workspace",
            "--exclude",
            "alloy-rt",
            "--exclude",
            "alloy-sim",
            "--exclude",
            "xtask",
            "--no-deps",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
            "-F",
            "clippy::disallowed_methods",
            "-F",
            "clippy::disallowed_types",
        ],
        hint: None,
        tool: None,
    },
    Step::Cargo {
        name: "test",
        args: &["test", "--workspace", "--all-features", "--locked"],
        hint: None,
        tool: None,
    },
    Step::Cargo {
        name: "deny",
        args: &["deny", "--locked", "--workspace", "check"],
        hint: None,
        tool: Some(Tool {
            probe: &["deny", "--version"],
            install: "this step needs cargo-deny: `cargo install --locked cargo-deny`",
        }),
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

/// The repository root: the parent of this crate's directory, fixed at build
/// time, so that every task gives the same result from any working directory.
fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

/// Runs one step for real: cargo steps as child processes, check-repo in process.
fn exec_step(step: &Step, root: &Path, options: &Options) -> Outcome {
    match step {
        Step::Cargo {
            args, hint, tool, ..
        } => run_cargo(root, args, *hint, *tool),
        Step::CheckRepo => check_repo(root, options),
    }
}

/// The cargo that invoked this program, so that every step uses the toolchain
/// pinned in `rust-toolchain.toml`.
fn cargo() -> Command {
    Command::new(env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
}

/// Runs cargo with `args` in `root`. On failure, adds `hint`, and adds the
/// install hint of `tool` only when the tool itself cannot be started.
fn run_cargo(root: &Path, args: &[&str], hint: Option<&str>, tool: Option<Tool>) -> Outcome {
    let command = format!("cargo {}", args.join(" "));
    let mut detail = match cargo().current_dir(root).args(args).status() {
        Ok(status) if status.success() => return Outcome::Passed,
        Ok(status) => format!("`{command}` exited with {status}"),
        Err(err) => format!("could not start `{command}`: {err}"),
    };
    if let Some(hint) = hint {
        detail = format!("{detail}; {hint}");
    }
    if let Some(tool) = tool {
        let starts = cargo()
            .current_dir(root)
            .args(tool.probe)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !starts {
            detail = format!("{detail}; {}", tool.install);
        }
    }
    Outcome::Failed(detail)
}

/// The repository policy checks: every rule in [`rules`], over the files git
/// would commit, leaving out the rule fixtures.
fn check_repo(root: &Path, options: &Options) -> Outcome {
    let tree = match Tree::from_git(root, &[FIXTURES]) {
        Ok(tree) => tree,
        Err(err) => return Outcome::Failed(err),
    };
    let diff = match &options.base {
        Some(base) => match branch_diff(root, base, options.branch.as_deref()) {
            Ok(diff) => Some(diff),
            Err(err) => return Outcome::Failed(err),
        },
        None => {
            eprintln!("xtask: path-lock skipped: no `--base` given");
            None
        }
    };
    let violations = rules::check_all(&tree, diff.as_ref());
    for violation in &violations {
        eprintln!("{violation}");
    }
    match violations.len() {
        0 => Outcome::Passed,
        n => Outcome::Failed(format!("{n} rule violation(s)")),
    }
}

/// The branch the path lock judges, the paths it changes since its merge base
/// with `base`, and its task file as it is on `base`.
fn branch_diff(root: &Path, base: &str, branch: Option<&str>) -> Result<BranchDiff, String> {
    let branch = match branch {
        Some(branch) => branch.to_owned(),
        None => tree::git(root, &["symbolic-ref", "--short", "-q", "HEAD"])
            .map(|name| name.trim().to_owned())
            .map_err(|_| "HEAD is not on a branch; pass `--branch <name>`".to_owned())?,
    };
    let range = format!("{base}...HEAD");
    let listing = tree::git(
        root,
        &["diff", "--name-only", "--no-renames", "-z", &range, "--"],
    )?;
    let changed = listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    let base_task = match rules::task_file(&branch) {
        Some(path) => tree::file_at(root, base, &path)?,
        None => None,
    };
    Ok(BranchDiff {
        branch,
        changed,
        base_task,
    })
}

/// Checks the commits in `range` and prints each breach with its commit id.
fn check_commits(root: &Path, range: &str) -> Result<(), Failure> {
    let failure = |detail| Failure {
        step: "check-commits",
        detail,
    };
    let verdicts = commits::check_range(root, range).map_err(failure)?;
    let mut breaches = 0;
    for verdict in &verdicts {
        for breach in &verdict.breaches {
            eprintln!("{}: {}: {}", verdict.id, breach.rule.name(), breach.message);
            breaches += 1;
        }
    }
    eprintln!("xtask: checked {} commit(s) in `{range}`", verdicts.len());
    match breaches {
        0 => Ok(()),
        n => Err(failure(format!("{n} commit rule violation(s)"))),
    }
}

fn main() -> ExitCode {
    let task = match parse(env::args_os().skip(1)) {
        Ok(task) => task,
        Err(err) => {
            eprintln!("xtask: {err}\n\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let root = repo_root();
    let result = match task {
        Task::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Task::Ci(options) => run_steps(&CI_STEPS, |step| exec_step(step, &root, &options)),
        Task::CheckRepo(options) => {
            run_steps(&[Step::CheckRepo], |step| exec_step(step, &root, &options))
        }
        Task::CheckCommits(range) => check_commits(&root, &range),
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

    fn opts(base: Option<&str>, branch: Option<&str>) -> Options {
        Options {
            base: base.map(str::to_owned),
            branch: branch.map(str::to_owned),
        }
    }

    #[test]
    fn parses_each_task() {
        assert_eq!(parse(args(&["ci"])), Ok(Task::Ci(Options::default())));
        assert_eq!(
            parse(args(&["check-repo"])),
            Ok(Task::CheckRepo(Options::default()))
        );
        assert_eq!(parse(args(&["help"])), Ok(Task::Help));
        assert_eq!(parse(args(&["--help"])), Ok(Task::Help));
        assert_eq!(parse(args(&["-h"])), Ok(Task::Help));
        assert_eq!(
            parse(args(&["check-commits", "40c70e9..main"])),
            Ok(Task::CheckCommits("40c70e9..main".to_owned()))
        );
    }

    #[test]
    fn check_commits_takes_exactly_one_range() {
        assert_eq!(
            parse(args(&["check-commits"])),
            Err(UsageError::MissingRange)
        );
        assert_eq!(
            parse(args(&["check-commits", "--all"])),
            Err(UsageError::BadValue("check-commits", "--all".to_owned()))
        );
        assert_eq!(
            parse(args(&["check-commits", "a..b", "c"])),
            Err(UsageError::Extra("c".to_owned()))
        );
    }

    #[test]
    fn parses_base_and_branch_options() {
        assert_eq!(
            parse(args(&["check-repo", "--base", "main"])),
            Ok(Task::CheckRepo(opts(Some("main"), None)))
        );
        assert_eq!(
            parse(args(&[
                "ci",
                "--branch",
                "task/T-0002",
                "--base",
                "origin/main"
            ])),
            Ok(Task::Ci(opts(Some("origin/main"), Some("task/T-0002"))))
        );
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
        assert_eq!(
            parse(args(&["help", "ci"])),
            Err(UsageError::Extra("ci".to_owned()))
        );
    }

    #[test]
    fn rejects_bad_option_use() {
        assert_eq!(
            parse(args(&["ci", "--base"])),
            Err(UsageError::MissingValue("--base"))
        );
        assert_eq!(
            parse(args(&["ci", "--base", "--output=x"])),
            Err(UsageError::BadValue("--base", "--output=x".to_owned()))
        );
        assert_eq!(
            parse(args(&["ci", "--base", "a", "--base", "b"])),
            Err(UsageError::Extra("--base".to_owned()))
        );
        assert_eq!(
            parse(args(&["check-repo", "--branch", "x"])),
            Err(UsageError::BranchWithoutBase)
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_is_total_over_non_utf8_arguments() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0x66, 0xff, 0x6f]);
        assert!(matches!(
            parse(vec![bad.clone()]),
            Err(UsageError::Unknown(_))
        ));
        let opts = vec![OsString::from("ci"), bad];
        assert!(matches!(parse(opts), Err(UsageError::Extra(_))));
    }

    #[cfg(windows)]
    #[test]
    fn parse_is_total_over_non_utf8_arguments() {
        use std::os::windows::ffi::OsStringExt;
        let bad = OsString::from_wide(&[0x66, 0xd800, 0x6f]);
        assert!(matches!(
            parse(vec![bad.clone()]),
            Err(UsageError::Unknown(_))
        ));
        let opts = vec![OsString::from("ci"), bad];
        assert!(matches!(parse(opts), Err(UsageError::Extra(_))));
    }

    #[test]
    fn ci_runs_the_gate_in_the_specified_order() {
        let names: Vec<_> = CI_STEPS.iter().map(Step::name).collect();
        assert_eq!(
            names,
            [
                "fmt",
                "clippy",
                "clippy-forbid",
                "test",
                "deny",
                "check-repo"
            ]
        );
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
        assert!(cargo_args[1].ends_with(&[
            "--",
            "-D",
            "warnings",
            "-D",
            "clippy::disallowed_methods",
            "-D",
            "clippy::disallowed_types"
        ]));
        assert_eq!(cargo_args[2][0], "clippy");
        assert!(cargo_args[2].ends_with(&[
            "--",
            "-D",
            "warnings",
            "-F",
            "clippy::disallowed_methods",
            "-F",
            "clippy::disallowed_types"
        ]));
        assert_eq!(cargo_args[3][..2], ["test", "--workspace"]);
        assert_eq!(cargo_args[4], ["deny", "--locked", "--workspace", "check"]);
    }

    /// The forbid step leaves out exactly the crates that opt out of the
    /// direct-io ban: the runtime crates `check-repo` exempts, and xtask.
    #[test]
    fn the_forbid_step_excludes_only_the_crates_that_opt_out() {
        let args = CI_STEPS
            .iter()
            .find_map(|step| match step {
                Step::Cargo {
                    name: "clippy-forbid",
                    args,
                    ..
                } => Some(*args),
                _ => None,
            })
            .expect("the gate has a `clippy-forbid` step");
        let excluded: Vec<&str> = args
            .windows(2)
            .filter(|pair| pair[0] == "--exclude")
            .map(|pair| pair[1])
            .collect();
        let mut expected = rules::IO_CRATES.to_vec();
        expected.push("xtask");
        assert_eq!(excluded, expected);
        assert!(args.contains(&"--workspace") && args.contains(&"--no-deps"));
    }

    #[test]
    fn only_the_deny_step_has_an_install_hint_and_it_is_behind_a_probe() {
        for step in CI_STEPS {
            if let Step::Cargo {
                name, hint, tool, ..
            } = step
            {
                let mentions_install = hint.is_some_and(|h| h.contains("cargo install"));
                assert!(
                    !mentions_install,
                    "{name} prints an install hint on every failure"
                );
                assert_eq!(tool.is_some(), name == "deny", "{name}");
            }
        }
    }

    #[test]
    fn repo_root_holds_the_workspace_manifest_and_this_crate() {
        let root = repo_root();
        assert!(root.join("Cargo.toml").is_file());
        assert!(root.join("xtask/src/main.rs").is_file());
    }

    #[test]
    fn run_steps_runs_every_step_when_all_pass() {
        let mut ran = Vec::new();
        let result = run_steps(&CI_STEPS, |step| {
            ran.push(step.name());
            Outcome::Passed
        });
        assert_eq!(result, Ok(()));
        assert_eq!(
            ran,
            [
                "fmt",
                "clippy",
                "clippy-forbid",
                "test",
                "deny",
                "check-repo"
            ]
        );
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
}
