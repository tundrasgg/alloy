//! Every drop rule of ARFC 0003 sections 3.1 and 3.4 that a peer's message
//! can trigger, through the real flow. A dropped message gets no reply
//! (section 6).

mod common;

use alloy_crypto::al::{
    EphemeralSecret, InitiatorHandshake, Mac1Key, PresharedKey, ResponderHandshake, StaticSecret,
};
use alloy_link::{DropReason, Event};
use alloy_wire::{Initiation, Response, SessionIndex, Vni};
use common::{A, B, Pair, SRC, ip, secret, with_rng};

/// Recomputes the `mac1` of a handshake message to node `to`, whose `mac1`
/// starts at `at`.
fn remac(bytes: &mut [u8], at: usize, to: &StaticSecret) {
    let mac = Mac1Key::new(&to.public_key()).mac1(&bytes[..at]);
    bytes[at..at + 16].copy_from_slice(&mac);
}

/// An initiation from `from` to node b carrying `payload` as its decrypted
/// payload, with a valid `mac1`.
fn forge_initiation(from: &StaticSecret, payload: [u8; 16]) -> Vec<u8> {
    let b = secret(2);
    let (_, noise) = InitiatorHandshake::write_initiation(
        from,
        &b.public_key(),
        EphemeralSecret::from_bytes([9; 32]),
        &payload,
    )
    .unwrap();
    let mut bytes = vec![1, 1, 0, 0, 0x40, 0, 0, 0];
    bytes.extend_from_slice(&noise);
    bytes.extend_from_slice(&[0; 32]);
    remac(&mut bytes, Initiation::MAC1_OFFSET, &b);
    bytes
}

/// An initiation payload: counter, queues, flags, reserved.
fn payload(counter: u64, queues: u16, flags: u16, reserved: u32) -> [u8; 16] {
    let mut p = [0; 16];
    p[..8].copy_from_slice(&counter.to_le_bytes());
    p[8..10].copy_from_slice(&queues.to_le_bytes());
    p[10..12].copy_from_slice(&flags.to_le_bytes());
    p[12..].copy_from_slice(&reserved.to_le_bytes());
    p
}

/// Hands `bytes` to node `i`, expects `reason`, and checks nothing was sent.
fn dropped(p: &mut Pair<'_>, i: usize, bytes: Vec<u8>, reason: DropReason) {
    assert_eq!(p.recv(i, bytes), Err(reason));
    assert!(p.take(1 - i).is_empty(), "no reply to a dropped message");
}

#[test]
fn initiation_drop_rules() {
    with_rng(21, |rng| {
        let mut p = Pair::new(rng);
        p.connect(A);
        let genuine = p.take(B).remove(0);
        let b = secret(2);

        let mut m = genuine.clone();
        m[1] = 2;
        dropped(&mut p, B, m, DropReason::Malformed);
        let mut m = genuine.clone();
        m[3] = 1;
        dropped(&mut p, B, m, DropReason::Malformed);
        dropped(&mut p, B, genuine[..151].to_vec(), DropReason::Malformed);
        let mut m = genuine.clone();
        m.push(0);
        dropped(&mut p, B, m, DropReason::Malformed);

        let mut m = genuine.clone();
        m[Initiation::MAC1_OFFSET] ^= 1;
        dropped(&mut p, B, m, DropReason::Mac1);
        // mac1 covers the header too.
        let mut m = genuine.clone();
        m[4] ^= 1;
        dropped(&mut p, B, m, DropReason::Mac1);

        let mut m = genuine.clone();
        m[50] ^= 1;
        remac(&mut m, Initiation::MAC1_OFFSET, &b);
        dropped(&mut p, B, m, DropReason::Unauthentic);

        let stranger = forge_initiation(&secret(3), payload(0, 1, 0, 0));
        dropped(&mut p, B, stranger, DropReason::UnknownPeer);

        for (n, bad) in [(0, 0, 0), (65, 0, 0), (1, 1, 0), (1, 0x8000, 0), (1, 0, 1)]
            .into_iter()
            .enumerate()
        {
            let (queues, flags, reserved) = bad;
            let m = forge_initiation(&secret(1), payload(n as u64, queues, flags, reserved));
            dropped(&mut p, B, m, DropReason::Payload);
        }

        p.recv(B, genuine.clone()).unwrap();
        assert_eq!(p.take(A).len(), 1, "the response");
        dropped(&mut p, B, genuine, DropReason::Replay);
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 1);
    });
}

#[test]
fn response_drop_rules() {
    with_rng(22, |rng| {
        let mut p = Pair::new(rng);
        let a = secret(1);
        p.connect(A);
        let initiation = p.take(B).remove(0);
        p.recv(B, initiation.clone()).unwrap();
        let genuine = p.take(A).remove(0);

        let mut m = genuine.clone();
        m[95] = 1;
        dropped(&mut p, A, m, DropReason::Malformed);
        let mut m = genuine.clone();
        m[1] = 0;
        dropped(&mut p, A, m, DropReason::Malformed);
        dropped(&mut p, A, genuine[..95].to_vec(), DropReason::Malformed);

        let mut m = genuine.clone();
        m[Response::MAC1_OFFSET + 3] ^= 1;
        dropped(&mut p, A, m, DropReason::Mac1);

        let mut m = genuine.clone();
        m[8] ^= 0x40;
        remac(&mut m, Response::MAC1_OFFSET, &a);
        dropped(&mut p, A, m, DropReason::UnknownIndex);

        // A forged response does not end the handshake.
        let mut m = genuine.clone();
        m[40] ^= 1;
        remac(&mut m, Response::MAC1_OFFSET, &a);
        dropped(&mut p, A, m, DropReason::Unauthentic);
        assert!(p.nodes[A].is_pending(&p.keys[B]));
        p.recv(A, genuine.clone()).unwrap();
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 1);
        // Once processed, the initiation is no longer pending.
        p.take(B);
        dropped(&mut p, A, genuine, DropReason::UnknownIndex);
    });
}

/// A response to `initiation` from node b whose decrypted payload is
/// `payload`.
fn forge_response(initiation: &[u8], payload: [u8; 4]) -> Vec<u8> {
    let msg = Initiation::decode(initiation).unwrap();
    let mut noise = [0; 112];
    noise.copy_from_slice(&initiation[Initiation::NOISE_RANGE]);
    let (handshake, _, _) = ResponderHandshake::read_initiation(&secret(2), &noise).unwrap();
    let (_, noise) = handshake
        .write_response(
            EphemeralSecret::from_bytes([8; 32]),
            &PresharedKey::from_bytes([7; 32]),
            &payload,
        )
        .unwrap();
    let mut bytes = vec![2, 1, 0, 0, 0x80, 0, 0, 0];
    bytes.extend_from_slice(&msg.sender_index.0.to_le_bytes());
    bytes.extend_from_slice(&noise);
    bytes.extend_from_slice(&[0; 32]);
    remac(&mut bytes, Response::MAC1_OFFSET, &secret(1));
    bytes
}

#[test]
fn a_response_payload_out_of_range_abandons_the_handshake() {
    with_rng(23, |rng| {
        for bad in [[0, 0, 0, 0], [65, 0, 0, 0], [1, 0, 1, 0], [1, 0, 0, 0x80]] {
            let mut p = Pair::new(rng);
            p.connect(A);
            let initiation = p.take(B).remove(0);
            dropped(
                &mut p,
                A,
                forge_response(&initiation, bad),
                DropReason::Payload,
            );
            assert!(!p.nodes[A].is_pending(&p.keys[B]));
            assert_eq!(p.nodes[A].session_count(&p.keys[B]), 0);
            assert_eq!(p.events(A), [Event::HandshakeAbandoned { peer: p.keys[B] }]);
        }
        // The same construction with a valid payload makes a session.
        let mut p = Pair::new(rng);
        p.connect(A);
        let initiation = p.take(B).remove(0);
        p.recv(A, forge_response(&initiation, [64, 0, 0, 0]))
            .unwrap();
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 1);
    });
}

#[test]
fn cookie_reply_drop_rules() {
    with_rng(24, |rng| {
        let mut p = Pair::new(rng);
        p.nodes[B].set_handshake_backlog(2_000);
        p.connect(A);
        let initiation = p.take(B).remove(0);
        p.recv(B, initiation).unwrap();
        let genuine = p.take(A).remove(0);
        assert_eq!(genuine.len(), 64);

        let mut m = genuine.clone();
        m[2] = 1;
        dropped(&mut p, A, m, DropReason::Malformed);
        dropped(&mut p, A, genuine[..63].to_vec(), DropReason::Malformed);
        let mut m = genuine.clone();
        m[4] ^= 0x40;
        dropped(&mut p, A, m, DropReason::UnknownIndex);
        let mut m = genuine.clone();
        m[40] ^= 1;
        dropped(&mut p, A, m, DropReason::Unauthentic);
        p.recv(A, genuine).unwrap();
    });
}

#[test]
fn transport_drop_rules() {
    with_rng(25, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        p.send(A, Vni::INFRASTRUCTURE, &ip(40, 1)).unwrap();
        let genuine = p.take(B).remove(0);

        dropped(&mut p, B, genuine[..25].to_vec(), DropReason::Malformed);
        for first in [0, 5, 6, 7, 0x80, 0xff] {
            let mut m = genuine.clone();
            m[0] = first;
            dropped(&mut p, B, m, DropReason::UnknownType);
        }
        dropped(&mut p, B, Vec::new(), DropReason::UnknownType);

        let index = u32::from_le_bytes([genuine[1], genuine[2], genuine[3], genuine[4]]);
        for other in [index ^ 0x8000_0000, index + 4, index + 63] {
            let mut m = genuine.clone();
            m[1..5].copy_from_slice(&SessionIndex(other).0.to_le_bytes());
            dropped(&mut p, B, m, DropReason::UnknownIndex);
        }

        let mut m = genuine.clone();
        m[20] ^= 1;
        dropped(&mut p, B, m, DropReason::Unauthentic);
        // The header is the associated data: a changed counter does not open.
        let mut m = genuine.clone();
        m[5] ^= 2;
        dropped(&mut p, B, m, DropReason::Unauthentic);

        p.recv(B, genuine.clone()).unwrap();
        assert_eq!(p.delivered(B).len(), 1);
        dropped(&mut p, B, genuine, DropReason::Replay);
        assert_eq!(p.nodes[B].drops(DropReason::Replay), 1);
    });
}

#[test]
fn an_initiation_from_any_source_shares_the_peer_s_counter() {
    with_rng(26, |rng| {
        let mut p = Pair::new(rng);
        p.connect(A);
        let initiation = p.take(B).remove(0);
        let mut copy = initiation.clone();
        p.call(B, |l, cx, out| l.receive(cx, SRC[B], &mut copy, out))
            .unwrap();
        // The same initiation from another address is still a replay.
        let mut again = initiation;
        let result = p.call(B, |l, cx, out| {
            l.receive(cx, &[192, 0, 2, 9, 0, 9], &mut again, out)
        });
        assert_eq!(result, Err(DropReason::Replay));
    });
}
