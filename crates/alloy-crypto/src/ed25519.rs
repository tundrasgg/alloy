//! Ed25519 signatures (RFC 8032): the cluster key, node identity keys, and
//! every other Alloy signing key that is not one of the three P-256 keys
//! (`agent/eng/crypto.md`, ARFC 0002 section 2).
//!
//! Signing is deterministic: the same key and message give the same
//! signature, and no random source is read. Verification is strict: it
//! rejects a non-canonical `S`, a public key or `R` of small order, and a
//! public key that is not a point, so a signature verifies under one encoding
//! of one key only.

use std::fmt;

use ed25519_dalek::{Signer as _, ed25519};
use zeroize::Zeroizing;

/// The length of a private key seed.
pub const SEED_LEN: usize = 32;

/// The length of a public key.
pub const PUBLIC_KEY_LEN: usize = 32;

/// The length of a signature.
pub const SIGNATURE_LEN: usize = 64;

/// A public key's bytes are not a valid Ed25519 public key, or a signature
/// does not verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Ed25519Error {
    /// The 32 bytes are not the encoding of a curve point.
    #[error("the bytes are not an Ed25519 public key")]
    InvalidPublicKey,
    /// The signature does not verify under the key for the message.
    #[error("the Ed25519 signature does not verify")]
    BadSignature,
}

/// An Ed25519 private key.
///
/// It is held on the heap and erased when it is dropped (the `zeroize`
/// feature of `ed25519-dalek`). It is not `Clone`, and its `Debug` output
/// shows no key bytes.
pub struct SigningKey(Box<ed25519_dalek::SigningKey>);

impl SigningKey {
    /// Returns the key with this 32-byte seed (RFC 8032 section 5.1.5).
    ///
    /// Any 32 bytes make a key. The caller draws them from the `alloy-rt`
    /// randomness trait, or loads a stored seed, and erases its own copy.
    #[must_use]
    pub fn from_seed(seed: &[u8; SEED_LEN]) -> Self {
        Self(Box::new(ed25519_dalek::SigningKey::from_bytes(seed)))
    }

    /// Returns a copy of the seed, for storage. The copy erases itself when
    /// it is dropped.
    #[must_use]
    pub fn seed(&self) -> Zeroizing<[u8; SEED_LEN]> {
        Zeroizing::new(self.0.to_bytes())
    }

    /// Returns the public key.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey(self.0.verifying_key())
    }

    /// Signs `message` (RFC 8032 section 5.1.6). Never fails.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> Signature {
        Signature(self.0.sign(message).to_bytes())
    }
}

/// Shows only the type name.
impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey").finish_non_exhaustive()
    }
}

/// An Ed25519 public key, known to be a valid point encoding.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VerifyingKey(ed25519_dalek::VerifyingKey);

impl VerifyingKey {
    /// Returns the public key with these bytes.
    ///
    /// # Errors
    ///
    /// [`Ed25519Error::InvalidPublicKey`] when the bytes do not decode to a
    /// curve point.
    pub fn from_bytes(bytes: &[u8; PUBLIC_KEY_LEN]) -> Result<Self, Ed25519Error> {
        ed25519_dalek::VerifyingKey::from_bytes(bytes)
            .map(Self)
            .map_err(|_| Ed25519Error::InvalidPublicKey)
    }

    /// Returns the 32 bytes of the key.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        self.0.as_bytes()
    }

    /// Checks `signature` over `message`, strictly (RFC 8032 section 5.1.7,
    /// with the checks the module documentation lists).
    ///
    /// # Errors
    ///
    /// [`Ed25519Error::BadSignature`] when it does not verify.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), Ed25519Error> {
        let signature = ed25519::Signature::from_bytes(&signature.0);
        self.0
            .verify_strict(message, &signature)
            .map_err(|_| Ed25519Error::BadSignature)
    }
}

/// Shows the key in hexadecimal. A public key is not secret.
impl fmt::Debug for VerifyingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VerifyingKey(")?;
        for byte in self.as_bytes() {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}

/// An Ed25519 signature: `R` then `S`, 64 bytes. Any 64 bytes are accepted
/// here; [`VerifyingKey::verify`] judges them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; SIGNATURE_LEN]);

impl Signature {
    /// Returns the signature with these bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SIGNATURE_LEN]) -> Self {
        Self(bytes)
    }

    /// Returns the 64 bytes of the signature.
    #[must_use]
    pub const fn to_bytes(&self) -> [u8; SIGNATURE_LEN] {
        self.0
    }
}

/// Shows the signature in hexadecimal. A signature is not secret.
impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature(")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}
