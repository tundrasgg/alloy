//! A crate on the list may use `unsafe`.

pub fn first(bytes: &[u8]) -> u8 {
    assert!(!bytes.is_empty());
    // SAFETY: the assertion above guarantees index 0 is in bounds.
    unsafe { *bytes.get_unchecked(0) }
}
