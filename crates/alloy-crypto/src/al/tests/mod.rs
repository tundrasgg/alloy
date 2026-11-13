//! Tests of the AL cryptography.
//!
//! - `primitives`: X25519, XChaCha20-Poly1305 and BLAKE2s against their
//!   published vectors, and this crate's HMAC and HKDF against the `hmac` and
//!   `hkdf` crates.
//! - `noise_vector`: the `IKpsk2` state machine against the cacophony vector
//!   for `Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s`.
//! - `reference`: the AL instantiation against an independent straight-line
//!   computation of section 3.1 and 3.3.
//! - `handshake`, `cookie`, `transport`: behavior of the public interface,
//!   failures and erasure.

mod cookie;
mod handshake;
mod noise_vector;
mod primitives;
mod reference;
mod transport;

use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305};

use super::{
    EphemeralSecret, INITIATION_NOISE_LEN, INITIATION_PAYLOAD_LEN, InitiatorHandshake,
    PresharedKey, PublicKey, RESPONSE_NOISE_LEN, RESPONSE_PAYLOAD_LEN, ResponderHandshake,
    SessionKeys, StaticSecret,
};

/// Decodes a hexadecimal string. Test input only.
fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
        .collect()
}

/// Decodes a hexadecimal string of exactly `N` bytes.
fn hex_array<const N: usize>(s: &str) -> [u8; N] {
    hex(s).try_into().expect("hex length")
}

/// XChaCha20-Poly1305 straight from the `chacha20poly1305` crate: returns
/// `ciphertext || tag`.
fn xchacha_seal(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let mut buf = plaintext.to_vec();
    let tag = XChaCha20Poly1305::new(key.into())
        .encrypt_inout_detached(nonce.into(), aad, buf.as_mut_slice().into())
        .expect("encrypt");
    buf.extend_from_slice(&tag);
    buf
}

/// The inverse of [`xchacha_seal`], or `None` if the tag does not verify.
fn xchacha_open(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    let (body, tag) = sealed.split_at(sealed.len().checked_sub(16)?);
    let tag: &[u8; 16] = tag.try_into().ok()?;
    let mut buf = body.to_vec();
    XChaCha20Poly1305::new(key.into())
        .decrypt_inout_detached(nonce.into(), aad, buf.as_mut_slice().into(), tag.into())
        .ok()?;
    Some(buf)
}

/// Fixed key bytes for tests, distinct per `tag`. These are test inputs, not
/// keys of any deployment.
fn key_bytes(tag: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = tag
            .wrapping_mul(31)
            .wrapping_add(u8::try_from(i).expect("i < 32"));
    }
    bytes
}

const INITIATION_PAYLOAD: [u8; INITIATION_PAYLOAD_LEN] = [
    7, 0, 0, 0, 0, 0, 0, 0, // counter 7
    4, 0, // queues 4
    0, 0, // flags
    0, 0, 0, 0, // reserved
];
const RESPONSE_PAYLOAD: [u8; RESPONSE_PAYLOAD_LEN] = [2, 0, 0, 0];

/// Two nodes and the inputs of one handshake between them.
struct Pair {
    initiator: StaticSecret,
    responder: StaticSecret,
    initiator_ephemeral: [u8; 32],
    responder_ephemeral: [u8; 32],
    psk: [u8; 32],
}

impl Pair {
    fn new() -> Self {
        Self {
            initiator: StaticSecret::from_bytes(key_bytes(1)),
            responder: StaticSecret::from_bytes(key_bytes(2)),
            initiator_ephemeral: key_bytes(3),
            responder_ephemeral: key_bytes(4),
            psk: key_bytes(5),
        }
    }

    fn psk(&self) -> PresharedKey {
        PresharedKey::from_bytes(self.psk)
    }

    fn initiate(&self) -> (InitiatorHandshake, [u8; INITIATION_NOISE_LEN]) {
        InitiatorHandshake::write_initiation(
            &self.initiator,
            &self.responder.public_key(),
            EphemeralSecret::from_bytes(self.initiator_ephemeral),
            &INITIATION_PAYLOAD,
        )
        .expect("initiation")
    }

    fn respond(
        &self,
        initiation: &[u8; INITIATION_NOISE_LEN],
    ) -> (SessionKeys, [u8; RESPONSE_NOISE_LEN], PublicKey) {
        let (pending, initiator, payload) =
            ResponderHandshake::read_initiation(&self.responder, initiation).expect("read");
        assert_eq!(payload, INITIATION_PAYLOAD);
        let (keys, response) = pending
            .write_response(
                EphemeralSecret::from_bytes(self.responder_ephemeral),
                &self.psk(),
                &RESPONSE_PAYLOAD,
            )
            .expect("response");
        (keys, response, initiator)
    }

    /// Runs the whole handshake: `(initiator keys, responder keys)`.
    fn run(&self) -> (SessionKeys, SessionKeys) {
        let (pending, initiation) = self.initiate();
        let (responder_keys, response, _) = self.respond(&initiation);
        let (initiator_keys, payload) = pending
            .read_response(&self.initiator, &self.psk(), &response)
            .expect("read response");
        assert_eq!(payload, RESPONSE_PAYLOAD);
        (initiator_keys, responder_keys)
    }
}
