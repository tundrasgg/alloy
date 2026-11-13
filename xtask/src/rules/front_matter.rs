//! `front-matter`: task files and reports carry the front matter of their
//! templates, and every path a board-`active` task reads exists.
//!
//! The keys, their order, the allowed values of `<a | b>` fields, and which
//! fields are lists come from `agent/templates/`, so a template change applies
//! without a code change. Checks the templates cannot express, such as file
//! names matching ids, are written here.

use super::{Rule, Violation};
use crate::text::{Field, front_matter, glob_match, list_items, md_lines};
use crate::tree::Tree;

const TASK_TEMPLATE: &str = "agent/templates/task.md";
const REPORT_TEMPLATE: &str = "agent/templates/report.md";
const TASKS: &str = "plan/tasks/";
const REPORTS: &str = "plan/reports/";
const BOARD: &str = "plan/STATE.md";

/// What a template allows for one field.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shape {
    /// `[...]`: a bracketed list, possibly empty.
    List,
    /// `<a | b | c>`: one of the listed words.
    OneOf(Vec<String>),
    /// Anything else: a non-empty value that is not a template placeholder.
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Template {
    fields: Vec<(String, Shape)>,
}

impl Template {
    fn parse(text: &str) -> Option<Self> {
        let fields = front_matter(text).ok()?;
        let fields = fields
            .into_iter()
            .map(|f| {
                let shape = shape_of(&f.value);
                (f.key, shape)
            })
            .collect();
        Some(Self { fields })
    }
}

fn shape_of(value: &str) -> Shape {
    if value.starts_with('[') {
        return Shape::List;
    }
    let choices: Option<Vec<String>> = value
        .strip_prefix('<')
        .and_then(|v| v.strip_suffix('>'))
        .map(|inner| inner.split('|').map(|c| c.trim().to_owned()).collect());
    match choices {
        Some(words)
            if words.len() > 1
                && words.iter().all(|w| {
                    !w.is_empty()
                        && w.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                }) =>
        {
            Shape::OneOf(words)
        }
        _ => Shape::Text,
    }
}

pub(super) fn check(tree: &Tree) -> Vec<Violation> {
    let mut out = Vec::new();
    let documents = [
        (TASK_TEMPLATE, TASKS, Kind::Task),
        (REPORT_TEMPLATE, REPORTS, Kind::Report),
    ];
    for (template_path, dir, kind) in documents {
        let files: Vec<&str> = tree
            .under(dir)
            .filter(|p| p.ends_with(".md") && !p[dir.len()..].contains('/'))
            .filter(|p| !p.ends_with("/README.md"))
            .collect();
        if files.is_empty() {
            continue;
        }
        let template = tree
            .read(template_path)
            .ok()
            .and_then(|t| Template::parse(&t));
        let Some(template) = template else {
            out.push(Violation::new(
                Rule::FrontMatter,
                template_path,
                None,
                "template is missing or has no front matter",
            ));
            continue;
        };
        for path in files {
            check_file(tree, path, &template, kind, &mut out);
        }
    }
    check_active_reads(tree, &mut out);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Task,
    Report,
}

fn check_file(tree: &Tree, path: &str, template: &Template, kind: Kind, out: &mut Vec<Violation>) {
    let mut report = |line: Option<usize>, message: String| {
        out.push(Violation::new(Rule::FrontMatter, path, line, message));
    };
    let text = match tree.read(path) {
        Ok(text) => text,
        Err(err) => return report(None, err),
    };
    let fields = match front_matter(&text) {
        Ok(fields) => fields,
        Err(err) => return report(Some(err.line), err.message),
    };
    let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
    let expected: Vec<&str> = template.fields.iter().map(|(k, _)| k.as_str()).collect();
    if keys != expected {
        report(
            Some(1),
            format!(
                "front matter keys are [{}], the template has [{}] in that order",
                keys.join(", "),
                expected.join(", ")
            ),
        );
    }
    for field in &fields {
        let shape = template
            .fields
            .iter()
            .find(|(k, _)| *k == field.key)
            .map(|(_, s)| s);
        if let Some(message) = shape.and_then(|s| shape_error(field, s)) {
            report(Some(field.line), message);
        }
    }
    let stem = path
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".md"))
        .unwrap_or_default();
    let messages = match kind {
        Kind::Task => task_errors(stem, &fields),
        Kind::Report => report_errors(stem, &fields),
    };
    for (line, message) in messages {
        report(Some(line), message);
    }
}

fn shape_error(field: &Field, shape: &Shape) -> Option<String> {
    let value = field.value.as_str();
    match shape {
        Shape::List if list_items(value).is_none() => Some(format!(
            "`{}` must be a list like `[a, b]` or `[]`, found `{value}`",
            field.key
        )),
        Shape::OneOf(words) if !words.iter().any(|w| w == value) => Some(format!(
            "`{}` must be one of {}, found `{value}`",
            field.key,
            words.join(", ")
        )),
        Shape::Text if value.is_empty() || value.starts_with('<') => Some(format!(
            "`{}` must be filled in, found `{value}`",
            field.key
        )),
        _ => None,
    }
}

fn is_task_id(value: &str) -> bool {
    value
        .strip_prefix("T-")
        .is_some_and(|d| d.len() >= 4 && d.chars().all(|c| c.is_ascii_digit()))
}

fn field<'a>(fields: &'a [Field], key: &str) -> Option<&'a Field> {
    fields.iter().find(|f| f.key == key)
}

fn task_errors(stem: &str, fields: &[Field]) -> Vec<(usize, String)> {
    let mut errors = Vec::new();
    if let Some(id) = field(fields, "id")
        && (!is_task_id(&id.value) || id.value != stem)
    {
        errors.push((
            id.line,
            format!(
                "`id` must be the file name's task id `{stem}`, found `{}`",
                id.value
            ),
        ));
    }
    if let Some(milestone) = field(fields, "milestone") {
        let ok = milestone
            .value
            .strip_prefix('M')
            .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
        if !ok {
            errors.push((
                milestone.line,
                format!("`milestone` must be like `M0`, found `{}`", milestone.value),
            ));
        }
    }
    if let Some(depends) = field(fields, "depends") {
        let items = list_items(&depends.value).unwrap_or_default();
        for item in items.into_iter().filter(|i| !is_task_id(i)) {
            errors.push((
                depends.line,
                format!("`depends` holds `{item}`, not a task id"),
            ));
        }
    }
    if let Some(pair) = field(fields, "pair")
        && pair.value != "none"
        && !is_task_id(&pair.value)
    {
        errors.push((
            pair.line,
            format!("`pair` must be a task id or `none`, found `{}`", pair.value),
        ));
    }
    errors
}

fn report_errors(stem: &str, fields: &[Field]) -> Vec<(usize, String)> {
    let mut errors = Vec::new();
    let mut parts = stem.splitn(3, '.');
    let names = [
        ("task", parts.next()),
        ("role", parts.next()),
        ("round", parts.next()),
    ];
    for (key, from_name) in names {
        let Some(f) = field(fields, key) else {
            continue;
        };
        if Some(f.value.as_str()) != from_name {
            errors.push((
                f.line,
                format!(
                    "`{key}` is `{}` but the file name `{stem}.md` says `{}`",
                    f.value,
                    from_name.unwrap_or_default()
                ),
            ));
        }
    }
    if let Some(task) = field(fields, "task")
        && !is_task_id(&task.value)
    {
        errors.push((
            task.line,
            format!("`task` must be a task id, found `{}`", task.value),
        ));
    }
    if let Some(round) = field(fields, "round")
        && !round.value.parse::<u32>().is_ok_and(|n| n >= 1)
    {
        errors.push((
            round.line,
            format!(
                "`round` must be a whole number from 1, found `{}`",
                round.value
            ),
        ));
    }
    errors
}

/// Checks that every path in `reads` exists, for each task the board marks
/// `active`.
fn check_active_reads(tree: &Tree, out: &mut Vec<Violation>) {
    let Ok(board) = tree.read(BOARD) else {
        return;
    };
    for (line, id) in active_tasks(&board) {
        let path = format!("{TASKS}{id}.md");
        let text = match tree.read(&path) {
            Ok(text) if tree.contains(&path) => text,
            _ => {
                out.push(Violation::new(
                    Rule::FrontMatter,
                    BOARD,
                    Some(line),
                    format!("active task `{id}` has no task file `{path}`"),
                ));
                continue;
            }
        };
        let Some(reads) = front_matter(&text)
            .ok()
            .and_then(|fields| fields.into_iter().find(|f| f.key == "reads"))
        else {
            continue;
        };
        for item in list_items(&reads.value).unwrap_or_default() {
            let exists = if item.contains('*') {
                tree.files().any(|file| glob_match(item, file))
            } else {
                tree.contains(item)
            };
            if !exists {
                out.push(Violation::new(
                    Rule::FrontMatter,
                    &path,
                    Some(reads.line),
                    format!(
                        "`reads` names `{item}`, which matches nothing, and the task is active"
                    ),
                ));
            }
        }
    }
}

/// The ids of board rows whose status is `active`, with their line numbers.
///
/// The board is the first table whose header has `Task` and `Status` columns.
fn active_tasks(board: &str) -> Vec<(usize, String)> {
    let mut columns: Option<(usize, usize)> = None;
    let mut found = Vec::new();
    for line in md_lines(board).into_iter().filter(|l| !l.in_fence) {
        let text = line.text.trim();
        if !text.starts_with('|') {
            if columns.is_some() && !found.is_empty() {
                break;
            }
            columns = None;
            continue;
        }
        let cells: Vec<&str> = text.trim_matches('|').split('|').map(str::trim).collect();
        match columns {
            None => {
                let task = cells.iter().position(|c| *c == "Task");
                let status = cells.iter().position(|c| *c == "Status");
                if let (Some(t), Some(s)) = (task, status) {
                    columns = Some((t, s));
                }
            }
            Some((t, s)) => {
                let id = cells.get(t).copied().unwrap_or_default();
                if cells.get(s) == Some(&"active") && is_task_id(id) {
                    found.push((line.number, id.to_owned()));
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_shapes_are_derived_from_placeholders() {
        assert_eq!(shape_of("[<path>, ...]"), Shape::List);
        assert_eq!(
            shape_of("<done | blocked | failed>"),
            Shape::OneOf(vec!["done".into(), "blocked".into(), "failed".into()])
        );
        assert_eq!(
            shape_of("<T-NNNN of the paired task, or none>"),
            Shape::Text
        );
        assert_eq!(shape_of("<role>"), Shape::Text);
    }

    #[test]
    fn board_rows_marked_active_are_found() {
        let board = "| Series | Next |\n|---|---|\n| Task `T-` | T-0030 |\n\n\
                     ## Board\n\n| Task | Type | Status |\n|---|---|---|\n\
                     | T-0001 | build | done |\n| T-0002 | build | active |\n";
        assert_eq!(active_tasks(board), [(10, "T-0002".to_owned())]);
    }
}
