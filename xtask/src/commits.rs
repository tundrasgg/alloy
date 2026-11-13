//! `cargo xtask check-commits <range>`: every commit in the range has a message
//! in the format of `agent/eng/git.md`, section Commits on main, and a commit
//! that changes a path locked in `agent/eng/testing.md` names a `verify` task in
//! its `Task:` footer (D-0016).
//!
//! Checking is pure: [`check`] judges a [`Message`] and the [`Facts`] about its
//! commit. [`check_range`] gathers both from git.

use std::collections::{BTreeMap, BTreeSet};
use std::iter;
use std::path::Path;

use crate::rules::{is_locked, is_task_id, task_type};
use crate::text::package_name;
use crate::tree;

/// The longest subject line, in characters.
const SUBJECT_LIMIT: usize = 72;

/// The commit types, in the order of the table in `agent/eng/git.md`.
const TYPES: [&str; 11] = [
    "revert", "feat", "fix", "perf", "refactor", "test", "spec", "docs", "build", "ci", "chore",
];

/// The scopes allowed besides the names of the crates under `crates/`.
const SCOPES: [&str; 11] = [
    "plan", "agent", "adr", "spec", "research", "product", "ui", "xtask", "tests", "ci", "repo",
];

/// The allowed footers, in the order they must appear.
const FOOTERS: [&str; 3] = ["Task", "Spec", "BREAKING CHANGE"];

/// A rule that a commit message on main follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitRule {
    /// The subject is `<type>(<scope>)[!]: <description>`.
    SubjectFormat,
    /// The subject is at most [`SUBJECT_LIMIT`] characters.
    SubjectLength,
    /// The type is one of [`TYPES`].
    Type,
    /// The scope is present and is a crate name or one of [`SCOPES`].
    Scope,
    /// The subject does not end with a period.
    TrailingPeriod,
    /// The line after the subject is blank.
    BlankLine,
    /// Footers are `Key: value` lines in the last paragraph, each at most once,
    /// in the order of [`FOOTERS`].
    Footers,
    /// No footer or trailer other than [`FOOTERS`].
    Trailers,
    /// `Task:` lists `T-NNNN` ids separated by `, `.
    TaskId,
    /// `Task:` is present unless the commit is `chore(plan)`.
    TaskRequired,
    /// The subject carries `!` exactly when a `BREAKING CHANGE:` footer is present.
    Breaking,
    /// A commit that changes a locked path names a `verify` task.
    PathLock,
}

impl CommitRule {
    /// Every rule, in the order they are checked. The fixture tests use it to
    /// prove that every rule has fixtures.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 12] = [
        Self::SubjectFormat,
        Self::SubjectLength,
        Self::Type,
        Self::Scope,
        Self::TrailingPeriod,
        Self::BlankLine,
        Self::Footers,
        Self::Trailers,
        Self::TaskId,
        Self::TaskRequired,
        Self::Breaking,
        Self::PathLock,
    ];

    /// The rule's name, as printed and used for fixture directories.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::SubjectFormat => "subject-format",
            Self::SubjectLength => "subject-length",
            Self::Type => "type",
            Self::Scope => "scope",
            Self::TrailingPeriod => "trailing-period",
            Self::BlankLine => "blank-line",
            Self::Footers => "footers",
            Self::Trailers => "trailers",
            Self::TaskId => "task-id",
            Self::TaskRequired => "task-required",
            Self::Breaking => "breaking",
            Self::PathLock => "path-lock",
        }
    }
}

/// One breach of a commit rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Breach {
    pub(crate) rule: CommitRule,
    pub(crate) message: String,
}

/// What the checks need to know about a commit besides its message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    /// Paths the commit changes relative to its first parent.
    pub(crate) changed: Vec<String>,
    /// Package names of the crates under `crates/`, in the commit or its first
    /// parent. Only needed when the scope is not one of [`SCOPES`].
    pub(crate) crates: BTreeSet<String>,
    /// For each task the `Task:` footer names, the `type` in its task file at
    /// the first parent, or `None` when there is no such file or field. Only
    /// needed when the commit changes a locked path.
    pub(crate) task_types: BTreeMap<String, Option<String>>,
}

/// A subject that has the shape `<type>(<scope>)[!]: <description>`, with the
/// scope optional so that a missing scope is reported by its own rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Subject<'a> {
    kind: &'a str,
    scope: Option<&'a str>,
    bang: bool,
    description: &'a str,
}

impl<'a> Subject<'a> {
    /// Total over any line; `None` when the line does not have the shape.
    fn parse(line: &'a str) -> Option<Self> {
        let (head, description) = line.split_once(": ")?;
        if description.is_empty() || description.starts_with(char::is_whitespace) {
            return None;
        }
        let (head, bang) = match head.strip_suffix('!') {
            Some(head) => (head, true),
            None => (head, false),
        };
        let (kind, scope) = match head.split_once('(') {
            Some((kind, rest)) => (kind, Some(rest.strip_suffix(')')?)),
            None => (head, None),
        };
        let is_word =
            |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        let kind_ok = !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphabetic());
        (kind_ok && scope.is_none_or(is_word)).then_some(Self {
            kind,
            scope,
            bang,
            description,
        })
    }
}

/// A commit message split into its subject, body, and footer lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message<'a> {
    lines: Vec<&'a str>,
    /// Index of the first footer line: the start of the trailer block that
    /// [`trailer_block_start`] finds, else `lines.len()`.
    footers_from: usize,
}

impl<'a> Message<'a> {
    /// Total over any text. Trailing blank lines are ignored.
    pub(crate) fn parse(text: &'a str) -> Self {
        let lines: Vec<&str> = text.trim_end().lines().collect();
        let footers_from = trailer_block_start(&lines);
        Self {
            lines,
            footers_from,
        }
    }

    fn subject_line(&self) -> &'a str {
        self.lines.first().copied().unwrap_or_default()
    }

    /// The scope in the subject, when the subject has the expected shape.
    pub(crate) fn scope(&self) -> Option<&'a str> {
        Subject::parse(self.subject_line()).and_then(|s| s.scope)
    }

    fn footers(&self) -> &[&'a str] {
        self.lines.get(self.footers_from..).unwrap_or_default()
    }

    /// The well-formed task ids in the `Task:` footers.
    pub(crate) fn task_ids(&self) -> Vec<&'a str> {
        self.footers()
            .iter()
            .filter_map(|line| trailer(line))
            .filter(|(key, _)| *key == "Task")
            .flat_map(|(_, value)| value.split(',').map(str::trim))
            .filter(|id| is_task_id(id))
            .collect()
    }
}

/// Splits a line that git would parse as a trailer into the key and the text
/// after the colon. The key is `BREAKING CHANGE`, or, as git's
/// `interpret-trailers` reads it, a run of ASCII letters, digits, and hyphens,
/// which spaces or tabs may separate from the colon. Nothing needs to follow
/// the colon, so `Task:T-18` is a trailer with key `Task`. `None` for any other
/// line. The footer rules then require the exact `Key: value` form.
fn trailer(line: &str) -> Option<(&str, &str)> {
    if let Some(value) = line.strip_prefix("BREAKING CHANGE:") {
        return Some(("BREAKING CHANGE", value));
    }
    let (head, value) = line.split_once(':')?;
    let key = head.trim_end_matches([' ', '\t']);
    let is_token = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    is_token.then_some((key, value))
}

/// Prefixes git counts as trailers it generated itself.
const GIT_GENERATED: [&str; 2] = ["Signed-off-by: ", "(cherry picked from commit "];

/// Whether git's `isspace` holds for `c`.
fn is_git_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Whether git counts the line as blank: empty or only spaces.
fn is_blank(line: &str) -> bool {
    line.chars().all(is_git_space)
}

/// The index of the first line of the trailer block, or `lines.len()` when the
/// message has none.
///
/// This is `find_trailer_block_start` in git's `trailer.c`, which
/// `git interpret-trailers --parse` and `%(trailers)` use, with the default
/// comment character `#` and no `trailer.*` configuration. The block can only
/// be the last paragraph after the title, and it is one when every line in it
/// is a trailer or a continuation line indented under one, or when it holds a
/// git-generated trailer and at least a quarter of its lines are trailers. A
/// `Key: value` line in any earlier paragraph is body text. One deviation:
/// `BREAKING CHANGE:` counts as a trailer, as Conventional Commits 1.0.0 says,
/// though git's key grammar has no space.
fn trailer_block_start(lines: &[&str]) -> usize {
    let none = lines.len();
    let title_end = lines.iter().position(|line| is_blank(line)).unwrap_or(none);
    let mut only_blank = true;
    let mut generated = false;
    let mut trailers = 0_usize;
    let mut others = 0_usize;
    let mut continuations = 0_usize;
    for (index, line) in lines.iter().enumerate().skip(title_end).rev() {
        if line.starts_with('#') {
            others += continuations;
            continuations = 0;
        } else if is_blank(line) {
            if only_blank {
                continue;
            }
            others += continuations;
            let block = (generated && trailers * 3 >= others) || (trailers > 0 && others == 0);
            return if block { index + 1 } else { none };
        } else {
            only_blank = false;
            if GIT_GENERATED.iter().any(|prefix| line.starts_with(prefix)) {
                generated = true;
                trailers += 1;
                continuations = 0;
            } else if trailer(line).is_some() {
                trailers += 1;
                continuations = 0;
            } else if line.starts_with(is_git_space) {
                continuations += 1;
            } else {
                others += 1 + continuations;
                continuations = 0;
            }
        }
    }
    none
}

/// Judges one commit. Returns every breach, in the order of [`CommitRule::ALL`]
/// within each part of the message.
pub(crate) fn check(message: &Message<'_>, facts: &Facts) -> Vec<Breach> {
    let mut out = Vec::new();
    let mut add = |rule: CommitRule, text: String| {
        out.push(Breach {
            rule,
            message: text,
        })
    };

    let line = message.subject_line();
    let length = line.chars().count();
    if length > SUBJECT_LIMIT {
        add(
            CommitRule::SubjectLength,
            format!("the subject is {length} characters; the limit is {SUBJECT_LIMIT}"),
        );
    }
    let subject = Subject::parse(line);
    match subject {
        None => add(
            CommitRule::SubjectFormat,
            format!("the subject `{line}` is not `<type>(<scope>): <description>`"),
        ),
        Some(s) => {
            if !TYPES.contains(&s.kind) {
                add(
                    CommitRule::Type,
                    format!("`{}` is not a type in agent/eng/git.md", s.kind),
                );
            }
            match s.scope {
                None => add(CommitRule::Scope, "the subject has no scope".to_owned()),
                Some(scope) if !SCOPES.contains(&scope) && !facts.crates.contains(scope) => add(
                    CommitRule::Scope,
                    format!("`{scope}` is neither a crate under crates/ nor an allowed scope"),
                ),
                Some(_) => {}
            }
            if s.description.trim_end().ends_with('.') {
                add(
                    CommitRule::TrailingPeriod,
                    "the subject ends with a period".to_owned(),
                );
            }
        }
    }
    if message.lines.get(1).is_some_and(|l| !l.trim().is_empty()) {
        add(
            CommitRule::BlankLine,
            "the line after the subject is not blank".to_owned(),
        );
    }

    let not_allowed = |key: &str| {
        format!(
            "`{key}:` is not allowed; the only footers are `Task:`, `Spec:`, `BREAKING CHANGE:`"
        )
    };
    let mut last_rank = None;
    let mut present = [false; FOOTERS.len()];
    for line in message.footers() {
        let Some((key, value)) = trailer(line) else {
            add(
                CommitRule::Footers,
                format!("`{line}` in the footers is not a `Key: value` line"),
            );
            continue;
        };
        let Some(rank) = FOOTERS.iter().position(|k| *k == key) else {
            add(CommitRule::Trailers, not_allowed(key));
            continue;
        };
        if last_rank.is_some_and(|last| rank <= last) {
            add(
                CommitRule::Footers,
                format!(
                    "`{key}:` is repeated or out of order; the order is `Task:`, `Spec:`, `BREAKING CHANGE:`"
                ),
            );
        }
        last_rank = last_rank.max(Some(rank));
        present[rank] = true;
        let exact = line
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(": "))
            .filter(|v| !v.is_empty() && !v.starts_with(char::is_whitespace));
        if exact.is_none() {
            add(
                CommitRule::Footers,
                format!(
                    "`{line}` is not `{key}: <value>`, with no space before the colon and one after it"
                ),
            );
        }
        let text = exact.unwrap_or_else(|| value.trim());
        if key == "Task" && !text.is_empty() && !text.split(", ").all(is_task_id) {
            add(
                CommitRule::TaskId,
                format!("`Task: {text}` is not a list of `T-NNNN` ids separated by `, `"),
            );
        }
    }

    let plan_chore = subject.is_some_and(|s| s.kind == "chore" && s.scope == Some("plan"));
    if !present[0] && !plan_chore {
        add(
            CommitRule::TaskRequired,
            "there is no `Task:` footer, and only `chore(plan)` commits may omit it".to_owned(),
        );
    }
    if let Some(s) = subject
        && s.bang != present[2]
    {
        let text = if s.bang {
            "the subject has `!` and there is no `BREAKING CHANGE:` footer"
        } else {
            "there is a `BREAKING CHANGE:` footer and the subject has no `!`"
        };
        add(CommitRule::Breaking, text.to_owned());
    }

    let locked: Vec<&str> = facts
        .changed
        .iter()
        .map(String::as_str)
        .filter(|path| is_locked(path))
        .collect();
    if let Some(first) = locked.first() {
        let ids = message.task_ids();
        let kind = |id: &str| facts.task_types.get(id).cloned().flatten();
        if !ids.iter().any(|id| kind(id).as_deref() == Some("verify")) {
            let tasks: Vec<String> = ids
                .iter()
                .map(|id| match kind(id) {
                    Some(kind) => format!("{id} is `{kind}`"),
                    None => format!("{id} has no typed task file"),
                })
                .collect();
            let tasks = if tasks.is_empty() {
                "it names no task".to_owned()
            } else {
                tasks.join(", ")
            };
            add(
                CommitRule::PathLock,
                format!(
                    "it changes {} locked path(s), first `{first}`, and no task in `Task:` is a `verify` task at the parent: {tasks}",
                    locked.len()
                ),
            );
        }
    }
    out
}

/// One commit in a range and its breaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub(crate) id: String,
    pub(crate) breaches: Vec<Breach>,
}

/// One commit as `git log -z --format=%H%n%P%n%B` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    id: String,
    parent: Option<String>,
    message: String,
}

/// Parses the output of `git log -z --format=%H%n%P%n%B`. Total over any text.
fn parse_log(output: &str) -> Vec<Entry> {
    output
        .split('\0')
        .map(|record| record.trim_start_matches('\n'))
        .filter(|record| !record.is_empty())
        .map(|record| {
            let mut parts = record.splitn(3, '\n');
            let id = parts.next().unwrap_or_default().to_owned();
            let parent = parts
                .next()
                .and_then(|parents| parents.split_whitespace().next())
                .map(str::to_owned);
            let message = parts.next().unwrap_or_default().to_owned();
            Entry {
                id,
                parent,
                message,
            }
        })
        .collect()
}

/// Checks every commit in `range` along the first parent, oldest first.
///
/// Fails when git fails, for example because the range names a missing ref.
pub(crate) fn check_range(root: &Path, range: &str) -> Result<Vec<Verdict>, String> {
    let log = tree::git(
        root,
        &[
            "log",
            "--first-parent",
            "--reverse",
            "--no-show-signature",
            "-z",
            "--format=%H%n%P%n%B",
            range,
            "--",
        ],
    )?;
    parse_log(&log)
        .into_iter()
        .map(|entry| {
            let message = Message::parse(&entry.message);
            let facts = facts(root, &entry, &message)?;
            Ok(Verdict {
                breaches: check(&message, &facts),
                id: entry.id,
            })
        })
        .collect()
}

/// Reads from git the facts [`check`] needs about `entry`, and only those.
fn facts(root: &Path, entry: &Entry, message: &Message<'_>) -> Result<Facts, String> {
    let mut args = vec![
        "diff-tree",
        "-r",
        "--no-commit-id",
        "--name-only",
        "--no-renames",
        "-z",
    ];
    args.push(entry.parent.as_deref().unwrap_or("--root"));
    args.push(&entry.id);
    let changed: Vec<String> = tree::git(root, &args)?
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    let mut crates = BTreeSet::new();
    if message
        .scope()
        .is_some_and(|scope| !SCOPES.contains(&scope))
    {
        for rev in iter::once(entry.id.as_str()).chain(entry.parent.as_deref()) {
            crates.extend(crate_names(root, rev)?);
        }
    }
    let mut task_types = BTreeMap::new();
    if changed.iter().any(|path| is_locked(path)) {
        for id in message.task_ids() {
            let kind = match &entry.parent {
                Some(parent) => tree::file_at(root, parent, &format!("plan/tasks/{id}.md"))?
                    .as_deref()
                    .and_then(task_type),
                None => None,
            };
            task_types.insert(id.to_owned(), kind);
        }
    }
    Ok(Facts {
        changed,
        crates,
        task_types,
    })
}

/// The package names of the crates directly under `crates/` at `rev`.
fn crate_names(root: &Path, rev: &str) -> Result<BTreeSet<String>, String> {
    let listing = tree::git(
        root,
        &["ls-tree", "-r", "--name-only", "-z", rev, "--", "crates/"],
    )?;
    let mut names = BTreeSet::new();
    for path in listing.split('\0') {
        let dir = path
            .strip_prefix("crates/")
            .and_then(|rest| rest.strip_suffix("/Cargo.toml"))
            .filter(|dir| !dir.contains('/'));
        if let Some(dir) = dir {
            let manifest = tree::git(root, &["show", &format!("{rev}:{path}")])?;
            names.insert(package_name(&manifest).unwrap_or(dir).to_owned());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str, facts: &Facts) -> Vec<&'static str> {
        check(&Message::parse(text), facts)
            .iter()
            .map(|b| b.rule.name())
            .collect()
    }

    fn crates(names: &[&str]) -> Facts {
        Facts {
            crates: names.iter().map(|n| (*n).to_owned()).collect(),
            ..Facts::default()
        }
    }

    #[test]
    fn subjects_are_parsed() {
        let s = Subject::parse("feat(alloy-wire)!: add a frame").expect("valid");
        assert_eq!(
            (s.kind, s.scope, s.bang, s.description),
            ("feat", Some("alloy-wire"), true, "add a frame")
        );
        assert_eq!(Subject::parse("feat: x").map(|s| s.scope), Some(None));
        for bad in [
            "",
            "feat(x):  two spaces",
            "feat(x):",
            "feat(x) : y",
            "feat(): y",
            "feat(a)(b): y",
            "feat(a b): y",
            "Merge branch 'x'",
        ] {
            assert_eq!(Subject::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_missing_scope_and_a_crate_scope_are_judged() {
        assert_eq!(
            rules("feat: add x\n\nTask: T-0001", &Facts::default()),
            ["scope"]
        );
        let text = "feat(alloy-link): add x\n\nTask: T-0001";
        assert!(rules(text, &crates(&["alloy-link"])).is_empty());
        assert_eq!(rules(text, &Facts::default()), ["scope"]);
    }

    #[test]
    fn the_subject_limit_counts_characters() {
        let subject = |len: usize| format!("chore(plan): {}", "é".repeat(len - 13));
        assert!(rules(&subject(72), &Facts::default()).is_empty());
        assert_eq!(rules(&subject(73), &Facts::default()), ["subject-length"]);
    }

    #[test]
    fn plan_chores_need_no_task_and_no_body() {
        assert!(rules("chore(plan): mark T-0001 done", &Facts::default()).is_empty());
        assert_eq!(
            rules("chore(repo): tidy", &Facts::default()),
            ["task-required"]
        );
    }

    #[test]
    fn key_value_lines_in_the_body_are_not_footers() {
        let text = "chore(plan): send T-0006 back for round 2, T-0043 to review\n\nT-0006: one block (mem stream buffer bound, I-21) and six fixes.\n\nTask: T-0006, T-0043";
        assert!(rules(text, &Facts::default()).is_empty());
        let message = Message::parse(text);
        assert_eq!(message.task_ids(), ["T-0006", "T-0043"]);
        let text = "feat(xtask): add x\n\nCo-Authored-By: a\nmore prose\n\nTask: T-0001";
        assert!(rules(text, &Facts::default()).is_empty());
        let text = "feat(xtask): add x\n\nBody.\nTask: T-0001\n\nSigned-off-by: someone";
        assert_eq!(
            rules(text, &Facts::default()),
            ["trailers", "task-required"]
        );
        let message = Message::parse("feat(xtask): x\n\nTask: T-0001\n\nprose");
        assert!(message.task_ids().is_empty());
    }

    /// Each case is a message and its trailer block. The block is empty exactly
    /// where `git interpret-trailers --parse` (git 2.44.0) printed nothing, and
    /// is otherwise the paragraph whose trailers it printed.
    #[test]
    fn the_trailer_block_is_where_git_finds_it() {
        let cases: [(&str, &[&str]); 13] = [
            (
                "chore(plan): x\n\nT-0006: one block (I-21) and six fixes.\n\nTask: T-0006, T-0043",
                &["Task: T-0006, T-0043"],
            ),
            ("feat(xtask): x\n\nprose\nCo-Authored-By: a", &[]),
            (
                "feat(xtask): x\n\nprose\nSigned-off-by: a",
                &["prose", "Signed-off-by: a"],
            ),
            (
                "feat(xtask): x\n\nprose\nmore\nmore\nmore\nSigned-off-by: a",
                &[],
            ),
            (
                "feat(xtask): x\n\nTask: T-0001\n  continued",
                &["Task: T-0001", "  continued"],
            ),
            ("feat(xtask): x\n\n  indented\nTask: T-0001", &[]),
            ("feat(xtask): x\nTask: T-0001", &[]),
            (
                "feat(xtask): x\n\nTask: T-0001\n# comment",
                &["Task: T-0001", "# comment"],
            ),
            (
                "chore(plan): x\n\nT-0006: one block",
                &["T-0006: one block"],
            ),
            ("Task: T-0001", &[]),
            (
                "feat(xtask): x\n\nBody.\nCo-Authored-By:a\n\nTask: T-0001",
                &["Task: T-0001"],
            ),
            ("feat(xtask): x\n\nTask: T-0001\nprose", &[]),
            (
                "feat(xtask): x\n\nTask: T-0001\n\n\n \t\n",
                &["Task: T-0001"],
            ),
        ];
        for (text, block) in cases {
            assert_eq!(Message::parse(text).footers(), block, "{text:?}");
        }
    }

    /// git finds no trailer here, because its key grammar has no space;
    /// Conventional Commits 1.0.0 makes `BREAKING CHANGE` a footer token.
    #[test]
    fn a_breaking_change_footer_is_in_the_trailer_block() {
        let text = "feat(xtask)!: x\n\nTask: T-0001\nSpec: a\nBREAKING CHANGE: b";
        assert_eq!(
            Message::parse(text).footers(),
            ["Task: T-0001", "Spec: a", "BREAKING CHANGE: b"]
        );
    }

    #[test]
    fn lines_git_reads_as_trailers_are_trailers() {
        assert_eq!(trailer("Task:T-18"), Some(("Task", "T-18")));
        assert_eq!(trailer("Task : T-0001"), Some(("Task", " T-0001")));
        assert_eq!(trailer("Co-Authored-By:a"), Some(("Co-Authored-By", "a")));
        assert_eq!(trailer("Spec:"), Some(("Spec", "")));
        assert_eq!(
            trailer("BREAKING CHANGE: x"),
            Some(("BREAKING CHANGE", " x"))
        );
        for not_one in [
            "- item: x",
            " Task: T-0001",
            "Two words: x",
            ": x",
            "no colon",
        ] {
            assert_eq!(trailer(not_one), None, "{not_one}");
        }
    }

    #[test]
    fn trailers_without_a_space_are_judged() {
        let text = "chore(plan): probe a trailer with no space\n\nCo-Authored-By:Someone <someone@example.invalid>";
        assert_eq!(rules(text, &Facts::default()), ["trailers"]);
        let text = "chore(plan): probe a malformed task footer\n\nTask:T-18";
        assert_eq!(rules(text, &Facts::default()), ["footers", "task-id"]);
        let text = "feat(xtask): add x\n\nTask : T-0001";
        assert_eq!(rules(text, &Facts::default()), ["footers"]);
        let text = "feat(xtask): add x\n\nBody.\n\nTask: T-0001\nCo-Authored-By:a";
        assert_eq!(rules(text, &Facts::default()), ["trailers"]);
        let message = Message::parse("feat(xtask): x\n\nTask:T-0001");
        assert_eq!(message.task_ids(), ["T-0001"]);
    }

    #[test]
    fn trailing_whitespace_does_not_hide_a_trailing_period() {
        let text = "feat(xtask): probe a trailing period. \t\n\nTask: T-0001";
        assert_eq!(rules(text, &Facts::default()), ["trailing-period"]);
    }

    #[test]
    fn a_breaking_change_needs_both_marks() {
        let text = "feat(xtask)!: drop x\n\nBody.\n\nTask: T-0001\nSpec: arfc-0001#2\nBREAKING CHANGE: x is gone; see ADR 0001";
        assert!(rules(text, &Facts::default()).is_empty());
        let text = "feat(xtask): drop x\n\nTask: T-0001\nBREAKING CHANGE: x is gone";
        assert_eq!(rules(text, &Facts::default()), ["breaking"]);
    }

    #[test]
    fn task_ids_are_read_from_the_footers_only() {
        let message =
            Message::parse("feat(xtask): x\n\nTask: T-0002 here\n\nTask: T-0001, T-0042, T-12");
        assert_eq!(message.task_ids(), ["T-0001", "T-0042"]);
    }

    #[test]
    fn the_path_lock_names_each_task_type() {
        let facts = Facts {
            changed: vec!["tests/sim/a.rs".to_owned(), "tests/seeds/a.txt".to_owned()],
            task_types: [("T-0001".to_owned(), Some("build".to_owned()))].into(),
            ..Facts::default()
        };
        let found = check(
            &Message::parse("test(tests): x\n\nTask: T-0001, T-0002"),
            &facts,
        );
        assert_eq!(found.len(), 1);
        assert!(
            found[0]
                .message
                .contains("1 locked path(s), first `tests/sim/a.rs`")
        );
        assert!(
            found[0]
                .message
                .contains("T-0001 is `build`, T-0002 has no typed task file")
        );
    }

    #[test]
    fn git_log_output_is_parsed() {
        let output = "aaa\nppp qqq\nfeat(x): y\n\nTask: T-0001\n\0\nbbb\n\nchore(plan): z\n\0";
        assert_eq!(
            parse_log(output),
            [
                Entry {
                    id: "aaa".to_owned(),
                    parent: Some("ppp".to_owned()),
                    message: "feat(x): y\n\nTask: T-0001\n".to_owned(),
                },
                Entry {
                    id: "bbb".to_owned(),
                    parent: None,
                    message: "chore(plan): z\n".to_owned(),
                },
            ]
        );
        assert!(parse_log("").is_empty());
    }
}

#[cfg(test)]
mod fixture_tests {
    //! Each rule has `xtask/tests/fixtures/commits/<rule>/pass/` and
    //! `.../fail/`, and may have more cases named `pass-<what>/` and
    //! `fail-<what>/`. A case holds `message`, the commit message, and optionally
    //! `changed`, one changed path per line, `crates`, one crate name per line,
    //! and `parent/plan/tasks/`, task files as they are at the first parent. A
    //! passing case breaks no rule; a failing case breaks its rule once and no
    //! other rule.

    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;

    fn rule_dir(rule: CommitRule) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/commits")
            .join(rule.name())
    }

    /// The rule's cases of one kind, `pass` or `fail`, sorted. The case named
    /// `kind` must be among them, and every case must be named for a kind.
    fn cases(rule: CommitRule, kind: &str) -> Vec<String> {
        let is_kind = |name: &str, kind: &str| {
            name.strip_prefix(kind)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
        };
        let mut names: Vec<String> = fs::read_dir(rule_dir(rule))
            .expect("each rule has a fixture directory")
            .map(|entry| {
                let entry = entry.expect("fixture directory is readable");
                entry.file_name().to_string_lossy().into_owned()
            })
            .collect();
        names.sort();
        for name in &names {
            assert!(
                is_kind(name, "pass") || is_kind(name, "fail"),
                "{}: case `{name}` is not named `pass[-*]` or `fail[-*]`",
                rule.name()
            );
        }
        names.retain(|name| is_kind(name, kind));
        assert!(
            names.iter().any(|name| name == kind),
            "{}: there is no `{kind}` case",
            rule.name()
        );
        names
    }

    fn lines(dir: &Path, name: &str) -> Vec<String> {
        fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn run(rule: CommitRule, case: &str) -> Vec<&'static str> {
        let dir = rule_dir(rule).join(case);
        let text = fs::read_to_string(dir.join("message")).expect("each case has a message");
        let mut task_types = BTreeMap::new();
        if let Ok(entries) = fs::read_dir(dir.join("parent/plan/tasks")) {
            for entry in entries {
                let path = entry.expect("fixture directory is readable").path();
                let id = path.file_stem().expect("a task file").to_string_lossy();
                let text = fs::read_to_string(&path).expect("fixture file is readable");
                task_types.insert(id.into_owned(), task_type(&text));
            }
        }
        let facts = Facts {
            changed: lines(&dir, "changed"),
            crates: lines(&dir, "crates").into_iter().collect(),
            task_types,
        };
        check(&Message::parse(&text), &facts)
            .iter()
            .map(|b| b.rule.name())
            .collect()
    }

    macro_rules! fixture_tests {
        ($($rule:ident: $pass:ident, $fail:ident;)*) => {
            $(
                #[test]
                fn $pass() {
                    for case in cases(CommitRule::$rule, "pass") {
                        assert_eq!(run(CommitRule::$rule, &case), Vec::<&str>::new(), "{case}");
                    }
                }

                #[test]
                fn $fail() {
                    for case in cases(CommitRule::$rule, "fail") {
                        assert_eq!(
                            run(CommitRule::$rule, &case),
                            [CommitRule::$rule.name()],
                            "{case}"
                        );
                    }
                }
            )*

            #[test]
            fn every_commit_rule_has_fixtures() {
                let covered = [$(CommitRule::$rule),*];
                assert_eq!(covered, CommitRule::ALL);
            }
        };
    }

    fixture_tests! {
        SubjectFormat: subject_format_pass_fixture_passes, subject_format_fail_fixture_fails;
        SubjectLength: subject_length_pass_fixture_passes, subject_length_fail_fixture_fails;
        Type: type_pass_fixture_passes, type_fail_fixture_fails;
        Scope: scope_pass_fixture_passes, scope_fail_fixture_fails;
        TrailingPeriod: trailing_period_pass_fixture_passes, trailing_period_fail_fixture_fails;
        BlankLine: blank_line_pass_fixture_passes, blank_line_fail_fixture_fails;
        Footers: footers_pass_fixture_passes, footers_fail_fixture_fails;
        Trailers: trailers_pass_fixture_passes, trailers_fail_fixture_fails;
        TaskId: task_id_pass_fixture_passes, task_id_fail_fixture_fails;
        TaskRequired: task_required_pass_fixture_passes, task_required_fail_fixture_fails;
        Breaking: breaking_pass_fixture_passes, breaking_fail_fixture_fails;
        PathLock: path_lock_pass_fixture_passes, path_lock_fail_fixture_fails;
    }
}
