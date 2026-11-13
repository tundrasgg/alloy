//! Stand-in for getrandom 0.3.4: the public functions at the paths the real
//! crate has them (`src/lib.rs` of getrandom 0.3.4). Every one fails; nothing
//! here reaches the operating system.

use core::fmt;
use core::mem::MaybeUninit;

/// The one error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("stand-in: no randomness source")
    }
}

impl core::error::Error for Error {}

/// Fills `dest` from the operating system, in the real crate.
pub fn fill(_dest: &mut [u8]) -> Result<(), Error> {
    Err(Error)
}

/// Fills `dest` from the operating system, in the real crate.
pub fn fill_uninit(_dest: &mut [MaybeUninit<u8>]) -> Result<&mut [u8], Error> {
    Err(Error)
}

/// A random `u32` from the operating system, in the real crate.
pub fn u32() -> Result<u32, Error> {
    Err(Error)
}

/// A random `u64` from the operating system, in the real crate.
pub fn u64() -> Result<u64, Error> {
    Err(Error)
}
