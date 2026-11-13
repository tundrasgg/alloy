//! `path-lock`: only `verify` tasks change the verifier's paths
//! (`agent/eng/testing.md`, section Path lock).

use super::{BranchDiff, Rule, Violation};
use crate::text::front_matter;
use crate::tree::Tree;

/// The paths written only on verifier branches.
const LOCKED: [&str; 4] = [
    "tests/conformance/",
    "tests/sim/",
    "tests/natlab/",
    "tests/system/",
];

pub(super) fn check(tree: &Tree, diff: Option<&BranchDiff>) -> Vec<Violation> {
    let Some(diff) = diff else {
        return Vec::new();
    };
    let Some(id) = task_id(&diff.branch) else {
        return Vec::new();
    };
    let task_path = format!("plan/tasks/{id}.md");
    let violation =
        |path: &str, message: String| Violation::new(Rule::PathLock, path, None, message);
    let text = match tree.read(&task_path) {
        Ok(text) if tree.contains(&task_path) => text,
        _ => {
            return vec![violation(
                &task_path,
                format!("branch `{}` names a task with no task file", diff.branch),
            )];
        }
    };
    let kind = front_matter(&text)
        .ok()
        .and_then(|fields| fields.into_iter().find(|f| f.key == "type"))
        .map(|f| f.value);
    let Some(kind) = kind else {
        return vec![violation(
            &task_path,
            "the task file has no `type` in its front matter".to_owned(),
        )];
    };
    if kind == "verify" {
        return Vec::new();
    }
    diff.changed
        .iter()
        .filter(|path| LOCKED.iter().any(|prefix| path.starts_with(prefix)))
        .map(|path| {
            violation(
                path,
                format!(
                    "locked to verify tasks, and branch `{}` is a `{kind}` task",
                    diff.branch
                ),
            )
        })
        .collect()
}

/// The task id in a task branch name, `task/T-NNNN`.
fn task_id(branch: &str) -> Option<&str> {
    let id = branch.strip_prefix("task/")?;
    let digits = id.strip_prefix("T-")?;
    (digits.len() >= 4 && digits.chars().all(|c| c.is_ascii_digit())).then_some(id)
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
    }
}
