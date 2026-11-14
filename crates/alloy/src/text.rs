//! Text helpers: wrapping prose (principle T3) and finding the nearest valid
//! word for a typo (principle T29).

/// `text` wrapped at `width` columns, breaking only at spaces outside
/// backticks, so that a quoted command, path, or name is never split. A word
/// longer than `width` stands alone on its line. Columns are counted as
/// characters.
pub(crate) fn wrap(text: &str, width: usize) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in text.chars() {
        if c == '`' {
            quoted = !quoted;
        }
        if c == ' ' && !quoted {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let mut out = String::new();
    let mut line_len = 0;
    for word in words {
        let len = word.chars().count();
        if line_len > 0 && line_len + 1 + len > width {
            out.push('\n');
            line_len = 0;
        } else if line_len > 0 {
            out.push(' ');
            line_len += 1;
        }
        out.push_str(&word);
        line_len += len;
    }
    out
}

/// `text` with every control character escaped as Rust escapes it (`\n`,
/// `\u{1b}`), so that text the user or a server gave cannot move the cursor
/// or change the terminal when it is echoed (T-0091 review F3).
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// The candidate nearest to `word`, when it is at most two edits away and
/// fewer edits than `word` has characters; the first such candidate on a tie.
pub(crate) fn nearest<'c>(word: &str, candidates: &[&'c str]) -> Option<&'c str> {
    let limit = word.chars().count().min(3);
    candidates
        .iter()
        .map(|&candidate| (distance(word, candidate), candidate))
        .filter(|&(edits, _)| edits < limit)
        .min_by_key(|&(edits, _)| edits)
        .map(|(_, candidate)| candidate)
}

/// The Levenshtein distance between `a` and `b`, in characters.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let substitute = diagonal + usize::from(ca != cb);
            diagonal = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(diagonal + 1);
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_counts_edits() {
        assert_eq!(distance("", ""), 0);
        assert_eq!(distance("version", "version"), 0);
        assert_eq!(distance("versio", "version"), 1);
        assert_eq!(distance("verison", "version"), 2);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("", "abc"), 3);
    }

    #[test]
    fn nearest_needs_a_close_candidate() {
        let words = ["init", "join", "version"];
        assert_eq!(nearest("verison", &words), Some("version"));
        assert_eq!(nearest("jion", &words), Some("join"));
        assert_eq!(nearest("init", &words), Some("init"));
        assert_eq!(nearest("x", &words), None);
        assert_eq!(nearest("ab", &["a", "b"]), Some("a"));
        assert_eq!(nearest("frobnicate", &words), None);
        assert_eq!(nearest("version", &[]), None);
    }

    #[test]
    fn escape_escapes_control_characters_only() {
        assert_eq!(escape("plain é `x`"), "plain é `x`");
        assert_eq!(escape("\u{1b}[2J\n\t\u{7f}"), "\\u{1b}[2J\\n\\t\\u{7f}");
        assert_eq!(escape("\u{85}"), "\\u{85}");
    }

    #[test]
    fn wrap_breaks_at_spaces_outside_backticks() {
        assert_eq!(wrap("a b c", 3), "a b\nc");
        assert_eq!(wrap("one  two", 80), "one two");
        assert_eq!(wrap("x `alloy node list` y", 10), "x\n`alloy node list`\ny");
        assert_eq!(wrap("", 80), "");
        let long = "word ".repeat(40);
        assert!(
            wrap(&long, 80)
                .lines()
                .all(|line| line.chars().count() <= 80)
        );
    }
}
