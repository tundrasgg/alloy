//! The AL handshake and session keys against an independent straight-line
//! computation of ARFC 0003 sections 3.1 and 3.3 and ADR 0043.
//!
//! The reference below follows the Noise specification revision 34 token by
//! token for `IKpsk2`, with the `hkdf` crate for HKDF, the `blake2` crate for
//! the hash, `x25519-dalek`'s bare `x25519` function, and XChaCha20-Poly1305
//! with the section 3.1 nonce. It shares no code with the state machine under
//! test. The cacophony vector (`noise_vector`) checks the state machine
//! itself; this checks what AL changes: the 21-byte protocol name padded
//! rather than hashed, the empty prologue, the XChaCha20-Poly1305 nonce, and
//! the section 3.3 key schedule.

use blake2::Blake2s256;
use blake2::digest::Digest;

use super::super::{SessionKeys, TRANSPORT_HEADER_LEN};
use super::{INITIATION_PAYLOAD, Pair, RESPONSE_PAYLOAD, xchacha_seal};

type ReferenceHkdf = hkdf::SimpleHkdf<Blake2s256>;

struct Reference {
    ck: [u8; 32],
    h: [u8; 32],
    k: [u8; 32],
}

fn x25519_public(private: [u8; 32]) -> [u8; 32] {
    x25519_dalek::x25519(private, x25519_dalek::X25519_BASEPOINT_BYTES)
}

impl Reference {
    fn new() -> Self {
        let mut h = [0u8; 32];
        h[..21].copy_from_slice(b"alloy/al/handshake/v1");
        Self {
            ck: h,
            h,
            k: [0u8; 32],
        }
    }

    fn mix_hash(&mut self, data: &[u8]) {
        let mut state = Blake2s256::new();
        state.update(self.h);
        state.update(data);
        self.h = state.finalize().into();
    }

    fn hkdf(&self, ikm: &[u8], out: &mut [u8]) {
        ReferenceHkdf::new(Some(&self.ck), ikm)
            .expand(&[], out)
            .expect("length");
    }

    fn mix_key(&mut self, ikm: &[u8]) {
        let mut out = [0u8; 64];
        self.hkdf(ikm, &mut out);
        self.ck.copy_from_slice(&out[..32]);
        self.k.copy_from_slice(&out[32..]);
    }

    fn mix_key_and_hash(&mut self, ikm: &[u8]) {
        let mut out = [0u8; 96];
        self.hkdf(ikm, &mut out);
        self.ck.copy_from_slice(&out[..32]);
        self.mix_hash(&out[32..64]);
        self.k.copy_from_slice(&out[64..]);
    }

    /// Every `EncryptAndHash` of `IKpsk2` uses a fresh key, so `n` is 0.
    fn encrypt_and_hash(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; 24];
        nonce[16..].copy_from_slice(&0u64.to_le_bytes());
        let ciphertext = xchacha_seal(&self.k, &nonce, &self.h, plaintext);
        self.mix_hash(&ciphertext);
        ciphertext
    }
}

struct Expected {
    initiation: Vec<u8>,
    response: Vec<u8>,
    i2r: [u8; 32],
    r2i: [u8; 32],
    salt: [u8; 14],
    pi2r: [u8; 32],
    pr2i: [u8; 32],
}

fn reference(pair: &Pair) -> Expected {
    let s_i = pair.initiator.dalek().to_bytes();
    let s_r = pair.responder.dalek().to_bytes();
    let s_i_pub = x25519_public(s_i);
    let s_r_pub = x25519_public(s_r);
    let e_i = pair.initiator_ephemeral;
    let e_r = pair.responder_ephemeral;
    let e_i_pub = x25519_public(e_i);
    let e_r_pub = x25519_public(e_r);

    let mut state = Reference::new();
    state.mix_hash(&[]);
    state.mix_hash(&s_r_pub);

    // -> e, es, s, ss
    let mut initiation = e_i_pub.to_vec();
    state.mix_hash(&e_i_pub);
    state.mix_key(&e_i_pub);
    state.mix_key(&x25519_dalek::x25519(e_i, s_r_pub));
    initiation.extend(state.encrypt_and_hash(&s_i_pub));
    state.mix_key(&x25519_dalek::x25519(s_i, s_r_pub));
    initiation.extend(state.encrypt_and_hash(&INITIATION_PAYLOAD));

    // <- e, ee, se, psk
    let mut response = e_r_pub.to_vec();
    state.mix_hash(&e_r_pub);
    state.mix_key(&e_r_pub);
    state.mix_key(&x25519_dalek::x25519(e_r, e_i_pub));
    state.mix_key(&x25519_dalek::x25519(e_r, s_i_pub));
    state.mix_key_and_hash(&pair.psk);
    response.extend(state.encrypt_and_hash(&RESPONSE_PAYLOAD));

    // Split(), then section 3.3.
    let mut split = [0u8; 64];
    state.hkdf(&[], &mut split);
    let section_3_3 = ReferenceHkdf::new(Some(&state.h), &split);
    let expand = |label: &[u8], out: &mut [u8]| section_3_3.expand(label, out).expect("length");
    let mut expected = Expected {
        initiation,
        response,
        i2r: [0u8; 32],
        r2i: [0u8; 32],
        salt: [0u8; 14],
        pi2r: [0u8; 32],
        pr2i: [0u8; 32],
    };
    expand(b"alloy/al/session-i2r/v1", &mut expected.i2r);
    expand(b"alloy/al/session-r2i/v1", &mut expected.r2i);
    expand(b"alloy/al/session-salt/v1", &mut expected.salt);
    expand(b"alloy/al/probe-i2r/v1", &mut expected.pi2r);
    expand(b"alloy/al/probe-r2i/v1", &mut expected.pr2i);
    expected
}

/// Seals a frame with a raw key and the section 3.4 nonce.
fn reference_seal(
    key: &[u8; 32],
    salt: &[u8; 14],
    queue: u16,
    counter: u64,
    header: &[u8; TRANSPORT_HEADER_LEN],
    frame: &[u8],
) -> Vec<u8> {
    let mut nonce = [0u8; 24];
    nonce[..14].copy_from_slice(salt);
    nonce[14..16].copy_from_slice(&queue.to_le_bytes());
    nonce[16..].copy_from_slice(&counter.to_le_bytes());
    xchacha_seal(key, &nonce, header, frame)
}

/// Checks that `keys` seal as the raw `send` key does and open what the raw
/// `recv` key seals, both with `salt`.
fn check_keys(keys: &SessionKeys, send: &[u8; 32], recv: &[u8; 32], salt: &[u8; 14]) {
    assert_eq!(&keys.send.salt(), salt);
    assert_eq!(&keys.recv.salt(), salt);
    let header = [4, 1, 2, 3, 4, 9, 0, 0, 0];
    let frame = [0x45u8, 0, 0, 20, 1, 2, 3, 4];
    for (queue, counter) in [(0u16, 0u64), (3, 9), (63, (1 << 31) - 1)] {
        let mut buf = frame.to_vec();
        buf.extend_from_slice(&[0u8; 16]);
        keys.send
            .seal_in_place(queue, counter, &header, &mut buf)
            .expect("seal");
        assert_eq!(
            buf,
            reference_seal(send, salt, queue, counter, &header, &frame)
        );

        let mut sealed = reference_seal(recv, salt, queue, counter, &header, &frame);
        let opened = keys
            .recv
            .open_in_place(queue, counter, &header, &mut sealed)
            .expect("open");
        assert_eq!(opened, &frame[..]);
    }
}

#[test]
fn handshake_and_session_keys_match_reference() {
    let pair = Pair::new();
    let expected = reference(&pair);

    let (pending, initiation) = pair.initiate();
    assert_eq!(initiation.to_vec(), expected.initiation);
    let (responder_keys, response, initiator) = pair.respond(&initiation);
    assert_eq!(initiator, pair.initiator.public_key());
    assert_eq!(response.to_vec(), expected.response);
    let (initiator_keys, _) = pending
        .read_response(&pair.initiator, &pair.psk(), &response)
        .expect("response");

    check_keys(
        &initiator_keys,
        &expected.i2r,
        &expected.r2i,
        &expected.salt,
    );
    check_keys(
        &responder_keys,
        &expected.r2i,
        &expected.i2r,
        &expected.salt,
    );
    assert_eq!(initiator_keys.probe_bytes(), (expected.pi2r, expected.pr2i));
    assert_eq!(responder_keys.probe_bytes(), (expected.pr2i, expected.pi2r));

    // The five derived values are distinct (crypto rule 6: one key, one
    // purpose).
    let all = [expected.i2r, expected.r2i, expected.pi2r, expected.pr2i];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b);
        }
    }
}
