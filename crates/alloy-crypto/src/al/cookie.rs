//! `mac1`, `mac2`, cookies and the cookie reply of ARFC 0003 section 3.1.

use std::fmt;

use blake2::Blake2sMac;
use blake2::digest::consts::U16;
use blake2::digest::{FixedOutput, KeyInit, Output, Update};
use chacha20poly1305::aead::array::Array;
use chacha20poly1305::{AeadInOut, XChaCha20Poly1305};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::keys::{KEY_LEN, PublicKey};
use super::noise::{HASH_LEN, TAG_LEN, hash_into};

/// The length of `mac1`, `mac2` and a cookie.
pub const MAC_LEN: usize = 16;

/// The length of the cookie reply `nonce` field.
pub const COOKIE_NONCE_LEN: usize = 24;

/// The length of the cookie reply `encrypted cookie` field: the cookie and
/// its tag.
pub const ENCRYPTED_COOKIE_LEN: usize = MAC_LEN + TAG_LEN;

const LABEL_MAC1: &[u8] = b"alloy/al/mac1/v1";
const LABEL_COOKIE: &[u8] = b"alloy/al/cookie/v1";

/// `MAC(key, parts[0] || parts[1] || ...)`: keyed BLAKE2s (RFC 7693) with a
/// 16-byte output and `key` as the key, written into `out`.
///
/// The key is a 16 or 32-byte array, by [`MacKeyLen`]. The MAC state erases
/// itself on drop (the `zeroize` feature of `blake2`).
pub(super) fn mac_into<const N: usize>(key: &[u8; N], parts: &[&[u8]], out: &mut [u8; MAC_LEN])
where
    [u8; N]: MacKeyLen,
{
    let state = <Blake2sMac<U16> as KeyInit>::new_from_slice(key);
    // Invariant: `MacKeyLen` admits 16 and 32-byte keys; keyed BLAKE2s takes 1 to 32.
    let mut state = state.expect("keyed BLAKE2s accepts 16 and 32-byte keys");
    for part in parts {
        Update::update(&mut state, part);
    }
    state.finalize_into(<&mut Output<Blake2sMac<U16>>>::from(out));
}

/// The key lengths [`mac_into`] takes: a cookie (16 bytes) and the 32-byte
/// keys.
pub(super) trait MacKeyLen {}
impl MacKeyLen for [u8; MAC_LEN] {}
impl MacKeyLen for [u8; HASH_LEN] {}

/// Compares two MACs in constant time.
fn mac_eq(a: &[u8; MAC_LEN], b: &[u8; MAC_LEN]) -> bool {
    a.ct_eq(b).into()
}

/// The key of `mac1` for messages sent to one node:
/// `BLAKE2s("alloy/al/mac1/v1" || S)`, with `S` that node's static public
/// key.
///
/// The key is derived from a public key, so it is not secret, and it lives as
/// long as the static key (section 3.1). A node derives one for its own static
/// key, to check `mac1` on what it receives, and one per peer, to compute
/// `mac1` on what it sends.
#[derive(Clone)]
pub struct Mac1Key([u8; HASH_LEN]);

impl Mac1Key {
    /// Derives the `mac1` key for messages sent to the node whose static
    /// public key is `recipient`.
    #[must_use]
    pub fn new(recipient: &PublicKey) -> Self {
        let mut key = [0u8; HASH_LEN];
        hash_into(&[LABEL_MAC1, recipient.as_bytes()], &mut key);
        Self(key)
    }

    /// Returns `mac1` of a message: the MAC of `m1`, every byte of the
    /// message before `mac1`.
    #[must_use]
    pub fn mac1(&self, m1: &[u8]) -> [u8; MAC_LEN] {
        let mut out = [0u8; MAC_LEN];
        mac_into(&self.0, &[m1], &mut out);
        out
    }

    /// Whether `mac1` is the `mac1` of `m1`. The comparison runs in constant
    /// time.
    #[must_use]
    pub fn verify_mac1(&self, m1: &[u8], mac1: &[u8; MAC_LEN]) -> bool {
        mac_eq(&self.mac1(m1), mac1)
    }
}

/// Shows only the type name.
impl fmt::Debug for Mac1Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mac1Key").finish_non_exhaustive()
    }
}

/// A responder's 32-byte `cookie_secret` (section 3.1).
///
/// The caller draws it from the `alloy-rt` randomness trait, replaces it with
/// a fresh draw every 120 seconds, and keeps the one it replaced for 120 more.
/// It is erased when it is dropped and its `Debug` output shows no key bytes.
pub struct CookieSecret(Zeroizing<[u8; KEY_LEN]>);

impl CookieSecret {
    /// Returns the cookie secret with these bytes, which must be fresh random
    /// bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Returns the cookie for an initiation from `src`:
    /// `MAC(cookie_secret, src)`.
    ///
    /// `src` is the source the initiation came from, in the form section 3.1
    /// fixes: the IP address and port of the datagram or stream, or the
    /// envelope's `src` and `relay_handle`. This takes it as bytes.
    #[must_use]
    pub fn cookie(&self, src: &[u8]) -> Cookie {
        let mut cookie = Cookie([0u8; MAC_LEN]);
        mac_into(&self.0, &[src], &mut cookie.0);
        cookie
    }
}

/// Shows only the type name.
impl fmt::Debug for CookieSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CookieSecret").finish_non_exhaustive()
    }
}

impl ZeroizeOnDrop for CookieSecret {}

/// A 16-byte cookie: the key of `mac2` (section 3.1).
///
/// A responder computes one with [`CookieSecret::cookie`] and sends it in a
/// cookie reply; the initiator recovers it with [`CookieReplyKey::open`] and
/// keeps it for 120 seconds. It is erased when it is dropped and its `Debug`
/// output shows no bytes.
pub struct Cookie([u8; MAC_LEN]);

impl Cookie {
    /// Returns `mac2` of a message: `MAC(cookie, m2)`, where `m2` is every
    /// byte of the message before `mac2`, `mac1` included.
    #[must_use]
    pub fn mac2(&self, m2: &[u8]) -> [u8; MAC_LEN] {
        let mut out = [0u8; MAC_LEN];
        mac_into(&self.0, &[m2], &mut out);
        out
    }
}

impl Drop for Cookie {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl ZeroizeOnDrop for Cookie {}

/// Shows only the type name.
impl fmt::Debug for Cookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cookie").finish_non_exhaustive()
    }
}

/// Whether `mac2` is valid for an initiation from `src` (section 3.1): it
/// verifies under the cookie computed from `current`, or from `previous`, the
/// secret `current` replaced.
///
/// This makes at most two `mac2` checks, the limit section 6 sets, and each
/// comparison runs in constant time. `m2` is every byte of the initiation
/// before `mac2`. A responder calls this only under load, after `mac1`
/// verified.
#[must_use]
pub fn verify_mac2(
    current: &CookieSecret,
    previous: Option<&CookieSecret>,
    src: &[u8],
    m2: &[u8],
    mac2: &[u8; MAC_LEN],
) -> bool {
    if mac_eq(&current.cookie(src).mac2(m2), mac2) {
        return true;
    }
    previous.is_some_and(|previous| mac_eq(&previous.cookie(src).mac2(m2), mac2))
}

/// The key of the cookie reply `encrypted cookie`:
/// `BLAKE2s("alloy/al/cookie/v1" || S)`, with `S` the responder's static
/// public key, used with XChaCha20-Poly1305.
///
/// It is derived from a public key and lives as long as the static key
/// (section 3.1). A responder derives it for its own static key, an initiator
/// for each responder it sends to.
pub struct CookieReplyKey(XChaCha20Poly1305);

impl CookieReplyKey {
    /// Derives the cookie reply key of the responder whose static public key
    /// is `responder`.
    #[must_use]
    pub fn new(responder: &PublicKey) -> Self {
        let mut key = Zeroizing::new([0u8; HASH_LEN]);
        hash_into(&[LABEL_COOKIE, responder.as_bytes()], &mut key);
        Self(XChaCha20Poly1305::new(<&Array<u8, _>>::from(&*key)))
    }

    /// Returns the `encrypted cookie` field of a cookie reply: `cookie`
    /// encrypted under this key with `nonce`, the reply's `nonce` field, and
    /// with `mac1`, the `mac1` of the initiation answered, as associated data.
    ///
    /// `nonce` is 24 fresh random bytes the caller draws from the `alloy-rt`
    /// randomness trait for every reply; this crate never reads a generator.
    ///
    /// # Errors
    ///
    /// [`CookieError`] only if the AEAD refuses to encrypt, which it does
    /// only for inputs far longer than a cookie.
    pub fn seal(
        &self,
        nonce: &[u8; COOKIE_NONCE_LEN],
        cookie: &Cookie,
        mac1: &[u8; MAC_LEN],
    ) -> Result<[u8; ENCRYPTED_COOKIE_LEN], CookieError> {
        let mut out = [0u8; ENCRYPTED_COOKIE_LEN];
        let (body, tag_room) = out.split_at_mut(MAC_LEN);
        body.copy_from_slice(&cookie.0);
        let tag = self
            .0
            .encrypt_inout_detached(<&Array<u8, _>>::from(nonce), mac1, body.into())
            .map_err(|_| CookieError)?;
        tag_room.copy_from_slice(&tag);
        Ok(out)
    }

    /// Returns the cookie in a cookie reply's `encrypted cookie` field, if it
    /// decrypts under this key with `nonce` and with `mac1`, the `mac1` of the
    /// pending initiation the reply names, as associated data.
    ///
    /// # Errors
    ///
    /// [`CookieError`] when the field does not authenticate. The initiator
    /// drops the reply then.
    pub fn open(
        &self,
        nonce: &[u8; COOKIE_NONCE_LEN],
        encrypted: &[u8; ENCRYPTED_COOKIE_LEN],
        mac1: &[u8; MAC_LEN],
    ) -> Result<Cookie, CookieError> {
        let (body, tag) = encrypted.split_at(MAC_LEN);
        let tag: &[u8; TAG_LEN] = tag.try_into().map_err(|_| CookieError)?;
        let mut cookie = Cookie([0u8; MAC_LEN]);
        cookie.0.copy_from_slice(body);
        self.0
            .decrypt_inout_detached(
                <&Array<u8, _>>::from(nonce),
                mac1,
                (&mut cookie.0[..]).into(),
                <&Array<u8, _>>::from(tag),
            )
            .map_err(|_| CookieError)?;
        Ok(cookie)
    }
}

/// Shows only the type name.
impl fmt::Debug for CookieReplyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CookieReplyKey").finish_non_exhaustive()
    }
}

/// A cookie reply did not open, or could not be sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("cookie reply does not authenticate")]
pub struct CookieError;
