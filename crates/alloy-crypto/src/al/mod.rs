//! The cryptography of AL, the mesh wire protocol (ARFC 0003 sections 3.1 to
//! 3.4, ADR 0043).
//!
//! `alloy-link` runs handshakes and sessions through this module and never
//! touches a primitive. The module works on the cryptographic fields of AL
//! messages as byte arrays; framing, indices, timers, retries, session
//! tables, counters and replay windows belong to the caller.
//!
//! # Randomness
//!
//! The caller supplies every random byte: ephemeral private keys
//! ([`EphemeralSecret::from_bytes`]), cookie secrets
//! ([`CookieSecret::from_bytes`]) and cookie reply nonces
//! ([`CookieReplyKey::seal`]). It draws them from the `alloy-rt` randomness
//! trait. Nothing here reads a generator (`agent/eng/crypto.md` rule 4).
//!
//! # Handshake
//!
//! The handshake is Noise `IKpsk2` (revision 34) over X25519,
//! XChaCha20-Poly1305 and BLAKE2s, with the protocol name [`PROTOCOL_NAME`],
//! an empty prologue, and the handshake nonce `16 zero bytes || n` (8 bytes
//! little-endian). It is built from those primitives here.
//!
//! Initiator:
//!
//! 1. [`InitiatorHandshake::write_initiation`] returns the pending handshake
//!    and the 112 Noise bytes of the initiation.
//! 2. [`InitiatorHandshake::read_response`] takes the 52 Noise bytes of the
//!    response and the pre-shared key, consumes the handshake and returns the
//!    [`SessionKeys`] and the response payload. A response that does not
//!    authenticate hands the handshake back in [`ResponseRejected`].
//!
//! Responder:
//!
//! 1. [`ResponderHandshake::read_initiation`] takes the 112 Noise bytes and
//!    returns the pending handshake, the initiator's static key and the
//!    initiation payload.
//! 2. The caller checks the payload and the counter, picks the pre-shared key
//!    by the initiator's static key, then calls
//!    [`ResponderHandshake::write_response`], which consumes the handshake and
//!    returns the [`SessionKeys`] and the 52 Noise bytes of the response.
//!
//! ```
//! use alloy_crypto::al::{
//!     EphemeralSecret, HandshakeError, InitiatorHandshake, PresharedKey, ResponderHandshake,
//!     StaticSecret,
//! };
//!
//! // Fixed bytes stand in for keys drawn from the `alloy-rt` randomness trait.
//! let initiator = StaticSecret::from_bytes([1; 32]);
//! let responder = StaticSecret::from_bytes([2; 32]);
//! let psk = PresharedKey::none();
//!
//! let (pending, initiation) = InitiatorHandshake::write_initiation(
//!     &initiator,
//!     &responder.public_key(),
//!     EphemeralSecret::from_bytes([3; 32]),
//!     &[0; 16],
//! )?;
//! let (responding, initiator_key, _payload) =
//!     ResponderHandshake::read_initiation(&responder, &initiation)?;
//! assert_eq!(initiator_key, initiator.public_key());
//! let (responder_keys, response) =
//!     responding.write_response(EphemeralSecret::from_bytes([4; 32]), &psk, &[1, 0, 0, 0])?;
//! let (initiator_keys, _payload) = pending
//!     .read_response(&initiator, &psk, &response)
//!     .map_err(|rejected| rejected.error)?;
//!
//! let header = [4, 0, 0, 0, 0, 0, 0, 0, 0];
//! let mut packet = [0u8; 17];
//! initiator_keys.send.seal_in_place(0, 0, &header, &mut packet)?;
//! let frame = responder_keys.recv.open_in_place(0, 0, &header, &mut packet)?;
//! assert_eq!(frame, [0u8]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Erasure
//!
//! Every key type erases itself when it is dropped and shows no key bytes
//! through `Debug`; none is `Clone`. A pending handshake keeps its secrets on
//! the heap in one place. Writing the response or reading it consumes the
//! handshake, so its ephemeral private key, chaining key and cipher key are
//! erased, together with the `Split()` keys and `prk`, by the time the
//! [`SessionKeys`] exist (section 3.3). A handshake that fails or is dropped
//! erases the same values. The primitive crates are built with their
//! `zeroize` features, so the states they hold are erased too.
//!
//! The limit: erasure covers a value where it finally rests, not the bitwise
//! copies the compiler makes on the way there. A key built on the stack and
//! then moved into its `Box`, as a [`SendKey`] or [`RecvKey`] is, and an
//! [`EphemeralSecret`] passed by value into a handshake, may leave a copy in
//! a stack slot that nothing erases until later calls overwrite it. Rust
//! offers no way to erase such a copy, and the copies sit only in the stack
//! of the thread that made the key. Bytes the caller passes to a
//! `from_bytes` are the caller's to erase.
//!
//! The consumption is checked by the compiler. A handshake cannot be used
//! after it produced session keys:
//!
//! ```compile_fail,E0382
//! use alloy_crypto::al::{
//!     EphemeralSecret, InitiatorHandshake, PresharedKey, ResponderHandshake, StaticSecret,
//! };
//!
//! let initiator = StaticSecret::from_bytes([1; 32]);
//! let responder = StaticSecret::from_bytes([2; 32]);
//! let psk = PresharedKey::none();
//! let (pending, initiation) = InitiatorHandshake::write_initiation(
//!     &initiator,
//!     &responder.public_key(),
//!     EphemeralSecret::from_bytes([3; 32]),
//!     &[0; 16],
//! )
//! .unwrap();
//! let (responding, _, _) = ResponderHandshake::read_initiation(&responder, &initiation).unwrap();
//! let (_, response) = responding
//!     .write_response(EphemeralSecret::from_bytes([4; 32]), &psk, &[1, 0, 0, 0])
//!     .unwrap();
//! let _keys = pending.read_response(&initiator, &psk, &response).unwrap();
//! // `pending` was moved into `read_response` and erased there.
//! let _again = pending.read_response(&initiator, &psk, &response);
//! ```
//!
//! Nor can the responder's, after it wrote the response:
//!
//! ```compile_fail,E0382
//! use alloy_crypto::al::{
//!     EphemeralSecret, InitiatorHandshake, PresharedKey, ResponderHandshake, StaticSecret,
//! };
//!
//! let initiator = StaticSecret::from_bytes([1; 32]);
//! let responder = StaticSecret::from_bytes([2; 32]);
//! let psk = PresharedKey::none();
//! let (_, initiation) = InitiatorHandshake::write_initiation(
//!     &initiator,
//!     &responder.public_key(),
//!     EphemeralSecret::from_bytes([3; 32]),
//!     &[0; 16],
//! )
//! .unwrap();
//! let (responding, _, _) = ResponderHandshake::read_initiation(&responder, &initiation).unwrap();
//! let _ = responding.write_response(EphemeralSecret::from_bytes([4; 32]), &psk, &[1, 0, 0, 0]);
//! let _again = responding.write_response(EphemeralSecret::from_bytes([5; 32]), &psk, &[1, 0, 0, 0]);
//! ```
//!
//! And no key can be duplicated:
//!
//! ```compile_fail,E0599
//! fn duplicate(keys: &alloy_crypto::al::SessionKeys) {
//!     let _copy = keys.send.clone();
//! }
//! ```
//!
//! # MACs and cookies
//!
//! [`Mac1Key`] computes and checks `mac1`. [`CookieSecret`] makes cookies,
//! [`Cookie::mac2`] computes `mac2`, [`verify_mac2`] checks it, and
//! [`CookieReplyKey`] seals and opens the `encrypted cookie` of a cookie
//! reply. Every MAC comparison runs in constant time.
//!
//! # Transport
//!
//! [`SendKey::seal_in_place`] and [`RecvKey::open_in_place`] seal and open a
//! transport packet's ciphertext with the nonce `salt || queue || counter`
//! and the 9 header bytes as associated data.

mod cookie;
mod handshake;
mod keys;
mod noise;
mod session;

pub use cookie::{
    COOKIE_NONCE_LEN, Cookie, CookieError, CookieReplyKey, CookieSecret, ENCRYPTED_COOKIE_LEN,
    MAC_LEN, Mac1Key, verify_mac2,
};
pub use handshake::{
    HandshakeError, INITIATION_NOISE_LEN, INITIATION_PAYLOAD_LEN, InitiatorHandshake,
    PROTOCOL_NAME, RESPONSE_NOISE_LEN, RESPONSE_PAYLOAD_LEN, ResponderHandshake, ResponseRejected,
};
pub use keys::{EphemeralSecret, KEY_LEN, PresharedKey, PublicKey, StaticSecret};
pub use session::{
    COUNTER_LIMIT, MAX_QUEUES, OpenError, ProbeKey, RecvKey, SALT_LEN, SealError, SendKey,
    SessionKeys, TRANSPORT_HEADER_LEN, TRANSPORT_TAG_LEN,
};

#[cfg(test)]
mod tests;
