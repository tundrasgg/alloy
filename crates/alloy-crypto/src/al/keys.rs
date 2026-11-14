//! The long-lived and handshake keys of AL: static key pairs, ephemeral
//! private keys and pre-shared keys.

use std::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

/// The length of an X25519 key, private or public, and of a pre-shared key.
pub const KEY_LEN: usize = 32;

/// An X25519 public key: a node's static mesh key or a handshake ephemeral.
///
/// These are the 32 bytes of RFC 7748, as they travel on the wire. Any 32
/// bytes are accepted. A public key of small order is rejected when it is used
/// in a Diffie-Hellman operation, by [`HandshakeError::NonContributory`].
///
/// [`HandshakeError::NonContributory`]: crate::al::HandshakeError::NonContributory
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicKey([u8; KEY_LEN]);

impl PublicKey {
    /// Returns the public key with these bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Returns the 32 bytes of the key.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    pub(crate) fn to_dalek(self) -> x25519_dalek::PublicKey {
        x25519_dalek::PublicKey::from(self.0)
    }
}

/// Shows the key in hexadecimal. A public key is not secret.
impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey(")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}

/// A node's static X25519 private key, the private half of its mesh key.
///
/// It is erased when it is dropped, and its `Debug` output shows no key
/// bytes. It cannot be copied or cloned, so every copy of the key is one the
/// caller made on purpose.
pub struct StaticSecret(x25519_dalek::StaticSecret);

impl StaticSecret {
    /// Returns the private key with these bytes.
    ///
    /// The bytes are used as RFC 7748 specifies: X25519 clamps them, so any
    /// 32 bytes make a key. The caller draws them from the `alloy-rt`
    /// randomness trait, or loads a stored key, and erases its own copy.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(x25519_dalek::StaticSecret::from(bytes))
    }

    /// Returns the public key of this private key.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(x25519_dalek::PublicKey::from(&self.0).to_bytes())
    }

    /// Returns a copy of the 32 private key bytes as they were given to
    /// [`StaticSecret::from_bytes`], for storage (ARFC 0002 section 3.14).
    /// The copy erases itself when it is dropped.
    #[must_use]
    pub fn to_bytes(&self) -> zeroize::Zeroizing<[u8; KEY_LEN]> {
        zeroize::Zeroizing::new(self.0.to_bytes())
    }

    pub(crate) fn dalek(&self) -> &x25519_dalek::StaticSecret {
        &self.0
    }
}

/// The wrapped key erases itself on drop (the `zeroize` feature of
/// `x25519-dalek`).
impl ZeroizeOnDrop for StaticSecret {}

/// Shows only the type name.
impl fmt::Debug for StaticSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticSecret").finish_non_exhaustive()
    }
}

/// An ephemeral X25519 private key for one handshake.
///
/// The caller draws its 32 bytes from the `alloy-rt` randomness trait for
/// every handshake and hands them over with [`EphemeralSecret::from_bytes`];
/// this crate never reads a generator. A handshake consumes the key and erases
/// it when the handshake ends, whether it succeeds, fails or is dropped. Its
/// `Debug` output shows no key bytes.
pub struct EphemeralSecret(x25519_dalek::StaticSecret);

impl EphemeralSecret {
    /// Returns the ephemeral private key with these bytes.
    ///
    /// Any 32 bytes make a key, as for [`StaticSecret::from_bytes`]. They must
    /// be fresh random bytes, never reused: a repeated ephemeral key repeats
    /// the handshake's key material.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(x25519_dalek::StaticSecret::from(bytes))
    }

    /// Returns the public key of this private key.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(x25519_dalek::PublicKey::from(&self.0).to_bytes())
    }

    pub(crate) fn into_dalek(self) -> x25519_dalek::StaticSecret {
        self.0
    }
}

/// The wrapped key erases itself on drop (the `zeroize` feature of
/// `x25519-dalek`).
impl ZeroizeOnDrop for EphemeralSecret {}

/// Shows only the type name.
impl fmt::Debug for EphemeralSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EphemeralSecret").finish_non_exhaustive()
    }
}

/// The 32-byte pre-shared key of a pair of nodes, mixed at position `psk2`
/// (ARFC 0003 section 3.1).
///
/// When no pre-shared key is configured for a pair, the key is
/// [`PresharedKey::none`], 32 zero bytes. It is erased when it is dropped and
/// its `Debug` output shows no key bytes.
pub struct PresharedKey([u8; KEY_LEN]);

impl PresharedKey {
    /// Returns the pre-shared key with these bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Returns the key used when none is configured: 32 zero bytes.
    #[must_use]
    pub const fn none() -> Self {
        Self([0u8; KEY_LEN])
    }

    pub(crate) fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl Drop for PresharedKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl ZeroizeOnDrop for PresharedKey {}

/// Shows only the type name.
impl fmt::Debug for PresharedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresharedKey").finish_non_exhaustive()
    }
}
