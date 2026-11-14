//! ECDSA over P-256 with SHA-256: the API CA key, API server certificate
//! keys, and the ACME account key, and nothing else (`agent/eng/crypto.md`,
//! ADR 0053).
//!
//! Every signature takes its per-signature nonce from RFC 6979 section 3.2
//! with SHA-256 and no additional data, through the plain `Signer` of `p256`
//! and `ecdsa`. Signing therefore reads no random source, and the same key
//! signing the same message gives the same signature (ARFC 0002 section 2).
//! No randomized or hedged signer is reachable from here.

use std::fmt;

use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::pkcs8::{DecodePrivateKey as _, EncodePrivateKey as _, EncodePublicKey as _};
use sha2::Digest as _;
use zeroize::Zeroizing;

/// The length of a private scalar.
pub const SCALAR_LEN: usize = 32;

/// The length of an uncompressed public point: `0x04`, then `x` and `y`.
pub const POINT_LEN: usize = 65;

/// The length of the DER SubjectPublicKeyInfo of a P-256 key with the point
/// uncompressed (ARFC 0002 section 2).
pub const SPKI_LEN: usize = 91;

/// The length of a signature as `R` then `S` (RFC 7518 section 3.4).
pub const SIGNATURE_LEN: usize = 64;

/// The length of a key identifier (RFC 7093 section 2, method 1).
pub const KEY_ID_LEN: usize = 20;

/// Why a P-256 operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EcdsaError {
    /// The 32 bytes are zero or not below the group order, so they are not a
    /// private scalar. The caller draws new bytes.
    #[error("the bytes are not a P-256 private scalar")]
    InvalidScalar,
    /// The bytes are not a PKCS#8 P-256 private key.
    #[error("the bytes are not a PKCS#8 P-256 private key")]
    InvalidPkcs8,
    /// The bytes are not a SEC1 encoding of a P-256 point.
    #[error("the bytes are not a P-256 public key")]
    InvalidPublicKey,
    /// Encoding a key failed.
    #[error("encoding the P-256 key failed")]
    Encode,
    /// The signature does not verify under the key for the message.
    #[error("the ECDSA signature does not verify")]
    BadSignature,
}

/// A P-256 private key.
///
/// It is held on the heap and erased when it is dropped (`p256` erases its
/// scalar on drop). It is not `Clone`, and its `Debug` output shows no key
/// bytes.
pub struct SigningKey(Box<p256::ecdsa::SigningKey>);

impl SigningKey {
    /// Returns the key whose private scalar is these 32 bytes, big endian.
    ///
    /// The caller draws them from the `alloy-rt` randomness trait. Fewer than
    /// one draw in 2^32 is out of range; the caller then draws again.
    ///
    /// # Errors
    ///
    /// [`EcdsaError::InvalidScalar`] for zero or a value not below the group
    /// order.
    pub fn from_scalar(bytes: &[u8; SCALAR_LEN]) -> Result<Self, EcdsaError> {
        p256::ecdsa::SigningKey::from_slice(bytes)
            .map(|key| Self(Box::new(key)))
            .map_err(|_| EcdsaError::InvalidScalar)
    }

    /// Returns the key encoded as PKCS#8 DER (RFC 5208), as `NodeSecrets`
    /// stores it (ARFC 0002 section 3.14). The buffer erases itself when it
    /// is dropped.
    ///
    /// # Errors
    ///
    /// [`EcdsaError::Encode`] if the encoder fails, which it does not for a
    /// valid key.
    pub fn to_pkcs8_der(&self) -> Result<Zeroizing<Vec<u8>>, EcdsaError> {
        let document = self.0.to_pkcs8_der().map_err(|_| EcdsaError::Encode)?;
        Ok(Zeroizing::new(document.as_bytes().to_vec()))
    }

    /// Returns the key that this PKCS#8 DER encodes.
    ///
    /// # Errors
    ///
    /// [`EcdsaError::InvalidPkcs8`] for anything but a PKCS#8 P-256 private
    /// key with a valid scalar.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self, EcdsaError> {
        p256::ecdsa::SigningKey::from_pkcs8_der(der)
            .map(|key| Self(Box::new(key)))
            .map_err(|_| EcdsaError::InvalidPkcs8)
    }

    /// Returns the public key.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(*self.0.verifying_key())
    }

    /// Signs `message` with SHA-256 and an RFC 6979 nonce, and returns `R`
    /// then `S`, each 32 bytes big endian: the `ES256` form of RFC 7518
    /// section 3.4.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        let signature: p256::ecdsa::Signature = self.0.sign(message);
        signature.to_bytes().into()
    }

    /// Signs as [`SigningKey::sign`] and returns the signature as the DER
    /// `Ecdsa-Sig-Value` of RFC 5480 section 2.2.3, as X.509 and PKCS#10
    /// carry it.
    #[must_use]
    pub fn sign_der(&self, message: &[u8]) -> Vec<u8> {
        let signature: p256::ecdsa::Signature = self.0.sign(message);
        signature.to_der().as_bytes().to_vec()
    }
}

/// Shows only the type name.
impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey").finish_non_exhaustive()
    }
}

/// A P-256 public key, known to be a point on the curve other than the
/// identity.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(p256::ecdsa::VerifyingKey);

impl PublicKey {
    /// Returns the public key that these SEC1 bytes encode, compressed or
    /// uncompressed.
    ///
    /// # Errors
    ///
    /// [`EcdsaError::InvalidPublicKey`] for anything else.
    pub fn from_sec1(bytes: &[u8]) -> Result<Self, EcdsaError> {
        p256::ecdsa::VerifyingKey::from_sec1_bytes(bytes)
            .map(Self)
            .map_err(|_| EcdsaError::InvalidPublicKey)
    }

    /// Returns the uncompressed point: `0x04`, `x`, then `y` (RFC 5480
    /// section 2.2).
    #[must_use]
    pub fn to_uncompressed(&self) -> [u8; POINT_LEN] {
        let point = self.0.to_sec1_point(false);
        let mut out = [0u8; POINT_LEN];
        // An uncompressed SEC1 point of P-256 is 65 bytes.
        out.copy_from_slice(point.as_bytes());
        out
    }

    /// Returns the DER SubjectPublicKeyInfo: algorithm `id-ecPublicKey`,
    /// named curve `secp256r1`, the point uncompressed (RFC 5480 sections
    /// 2.1.1 and 2.2). It is [`SPKI_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// [`EcdsaError::Encode`] if the encoder fails, which it does not for a
    /// valid key.
    pub fn spki_der(&self) -> Result<Vec<u8>, EcdsaError> {
        self.0
            .to_public_key_der()
            .map(|document| document.as_bytes().to_vec())
            .map_err(|_| EcdsaError::Encode)
    }

    /// Returns the key identifier of RFC 7093 section 2, method 1: the
    /// leftmost 160 bits of the SHA-256 hash of the subjectPublicKey BIT
    /// STRING value, which is the uncompressed point.
    #[must_use]
    pub fn key_identifier(&self) -> [u8; KEY_ID_LEN] {
        let digest = sha2::Sha256::digest(self.to_uncompressed());
        let mut out = [0u8; KEY_ID_LEN];
        out.copy_from_slice(&digest[..KEY_ID_LEN]);
        out
    }

    /// Checks an `R` then `S` signature over `message` with SHA-256.
    ///
    /// # Errors
    ///
    /// [`EcdsaError::BadSignature`] when it does not verify.
    pub fn verify(
        &self,
        message: &[u8],
        signature: &[u8; SIGNATURE_LEN],
    ) -> Result<(), EcdsaError> {
        let signature =
            p256::ecdsa::Signature::from_slice(signature).map_err(|_| EcdsaError::BadSignature)?;
        self.0
            .verify(message, &signature)
            .map_err(|_| EcdsaError::BadSignature)
    }

    /// Checks a DER `Ecdsa-Sig-Value` signature over `message` with
    /// SHA-256.
    ///
    /// # Errors
    ///
    /// [`EcdsaError::BadSignature`] when it does not parse or verify.
    pub fn verify_der(&self, message: &[u8], signature: &[u8]) -> Result<(), EcdsaError> {
        let signature =
            p256::ecdsa::Signature::from_der(signature).map_err(|_| EcdsaError::BadSignature)?;
        self.0
            .verify(message, &signature)
            .map_err(|_| EcdsaError::BadSignature)
    }
}

/// Shows the uncompressed point in hexadecimal. A public key is not secret.
impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey(")?;
        for byte in self.to_uncompressed() {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}
