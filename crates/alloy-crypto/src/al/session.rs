//! Session keys (ARFC 0003 section 3.3) and transport sealing and opening
//! (section 3.4).

use std::fmt;

use chacha20poly1305::aead::array::Array;
use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::noise::{HASH_LEN, TAG_LEN, hkdf_expand_one, hkdf_extract};

/// The length of the per-session `salt` that starts every transport nonce.
pub const SALT_LEN: usize = 14;

/// The length of a transport header: `type` (1), `receiver_index` (4) and
/// `counter` (4). It is the associated data of every transport packet.
pub const TRANSPORT_HEADER_LEN: usize = 9;

/// The length of the Poly1305 tag at the end of every transport ciphertext.
pub const TRANSPORT_TAG_LEN: usize = TAG_LEN;

/// Transport counters stay below this value: a sender never seals with a
/// counter of 2^31 or more (ARFC 0003 section 3.4).
pub const COUNTER_LIMIT: u64 = 1 << 31;

/// The number of sending queues a session can have (ARFC 0003 section 7).
/// Queues are numbered from 0 to 63.
pub const MAX_QUEUES: u16 = 64;

const LABEL_I2R: &[u8] = b"alloy/al/session-i2r/v1";
const LABEL_R2I: &[u8] = b"alloy/al/session-r2i/v1";
const LABEL_SALT: &[u8] = b"alloy/al/session-salt/v1";
const LABEL_PROBE_I2R: &[u8] = b"alloy/al/probe-i2r/v1";
const LABEL_PROBE_R2I: &[u8] = b"alloy/al/probe-r2i/v1";

/// Which side of the handshake a node was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Initiator,
    Responder,
}

/// The keys one side holds for a session once its handshake completes.
///
/// They are derived as ARFC 0003 section 3.3 states, from the `Split()` keys
/// `k1`, `k2` and the handshake hash `h`:
///
/// ```text
/// prk  = HKDF-Extract(salt = h, IKM = k1 || k2)      over HMAC-BLAKE2s
/// i2r  = HKDF-Expand(prk, "alloy/al/session-i2r/v1",  32)
/// r2i  = HKDF-Expand(prk, "alloy/al/session-r2i/v1",  32)
/// salt = HKDF-Expand(prk, "alloy/al/session-salt/v1", 14)
/// pi2r = HKDF-Expand(prk, "alloy/al/probe-i2r/v1",    32)
/// pr2i = HKDF-Expand(prk, "alloy/al/probe-r2i/v1",    32)
/// ```
///
/// The initiator's [`send`](Self::send) is `i2r` and its
/// [`recv`](Self::recv) is `r2i`, its [`probe_send`](Self::probe_send) is
/// `pi2r` and its [`probe_recv`](Self::probe_recv) is `pr2i`. The responder's
/// are the reverse. Both directions use the same `salt`, which
/// [`send`](Self::send) and [`recv`](Self::recv) each carry.
///
/// By the time a value of this type exists, `k1`, `k2`, `prk`, the chaining
/// key, the handshake cipher key and the ephemeral private key are erased.
/// Each key here is erased when it is dropped, so the caller discards a key at
/// the end of its section 3.3 lifetime by dropping it. No key has a `Debug`
/// output that shows key bytes, and none can be cloned.
#[derive(Debug)]
pub struct SessionKeys {
    /// Seals the transport packets this side sends.
    pub send: SendKey,
    /// Opens the transport packets this side receives.
    pub recv: RecvKey,
    /// `probe_send_key`: the key that seals the probe messages this side
    /// sends (section 3.6).
    pub probe_send: ProbeKey,
    /// `probe_recv_key`: the key that opens the probe messages this side
    /// receives (section 3.6).
    pub probe_recv: ProbeKey,
}

impl SessionKeys {
    /// Derives the keys of `role` from the `Split()` output `split` (`k1`,
    /// `k2`) and the handshake hash `h`. `prk` and every expanded key buffer
    /// are erased before it returns.
    pub(crate) fn derive(split: &[[u8; HASH_LEN]; 2], h: &[u8; HASH_LEN], role: Role) -> Self {
        let mut prk = Zeroizing::new([0u8; HASH_LEN]);
        hkdf_extract(h, &[&split[0], &split[1]], &mut prk);

        let mut i2r = Zeroizing::new([0u8; HASH_LEN]);
        let mut r2i = Zeroizing::new([0u8; HASH_LEN]);
        let mut salt = Zeroizing::new([0u8; SALT_LEN]);
        let mut pi2r = Zeroizing::new([0u8; HASH_LEN]);
        let mut pr2i = Zeroizing::new([0u8; HASH_LEN]);
        hkdf_expand_one(&prk, LABEL_I2R, &mut i2r);
        hkdf_expand_one(&prk, LABEL_R2I, &mut r2i);
        hkdf_expand_one(&prk, LABEL_SALT, &mut salt);
        hkdf_expand_one(&prk, LABEL_PROBE_I2R, &mut pi2r);
        hkdf_expand_one(&prk, LABEL_PROBE_R2I, &mut pr2i);

        let (send, recv, probe_send, probe_recv) = match role {
            Role::Initiator => (&i2r, &r2i, &pi2r, &pr2i),
            Role::Responder => (&r2i, &i2r, &pr2i, &pi2r),
        };
        Self {
            send: SendKey(Box::new(TransportKey::new(send, &salt))),
            recv: RecvKey(Box::new(TransportKey::new(recv, &salt))),
            probe_send: ProbeKey::new(probe_send),
            probe_recv: ProbeKey::new(probe_recv),
        }
    }
}

/// An XChaCha20-Poly1305 key for one direction of a session, and the
/// session's salt.
struct TransportKey {
    aead: XChaCha20Poly1305,
    salt: [u8; SALT_LEN],
}

impl TransportKey {
    fn new(key: &[u8; HASH_LEN], salt: &[u8; SALT_LEN]) -> Self {
        Self {
            // The cipher keeps its own copy of the key and erases it on drop
            // (the `zeroize` feature of `chacha20poly1305`).
            aead: XChaCha20Poly1305::new(<&Array<u8, _>>::from(key)),
            salt: *salt,
        }
    }

    /// The section 3.4 nonce: `salt (14) || queue (2) || counter (8)`, the
    /// integers little-endian.
    fn nonce(&self, queue: u16, counter: u64) -> [u8; 24] {
        let mut nonce = [0u8; 24];
        nonce[..SALT_LEN].copy_from_slice(&self.salt);
        nonce[SALT_LEN..SALT_LEN + 2].copy_from_slice(&queue.to_le_bytes());
        nonce[SALT_LEN + 2..].copy_from_slice(&counter.to_le_bytes());
        nonce
    }
}

impl Drop for TransportKey {
    fn drop(&mut self) {
        self.salt.zeroize();
    }
}

/// The sending key of a session: seals transport packets (ARFC 0003 section
/// 3.4).
///
/// It is erased when it is dropped. Its `Debug` output shows no key bytes.
pub struct SendKey(Box<TransportKey>);

impl SendKey {
    /// Seals one transport packet in place.
    ///
    /// `buf` holds the inner frame followed by [`TRANSPORT_TAG_LEN`] bytes of
    /// room for the tag. On success the frame is replaced by its ciphertext
    /// and the room by the tag, so `buf` is the packet's `ciphertext` field.
    /// `header` is the packet's 9 header bytes, the associated data. `queue`
    /// is the sending queue and `counter` the queue's full 64-bit counter; the
    /// nonce is `salt || queue || counter`.
    ///
    /// The caller gives each `(queue, counter)` pair at most once per key, as
    /// section 3.4 requires: one counter per queue that grows by one with
    /// every packet. This method cannot see earlier calls and does not check
    /// that.
    ///
    /// # Errors
    ///
    /// Nothing is written on error.
    ///
    /// - [`SealError::CounterExhausted`] when `counter` is
    ///   [`COUNTER_LIMIT`] (2^31) or more: section 3.4 forbids sealing with
    ///   it.
    /// - [`SealError::QueueOutOfRange`] when `queue` is
    ///   [`MAX_QUEUES`] (64) or more.
    /// - [`SealError::BufferTooShort`] when `buf` has no room for a tag.
    pub fn seal_in_place(
        &self,
        queue: u16,
        counter: u64,
        header: &[u8; TRANSPORT_HEADER_LEN],
        buf: &mut [u8],
    ) -> Result<(), SealError> {
        if counter >= COUNTER_LIMIT {
            return Err(SealError::CounterExhausted);
        }
        if queue >= MAX_QUEUES {
            return Err(SealError::QueueOutOfRange);
        }
        let Some(frame_len) = buf.len().checked_sub(TRANSPORT_TAG_LEN) else {
            return Err(SealError::BufferTooShort);
        };
        let (frame, tag_room) = buf.split_at_mut(frame_len);
        let nonce = self.0.nonce(queue, counter);
        let tag = self
            .0
            .aead
            .encrypt_inout_detached(<&Array<u8, _>>::from(&nonce), header, frame.into())
            .map_err(|_| SealError::BufferTooShort)?;
        tag_room.copy_from_slice(&tag);
        Ok(())
    }
}

/// Shows only the type name.
impl fmt::Debug for SendKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendKey").finish_non_exhaustive()
    }
}

impl ZeroizeOnDrop for SendKey {}

/// The receiving key of a session: opens transport packets (ARFC 0003
/// section 3.4).
///
/// It is erased when it is dropped. Its `Debug` output shows no key bytes.
pub struct RecvKey(Box<TransportKey>);

impl RecvKey {
    /// Opens one transport packet in place and returns its inner frame.
    ///
    /// `buf` is the packet's `ciphertext` field: the encrypted frame followed
    /// by the tag. `header` is the packet's 9 header bytes. `queue` is the
    /// sending queue its `receiver_index` names and `counter` the full counter
    /// reconstructed from the header (section 3.4). On success the frame is
    /// decrypted in place and the returned slice is the frame, `buf` without
    /// its last [`TRANSPORT_TAG_LEN`] bytes.
    ///
    /// The tag check runs in constant time. The replay window and the receive
    /// checks of section 3.4 belong to the caller.
    ///
    /// # Errors
    ///
    /// [`OpenError`] when `buf` is shorter than a tag or the tag does not
    /// verify under this key, nonce and header. `buf` is unchanged then.
    pub fn open_in_place<'a>(
        &self,
        queue: u16,
        counter: u64,
        header: &[u8; TRANSPORT_HEADER_LEN],
        buf: &'a mut [u8],
    ) -> Result<&'a mut [u8], OpenError> {
        let Some(frame_len) = buf.len().checked_sub(TRANSPORT_TAG_LEN) else {
            return Err(OpenError);
        };
        let (frame, tag) = buf.split_at_mut(frame_len);
        let tag: &[u8; TRANSPORT_TAG_LEN] = (&*tag).try_into().map_err(|_| OpenError)?;
        let nonce = self.0.nonce(queue, counter);
        self.0
            .aead
            .decrypt_inout_detached(
                <&Array<u8, _>>::from(&nonce),
                header,
                (&mut *frame).into(),
                <&Array<u8, _>>::from(tag),
            )
            .map_err(|_| OpenError)?;
        Ok(frame)
    }
}

/// Shows only the type name.
impl fmt::Debug for RecvKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvKey").finish_non_exhaustive()
    }
}

impl ZeroizeOnDrop for RecvKey {}

/// A probe key of a session, `probe_send_key` or `probe_recv_key` (ARFC 0003
/// section 3.3).
///
/// It is derived with the session keys and erased when it is dropped. Its
/// `Debug` output shows no key bytes. Probe sealing and opening (section 3.6)
/// are not part of this interface, which covers sections 3.1 to 3.4, so this
/// type has no operation.
pub struct ProbeKey(Box<[u8; HASH_LEN]>);

impl ProbeKey {
    fn new(key: &[u8; HASH_LEN]) -> Self {
        let mut boxed = Box::new([0u8; HASH_LEN]);
        boxed.copy_from_slice(key);
        Self(boxed)
    }
}

impl Drop for ProbeKey {
    fn drop(&mut self) {
        self.0.as_mut().zeroize();
    }
}

impl ZeroizeOnDrop for ProbeKey {}

/// Shows only the type name.
impl fmt::Debug for ProbeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProbeKey").finish_non_exhaustive()
    }
}

/// Why [`SendKey::seal_in_place`] sealed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SealError {
    /// The counter is 2^31 or more. The queue seals nothing more under this
    /// session (ARFC 0003 section 3.4).
    #[error("transport counter is at or above 2^31")]
    CounterExhausted,
    /// The queue is 64 or more (ARFC 0003 section 7).
    #[error("sending queue is 64 or more")]
    QueueOutOfRange,
    /// The buffer has no room for the 16-byte tag.
    #[error("buffer has no room for the tag")]
    BufferTooShort,
}

/// A transport packet did not open: it is shorter than a tag, or it does not
/// authenticate under the key, nonce and header given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("transport packet does not open")]
pub struct OpenError;

#[cfg(test)]
impl SessionKeys {
    /// The bytes of `(probe_send, probe_recv)`. The transport keys live inside
    /// their ciphers, so tests check those by behavior.
    pub(crate) fn probe_bytes(&self) -> ([u8; HASH_LEN], [u8; HASH_LEN]) {
        (*self.probe_send.0, *self.probe_recv.0)
    }
}

#[cfg(test)]
impl SendKey {
    pub(crate) fn salt(&self) -> [u8; SALT_LEN] {
        self.0.salt
    }
}

#[cfg(test)]
impl RecvKey {
    pub(crate) fn salt(&self) -> [u8; SALT_LEN] {
        self.0.salt
    }
}
