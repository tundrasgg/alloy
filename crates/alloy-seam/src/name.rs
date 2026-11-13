//! Registry names: [`Kind`] and [`ImplName`] (ARFC 0001 clause 3.10).

use std::fmt;

/// The longest registry name, in bytes (ARFC 0001 section 7).
const MAX_NAME_LEN: usize = 32;

/// Checks a registry name against ARFC 0001 clause 3.10: 1 to 32 bytes, each
/// from `[a-z0-9-]`. Length is checked before content, so an over-long name
/// reports `TooLong` even when it also holds an invalid byte.
fn validate(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > MAX_NAME_LEN {
        return Err(NameError::TooLong { len: name.len() });
    }
    match name
        .bytes()
        .position(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
    {
        Some(index) => Err(NameError::InvalidByte { index }),
        None => Ok(()),
    }
}

/// A registry kind, such as `disk` or `hypervisor`: the name under which one
/// seam's implementations are registered.
///
/// Holds 1 to 32 bytes, each from `[a-z0-9-]` (ARFC 0001 clause 3.10 and
/// section 7). A value of this type always satisfies these rules.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kind(String);

impl Kind {
    /// Accepts exactly the strings of 1 to 32 bytes drawn from `[a-z0-9-]`.
    ///
    /// # Errors
    ///
    /// [`NameError::Empty`] for an empty string, [`NameError::TooLong`] for more
    /// than 32 bytes, and otherwise [`NameError::InvalidByte`] with the index of
    /// the first byte outside `[a-z0-9-]`.
    pub fn new(name: &str) -> Result<Kind, NameError> {
        validate(name).map(|()| Kind(name.to_owned()))
    }

    /// The name as given to [`Kind::new`].
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Kind {
    /// Writes [`Kind::as_str`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An implementation name, such as `mem` or `os`: one implementation of a seam
/// within its [`Kind`].
///
/// Follows the same rules as [`Kind`]: 1 to 32 bytes, each from `[a-z0-9-]`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImplName(String);

impl ImplName {
    /// Accepts exactly the strings of 1 to 32 bytes drawn from `[a-z0-9-]`.
    ///
    /// # Errors
    ///
    /// As [`Kind::new`].
    pub fn new(name: &str) -> Result<ImplName, NameError> {
        validate(name).map(|()| ImplName(name.to_owned()))
    }

    /// The name as given to [`ImplName::new`].
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImplName {
    /// Writes [`ImplName::as_str`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a string is not a valid [`Kind`] or [`ImplName`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// The string has no bytes.
    #[error("name is empty")]
    Empty,
    /// The string is longer than 32 bytes.
    #[error("name is {len} bytes, the limit is 32")]
    TooLong {
        /// The string's length in bytes.
        len: usize,
    },
    /// A byte is outside `[a-z0-9-]`.
    #[error("byte {index} is not in [a-z0-9-]")]
    InvalidByte {
        /// The index of the first such byte.
        index: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exactly_the_allowed_names() {
        for ok in ["a", "disk", "disk-io", "0", "-", "a-b-0-9", &"z".repeat(32)] {
            assert_eq!(
                Kind::new(ok).map(|k| k.as_str().to_owned()),
                Ok(ok.to_owned())
            );
            assert_eq!(
                ImplName::new(ok).map(|n| n.as_str().to_owned()),
                Ok(ok.to_owned())
            );
        }
    }

    #[test]
    fn rejects_empty_long_and_invalid_names() {
        assert_eq!(Kind::new(""), Err(NameError::Empty));
        assert_eq!(
            Kind::new(&"a".repeat(33)),
            Err(NameError::TooLong { len: 33 })
        );
        assert_eq!(
            ImplName::new("Disk"),
            Err(NameError::InvalidByte { index: 0 })
        );
        assert_eq!(
            ImplName::new("disk_io"),
            Err(NameError::InvalidByte { index: 4 })
        );
        assert_eq!(Kind::new("ab c"), Err(NameError::InvalidByte { index: 2 }));
        // A multi-byte character reports the index of its first byte.
        assert_eq!(Kind::new("aé"), Err(NameError::InvalidByte { index: 1 }));
    }

    #[test]
    fn displays_the_name() {
        let kind = Kind::new("disk-io").unwrap();
        assert_eq!(kind.to_string(), "disk-io");
        assert_eq!(ImplName::new("mem").unwrap().to_string(), "mem");
    }

    #[test]
    fn validation_is_total_over_every_short_byte_string() {
        for a in 0..=255u8 {
            let s = String::from_utf8_lossy(&[a]).into_owned();
            let ok = a.is_ascii_lowercase() || a.is_ascii_digit() || a == b'-';
            assert_eq!(Kind::new(&s).is_ok(), ok, "byte {a}");
        }
    }
}
