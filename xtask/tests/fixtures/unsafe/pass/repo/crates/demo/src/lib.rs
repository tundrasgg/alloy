//! Other crates forbid unsafe code, and may say unsafe in comments.

#![forbid(unsafe_code)]

/// Returns a word.
pub fn word() -> &'static str {
    "unsafe"
}
