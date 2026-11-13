//! The Clippy fixture for the `direct-io` ban (D-0017).
//!
//! `xtask/tests/fixtures/clippy/` is a Cargo workspace of its own, with
//! `crates/alloy-rt`, `crates/alloy-sim`, `crates/demo`, and
//! `crates/switches`. These tests run Clippy over it as the gate's two Clippy
//! steps run it, with the repository's `clippy.toml`, and compare what Clippy
//! reports with two files in the fixture:
//!
//! - `expected`: one `path:line: item` for each use of a disallowed method or
//!   type that the `clippy-forbid` step must report, and one
//!   `path:line: level(lint)` for each lint attribute it must reject as
//!   incompatible with the forbid. Only `demo` and `switches` have any: the
//!   runtime crates opt out.
//! - `unresolved`: the configured paths that resolve to nothing in the
//!   fixture's dependency graph, one per line.
//!
//! A line in either file may end with `unix` or `windows`: it then holds on
//! that platform family only.
//!
//! The runs read a copy of `clippy.toml` with every `allow-invalid = true`
//! removed, so that Clippy names every path that resolves to nothing, and a
//! misspelled path fails the `unresolved` comparison.
//!
//! The fixture's dependencies are local stand-ins (`stand-in/`) for the
//! third-party crates whose paths `clippy.toml` names, so the fixture builds
//! nothing from outside the repository. The build goes to
//! `target/xtask-fixtures/`.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{self, Command};
use std::sync::OnceLock;

use super::{CI_STEPS, Step, repo_root};

/// What one Clippy run over the fixture reported.
#[derive(Debug)]
struct Report {
    /// Whether cargo exited 0.
    success: bool,
    /// `path:line: item` for each use of a disallowed method or type, and
    /// `path:line: level(lint)` for each lint attribute rejected under forbid.
    found: BTreeSet<String>,
    /// The configured paths Clippy says resolve to nothing.
    unresolved: BTreeSet<String>,
    /// Everything Clippy and Cargo printed, for failure messages.
    output: String,
}

fn fixture() -> PathBuf {
    repo_root().join("xtask/tests/fixtures/clippy")
}

/// The arguments of the gate's cargo step `name`, split at `--` into the
/// arguments to cargo and the arguments to the lints.
fn step_args(name: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    CI_STEPS
        .iter()
        .find_map(|step| match step {
            Step::Cargo {
                name: step_name,
                args,
                ..
            } if *step_name == name => Some(*args),
            _ => None,
        })
        .and_then(|args| {
            let dashes = args.iter().position(|arg| *arg == "--")?;
            Some((args[..dashes].to_vec(), args[dashes + 1..].to_vec()))
        })
        .unwrap_or_else(|| panic!("the gate has a `{name}` step with lint arguments after `--`"))
}

/// `clippy.toml` with every `allow-invalid = true` removed.
fn strict_config(config: &str) -> String {
    config.replace(", allow-invalid = true", "")
}

/// A directory holding the strict copy of `clippy.toml`, removed when dropped.
struct ConfDir(PathBuf);

impl ConfDir {
    fn create(tag: &str) -> Self {
        let dir =
            Self(env::temp_dir().join(format!("alloy-xtask-clippy-conf-{}-{tag}", process::id())));
        let config = fs::read_to_string(repo_root().join("clippy.toml"))
            .expect("the repository has a clippy.toml");
        fs::create_dir_all(&dir.0).expect("temporary directory is writable");
        fs::write(dir.0.join("clippy.toml"), strict_config(&config))
            .expect("temporary file is writable");
        dir
    }
}

impl Drop for ConfDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The text between the first pair of backticks in `text`.
fn backticked(text: &str) -> Option<&str> {
    let (_, rest) = text.split_once('`')?;
    rest.split_once('`').map(|(inside, _)| inside)
}

/// `path:line` from the `path:line:col: level` before a message.
fn file_line(head: &str) -> Option<String> {
    let (location, _level) = head.rsplit_once(": ")?;
    let file_line = location.rsplit_once(':').map_or(location, |(fl, _col)| fl);
    Some(file_line.replace('\\', "/"))
}

/// Reads Clippy's `--message-format short` output.
fn parse(output: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut found = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    for line in output.lines() {
        if line.contains(" does not refer to a reachable ") {
            if let Some(path) = backticked(line) {
                unresolved.insert(path.to_owned());
            }
        } else if let Some(at) = line.find(": use of a disallowed ") {
            if let (Some(place), Some(item)) = (file_line(&line[..at]), backticked(&line[at..])) {
                found.insert(format!("{place}: {item}"));
            }
        } else if let Some(at) = line.find(": error[E0453]: ") {
            let message = &line[at + ": error[E0453]: ".len()..];
            let attribute = message
                .split_once(" incompatible with previous forbid")
                .map(|(attribute, _)| attribute);
            if let (Some(place), Some(attribute)) =
                (file_line(&line[..at + ": error".len()]), attribute)
            {
                found.insert(format!("{place}: {attribute}"));
            }
        }
    }
    (found, unresolved)
}

/// Runs `cargo <cargo_args>` over the fixture, with Clippy's short message
/// format and the fixture target directory, then `-- <lint_args>`.
fn run(cargo_args: &[&str], lint_args: &[&str], tag: &str) -> Report {
    let conf = ConfDir::create(tag);
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let result = Command::new(cargo)
        .current_dir(fixture())
        .env("CLIPPY_CONF_DIR", &conf.0)
        .args(cargo_args)
        .args(["--keep-going", "--message-format", "short", "--target-dir"])
        .arg(repo_root().join("target/xtask-fixtures"))
        .arg("--")
        .args(lint_args)
        .output()
        .expect("cargo starts");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let (found, unresolved) = parse(&output);
    Report {
        success: result.status.success(),
        found,
        unresolved,
        output,
    }
}

/// The `clippy-forbid` step, run over the fixture as the gate runs it over the
/// repository: every crate but the ones it excludes, at forbid level.
fn forbid_report() -> &'static Report {
    static REPORT: OnceLock<Report> = OnceLock::new();
    REPORT.get_or_init(|| {
        let (cargo_args, lint_args) = step_args("clippy-forbid");
        run(&cargo_args, &lint_args, "forbid")
    })
}

/// The `clippy` step's lints, over the two runtime crates only.
fn runtime_report() -> &'static Report {
    static REPORT: OnceLock<Report> = OnceLock::new();
    REPORT.get_or_init(|| {
        let (_, lint_args) = step_args("clippy");
        let cargo_args = [
            "clippy",
            "-p",
            "alloy-rt",
            "-p",
            "alloy-sim",
            "--all-targets",
            "--all-features",
            "--locked",
        ];
        run(&cargo_args, &lint_args, "runtime")
    })
}

/// The lines of a fixture file that hold on this platform, without blank
/// lines and without the platform word.
fn fixture_lines(name: &str) -> BTreeSet<String> {
    fs::read_to_string(fixture().join(name))
        .expect("the fixture file exists")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let (rest, platform) = line.rsplit_once(' ').unwrap_or((line, ""));
            let (line, applies) = match platform {
                "unix" => (rest, cfg!(unix)),
                "windows" => (rest, cfg!(windows)),
                _ => (line, true),
            };
            applies.then(|| line.to_owned())
        })
        .collect()
}

#[test]
fn the_gate_denies_both_lints_and_then_forbids_them() {
    for (step, level) in [("clippy", "-D"), ("clippy-forbid", "-F")] {
        let (_, args) = step_args(step);
        for lint in ["clippy::disallowed_methods", "clippy::disallowed_types"] {
            assert!(
                args.windows(2).any(|pair| pair == [level, lint]),
                "the {step} step passes {level} {lint}"
            );
        }
    }
}

#[test]
fn the_strict_config_keeps_no_allow_invalid() {
    let config = fs::read_to_string(repo_root().join("clippy.toml")).expect("clippy.toml exists");
    let entries = |text: &str| {
        text.lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .filter(|line| line.contains("allow-invalid"))
            .count()
    };
    assert!(entries(&config) > 0);
    assert_eq!(entries(&strict_config(&config)), 0);
}

#[test]
fn short_output_is_parsed() {
    let output = "\
\\\\?\\C:\\t\\clippy.toml:85:14: warning: `tokio::fs::symlink` does not refer to a reachable function
crates\\demo\\src\\lib.rs:8:19: error: use of a disallowed type `std::process::Command`
crates/demo/src/lib.rs:56:5: warning: use of a disallowed method `tokio::time::sleep`
crates\\switches\\src\\lib.rs:9:9: error[E0453]: warn(clippy::disallowed_methods) incompatible with previous forbid: overruled by previous forbid
crates\\switches\\src\\included.txt:2:9: error[E0453]: allow(clippy::all) incompatible with previous forbid: overruled by previous forbid
warning: `demo` (lib) generated 1 warning";
    let (found, unresolved) = parse(output);
    assert_eq!(
        found.into_iter().collect::<Vec<_>>(),
        [
            "crates/demo/src/lib.rs:56: tokio::time::sleep",
            "crates/demo/src/lib.rs:8: std::process::Command",
            "crates/switches/src/included.txt:2: allow(clippy::all)",
            "crates/switches/src/lib.rs:9: warn(clippy::disallowed_methods)",
        ]
    );
    assert_eq!(
        unresolved.into_iter().collect::<Vec<_>>(),
        ["tokio::fs::symlink"]
    );
}

#[test]
fn clippy_fixture_runtime_crates_pass() {
    let report = runtime_report();
    assert!(
        report.success && report.found.is_empty(),
        "alloy-rt and alloy-sim opt out, yet Clippy reported {:?}\n{}",
        report.found,
        report.output
    );
}

#[test]
fn clippy_fixture_other_crates_fail() {
    let report = forbid_report();
    let expected = fixture_lines("expected");
    assert!(!expected.is_empty());
    assert!(!report.success, "\n{}", report.output);
    assert_eq!(report.found, expected, "\n{}", report.output);
}

#[test]
fn clippy_fixture_every_configured_path_resolves() {
    let report = forbid_report();
    assert_eq!(
        report.unresolved,
        fixture_lines("unresolved"),
        "\n{}",
        report.output
    );
}

/// Every package in the fixture's lockfile is a path in the fixture, so the
/// test builds nothing from a registry or a git source.
#[test]
fn clippy_fixture_builds_nothing_from_outside() {
    let lock =
        fs::read_to_string(fixture().join("Cargo.lock")).expect("the fixture has a lockfile");
    let sources: Vec<&str> = lock
        .lines()
        .filter(|line| line.trim_start().starts_with("source"))
        .collect();
    assert!(sources.is_empty(), "{sources:?}");
    let names: BTreeSet<&str> = lock
        .lines()
        .filter_map(|line| line.strip_prefix("name = "))
        .map(|name| name.trim_matches('"'))
        .collect();
    let mut on_disk = BTreeSet::new();
    for dir in ["crates", "stand-in"] {
        for entry in fs::read_dir(fixture().join(dir)).expect("the fixture directory exists") {
            let entry = entry.expect("the fixture directory is readable");
            on_disk.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }
    let on_disk: BTreeSet<&str> = on_disk.iter().map(String::as_str).collect();
    assert_eq!(names, on_disk);
}
