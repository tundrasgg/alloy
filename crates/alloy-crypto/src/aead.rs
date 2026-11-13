//! XChaCha20-Poly1305 with a caller-chosen nonce, for data sealed at rest
//! (ARFC 0002 section 3.14, ARFC 0020 clause 3.7).
//!
//! The caller chooses each nonce by the layout its specification gives and
//! draws it from the `alloy-rt` randomness trait where that layout says so
//! (`agent/eng/crypto.md` rule 5). Nothing here reads a generator.

use std::fmt;

use chacha20poly1305::aead::array::Array;
use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305};
use zeroize::Zeroizing;

/// The length of a key.
pub const KEY_LEN: usize = 32;

/// The length of a nonce.
pub const NONCE_LEN: usize = 24;

/// The length of the Poly1305 tag that follows the ciphertext.
pub const TAG_LEN: usize = 16;

/// The ciphertext does not authenticate under the key, nonce, and associated
/// data, or is shorter than a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the sealed data does not authenticate")]
pub struct OpenError;

/// The plaintext is longer than XChaCha20 can encrypt under one nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the plaintext is too long to seal")]
pub struct SealError;

/// An XChaCha20-Poly1305 key.
///
/// It is held on the heap and erased when it is dropped (the `zeroize`
/// feature of `chacha20poly1305`). It is not `Clone`, and its `Debug` output
/// shows no key bytes.
pub struct AeadKey(Box<XChaCha20Poly1305>);

impl AeadKey {
    /// Returns the key with these bytes. The caller erases its own copy.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; KEY_LEN]) -> Self {
        Self(Box::new(XChaCha20Poly1305::new(<&Array<u8, _>>::from(
            bytes,
        ))))
    }

    /// Encrypts `plaintext` with `nonce` and `associated_data`, and returns
    /// the ciphertext followed by the tag: `plaintext.len() + TAG_LEN`
    /// bytes.
    ///
    /// # Errors
    ///
    /// [`SealError`] for a plaintext longer than XChaCha20 can encrypt under
    /// one nonce, 2^38 bytes.
    pub fn seal(
        &self,
        nonce: &[u8; NONCE_LEN],
        associated_data: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, SealError> {
        let mut out = Vec::with_capacity(plaintext.len().saturating_add(TAG_LEN));
        out.extend_from_slice(plaintext);
        let tag = self
            .0
            .encrypt_inout_detached(
                <&Array<u8, _>>::from(nonce),
                associated_data,
                out.as_mut_slice().into(),
            )
            .map_err(|_| SealError)?;
        out.extend_from_slice(&tag);
        Ok(out)
    }

    /// Decrypts `sealed`, a ciphertext followed by its tag, if it
    /// authenticates with `nonce` and `associated_data`. The plaintext
    /// erases itself when it is dropped.
    ///
    /// # Errors
    ///
    /// [`OpenError`] when it does not authenticate.
    pub fn open(
        &self,
        nonce: &[u8; NONCE_LEN],
        associated_data: &[u8],
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        let split = sealed.len().checked_sub(TAG_LEN).ok_or(OpenError)?;
        let (ciphertext, tag) = sealed.split_at(split);
        let mut out = Zeroizing::new(ciphertext.to_vec());
        self.0
            .decrypt_inout_detached(
                <&Array<u8, _>>::from(nonce),
                associated_data,
                out.as_mut_slice().into(),
                <&Array<u8, _>>::try_from(tag).map_err(|_| OpenError)?,
            )
            .map_err(|_| OpenError)?;
        Ok(out)
    }
}

/// Shows only the type name.
impl fmt::Debug for AeadKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AeadKey").finish_non_exhaustive()
    }
}
