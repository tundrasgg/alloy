//! Bounded failure text: [`Detail`].

use std::fmt;

/// Human-readable failure text for operators, at most 1,024 bytes (ARFC 0001
/// section 7).
///
/// Never holds secrets, key material, tokens, or tenant payloads (I-22): the
/// code that builds a `Detail` is responsible for what it puts in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail(String);

impl Detail {
    /// The most bytes a `Detail` holds.
    const MAX_LEN: usize = 1_024;

    /// Copies `text`, truncated at the last UTF-8 character boundary at or below
    /// 1,024 bytes when it is longer. Never fails.
    pub fn new(text: &str) -> Detail {
        let mut end = text.len().min(Self::MAX_LEN);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Detail(text[..end].to_owned())
    }

    /// The text, after any truncation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Detail {
    /// Writes [`Detail::as_str`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_text() {
        assert_eq!(Detail::new("").as_str(), "");
        assert_eq!(Detail::new("disk full").as_str(), "disk full");
        assert_eq!(Detail::new("disk full").to_string(), "disk full");
        let exact = "x".repeat(1_024);
        assert_eq!(Detail::new(&exact).as_str(), exact);
    }

    #[test]
    fn truncates_long_text_at_a_character_boundary() {
        assert_eq!(Detail::new(&"y".repeat(5_000)).as_str().len(), 1_024);
        // 1,023 ASCII bytes then a 2-byte character that would end at 1,025.
        let text = format!("{}é", "a".repeat(1_023));
        assert_eq!(Detail::new(&text).as_str(), "a".repeat(1_023));
        // 4-byte characters: 256 fit exactly, the 257th is dropped whole.
        let text = "😀".repeat(300);
        assert_eq!(Detail::new(&text).as_str(), "😀".repeat(256));
        let text = format!("ab{}", "😀".repeat(300));
        assert_eq!(
            Detail::new(&text).as_str(),
            format!("ab{}", "😀".repeat(255))
        );
    }
}
