//! `path-lock`: only `verify` tasks change the verifier's paths
//! (`agent/eng/testing.md`, section Path lock).

use super::{BranchDiff, Rule, Violation};
use crate::text::front_matter;

/// The paths written only on verifier branches.
const LOCKED: [&str; 4] = [
    "tests/conformance/",
    "tests/sim/",
    "tests/natlab/",
    "tests/system/",
];

/// Judges `diff`. The task type comes from `diff.base_task`, the task file as it
/// is on the base ref, and never from the tree being checked, which the judged
/// branch controls.
pub(super) fn check(diff: Option<&BranchDiff>) -> Vec<Violation> {
    let Some(diff) = diff else {
        return Vec::new();
    };
    let Some(task_path) = task_file(&diff.branch) else {
        return Vec::new();
    };
    let violation =
        |path: &str, message: String| Violation::new(Rule::PathLock, path, None, message);
    let Some(text) = &diff.base_task else {
        return vec![violation(
            &task_path,
            format!(
                "branch `{}` names a task with no task file on the base ref",
                diff.branch
            ),
        )];
    };
    let Some(kind) = task_type(text) else {
        return vec![violation(
            &task_path,
            "the task file on the base ref has no `type` in its front matter".to_owned(),
        )];
    };
    if kind == "verify" {
        return Vec::new();
    }
    diff.changed
        .iter()
        .filter(|path| is_locked(path))
        .map(|path| {
            violation(
                path,
                format!(
                    "locked to verify tasks, and branch `{}` is a `{kind}` task on the base ref",
                    diff.branch
                ),
            )
        })
        .collect()
}

/// True when `path` is under one of the paths written only on verifier branches.
pub(crate) fn is_locked(path: &str) -> bool {
    LOCKED.iter().any(|prefix| path.starts_with(prefix))
}

/// The `type` field of a task file's front matter, or `None` when the file has
/// no readable front matter or no `type`.
pub(crate) fn task_type(text: &str) -> Option<String> {
    front_matter(text)
        .ok()
        .and_then(|fields| fields.into_iter().find(|f| f.key == "type"))
        .map(|f| f.value)
}

/// True for a task id: `T-` followed by at least four digits.
pub(crate) fn is_task_id(id: &str) -> bool {
    id.strip_prefix("T-")
        .is_some_and(|digits| digits.len() >= 4 && digits.chars().all(|c| c.is_ascii_digit()))
}

/// The task file of a task branch: `plan/tasks/T-NNNN.md` for `task/T-NNNN`, and
/// `None` for any other branch.
pub(crate) fn task_file(branch: &str) -> Option<String> {
    task_id(branch).map(|id| format!("plan/tasks/{id}.md"))
}

/// The task id in a task branch name, `task/T-NNNN`.
fn task_id(branch: &str) -> Option<&str> {
    let id = branch.strip_prefix("task/")?;
    is_task_id(id).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_branches_are_recognized() {
        assert_eq!(task_id("task/T-0002"), Some("T-0002"));
        assert_eq!(task_id("task/T-12345"), Some("T-12345"));
        assert_eq!(task_id("task/T-002"), None);
        assert_eq!(task_id("task/T-0002-x"), None);
        assert_eq!(task_id("review/T-0002.1"), None);
        assert_eq!(task_id("main"), None);
        assert_eq!(
            task_file("task/T-0002").as_deref(),
            Some("plan/tasks/T-0002.md")
        );
        assert_eq!(task_file("review/T-0002.1"), None);
    }

    fn task(kind: &str) -> String {
        format!("---\nid: T-0101\ntype: {kind}\n---\n\n## Goal\n")
    }

    fn diff(base_task: Option<String>) -> BranchDiff {
        BranchDiff {
            branch: "task/T-0101".to_owned(),
            changed: vec![
                "tests/conformance/x.rs".to_owned(),
                "crates/a/src/lib.rs".to_owned(),
            ],
            base_task,
        }
    }

    fn locations(diff: &BranchDiff) -> Vec<String> {
        check(Some(diff)).iter().map(Violation::location).collect()
    }

    #[test]
    fn the_task_type_comes_from_the_base_ref() {
        assert_eq!(
            locations(&diff(Some(task("build")))),
            ["tests/conformance/x.rs: path-lock"]
        );
        assert!(locations(&diff(Some(task("verify")))).is_empty());
    }

    #[test]
    fn a_task_branch_needs_a_typed_task_file_on_the_base_ref() {
        assert_eq!(locations(&diff(None)), ["plan/tasks/T-0101.md: path-lock"]);
        assert_eq!(
            locations(&diff(Some("no front matter".to_owned()))),
            ["plan/tasks/T-0101.md: path-lock"]
        );
    }

    #[test]
    fn other_branches_and_runs_without_a_base_pass() {
        assert!(check(None).is_empty());
        let review = BranchDiff {
            branch: "review/T-0101.1".to_owned(),
            ..diff(None)
        };
        assert!(locations(&review).is_empty());
    }
}
