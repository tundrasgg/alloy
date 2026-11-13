//! The repository policy rules that `cargo xtask check-repo` enforces.
//!
//! Each rule reads a [`Tree`] and returns its violations. Rules never modify the
//! tree. The rules are mechanical: anything that needs judgment is left to review.

mod code;
mod docs;
mod front_matter;
mod instruction;
mod path_lock;

pub(crate) use path_lock::{is_locked, is_task_id, task_file, task_type};

use std::fmt;

use crate::tree::Tree;

/// One breach of a rule, located by file and, where it applies, by line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Violation {
    pub(crate) path: String,
    /// 1-based line number, or `None` when the whole file is at fault.
    pub(crate) line: Option<usize>,
    pub(crate) rule: &'static str,
    pub(crate) message: String,
}

impl Violation {
    pub(crate) fn new(
        rule: Rule,
        path: &str,
        line: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            path: path.to_owned(),
            line,
            rule: rule.name(),
            message: message.into(),
        }
    }

    /// `path:line: rule`, the part of the display that tests compare.
    pub(crate) fn location(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}: {}", self.path, self.rule),
            None => format!("{}: {}", self.path, self.rule),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location(), self.message)
    }
}

/// The current branch and the files it changes relative to a base ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchDiff {
    pub(crate) branch: String,
    /// Paths added, modified, or deleted, with `/` separators.
    pub(crate) changed: Vec<String>,
    /// The text of the branch's task file, [`task_file`], as it is on the base
    /// ref, so that the branch being judged cannot change it. `None` when the
    /// branch is not a task branch or the base ref has no such file.
    pub(crate) base_task: Option<String>,
}

/// A repository rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    /// No vendor instruction files; `AGENTS.md` only at the root.
    InstructionFiles,
    /// The banned code patterns of `agent/core/4-evidence.md`, in `crates/`.
    BannedPatterns,
    /// No clock, network, file, thread, process, or randomness calls outside the
    /// runtime.
    DirectIo,
    /// No `unsafe` outside the crates `agent/eng/rust.md` allows.
    Unsafe,
    /// Build branches leave the verifier's paths alone.
    PathLock,
    /// Task and report front matter follows the templates; active tasks' reads exist.
    FrontMatter,
    /// Relative links and backticked repository paths in documents resolve.
    Links,
    /// No `YYYY-MM-DD` dates in documents outside code blocks.
    Dates,
}

impl Rule {
    /// Every rule, in the order they run.
    pub(crate) const ALL: [Self; 8] = [
        Self::InstructionFiles,
        Self::BannedPatterns,
        Self::DirectIo,
        Self::Unsafe,
        Self::PathLock,
        Self::FrontMatter,
        Self::Links,
        Self::Dates,
    ];

    /// The rule's name, as printed in violations and used for fixture directories.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::InstructionFiles => "instruction-files",
            Self::BannedPatterns => "banned-patterns",
            Self::DirectIo => "direct-io",
            Self::Unsafe => "unsafe",
            Self::PathLock => "path-lock",
            Self::FrontMatter => "front-matter",
            Self::Links => "links",
            Self::Dates => "dates",
        }
    }

    /// Runs this rule. `diff` is needed only by the path lock, which passes when
    /// it is `None`.
    pub(crate) fn check(self, tree: &Tree, diff: Option<&BranchDiff>) -> Vec<Violation> {
        match self {
            Self::InstructionFiles => instruction::check(tree),
            Self::BannedPatterns => code::check_banned(tree),
            Self::DirectIo => code::check_direct_io(tree),
            Self::Unsafe => code::check_unsafe(tree),
            Self::PathLock => path_lock::check(diff),
            Self::FrontMatter => front_matter::check(tree),
            Self::Links => docs::check_links(tree),
            Self::Dates => docs::check_dates(tree),
        }
    }
}

/// Runs every rule and returns all violations, sorted by location.
pub(crate) fn check_all(tree: &Tree, diff: Option<&BranchDiff>) -> Vec<Violation> {
    let mut all: Vec<Violation> = Rule::ALL
        .iter()
        .flat_map(|rule| rule.check(tree, diff))
        .collect();
    all.sort();
    all
}

#[cfg(test)]
mod fixture_tests {
    //! Each rule has `xtask/tests/fixtures/<rule>/pass/` and `.../fail/`. A case
    //! holds the tree to check, either as `repo/` or as `files`, and optionally
    //! `branch`, whose first line is the branch name and whose other lines are
    //! the changed paths, and `base/`, files as they are on the base ref. A
    //! failing case also holds `expected`: one `path:line: rule` per expected
    //! violation.
    //!
    //! `files` lists one path per line. The test creates each in a temporary
    //! directory and checks that directory, so that files the repository must
    //! never hold, such as vendor instruction files, are never committed.

    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process;

    use super::*;

    /// The text of every file created from a `files` list.
    const CREATED: &str = "Created by the fixture test; never committed.\n";

    fn case_dir(rule: Rule, case: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rule.name())
            .join(case)
    }

    /// A temporary directory, removed when dropped, also when a test fails.
    struct TempRepo(PathBuf);

    impl TempRepo {
        /// Creates every path in `list` under a directory unique to this
        /// process, rule, and case, so that tests running in parallel do not
        /// share it.
        fn create(rule: Rule, case: &str, list: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "alloy-xtask-fixture-{}-{}-{case}",
                process::id(),
                rule.name()
            ));
            let repo = Self(root);
            // A directory left by a process with a reused id would add files.
            let _ = fs::remove_dir_all(&repo.0);
            for path in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let file = repo.0.join(path);
                let parent = file.parent().expect("a listed path names a file");
                fs::create_dir_all(parent).expect("temporary directory is writable");
                fs::write(&file, CREATED).expect("temporary file is writable");
            }
            repo
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn instruction_file_fixtures_are_created_at_test_time() {
        for case in ["pass", "fail"] {
            let dir = case_dir(Rule::InstructionFiles, case);
            assert!(dir.join("files").is_file(), "{case} lists its files");
            assert!(!dir.join("repo").exists(), "{case} commits no tree");
        }
    }

    /// The file at `path` in the case's `base/`, if there is one.
    fn base_file(dir: &Path, path: &str) -> Option<String> {
        fs::read_to_string(dir.join("base").join(path)).ok()
    }

    fn run(rule: Rule, case: &str) -> Vec<String> {
        let dir = case_dir(rule, case);
        let created = fs::read_to_string(dir.join("files"))
            .ok()
            .map(|list| TempRepo::create(rule, case, &list));
        let root = created
            .as_ref()
            .map_or_else(|| dir.join("repo"), |t| t.0.clone());
        let tree = Tree::from_walk(&root).expect("fixture repo is readable");
        let diff = fs::read_to_string(dir.join("branch")).ok().map(|text| {
            let mut lines = text.lines().map(str::to_owned);
            let branch = lines.next().unwrap_or_default();
            let base_task = task_file(&branch).and_then(|path| base_file(&dir, &path));
            BranchDiff {
                branch,
                changed: lines.filter(|l| !l.is_empty()).collect(),
                base_task,
            }
        });
        let mut found: Vec<String> = rule
            .check(&tree, diff.as_ref())
            .iter()
            .map(Violation::location)
            .collect();
        found.sort();
        found
    }

    fn expected(rule: Rule) -> Vec<String> {
        let text = fs::read_to_string(case_dir(rule, "fail").join("expected"))
            .expect("failing fixture has an `expected` file");
        let mut lines: Vec<String> = text
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        lines.sort();
        lines
    }

    macro_rules! fixture_tests {
        ($($rule:ident: $pass:ident, $fail:ident;)*) => {
            $(
                #[test]
                fn $pass() {
                    assert_eq!(run(Rule::$rule, "pass"), Vec::<String>::new());
                }

                #[test]
                fn $fail() {
                    let expected = expected(Rule::$rule);
                    assert!(!expected.is_empty());
                    assert_eq!(run(Rule::$rule, "fail"), expected);
                }
            )*

            #[test]
            fn every_rule_has_fixtures() {
                let covered = [$(Rule::$rule),*];
                assert_eq!(covered, Rule::ALL);
            }
        };
    }

    fixture_tests! {
        InstructionFiles: instruction_files_pass_fixture_passes, instruction_files_fail_fixture_fails;
        BannedPatterns: banned_patterns_pass_fixture_passes, banned_patterns_fail_fixture_fails;
        DirectIo: direct_io_pass_fixture_passes, direct_io_fail_fixture_fails;
        Unsafe: unsafe_pass_fixture_passes, unsafe_fail_fixture_fails;
        PathLock: path_lock_pass_fixture_passes, path_lock_fail_fixture_fails;
        FrontMatter: front_matter_pass_fixture_passes, front_matter_fail_fixture_fails;
        Links: links_pass_fixture_passes, links_fail_fixture_fails;
        Dates: dates_pass_fixture_passes, dates_fail_fixture_fails;
    }
}
