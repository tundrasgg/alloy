//! Text helpers shared by the repository rules: Markdown fences and code spans,
//! front matter, and token search with identifier boundaries.

/// One line of a Markdown file with its 1-based number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MdLine<'a> {
    pub(crate) number: usize,
    pub(crate) text: &'a str,
    /// True for fence lines and for every line between an opening and a closing
    /// fence.
    pub(crate) in_fence: bool,
}

/// Splits Markdown into lines and marks the ones inside fenced code blocks.
///
/// A fence is a line whose first non-blank characters are three or more backticks
/// or tildes. It closes at the next fence of the same character that is at least
/// as long. An unclosed fence runs to the end of the file.
pub(crate) fn md_lines(text: &str) -> Vec<MdLine<'_>> {
    let mut open: Option<(char, usize)> = None;
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let fence = fence_of(line);
        let in_fence = match (open, fence) {
            (None, Some(f)) => {
                open = Some(f);
                true
            }
            (Some((c, n)), Some((fc, fnum))) if c == fc && fnum >= n => {
                open = None;
                true
            }
            (Some(_), _) => true,
            (None, None) => false,
        };
        out.push(MdLine {
            number: index + 1,
            text: line,
            in_fence,
        });
    }
    out
}

fn fence_of(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let run = trimmed.chars().take_while(|&c| c == first).count();
    (run >= 3).then_some((first, run))
}

/// Splits a Markdown line into prose (with code spans blanked out) and the
/// contents of its inline code spans.
///
/// Backtick runs of any length delimit spans. An unmatched opening run is kept
/// as prose, as CommonMark does.
pub(crate) fn code_spans(line: &str) -> (String, Vec<&str>) {
    let mut prose = String::new();
    let mut spans = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        prose.push_str(&rest[..start]);
        let after = &rest[start..];
        let run = after.chars().take_while(|&c| c == '`').count();
        let delimiter = &after[..run];
        let body = &after[run..];
        match find_closing_run(body, run) {
            Some(end) => {
                spans.push(body[..end].trim());
                prose.push(' ');
                rest = &body[end + run..];
            }
            None => {
                prose.push_str(delimiter);
                rest = body;
            }
        }
    }
    prose.push_str(rest);
    (prose, spans)
}

/// Finds a run of exactly `run` backticks in `body`.
fn find_closing_run(body: &str, run: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let len = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if len == run {
                return Some(i);
            }
            i += len;
        } else {
            i += 1;
        }
    }
    None
}

/// One `key: value` line of front matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    /// 1-based line number in the file.
    pub(crate) line: usize,
    pub(crate) key: String,
    pub(crate) value: String,
}

/// Why front matter could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrontMatterError {
    pub(crate) line: usize,
    pub(crate) message: String,
}

/// Parses the front matter at the top of a file: a `---` line, `key: value`
/// lines, and a closing `---` line.
///
/// Total over any input.
pub(crate) fn front_matter(text: &str) -> Result<Vec<Field>, FrontMatterError> {
    let mut lines = text.lines().enumerate();
    match lines.next() {
        Some((_, "---")) => {}
        _ => {
            return Err(FrontMatterError {
                line: 1,
                message: "file does not start with a `---` front matter line".to_owned(),
            });
        }
    }
    let mut fields = Vec::new();
    for (index, line) in lines {
        let number = index + 1;
        if line == "---" {
            return Ok(fields);
        }
        match line.split_once(':') {
            Some((key, value)) if is_key(key) => fields.push(Field {
                line: number,
                key: key.to_owned(),
                value: value.trim().to_owned(),
            }),
            _ => {
                return Err(FrontMatterError {
                    line: number,
                    message: format!("front matter line is not `key: value`: `{line}`"),
                });
            }
        }
    }
    Err(FrontMatterError {
        line: 1,
        message: "front matter has no closing `---` line".to_owned(),
    })
}

fn is_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The `name` in the `[package]` table of a Cargo manifest, or `None` when the
/// manifest names none.
pub(crate) fn package_name(manifest: &str) -> Option<&str> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package {
            let value = line
                .strip_prefix("name")
                .map(str::trim_start)
                .and_then(|rest| rest.strip_prefix('='))
                .map(|v| v.trim().trim_matches('"'));
            if let Some(name) = value.filter(|v| !v.is_empty()) {
                return Some(name);
            }
        }
    }
    None
}

/// Parses a front matter list, `[a, b]` or `[]`. Returns `None` when the value
/// is not bracketed or holds an empty item.
pub(crate) fn list_items(value: &str) -> Option<Vec<&str>> {
    let inner = value.strip_prefix('[')?.strip_suffix(']')?.trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    let items: Vec<&str> = inner.split(',').map(str::trim).collect();
    if items.iter().any(|item| item.is_empty()) {
        return None;
    }
    Some(items)
}

/// Matches a `/`-separated path against a glob: `*` matches any run of
/// characters within one component, and a `**` component matches any number of
/// components, including none.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_components(&pattern, &path)
}

fn match_components(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_components(rest, &path[skip..])),
        Some((first, rest)) => path.split_first().is_some_and(|(head, tail)| {
            match_component(first, head) && match_components(rest, tail)
        }),
    }
}

fn match_component(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((prefix, rest)) => {
            let Some(remaining) = name.strip_prefix(prefix) else {
                return false;
            };
            (0..=remaining.len())
                .filter(|&k| remaining.is_char_boundary(k))
                .any(|k| match_component(rest, &remaining[k..]))
        }
    }
}

/// True for characters that can continue a Rust identifier.
pub(crate) fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Finds `pat` in `hay` where it does not extend an identifier on either side.
///
/// The check applies only on a side where `pat` itself starts or ends with an
/// identifier character, so `.unwrap()` matches after any character.
pub(crate) fn find_token(hay: &str, pat: &str) -> Option<usize> {
    let check_before = pat.chars().next().is_some_and(is_ident);
    let check_after = pat.chars().last().is_some_and(is_ident);
    let mut from = 0;
    while let Some(offset) = hay[from..].find(pat) {
        let start = from + offset;
        let end = start + pat.len();
        let before_ok = !check_before || !hay[..start].chars().next_back().is_some_and(is_ident);
        let after_ok = !check_after || !hay[end..].chars().next().is_some_and(is_ident);
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + pat.chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// Finds every date-like `YYYY-MM-DD` string in `line`: four digits, a month from
/// 01 to 12, and a day from 01 to 31, with no digit directly before or after.
pub(crate) fn find_dates(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i + 10 <= bytes.len() {
        let window = &bytes[i..i + 10];
        let shape = window.iter().enumerate().all(|(k, b)| match k {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        });
        let before = i == 0 || !bytes[i - 1].is_ascii_digit();
        let after = bytes.get(i + 10).is_none_or(|b| !b.is_ascii_digit());
        if shape && before && after {
            let month = u32::from(window[5] - b'0') * 10 + u32::from(window[6] - b'0');
            let day = u32::from(window[8] - b'0') * 10 + u32::from(window[9] - b'0');
            if (1..=12).contains(&month) && (1..=31).contains(&day) {
                found.push(&line[i..i + 10]);
                i += 10;
                continue;
            }
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_cover_their_own_lines_and_the_body() {
        // A tilde line does not close a backtick fence; a longer backtick run does.
        let text = "a\n```\nb\n~~~\n````\nc\n";
        let flags: Vec<bool> = md_lines(text).iter().map(|l| l.in_fence).collect();
        assert_eq!(flags, [false, true, true, true, true, false]);
    }

    #[test]
    fn code_spans_are_split_from_prose() {
        let (prose, spans) = code_spans("see `a/b.md` and ``x ` y`` then `open");
        assert_eq!(spans, ["a/b.md", "x ` y"]);
        assert_eq!(prose, "see   and   then `open");
    }

    #[test]
    fn front_matter_reads_fields_and_rejects_bad_shapes() {
        let ok = front_matter("---\nid: T-0001\nspec: []\n---\nbody").map(|f| {
            f.into_iter()
                .map(|f| (f.line, f.key, f.value))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            ok,
            Ok(vec![
                (2, "id".to_owned(), "T-0001".to_owned()),
                (3, "spec".to_owned(), "[]".to_owned()),
            ])
        );
        assert_eq!(front_matter("body").map_err(|e| e.line), Err(1));
        assert_eq!(front_matter("---\nid: x\n").map_err(|e| e.line), Err(1));
        assert_eq!(
            front_matter("---\nnot a field\n---").map_err(|e| e.line),
            Err(2)
        );
    }

    #[test]
    fn lists_parse_and_reject_empty_items() {
        assert_eq!(list_items("[]"), Some(vec![]));
        assert_eq!(list_items("[a, b/c]"), Some(vec!["a", "b/c"]));
        assert_eq!(list_items("[a, ]"), None);
        assert_eq!(list_items("a"), None);
    }

    #[test]
    fn globs_match_within_and_across_components() {
        assert!(glob_match("adr/0037-*.md", "adr/0037-github-hosts.md"));
        assert!(!glob_match("adr/0037-*.md", "adr/0038-x.md"));
        assert!(!glob_match("adr/*.md", "adr/sub/x.md"));
        assert!(glob_match("xtask/**", "xtask/src/main.rs"));
        assert!(glob_match("crates/**/lib.rs", "crates/lib.rs"));
        assert!(glob_match("crates/*/src/*.rs", "crates/a/src/b.rs"));
        assert!(!glob_match("xtask/**", "plan/x.md"));
    }

    #[test]
    fn tokens_respect_identifier_boundaries() {
        assert_eq!(find_token("use std::fs;", "std::fs"), Some(4));
        assert_eq!(find_token("use mystd::fs;", "std::fs"), None);
        assert_eq!(find_token("std::fsx", "std::fs"), None);
        assert_eq!(find_token("x.unwrap()", ".unwrap()"), Some(1));
        assert_eq!(find_token("a_unsafe unsafe", "unsafe"), Some(9));
    }

    #[test]
    fn dates_need_the_shape_and_plausible_fields() {
        assert_eq!(find_dates("on 2026-11-09, ok"), ["2026-11-09"]);
        assert!(find_dates("2026-13-01 2026-01-32 12026-01-01 2026-01-019").is_empty());
        assert!(find_dates("arfc-0005#3.1.1 T-0001").is_empty());
    }
}
