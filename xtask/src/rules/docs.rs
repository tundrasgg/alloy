//! Rules over the document trees: `links` and `dates`.

use super::{Rule, Violation};
use crate::text::{code_spans, find_dates, md_lines};
use crate::tree::Tree;

/// The trees whose documents these rules read.
const DOC_TREES: [&str; 4] = ["agent/", "spec/", "adr/", "plan/"];

/// The top-level directories a backticked path must start with to count as a
/// repository path. These hold documents that exist when they are cited. Paths
/// into code and test trees often name work that a later task creates, so they
/// are not checked.
const PATH_ROOTS: [&str; 6] = ["agent/", "spec/", "adr/", "plan/", "product/", "research/"];

/// The trees whose links the `links` rule checks: every document tree except
/// `plan/` (D-0011). Task files and reports are immutable once written and cite
/// files on branches that are not merged yet, and `plan/STATE.md` cites them
/// too, so a broken citation there could never be fixed.
const LINK_TREES: [&str; 3] = ["agent/", "spec/", "adr/"];

fn documents(tree: &Tree) -> impl Iterator<Item = &str> {
    DOC_TREES.into_iter().flat_map(|dir| tree.under(dir))
}

/// `links`: relative Markdown links and backticked repository paths resolve.
/// Documents under `plan/` are not checked, though paths into `plan/` cited
/// from other trees are.
pub(super) fn check_links(tree: &Tree) -> Vec<Violation> {
    let mut out = Vec::new();
    let linked = LINK_TREES.into_iter().flat_map(|dir| tree.under(dir));
    for path in linked.filter(|p| p.ends_with(".md")) {
        let text = match tree.read(path) {
            Ok(text) => text,
            Err(err) => {
                out.push(Violation::new(Rule::Links, path, None, err));
                continue;
            }
        };
        let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        for line in md_lines(&text).into_iter().filter(|l| !l.in_fence) {
            let (prose, spans) = code_spans(line.text);
            for target in link_targets(&prose) {
                if let Some(problem) = link_problem(tree, dir, target) {
                    out.push(Violation::new(
                        Rule::Links,
                        path,
                        Some(line.number),
                        problem,
                    ));
                }
            }
            for span in spans {
                if let Some(cited) = repository_path(span)
                    && !tree.contains(cited)
                {
                    out.push(Violation::new(
                        Rule::Links,
                        path,
                        Some(line.number),
                        format!("`{span}` does not exist"),
                    ));
                }
            }
        }
    }
    out
}

/// The targets of inline links `[text](target)` and reference definitions
/// `[label]: target` in a line of prose.
fn link_targets(prose: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut rest = prose;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let end = after.find(')').unwrap_or(after.len());
        let inner = after[..end].trim();
        let target = inner
            .strip_prefix('<')
            .and_then(|t| t.split_once('>'))
            .map_or_else(|| inner.split_whitespace().next().unwrap_or(""), |(t, _)| t);
        targets.push(target);
        rest = &after[end..];
    }
    let trimmed = prose.trim_start();
    if trimmed.starts_with('[')
        && let Some((_, def)) = trimmed.split_once("]:")
        && let Some(target) = def.split_whitespace().next()
    {
        targets.push(target);
    }
    targets
}

/// Why a link target from a file in `dir` does not resolve, or `None` when it
/// does or is not a repository link.
fn link_problem(tree: &Tree, dir: &str, target: &str) -> Option<String> {
    let path = target.split(['#', '?']).next().unwrap_or_default();
    let external = target.contains("://") || target.starts_with("mailto:");
    if path.is_empty() || external {
        return None;
    }
    let base = if path.starts_with('/') { "" } else { dir };
    match resolve(base, path) {
        Some(resolved) if tree.contains(&resolved) => None,
        Some(resolved) => Some(format!(
            "link `{target}` points to `{resolved}`, which does not exist"
        )),
        None => Some(format!("link `{target}` leaves the repository")),
    }
}

/// Joins `path` onto `dir` and removes `.` and `..` components. Returns `None`
/// when the result would leave the repository root.
fn resolve(dir: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The repository path a code span cites, or `None` when the span is not one.
///
/// A span cites a path when it starts with one of [`PATH_ROOTS`] and holds no
/// whitespace or placeholder (`<role>`, `T-NNNN`, `*`, `...`). A trailing
/// `:line`, `:first-last` line range, or `#fragment` is dropped.
fn repository_path(span: &str) -> Option<&str> {
    if !PATH_ROOTS.iter().any(|root| span.starts_with(root)) {
        return None;
    }
    let placeholder = span.contains(char::is_whitespace)
        || span.contains(['<', '>', '*', '?', '{', '}', '[', ']', '$', '|'])
        || span.contains("NNNN")
        || span.contains("...");
    if placeholder {
        return None;
    }
    let mut path = span.split('#').next().unwrap_or_default();
    while let Some((head, tail)) = path.rsplit_once(':') {
        if !is_line_location(tail) {
            break;
        }
        path = head;
    }
    Some(path)
}

/// Whether `tail`, the text after a `:`, is a line number `N` or a line range
/// `N-M`.
fn is_line_location(tail: &str) -> bool {
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    match tail.split_once('-') {
        Some((first, last)) => number(first) && number(last),
        None => number(tail),
    }
}

/// `dates`: no `YYYY-MM-DD` dates in documents outside fenced code blocks
/// (ADR 0032).
pub(super) fn check_dates(tree: &Tree) -> Vec<Violation> {
    let mut out = Vec::new();
    for path in documents(tree) {
        let text = match tree.read(path) {
            Ok(text) => text,
            Err(err) => {
                out.push(Violation::new(Rule::Dates, path, None, err));
                continue;
            }
        };
        let markdown = path.ends_with(".md");
        for line in md_lines(&text) {
            if markdown && line.in_fence {
                continue;
            }
            for date in find_dates(line.text) {
                out.push(Violation::new(
                    Rule::Dates,
                    path,
                    Some(line.number),
                    format!("date `{date}`; files carry no dates, git is the clock"),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_targets_are_extracted() {
        assert_eq!(
            link_targets("see [a](x.md) and [b](<y z.md>) or [c](../w.md \"t\")"),
            ["x.md", "y z.md", "../w.md"]
        );
        assert_eq!(link_targets("[ref]: ./r.md"), ["./r.md"]);
        assert!(link_targets("no links [here]").is_empty());
    }

    #[test]
    fn resolve_handles_dots_and_refuses_to_leave_the_root() {
        assert_eq!(resolve("spec", "arfc.md"), Some("spec/arfc.md".to_owned()));
        assert_eq!(
            resolve("plan/tasks", "../STATE.md"),
            Some("plan/STATE.md".to_owned())
        );
        assert_eq!(
            resolve("", "/agent/./ROOT.md"),
            Some("agent/ROOT.md".to_owned())
        );
        assert_eq!(resolve("spec", "../../x"), None);
    }

    #[test]
    fn repository_paths_skip_placeholders_and_strip_locations() {
        assert_eq!(repository_path("agent/ROOT.md"), Some("agent/ROOT.md"));
        assert_eq!(
            repository_path("agent/roles/lead.md:35"),
            Some("agent/roles/lead.md")
        );
        assert_eq!(repository_path("spec/a.md:117-120"), Some("spec/a.md"));
        assert_eq!(repository_path("spec/a.md#3.1"), Some("spec/a.md"));
        assert_eq!(repository_path("spec/a.md:1-"), Some("spec/a.md:1-"));
        assert_eq!(repository_path("spec/a.md:-2"), Some("spec/a.md:-2"));
        assert_eq!(repository_path("spec/a.md:1-2-3"), Some("spec/a.md:1-2-3"));
        assert_eq!(repository_path("plan/tasks/T-NNNN.md"), None);
        assert_eq!(repository_path("adr/0037-<slug>.md"), None);
        assert_eq!(repository_path("crates/*/src"), None);
        assert_eq!(repository_path("tests/conformance/"), None);
        assert_eq!(repository_path("cargo xtask"), None);
    }
}
