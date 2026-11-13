//! Cryptography for Alloy.
//!
//! This is the only crate in the workspace that depends on cryptographic
//! crates (`agent/eng/crypto.md` rule 1). Every other crate reaches a
//! primitive through the interface here, so the choice of implementation, its
//! version and its features are made in one place.
//!
//! It currently offers BLAKE3 content hashing, which `alloy-sim` uses for the
//! trace hash and record digests of ARFC 0005 clause 3.2.5:
//!
//! - [`hash`] hashes one byte slice in a single call.
//! - [`Hasher`] hashes input that arrives in pieces.
//!
//! Both produce the default 32-byte BLAKE3 output in unkeyed hash mode, and
//! both give the same result for the same bytes however those bytes are split.
//! The output depends only on the input: it is the same on every platform and
//! whichever SIMD path the CPU selects at run time.

#![forbid(unsafe_code)]

use std::fmt;

/// Returns the 32-byte BLAKE3 hash of `input`.
///
/// This is BLAKE3 in its unkeyed hash mode with the default output length.
/// It accepts input of any length, including empty input, and never fails or
/// panics. The result equals that of a [`Hasher`] given the same bytes.
///
/// ```
/// let digest = alloy_crypto::hash(b"abc");
/// assert_eq!(digest.len(), 32);
/// ```
#[must_use]
pub fn hash(input: &[u8]) -> [u8; 32] {
    *blake3::hash(input).as_bytes()
}

/// An incremental BLAKE3 hasher.
///
/// Feed bytes with [`Hasher::update`] in as many calls as convenient, then read
/// the hash with [`Hasher::finalize`]. The hash depends only on the
/// concatenation of all updated bytes, not on how they were split, so it
/// equals [`hash`] of the concatenation.
///
/// Memory use is fixed and independent of the input length: the hasher holds
/// one partial 1 KiB chunk and a stack of at most 55 chaining values of 32
/// bytes, enough for the 2^64 byte input limit of BLAKE3. Input beyond that
/// limit is outside the BLAKE3 specification.
///
/// ```
/// let mut hasher = alloy_crypto::Hasher::new();
/// hasher.update(b"a").update(b"bc");
/// assert_eq!(hasher.finalize(), alloy_crypto::hash(b"abc"));
/// ```
#[derive(Clone, Default)]
pub struct Hasher {
    inner: blake3::Hasher,
}

impl Hasher {
    /// Returns a hasher that has seen no input.
    ///
    /// Finalizing it at once gives the hash of the empty input.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: blake3::Hasher::new(),
        }
    }

    /// Appends `input` to the bytes being hashed and returns the hasher, so
    /// calls can be chained.
    ///
    /// Empty input is accepted and changes nothing. This never fails or panics.
    pub fn update(&mut self, input: &[u8]) -> &mut Self {
        self.inner.update(input);
        self
    }

    /// Returns the 32-byte BLAKE3 hash of every byte passed to
    /// [`Hasher::update`] so far.
    ///
    /// It does not change the hasher: further updates continue the same input,
    /// and finalizing again without an update returns the same hash.
    #[must_use]
    pub fn finalize(&self) -> [u8; 32] {
        *self.inner.finalize().as_bytes()
    }
}

/// Shows only the type name. The hasher's state is derived from its input,
/// which may be data that does not belong in debug output.
impl fmt::Debug for Hasher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hasher").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
