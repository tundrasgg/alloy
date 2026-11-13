//! `instruction-files`: `AGENTS.md` at the root is the only instruction entry
//! file (ADR 0004). Vendor instruction files are rejected at any depth.

use super::{Rule, Violation};
use crate::tree::Tree;

/// File names that agent tools read as instructions, compared without case.
const VENDOR_FILES: [&str; 9] = [
    "claude.md",
    "claude.local.md",
    "gemini.md",
    "agent.md",
    ".cursorrules",
    ".windsurfrules",
    ".clinerules",
    ".roorules",
    ".goosehints",
];

/// Directories whose contents agent tools read as instructions, compared
/// without case, as leading path components anywhere in a path.
const VENDOR_DIRS: [&[&str]; 5] = [
    &[".cursor", "rules"],
    &[".windsurf", "rules"],
    &[".clinerules"],
    &[".github", "instructions"],
    &[".github", "copilot-instructions.md"],
];

/// The one instruction entry file, valid only at the repository root.
const ENTRY: &str = "AGENTS.md";

pub(super) fn check(tree: &Tree) -> Vec<Violation> {
    tree.files()
        .filter_map(|path| {
            reason(path).map(|why| Violation::new(Rule::InstructionFiles, path, None, why))
        })
        .collect()
}

/// Why `path` is a forbidden instruction file, or `None` when it is not one.
fn reason(path: &str) -> Option<String> {
    let parts: Vec<String> = path.split('/').map(str::to_ascii_lowercase).collect();
    let name = parts.last()?;
    if name == "agents.md" && path != ENTRY {
        return Some(format!(
            "`{ENTRY}` is the only instruction entry file and lives only at the repository root"
        ));
    }
    if VENDOR_FILES.contains(&name.as_str()) {
        return Some(format!(
            "vendor instruction file; `{ENTRY}` is the only instruction entry file"
        ));
    }
    let in_vendor_dir = VENDOR_DIRS.iter().any(|dir| {
        parts
            .windows(dir.len())
            .any(|window| window.iter().zip(dir.iter()).all(|(a, b)| a == b))
    });
    in_vendor_dir.then(|| {
        format!(
            "file in a vendor instruction directory; `{ENTRY}` is the only instruction entry file"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_vendor_files_at_any_depth_and_in_any_case() {
        for path in [
            "CLAUDE.md",
            "CLAUDE.local.md",
            "a/b/claude.md",
            ".claude/CLAUDE.md",
            "GEMINI.md",
            "x/.cursorrules",
            ".cursor/rules/style.mdc",
            ".github/copilot-instructions.md",
            "crates/x/AGENTS.md",
            "agents.md",
        ] {
            assert!(reason(path).is_some(), "{path} should be rejected");
        }
    }

    #[test]
    fn accepts_the_root_entry_and_vendor_bindings() {
        for path in [
            "AGENTS.md",
            ".claude/agents/developer.md",
            ".claude/harness.md",
            "agent/ROOT.md",
            "docs/claude.mdx",
            ".github/workflows/ci.yml",
        ] {
            assert_eq!(reason(path), None, "{path} should be accepted");
        }
    }
}
