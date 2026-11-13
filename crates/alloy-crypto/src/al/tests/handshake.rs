//! The handshake through the public interface: round trips, every failure
//! the task names, and erasure.

use super::super::noise::{AlCipher, SymmetricState};
use super::super::{
    EphemeralSecret, HandshakeError, INITIATION_NOISE_LEN, InitiatorHandshake, PresharedKey,
    PublicKey, RESPONSE_NOISE_LEN, ResponderHandshake, SessionKeys, StaticSecret,
};
use super::{INITIATION_PAYLOAD, Pair, RESPONSE_PAYLOAD, key_bytes};

/// Seals a frame with `from` and opens it with `to`, both ways round.
fn assert_paired(a: &SessionKeys, b: &SessionKeys) {
    let header = [4, 0, 0, 0, 0, 0, 0, 0, 0];
    for (from, to) in [(a, b), (b, a)] {
        let frame = b"\x45 inner ip packet";
        let mut buf = frame.to_vec();
        buf.extend_from_slice(&[0u8; 16]);
        from.send
            .seal_in_place(1, 5, &header, &mut buf)
            .expect("seal");
        assert_ne!(&buf[..frame.len()], &frame[..]);
        let opened = to
            .recv
            .open_in_place(1, 5, &header, &mut buf)
            .expect("open");
        assert_eq!(opened, &frame[..]);
    }
    let (a_probe_send, a_probe_recv) = a.probe_bytes();
    let (b_probe_send, b_probe_recv) = b.probe_bytes();
    assert_eq!(a_probe_send, b_probe_recv);
    assert_eq!(a_probe_recv, b_probe_send);
    assert_ne!(a_probe_send, a_probe_recv);
    assert_eq!(a.send.salt(), b.recv.salt());
}

#[test]
fn full_handshake_yields_equal_keys() {
    let pair = Pair::new();
    let (initiator_keys, responder_keys) = pair.run();
    assert_paired(&initiator_keys, &responder_keys);
}

#[test]
fn responder_learns_initiator_static_key_and_payloads_arrive() {
    let pair = Pair::new();
    let (pending, initiation) = pair.initiate();
    let (responder, initiator, payload) =
        ResponderHandshake::read_initiation(&pair.responder, &initiation).expect("read");
    assert_eq!(initiator, pair.initiator.public_key());
    assert_eq!(responder.initiator(), initiator);
    assert_eq!(payload, INITIATION_PAYLOAD);
    let (_, response) = responder
        .write_response(
            EphemeralSecret::from_bytes(pair.responder_ephemeral),
            &pair.psk(),
            &RESPONSE_PAYLOAD,
        )
        .expect("write");
    let (_, payload) = pending
        .read_response(&pair.initiator, &pair.psk(), &response)
        .expect("read response");
    assert_eq!(payload, RESPONSE_PAYLOAD);
}

#[test]
fn no_preshared_key_is_32_zero_bytes_and_works() {
    let mut pair = Pair::new();
    pair.psk = [0u8; 32];
    let (pending, initiation) = pair.initiate();
    let (responder_keys, response, _) = pair.respond(&initiation);
    let (initiator_keys, _) = pending
        .read_response(&pair.initiator, &PresharedKey::none(), &response)
        .expect("response");
    assert_paired(&initiator_keys, &responder_keys);
}

#[test]
fn fresh_ephemerals_give_fresh_keys() {
    let first = Pair::new();
    let mut second = Pair::new();
    second.initiator_ephemeral = key_bytes(9);
    second.responder_ephemeral = key_bytes(10);
    let (a, _) = first.run();
    let (b, _) = second.run();
    assert_ne!(a.probe_bytes(), b.probe_bytes());
    assert_ne!(a.send.salt(), b.send.salt());
}

#[test]
fn wrong_preshared_key_fails_at_the_initiator() {
    let pair = Pair::new();
    let (pending, initiation) = pair.initiate();
    let (_, response, _) = pair.respond(&initiation);
    let wrong = PresharedKey::from_bytes(key_bytes(77));
    let rejected = pending
        .read_response(&pair.initiator, &wrong, &response)
        .expect_err("a different pre-shared key must not authenticate");
    assert_eq!(rejected.error, HandshakeError::Unauthentic);
    let rejected = rejected
        .handshake
        .read_response(&pair.initiator, &PresharedKey::none(), &response)
        .expect_err("no pre-shared key must not authenticate either");
    assert_eq!(rejected.error, HandshakeError::Unauthentic);
}

#[test]
fn every_tampered_initiation_byte_fails() {
    let pair = Pair::new();
    let (_, initiation) = pair.initiate();
    for index in 0..INITIATION_NOISE_LEN {
        for flip in [0x01u8, 0x80] {
            let mut tampered = initiation;
            tampered[index] ^= flip;
            let error = ResponderHandshake::read_initiation(&pair.responder, &tampered)
                .expect_err("tampered initiation must fail");
            assert_eq!(
                error,
                HandshakeError::Unauthentic,
                "byte {index} flip {flip:#04x}"
            );
        }
    }
}

#[test]
fn every_tampered_response_byte_fails_and_the_handshake_survives() {
    let pair = Pair::new();
    let (mut pending, initiation) = pair.initiate();
    let (responder_keys, response, _) = pair.respond(&initiation);
    for index in 0..RESPONSE_NOISE_LEN {
        for flip in [0x01u8, 0x80] {
            let mut tampered = response;
            tampered[index] ^= flip;
            let rejected = pending
                .read_response(&pair.initiator, &pair.psk(), &tampered)
                .expect_err("tampered response must fail");
            assert_eq!(rejected.error, HandshakeError::Unauthentic, "byte {index}");
            pending = rejected.handshake;
        }
    }
    // The handshake was handed back unchanged each time, so the genuine
    // response still completes it.
    let (initiator_keys, _) = pending
        .read_response(&pair.initiator, &pair.psk(), &response)
        .expect("genuine response");
    assert_paired(&initiator_keys, &responder_keys);
}

#[test]
fn initiation_to_a_wrong_static_key_fails() {
    let pair = Pair::new();
    let other = StaticSecret::from_bytes(key_bytes(42));
    // The initiator addresses a key the responder does not hold.
    let (_, initiation) = InitiatorHandshake::write_initiation(
        &pair.initiator,
        &other.public_key(),
        EphemeralSecret::from_bytes(pair.initiator_ephemeral),
        &INITIATION_PAYLOAD,
    )
    .expect("initiation");
    let error = ResponderHandshake::read_initiation(&pair.responder, &initiation)
        .expect_err("wrong responder key");
    assert_eq!(error, HandshakeError::Unauthentic);

    // A responder with a different static key reads a correct initiation.
    let (_, initiation) = pair.initiate();
    let error =
        ResponderHandshake::read_initiation(&other, &initiation).expect_err("wrong responder");
    assert_eq!(error, HandshakeError::Unauthentic);
}

#[test]
fn response_read_with_a_wrong_initiator_static_key_fails() {
    let pair = Pair::new();
    let (pending, initiation) = pair.initiate();
    let (_, response, _) = pair.respond(&initiation);
    let other = StaticSecret::from_bytes(key_bytes(43));
    let rejected = pending
        .read_response(&other, &pair.psk(), &response)
        .expect_err("se differs");
    assert_eq!(rejected.error, HandshakeError::Unauthentic);
}

#[test]
fn small_order_keys_are_rejected() {
    let pair = Pair::new();
    // The all-zero u-coordinate has small order, so every DH with it is zero.
    let zero = PublicKey::from_bytes([0u8; 32]);
    let error = InitiatorHandshake::write_initiation(
        &pair.initiator,
        &zero,
        EphemeralSecret::from_bytes(pair.initiator_ephemeral),
        &INITIATION_PAYLOAD,
    )
    .expect_err("zero responder key");
    assert_eq!(error, HandshakeError::NonContributory);

    // An initiation whose ephemeral is the zero point.
    let (_, mut initiation) = pair.initiate();
    initiation[..32].copy_from_slice(&[0u8; 32]);
    let error = ResponderHandshake::read_initiation(&pair.responder, &initiation)
        .expect_err("zero ephemeral");
    assert_eq!(error, HandshakeError::NonContributory);

    // A response whose ephemeral is the zero point.
    let (pending, initiation) = pair.initiate();
    let (_, mut response, _) = pair.respond(&initiation);
    response[..32].copy_from_slice(&[0u8; 32]);
    let rejected = pending
        .read_response(&pair.initiator, &pair.psk(), &response)
        .expect_err("zero ephemeral");
    assert_eq!(rejected.error, HandshakeError::NonContributory);
}

#[test]
fn same_inputs_give_same_bytes() {
    let pair = Pair::new();
    let (_, first) = pair.initiate();
    let (_, second) = pair.initiate();
    assert_eq!(first, second);
    let (_, response_a, _) = pair.respond(&first);
    let (_, response_b, _) = pair.respond(&second);
    assert_eq!(response_a, response_b);
}

#[test]
fn symmetric_state_erase_zeroes_every_secret() {
    let mut state = SymmetricState::<AlCipher>::initialize(b"alloy/al/handshake/v1");
    state.mix_hash(b"x");
    state.mix_key(b"input key material");
    assert!(!state.is_erased());
    let copy = state.duplicate();
    assert!(!copy.is_erased());
    state.erase();
    assert!(state.is_erased());
    // The duplicate is a separate copy with its own erasure.
    assert!(!copy.is_erased());
}

#[test]
fn debug_output_shows_no_secret() {
    let pair = Pair::new();
    let (pending, initiation) = pair.initiate();
    let (responder, _, _) =
        ResponderHandshake::read_initiation(&pair.responder, &initiation).expect("read");
    let (responder_keys, response) = responder
        .write_response(
            EphemeralSecret::from_bytes(pair.responder_ephemeral),
            &pair.psk(),
            &RESPONSE_PAYLOAD,
        )
        .expect("write");
    let (pending2, _) = pair.initiate();
    let (responder2, _, _) =
        ResponderHandshake::read_initiation(&pair.responder, &initiation).expect("read");
    let (keys, _) = pending
        .read_response(&pair.initiator, &pair.psk(), &response)
        .expect("response");

    assert_eq!(format!("{:?}", pair.initiator), "StaticSecret { .. }");
    assert_eq!(
        format!("{:?}", EphemeralSecret::from_bytes(key_bytes(1))),
        "EphemeralSecret { .. }"
    );
    assert_eq!(format!("{:?}", pair.psk()), "PresharedKey { .. }");
    assert_eq!(format!("{pending2:?}"), "InitiatorHandshake { .. }");
    assert_eq!(format!("{responder2:?}"), "ResponderHandshake { .. }");
    assert_eq!(
        format!("{keys:?}"),
        "SessionKeys { send: SendKey { .. }, recv: RecvKey { .. }, \
         probe_send: ProbeKey { .. }, probe_recv: ProbeKey { .. } }"
    );
    assert_eq!(format!("{:?}", responder_keys.send), "SendKey { .. }");

    // A public key is not secret and shows in hexadecimal.
    let public = PublicKey::from_bytes([0xab; 32]);
    assert_eq!(
        format!("{public:?}"),
        format!("PublicKey({})", "ab".repeat(32))
    );
}
