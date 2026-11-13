//! A crate that is not on the list.

pub unsafe fn raw(bytes: &[u8]) -> u8 {
    *bytes.get_unchecked(0)
}

pub fn first(bytes: &[u8]) -> u8 {
    unsafe { raw(bytes) }
}
