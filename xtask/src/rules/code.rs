//! Rules over Rust sources in `crates/`: `banned-patterns`, `direct-io`, and
//! `unsafe`.
//!
//! Sources are lexed so that comments and string contents never match a code
//! pattern. The lexer is line based and approximate: it knows comments, string,
//! raw string, and character literals, and nothing else of Rust's grammar.
//!
//! Clippy enforces the `direct-io` ban itself, on resolved paths, through
//! `disallowed-methods` and `disallowed-types` in the root `clippy.toml`
//! (D-0017). The `direct-io` rule here keeps that ban in force: it rejects the
//! ways to switch it off, and the values Clippy cannot see.

use super::{Rule, Violation};
use crate::text::{find_token, is_ident, package_name};
use crate::tree::Tree;

/// Crates that may call the operating system directly (`agent/eng/rust.md`,
/// ARFC 0005 clause 3.1.1).
const IO_CRATES: [&str; 2] = ["alloy-rt", "alloy-sim"];

/// Crates that may contain `unsafe` (`agent/eng/rust.md`, section Unsafe).
const UNSAFE_CRATES: [&str; 3] = ["alloy-rt", "alloy-net", "alloy-store"];

/// Unit structs that reach operating system randomness when used as a value,
/// as in `OsRng.try_next_u64()`. Clippy's `disallowed_types` sees a type only
/// where it is named as a type, never as a value, so these stay text patterns.
const IO_VALUES: [&str; 2] = ["OsRng", "SysRng"];

/// The lints that carry the `direct-io` ban and the lint groups that hold
/// them, as an `allow` or `expect` names them. A source attribute overrides the
/// `-D` that `cargo xtask ci` passes on the command line. The singular names
/// are the lints' former names, which rustc still applies to them.
const BAN_LINTS: [&str; 6] = [
    "clippy::disallowed_methods",
    "clippy::disallowed_types",
    "clippy::disallowed_method",
    "clippy::disallowed_type",
    "clippy::style",
    "clippy::all",
];

/// The one Clippy configuration file. Clippy reads the nearest `clippy.toml` or
/// `.clippy.toml` at or above a crate's manifest, so any other would replace
/// this one, and its `direct-io` ban, for the crates below it.
const CLIPPY_CONFIG: &str = "clippy.toml";

/// Keywords that start an item. A test attribute on an item that starts with
/// one applies up to the item's body or `;`; on anything else, such as a
/// field, a match arm, or a statement, it also ends at a `,`.
const ITEM_KEYWORDS: [&str; 15] = [
    "async",
    "const",
    "enum",
    "extern",
    "fn",
    "impl",
    "macro_rules",
    "mod",
    "static",
    "struct",
    "trait",
    "type",
    "union",
    "unsafe",
    "use",
];

/// One source line, split into code and comment text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Line {
    /// The code with comments removed and literal contents blanked, and with
    /// whitespace next to punctuation removed, so `x . unwrap ( )` reads
    /// `x.unwrap()`.
    code: String,
    /// The text of any comments on the line.
    comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Code,
    LineComment,
    BlockComment(usize),
    Str,
    RawStr(usize),
}

/// Lexes Rust source into lines. Total over any input.
fn lex(src: &str) -> Vec<Line> {
    let chars: Vec<char> = src.chars().filter(|&c| c != '\r').collect();
    let at = |i: usize| chars.get(i).copied();
    let mut lines = Vec::new();
    let mut code = String::new();
    let mut comment = String::new();
    let mut state = State::Code;
    let mut i = 0;
    while let Some(c) = at(i) {
        if c == '\n' {
            if state == State::LineComment {
                state = State::Code;
            }
            lines.push(Line {
                code: squeeze(&code),
                comment: comment.trim().to_owned(),
            });
            code.clear();
            comment.clear();
            i += 1;
            continue;
        }
        match state {
            State::Code => {
                if c == '/' && at(i + 1) == Some('/') {
                    state = State::LineComment;
                    i += 2;
                } else if c == '/' && at(i + 1) == Some('*') {
                    state = State::BlockComment(1);
                    code.push(' ');
                    i += 2;
                } else if c == '"' {
                    state = State::Str;
                    code.push('"');
                    i += 1;
                } else if let Some((hashes, len)) = raw_string_start(&chars, i) {
                    state = State::RawStr(hashes);
                    code.push('"');
                    i += len;
                } else if c == '\'' {
                    match char_literal_end(&chars, i) {
                        Some(end) => {
                            code.push_str("''");
                            i = end;
                        }
                        None => {
                            code.push('\'');
                            i += 1;
                        }
                    }
                } else {
                    code.push(c);
                    i += 1;
                }
            }
            State::LineComment => {
                comment.push(c);
                i += 1;
            }
            State::BlockComment(depth) => {
                if c == '*' && at(i + 1) == Some('/') {
                    state = if depth == 1 {
                        State::Code
                    } else {
                        State::BlockComment(depth - 1)
                    };
                    i += 2;
                } else if c == '/' && at(i + 1) == Some('*') {
                    state = State::BlockComment(depth + 1);
                    i += 2;
                } else {
                    comment.push(c);
                    i += 1;
                }
            }
            State::Str => {
                if c == '\\' {
                    i += if at(i + 1) == Some('\n') { 1 } else { 2 };
                } else {
                    if c == '"' {
                        state = State::Code;
                        code.push('"');
                    }
                    i += 1;
                }
            }
            State::RawStr(hashes) => {
                let closes = c == '"' && (1..=hashes).all(|k| at(i + k) == Some('#'));
                if closes {
                    state = State::Code;
                    code.push('"');
                    i += 1 + hashes;
                } else {
                    i += 1;
                }
            }
        }
    }
    lines.push(Line {
        code: squeeze(&code),
        comment: comment.trim().to_owned(),
    });
    lines
}

/// At `r"`, `r#"`, `br"`, or `cr"` that starts a token, returns the number of
/// `#` and the length of the opening delimiter from `i`.
fn raw_string_start(chars: &[char], i: usize) -> Option<(usize, usize)> {
    if chars.get(i) != Some(&'r') {
        return None;
    }
    let before = i.checked_sub(1).and_then(|k| chars.get(k)).copied();
    let starts_token = match before {
        None => true,
        Some('b' | 'c') => i
            .checked_sub(2)
            .and_then(|k| chars.get(k))
            .is_none_or(|&c| !is_ident(c)),
        Some(c) => !is_ident(c),
    };
    if !starts_token {
        return None;
    }
    let hashes = chars[i + 1..].iter().take_while(|&&c| c == '#').count();
    (chars.get(i + 1 + hashes) == Some(&'"')).then_some((hashes, 2 + hashes))
}

/// At a `'`, returns the index after the character literal it opens, or `None`
/// when it is a lifetime or a label.
fn char_literal_end(chars: &[char], i: usize) -> Option<usize> {
    match chars.get(i + 1)? {
        '\\' => {
            let rest = chars.get(i + 3..)?;
            let close = rest.iter().position(|&c| c == '\'' || c == '\n')?;
            (rest[close] == '\'').then_some(i + 4 + close)
        }
        '\n' => None,
        _ => (chars.get(i + 2) == Some(&'\'')).then_some(i + 3),
    }
}

/// Removes whitespace that touches a non-identifier character and collapses the
/// rest to one space.
fn squeeze(code: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in code.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space && is_ident(c) && out.chars().next_back().is_some_and(is_ident) {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    out
}

/// True when `code` holds an attribute that marks the next item as test code:
/// `#[test]`, `#[cfg(test)]`, or `#[<path>::test]`, the last with or without
/// arguments. A call such as `checks::test(1)` is not an attribute.
#[cfg(test)]
fn has_test_attribute(code: &str) -> bool {
    code.match_indices("#[")
        .any(|(at, open)| is_test_attribute(&code[at + open.len()..]))
}

/// True when `attr`, the text after a `#[`, starts a test attribute.
fn is_test_attribute(attr: &str) -> bool {
    if attr.starts_with("cfg(test)]") {
        return true;
    }
    let attr = attr.strip_prefix("::").unwrap_or(attr);
    let end = attr
        .find(|c: char| !is_ident(c) && c != ':')
        .unwrap_or(attr.len());
    let (path, after) = attr.split_at(end);
    let well_formed = path
        .split("::")
        .all(|segment| !segment.is_empty() && segment.chars().all(is_ident));
    well_formed
        && path.rsplit("::").next() == Some("test")
        && (after.starts_with(']') || after.starts_with('('))
}

/// What a test attribute applies to, as far as the first word after it tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// No word yet, or only `pub`.
    Unknown,
    /// An item: it ends at its body or at a `;`.
    Item,
    /// A field, variant, match arm, statement, or expression: it ends at its
    /// body, a `;`, or a `,`.
    Other,
}

/// A test attribute whose target has not reached its body yet.
#[derive(Debug, Clone, Copy)]
struct Pending {
    /// The bracket depth of the attribute, which is also its target's.
    depth: usize,
    target: Target,
}

/// For each line, whether it is inside test code: the target of a `#[test]` or
/// `#[cfg(test)]` attribute and that target's body, found by bracket matching.
///
/// A target with no body of its own, such as a struct field, a match arm, or
/// `mod tests;`, ends at the `,` or `;` after it, or at the bracket that closes
/// around it, so its attribute never reaches the next unrelated body.
fn test_lines(lines: &[Line]) -> Vec<bool> {
    let mut depth = 0usize;
    let mut regions: Vec<usize> = Vec::new();
    let mut pending: Option<Pending> = None;
    lines
        .iter()
        .map(|line| {
            let code = line.code.as_str();
            let mut in_test = !regions.is_empty() || pending.is_some();
            let mut chars = code.char_indices().peekable();
            while let Some((at, c)) = chars.next() {
                let rest = &code[at + c.len_utf8()..];
                if c == '#' && rest.strip_prefix('[').is_some_and(is_test_attribute) {
                    pending = Some(Pending {
                        depth,
                        target: Target::Unknown,
                    });
                    in_test = true;
                    continue;
                }
                if is_ident(c) {
                    let end = rest
                        .find(|w: char| !is_ident(w))
                        .map_or(code.len(), |n| at + c.len_utf8() + n);
                    while chars.next_if(|&(next, _)| next < end).is_some() {}
                    let word = &code[at..end];
                    if let Some(p) = pending.as_mut()
                        && p.depth == depth
                        && p.target == Target::Unknown
                        && word != "pub"
                    {
                        p.target = if ITEM_KEYWORDS.contains(&word) {
                            Target::Item
                        } else {
                            Target::Other
                        };
                    }
                    continue;
                }
                let at_target = pending.is_some_and(|p| p.depth == depth);
                match c {
                    '{' | '(' | '[' => {
                        if c == '{' && at_target {
                            regions.push(depth + 1);
                            pending = None;
                            in_test = true;
                        }
                        depth += 1;
                    }
                    '}' | ')' | ']' => {
                        if regions.last() == Some(&depth) {
                            regions.pop();
                        }
                        depth = depth.saturating_sub(1);
                        if pending.is_some_and(|p| p.depth > depth) {
                            pending = None;
                        }
                    }
                    ';' if at_target => pending = None,
                    ',' if at_target && pending.is_some_and(|p| p.target != Target::Item) => {
                        pending = None;
                    }
                    _ => {}
                }
            }
            in_test
        })
        .collect()
}

/// A Rust source file in `crates/`, lexed.
#[derive(Debug)]
struct Source<'a> {
    path: &'a str,
    krate: String,
    /// True for files that are tests as a whole: under a `tests` directory, or
    /// named `tests.rs`.
    test_file: bool,
    lines: Vec<Line>,
}

/// Lexes every `.rs` file under `crates/`. A file that cannot be read becomes a
/// violation of `rule`.
fn sources(tree: &Tree, rule: Rule, out: &mut Vec<Violation>) -> Vec<(String, Vec<Line>)> {
    let mut found = Vec::new();
    for path in tree.under("crates/").filter(|p| p.ends_with(".rs")) {
        match tree.read(path) {
            Ok(text) => found.push((path.to_owned(), lex(&text))),
            Err(err) => out.push(Violation::new(rule, path, None, err)),
        }
    }
    found
}

/// The package name of the crate holding `path`, from `crates/<dir>/Cargo.toml`,
/// or `<dir>` when the manifest names none.
fn crate_name(tree: &Tree, path: &str) -> String {
    let dir = path
        .strip_prefix("crates/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default();
    let manifest = tree
        .read(&format!("crates/{dir}/Cargo.toml"))
        .unwrap_or_default();
    package_name(&manifest).unwrap_or(dir).to_owned()
}

fn with_sources<F>(tree: &Tree, rule: Rule, mut visit: F) -> Vec<Violation>
where
    F: FnMut(&Source<'_>, &mut Vec<Violation>),
{
    let mut out = Vec::new();
    for (path, lines) in sources(tree, rule, &mut out) {
        let parts: Vec<&str> = path.split('/').collect();
        let source = Source {
            path: &path,
            krate: crate_name(tree, &path),
            test_file: parts.iter().skip(2).any(|p| *p == "tests")
                || parts.last() == Some(&"tests.rs"),
            lines,
        };
        visit(&source, &mut out);
    }
    out
}

/// `banned-patterns`: the code patterns that `agent/core/4-evidence.md` bans.
pub(super) fn check_banned(tree: &Tree) -> Vec<Violation> {
    with_sources(tree, Rule::BannedPatterns, |src, out| {
        let tests = test_lines(&src.lines);
        for (index, line) in src.lines.iter().enumerate() {
            let number = index + 1;
            let mut report = |message: &str| {
                out.push(Violation::new(
                    Rule::BannedPatterns,
                    src.path,
                    Some(number),
                    message,
                ));
            };
            let code = line.code.as_str();
            if find_token(code, "todo!").is_some() {
                report("`todo!` is a placeholder");
            }
            if find_token(code, "unimplemented!").is_some() {
                report("`unimplemented!` is a placeholder");
            }
            if find_token(code, "unreachable!()").is_some() {
                report(
                    "`unreachable!()` without a message is a placeholder; state why it cannot be reached",
                );
            }
            if find_token(code, "#[ignore").is_some() {
                report("`#[ignore]` on a test");
            }
            let note = line
                .comment
                .trim_start_matches(['/', '!', '*'])
                .trim_start();
            if note.starts_with("#[") && note.contains("test") {
                report("commented-out test");
            }
            let panics = find_token(code, ".unwrap()").is_some() || code.contains(".expect(");
            let in_test = src.test_file || tests[index];
            if panics && !in_test && !invariant_above(&src.lines, index) {
                report(
                    "`unwrap()` or `expect()` outside tests needs the invariant that makes it safe in a comment on the line above",
                );
            }
        }
    })
}

/// True when the line above `index` is a comment and nothing else.
fn invariant_above(lines: &[Line], index: usize) -> bool {
    index
        .checked_sub(1)
        .and_then(|above| lines.get(above))
        .is_some_and(|line| line.code.is_empty() && !line.comment.is_empty())
}

/// `direct-io`: what keeps the Clippy ban on operating system calls in force
/// outside the runtime crates. Rejects any Clippy configuration file but the
/// root `clippy.toml`; and, in every crate under `crates/` except `alloy-rt`
/// and `alloy-sim`, an `allow` or `expect` of a lint in [`BAN_LINTS`] and the
/// values in [`IO_VALUES`].
pub(super) fn check_direct_io(tree: &Tree) -> Vec<Violation> {
    let mut out: Vec<Violation> = tree
        .files()
        .filter(|path| is_clippy_config(path) && *path != CLIPPY_CONFIG)
        .map(|path| {
            Violation::new(
                Rule::DirectIo,
                path,
                None,
                format!("only the root `{CLIPPY_CONFIG}` configures Clippy; this file would replace it, and its direct-io ban, for the crates below it"),
            )
        })
        .collect();
    out.extend(with_sources(tree, Rule::DirectIo, |src, out| {
        if IO_CRATES.contains(&src.krate.as_str()) {
            return;
        }
        for (index, level, lint) in lint_switches(&src.lines) {
            out.push(Violation::new(
                Rule::DirectIo,
                src.path,
                Some(index + 1),
                format!("`{level}` of `{lint}` switches off the Clippy direct-io ban; only `alloy-rt` and `alloy-sim` may call the operating system"),
            ));
        }
        for (index, line) in src.lines.iter().enumerate() {
            for value in IO_VALUES {
                if find_token(&line.code, value).is_some() {
                    out.push(Violation::new(
                        Rule::DirectIo,
                        src.path,
                        Some(index + 1),
                        format!("`{value}` reaches randomness; only `alloy-rt` and `alloy-sim` call the operating system, everything else goes through the `alloy-rt` traits"),
                    ));
                }
            }
        }
    }));
    out
}

/// True for a file that Clippy reads as its configuration.
fn is_clippy_config(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name == "clippy.toml" || name == ".clippy.toml"
}

/// The byte index of the bracket that closes the `open_with` at byte `open` of
/// `text`, or `None` when it is not closed.
fn closing(text: &str, open: usize, open_with: char, close_with: char) -> Option<usize> {
    let mut depth = 0usize;
    for (at, c) in text.get(open..)?.char_indices() {
        if c == open_with {
            depth += 1;
        } else if c == close_with {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(open + at);
            }
        }
    }
    None
}

/// Every lint in [`BAN_LINTS`] that an `allow(...)` or `expect(...)` inside an
/// attribute names, as `(line index, level, lint)`. An attribute may span
/// lines, and may wrap the level in `cfg_attr`.
fn lint_switches(lines: &[Line]) -> Vec<(usize, &'static str, &'static str)> {
    let mut code = String::new();
    let mut starts = Vec::new();
    for line in lines {
        starts.push(code.len());
        code.push_str(&line.code);
        code.push('\n');
    }
    let line_of = |at: usize| {
        starts
            .partition_point(|&start| start <= at)
            .saturating_sub(1)
    };
    let mut found = Vec::new();
    for (hash, _) in code.match_indices('#') {
        let rest = &code[hash + 1..];
        let open = if rest.starts_with("![") {
            hash + 2
        } else if rest.starts_with('[') {
            hash + 1
        } else {
            continue;
        };
        let close = closing(&code, open, '[', ']').unwrap_or(code.len());
        let attr = &code[open..close];
        for level in ["allow", "expect"] {
            let call = format!("{level}(");
            let mut from = 0;
            while let Some(offset) = find_token(&attr[from..], &call) {
                let paren = from + offset + level.len();
                let end = closing(attr, paren, '(', ')').unwrap_or(attr.len());
                let group = &attr[paren..end];
                for lint in BAN_LINTS {
                    if let Some(at) = find_token(group, lint) {
                        found.push((line_of(open + paren + at), level, lint));
                    }
                }
                from = paren + 1;
            }
        }
    }
    found
}

/// `unsafe`: the keyword outside the crates that may use it.
pub(super) fn check_unsafe(tree: &Tree) -> Vec<Violation> {
    with_sources(tree, Rule::Unsafe, |src, out| {
        if UNSAFE_CRATES.contains(&src.krate.as_str()) {
            return;
        }
        for (index, line) in src.lines.iter().enumerate() {
            if find_token(&line.code, "unsafe").is_some() {
                out.push(Violation::new(
                    Rule::Unsafe,
                    src.path,
                    Some(index + 1),
                    format!(
                        "`unsafe` in `{}`; only {} may use it",
                        src.krate,
                        UNSAFE_CRATES.join(", ")
                    ),
                ));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(src: &str) -> Vec<String> {
        lex(src).into_iter().map(|l| l.code).collect()
    }

    #[test]
    fn lexer_drops_comments_and_literal_contents() {
        let src = "let a = \"x.unwrap()\"; // b.unwrap()\n/* c.unwrap()\n */ d . unwrap ( );";
        assert_eq!(codes(src), ["let a=\"\";", "", "d.unwrap();"]);
        let lines = lex(src);
        assert_eq!(lines[0].comment, "b.unwrap()");
        assert_eq!(lines[1].comment, "c.unwrap()");
    }

    #[test]
    fn lexer_handles_raw_strings_chars_and_lifetimes() {
        let src =
            "let r = r#\"a \" todo!()\"#; let c = '\"'; let e = '\\''; fn f<'a>(x: &'a str) {}";
        assert_eq!(
            codes(src),
            ["let r=\"\";let c='';let e='';fn f<'a>(x:&'a str){}"]
        );
        assert_eq!(codes("br\"x\" b'y'"), ["b\"\"b''"]);
    }

    #[test]
    fn lexer_keeps_lines_across_multi_line_strings() {
        let src = "let s = \"a\nb\\\nc\";\nx";
        assert_eq!(codes(src), ["let s=\"", "", "\";", "x"]);
    }

    #[test]
    fn test_regions_follow_test_attributes() {
        let src =
            "fn a() {}\n#[cfg(test)]\nmod t {\n    fn b() { {} }\n}\nfn c() {}\n#[test] fn d() {}";
        assert_eq!(
            test_lines(&lex(src)),
            [false, true, true, true, true, false, true]
        );
        let declared = "#[cfg(test)]\nmod tests;\nfn e() {}";
        assert_eq!(test_lines(&lex(declared)), [true, true, false]);
    }

    #[test]
    fn a_test_attribute_on_a_field_or_arm_ends_with_it() {
        let src = "pub struct S {
    #[cfg(test)]
    pub a: u8,
}
pub fn after_field(x: Option<u8>) -> u8 {
    x.unwrap()
}
pub struct T {
    #[cfg(test)]
    a: u8
}
pub fn after_last_field(x: Option<u8>) -> u8 {
    x.unwrap()
}
pub fn in_match(v: u8, x: Option<u8>) -> u8 {
    match v {
        #[cfg(test)]
        0 => 0,
        _ => {
            x.unwrap()
        }
    }
}";
        let tests = test_lines(&lex(src));
        for line in [6, 13, 20] {
            assert!(!tests[line - 1], "line {line} is not test code");
        }
        assert!(tests[2] && tests[9] && tests[17], "attributed lines are");
    }

    #[test]
    fn a_test_attribute_reaches_the_body_of_its_own_item() {
        let src = "#[test]
fn f<A, B>(a: [u8; 2], b: (u8, u8)) where A: Copy, B: Copy {
    x.unwrap();
}
fn g() {
    match v {
        #[cfg(test)]
        0 => {
            x.unwrap()
        }
        _ => 0,
    }
    #[cfg(test)]
    let y = vec![1, 2];
    z.unwrap();
}";
        assert_eq!(
            test_lines(&lex(src)),
            [
                true, true, true, true, false, false, true, true, true, true, false, false, true,
                true, false, false
            ]
        );
    }

    fn switches(src: &str) -> Vec<(usize, &'static str, &'static str)> {
        lint_switches(&lex(src))
    }

    #[test]
    fn lint_switches_are_found_across_lines_and_in_cfg_attr() {
        let src = "#![allow(
    clippy::disallowed_methods,
    reason = \"no\"
)]
#![cfg_attr(test, expect(clippy::disallowed_types))]
#[allow(dead_code, clippy::style)]
#[expect(clippy::all)]
#[allow(clippy::disallowed_method)]
fn f() {}";
        assert_eq!(
            switches(src),
            [
                (1, "allow", "clippy::disallowed_methods"),
                (4, "expect", "clippy::disallowed_types"),
                (5, "allow", "clippy::style"),
                (6, "expect", "clippy::all"),
                (7, "allow", "clippy::disallowed_method"),
            ]
        );
    }

    #[test]
    fn other_lint_levels_and_lints_are_not_switches() {
        let src = "#![deny(clippy::all)]
#![warn(clippy::disallowed_methods)]
#![cfg_attr(test, deny(clippy::style), allow(dead_code))]
#[allow(clippy::all_kinds, clippy::needless_return)]
// #[allow(clippy::disallowed_methods)]
fn f() { let s = \"#[allow(clippy::all)]\"; disallow(clippy::all); }";
        assert_eq!(switches(src), []);
    }

    #[test]
    fn only_test_attributes_open_test_regions() {
        for code in [
            "#[test]",
            "#[cfg(test)]",
            "#[tokio::test]",
            "#[tokio::test(flavor=\"\")]",
            "#[::tokio::test]",
            "#[allow(dead_code)]#[test]fn f(){}",
        ] {
            assert!(has_test_attribute(code), "{code} is a test attribute");
        }
        for code in [
            "if checks::test(1){",
            "let t=x::test;",
            "#[testing]",
            "#[tokio::tests]",
            "#[cfg(not(test))]",
            "#[a::::test]",
            "#[allow(x)]checks::test(1);",
        ] {
            assert!(!has_test_attribute(code), "{code} is not a test attribute");
        }
    }

    #[test]
    fn a_call_named_test_does_not_exempt_the_block_after_it() {
        let src = "fn f(x: Option<u8>) -> u8 {
    if checks::test(1) {
        return x.unwrap();
    }
    0
}";
        assert_eq!(test_lines(&lex(src)), [false; 6]);
    }

    #[test]
    fn squeeze_keeps_spaces_between_identifiers_only() {
        assert_eq!(squeeze("use  std :: fs ;"), "use std::fs;");
        assert_eq!(squeeze("pub unsafe fn"), "pub unsafe fn");
    }
}
