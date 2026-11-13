//! The Clippy fixture for the `direct-io` ban (D-0017).
//!
//! `xtask/tests/fixtures/clippy/` is a Cargo workspace of its own, with
//! `crates/alloy-rt`, `crates/alloy-sim`, and `crates/demo`. These tests run
//! Clippy over it once, with the lint arguments of the gate's `clippy` step
//! and the repository's `clippy.toml`, and compare what Clippy reports with
//! two files in the fixture:
//!
//! - `expected`: one `path:line: item` for each use of a disallowed method or
//!   type that Clippy must report. Only `crates/demo` has any: the runtime
//!   crates opt out.
//! - `unresolved`: the configured paths that resolve to nothing in the
//!   fixture's dependency graph, one per line, followed by `unix` or `windows`
//!   when a path resolves to nothing on that platform family only.
//!
//! The run reads a copy of `clippy.toml` with every `allow-invalid = true`
//! removed, so that Clippy names every path that resolves to nothing, and a
//! misspelled path fails the `unresolved` comparison.
//!
//! The fixture's dependencies, pinned by its `Cargo.lock`, are fetched by Cargo
//! like any other; the build goes to `target/xtask-fixtures/`.

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
    /// `path:line: item` for each use of a disallowed method or type.
    disallowed: BTreeSet<String>,
    /// The configured paths Clippy says resolve to nothing.
    unresolved: BTreeSet<String>,
    /// Everything Clippy and Cargo printed, for failure messages.
    output: String,
}

fn fixture() -> PathBuf {
    repo_root().join("xtask/tests/fixtures/clippy")
}

/// The arguments the gate's `clippy` step passes to the lints, after `--`.
fn lint_args() -> Vec<&'static str> {
    CI_STEPS
        .iter()
        .find_map(|step| match step {
            Step::Cargo {
                name: "clippy",
                args,
                ..
            } => Some(*args),
            _ => None,
        })
        .and_then(|args| {
            let dashes = args.iter().position(|arg| *arg == "--")?;
            Some(args[dashes + 1..].to_vec())
        })
        .expect("the gate has a `clippy` step with lint arguments after `--`")
}

/// `clippy.toml` with every `allow-invalid = true` removed.
fn strict_config(config: &str) -> String {
    config.replace(", allow-invalid = true", "")
}

/// A directory holding the strict copy of `clippy.toml`, removed when dropped.
struct ConfDir(PathBuf);

impl ConfDir {
    fn create() -> Self {
        let dir = Self(env::temp_dir().join(format!("alloy-xtask-clippy-conf-{}", process::id())));
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

/// Reads Clippy's `--message-format short` output.
fn parse(output: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut disallowed = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    for line in output.lines() {
        if line.contains(" does not refer to a reachable ") {
            if let Some(path) = backticked(line) {
                unresolved.insert(path.to_owned());
            }
            continue;
        }
        let Some(at) = line.find(": use of a disallowed ") else {
            continue;
        };
        // `path:line:col: error`, then the message.
        let Some((location, _level)) = line[..at].rsplit_once(": ") else {
            continue;
        };
        let file_line = location.rsplit_once(':').map_or(location, |(fl, _col)| fl);
        if let Some(item) = backticked(&line[at..]) {
            disallowed.insert(format!("{}: {item}", file_line.replace('\\', "/")));
        }
    }
    (disallowed, unresolved)
}

fn run() -> Report {
    let conf = ConfDir::create();
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let result = Command::new(cargo)
        .current_dir(fixture())
        .env("CLIPPY_CONF_DIR", &conf.0)
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--keep-going",
            "--message-format",
            "short",
            "--target-dir",
        ])
        .arg(repo_root().join("target/xtask-fixtures"))
        .arg("--")
        .args(lint_args())
        .output()
        .expect("cargo starts");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let (disallowed, unresolved) = parse(&output);
    Report {
        disallowed,
        unresolved,
        output,
    }
}

/// The one Clippy run all tests in this module share.
fn report() -> &'static Report {
    static REPORT: OnceLock<Report> = OnceLock::new();
    REPORT.get_or_init(run)
}

/// The lines of a fixture file, without blank lines.
fn fixture_lines(name: &str) -> Vec<String> {
    fs::read_to_string(fixture().join(name))
        .expect("the fixture file exists")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_gate_denies_both_lints() {
    let args = lint_args();
    for lint in ["clippy::disallowed_methods", "clippy::disallowed_types"] {
        assert!(
            args.windows(2).any(|pair| pair == ["-D", lint]),
            "the clippy step denies {lint}"
        );
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
warning: `demo` (lib) generated 1 warning";
    let (disallowed, unresolved) = parse(output);
    assert_eq!(
        disallowed.into_iter().collect::<Vec<_>>(),
        [
            "crates/demo/src/lib.rs:56: tokio::time::sleep",
            "crates/demo/src/lib.rs:8: std::process::Command",
        ]
    );
    assert_eq!(
        unresolved.into_iter().collect::<Vec<_>>(),
        ["tokio::fs::symlink"]
    );
}

#[test]
fn clippy_fixture_runtime_crates_pass() {
    let report = report();
    let in_runtime: Vec<&String> = report
        .disallowed
        .iter()
        .filter(|line| !line.starts_with("crates/demo/"))
        .collect();
    assert!(
        in_runtime.is_empty(),
        "alloy-rt and alloy-sim opt out, yet Clippy reported {in_runtime:?}\n{}",
        report.output
    );
}

#[test]
fn clippy_fixture_other_crates_fail() {
    let report = report();
    let expected: BTreeSet<String> = fixture_lines("expected").into_iter().collect();
    assert!(!expected.is_empty());
    let found: BTreeSet<String> = report
        .disallowed
        .iter()
        .filter(|line| line.starts_with("crates/demo/"))
        .cloned()
        .collect();
    assert_eq!(found, expected, "\n{}", report.output);
}

#[test]
fn clippy_fixture_every_configured_path_resolves() {
    let report = report();
    let expected: BTreeSet<String> = fixture_lines("unresolved")
        .iter()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let path = words.next()?;
            let applies = match words.next() {
                None => true,
                Some("unix") => cfg!(unix),
                Some("windows") => cfg!(windows),
                Some(other) => panic!("`{other}` in `unresolved` is not `unix` or `windows`"),
            };
            applies.then(|| path.to_owned())
        })
        .collect();
    assert_eq!(report.unresolved, expected, "\n{}", report.output);
}
