//! Key generation from the `alloy-rt` randomness trait (ARFC 0002 sections 2
//! and 3.1, `agent/eng/crypto.md` rule 4).

use alloy_crypto::Zeroizing;
use alloy_crypto::aead::{self, AeadKey};
use alloy_crypto::al::StaticSecret;
use alloy_crypto::{ecdsa, ed25519};
use alloy_rt::Rng;

use crate::IdError;

/// The most draws [`generate_p256`] makes before it gives up. A draw is out
/// of range with probability below 2^-32, so 8 draws all fail with
/// probability below 2^-256.
const P256_DRAWS: usize = 8;

/// Draws a new Ed25519 key: the cluster key or a node identity key.
///
/// # Errors
///
/// [`IdError::Rng`] when the generator fails.
pub fn generate_ed25519(rng: &dyn Rng) -> Result<ed25519::SigningKey, IdError> {
    let mut seed = Zeroizing::new([0u8; ed25519::SEED_LEN]);
    rng.fill(&mut seed[..])?;
    Ok(ed25519::SigningKey::from_seed(&seed))
}

/// Draws a new X25519 key: a node mesh key.
///
/// # Errors
///
/// [`IdError::Rng`] when the generator fails.
pub fn generate_x25519(rng: &dyn Rng) -> Result<StaticSecret, IdError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    rng.fill(&mut bytes[..])?;
    Ok(StaticSecret::from_bytes(*bytes))
}

/// Draws a new P-256 key: the API CA key or an API server key. A draw that
/// is not a valid scalar is discarded and drawn again.
///
/// # Errors
///
/// [`IdError::Rng`] when the generator fails, and [`IdError::Generator`]
/// when 8 draws in a row are out of range, which a working generator does
/// with probability below 2^-256.
pub fn generate_p256(rng: &dyn Rng) -> Result<ecdsa::SigningKey, IdError> {
    let mut scalar = Zeroizing::new([0u8; ecdsa::SCALAR_LEN]);
    for _ in 0..P256_DRAWS {
        rng.fill(&mut scalar[..])?;
        if let Ok(key) = ecdsa::SigningKey::from_scalar(&scalar) {
            return Ok(key);
        }
    }
    Err(IdError::Generator)
}

/// A node's at-rest key: 32 bytes from the randomness trait (section 3.14,
/// ARFC 0020 clause 3.7). The bytes are kept so that they can be stored, and
/// erase themselves when the key is dropped.
pub struct AtRestKey {
    bytes: Zeroizing<[u8; aead::KEY_LEN]>,
    aead: AeadKey,
}

impl AtRestKey {
    /// Draws a new key.
    ///
    /// # Errors
    ///
    /// [`IdError::Rng`] when the generator fails.
    pub fn generate(rng: &dyn Rng) -> Result<Self, IdError> {
        let mut bytes = Zeroizing::new([0u8; aead::KEY_LEN]);
        rng.fill(&mut bytes[..])?;
        Ok(Self::from_bytes(&bytes))
    }

    /// The key with these bytes, as `at-rest.key` holds them.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; aead::KEY_LEN]) -> Self {
        Self {
            bytes: Zeroizing::new(*bytes),
            aead: AeadKey::from_bytes(bytes),
        }
    }

    /// The 32 bytes of the key.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; aead::KEY_LEN] {
        &self.bytes
    }

    pub(crate) fn aead(&self) -> &AeadKey {
        &self.aead
    }
}

/// Shows only the type name.
impl std::fmt::Debug for AtRestKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtRestKey").finish_non_exhaustive()
    }
}
