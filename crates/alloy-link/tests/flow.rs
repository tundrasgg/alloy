//! Two links driven against each other in memory: handshake, data both ways,
//! bindings, rekey, expiry, loss, reordering, replay, fragmentation and the
//! section 7 limits (ARFC 0003 sections 3.1 to 3.5 and 7).

mod common;

use alloy_crypto::al::PresharedKey;
use alloy_link::{
    DropReason, Event, HOLD_TIME, LOAD_THRESHOLD, MAX_BINDINGS_PER_SESSION, MAX_HELD_PER_VNI,
    MAX_SESSIONS_PER_PEER, PREVIOUS_RECV_TIME, REJECT_AFTER, REKEY_AFTER, RETRY_GIVE_UP,
    RETRY_INTERVAL, Sent,
};
use alloy_wire::{FRAGMENT_HEADER_LEN, Message, Vni};
use common::{A, B, BUDGET, Pair, ip, secs, vni, with_rng};

const VNI_0: Vni = Vni::INFRASTRUCTURE;

#[test]
fn handshake_then_data_both_ways() {
    with_rng(1, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 1);
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 1);
        let peer_b = p.keys[B];
        let peer_a = p.keys[A];
        assert_eq!(
            p.events(A),
            [Event::SessionMade {
                peer: peer_b,
                initiator: true
            }]
        );
        // b confirmed the session on a's keepalive.
        assert_eq!(
            p.events(B),
            [
                Event::SessionMade {
                    peer: peer_a,
                    initiator: false
                },
                Event::SessionConfirmed { peer: peer_a }
            ]
        );
        for n in 0..3 {
            assert_eq!(p.send(A, VNI_0, &ip(60, n)), Ok(Sent::Sealed));
            assert_eq!(p.send(B, VNI_0, &ip(80, n)), Ok(Sent::Sealed));
        }
        p.settle();
        let to_b: Vec<_> = (0..3).map(|n| (VNI_0, ip(60, n))).collect();
        let to_a: Vec<_> = (0..3).map(|n| (VNI_0, ip(80, n))).collect();
        assert_eq!(p.delivered(B), to_b);
        assert_eq!(p.delivered(A), to_a);
    });
}

#[test]
fn a_session_with_a_pre_shared_key_mismatch_never_forms() {
    with_rng(2, |rng| {
        let mut p = Pair::new(rng);
        p.nodes[B].add_peer(p.keys[A], PresharedKey::none());
        p.connect(A);
        let initiation = p.take(B).remove(0);
        p.recv(B, initiation).unwrap();
        let response = p.take(A).remove(0);
        assert_eq!(p.recv(A, response), Err(DropReason::Unauthentic));
        assert!(p.nodes[A].is_pending(&p.keys[B]));
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 0);
    });
}

#[test]
fn data_on_a_vni_waits_for_a_bind() {
    with_rng(3, |rng| {
        let mut p = Pair::new(rng);
        p.nodes[A].add_local_vni(vni(7));
        p.nodes[B].add_local_vni(vni(7));
        p.handshake();
        // No binding yet: the packet is held and a bind request goes out.
        assert_eq!(p.send(A, vni(7), &ip(40, 1)), Ok(Sent::Held));
        p.settle();
        assert_eq!(p.delivered(B), [(vni(7), ip(40, 1))]);
        // Now bound: sealed at once.
        assert_eq!(p.send(A, vni(7), &ip(40, 2)), Ok(Sent::Sealed));
        // The other way binds on its own.
        assert_eq!(p.send(B, vni(7), &ip(40, 3)), Ok(Sent::Held));
        p.settle();
        assert_eq!(p.delivered(B), [(vni(7), ip(40, 2))]);
        assert_eq!(p.delivered(A), [(vni(7), ip(40, 3))]);
    });
}

#[test]
fn a_bind_request_for_a_vni_without_a_port_is_dropped() {
    with_rng(4, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        assert_eq!(p.send(A, vni(9), &ip(40, 1)), Ok(Sent::Held));
        let request = p.take(B).remove(0);
        assert_eq!(p.recv(B, request), Err(DropReason::Bind));
        assert!(p.take(A).is_empty());
        // A second packet within the second sends no second request.
        assert_eq!(p.send(A, vni(9), &ip(40, 2)), Ok(Sent::Held));
        assert!(p.take(B).is_empty());
        // Held packets are dropped after 1 second.
        p.advance(HOLD_TIME);
        assert_eq!(p.nodes[A].drops(DropReason::Held), 2);
    });
}

#[test]
fn held_packets_are_bounded_per_vni() {
    with_rng(5, |rng| {
        let mut p = Pair::new(rng);
        for n in 0..MAX_HELD_PER_VNI {
            assert_eq!(p.send(A, VNI_0, &ip(40, n as u8)), Ok(Sent::Held));
        }
        assert_eq!(p.send(A, VNI_0, &ip(40, 99)), Err(DropReason::Held));
        // The first send started the handshake; completing it seals all 16.
        p.settle();
        let held: Vec<_> = (0..MAX_HELD_PER_VNI)
            .map(|n| (VNI_0, ip(40, n as u8)))
            .collect();
        assert_eq!(p.delivered(B), held);
        assert_eq!(p.nodes[A].drops(DropReason::Held), 1);
    });
}

#[test]
fn a_sender_holds_1023_bindings() {
    with_rng(6, |rng| {
        let mut p = Pair::new(rng);
        let max = MAX_BINDINGS_PER_SESSION as u32;
        for n in 1..=max + 1 {
            p.nodes[B].add_local_vni(vni(n));
        }
        p.handshake();
        for n in 1..=max + 1 {
            assert_eq!(p.send(A, vni(n), &ip(30, 0)), Ok(Sent::Held));
        }
        p.settle();
        // 1,023 VNIs bound and delivered; the 1,024th was never requested.
        assert_eq!(p.delivered(B).len(), MAX_BINDINGS_PER_SESSION);
        p.advance(HOLD_TIME);
        assert_eq!(p.nodes[A].drops(DropReason::Held), 1);
    });
}

#[test]
fn rekey_moves_traffic_to_the_new_session() {
    with_rng(7, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        let old = {
            p.send(A, VNI_0, &ip(40, 1)).unwrap();
            p.take(B).remove(0)
        };
        p.advance(REKEY_AFTER);
        p.events(A);
        // The first packet at 120 seconds starts a handshake and still goes
        // under the old session.
        assert_eq!(p.send(A, VNI_0, &ip(40, 2)), Ok(Sent::Sealed));
        assert!(p.nodes[A].is_pending(&p.keys[B]));
        p.settle();
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 2);
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 2);
        assert_eq!(p.delivered(B), [(VNI_0, ip(40, 2))]);
        // Both directions run on the new session.
        p.send(A, VNI_0, &ip(40, 3)).unwrap();
        p.send(B, VNI_0, &ip(40, 4)).unwrap();
        p.settle();
        assert_eq!(p.delivered(B), [(VNI_0, ip(40, 3))]);
        assert_eq!(p.delivered(A), [(VNI_0, ip(40, 4))]);
        // The old session's receive key lasts 10 seconds after the rekey.
        p.advance(PREVIOUS_RECV_TIME);
        assert_eq!(p.recv(B, old), Err(DropReason::UnknownIndex));
        // 180 seconds after its handshake the old session is discarded.
        p.advance(REJECT_AFTER - REKEY_AFTER - PREVIOUS_RECV_TIME);
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 1);
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 1);
        assert!(
            p.events(A)
                .contains(&Event::SessionDiscarded { peer: p.keys[B] })
        );
    });
}

#[test]
fn keys_are_discarded_180_seconds_after_the_handshake() {
    with_rng(8, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        p.advance(REJECT_AFTER - secs(1));
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 1);
        p.advance(secs(1));
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 0);
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 0);
    });
}

#[test]
fn a_lost_initiation_is_retried_then_abandoned() {
    with_rng(9, |rng| {
        let mut p = Pair::new(rng);
        p.connect(A);
        let first = p.take(B).remove(0);
        p.advance(RETRY_INTERVAL);
        let second = p.take(B).remove(0);
        assert_ne!(first, second, "a fresh initiation");
        p.recv(B, second).unwrap();
        // The first, delivered late, is a replay of an older counter.
        assert_eq!(p.recv(B, first), Err(DropReason::Replay));
        p.settle();
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 1);

        let mut q = Pair::new(rng);
        q.connect(A);
        let mut sent = 0;
        while q.nodes[A].is_pending(&q.keys[B]) {
            sent += q.take(B).len();
            q.advance(RETRY_INTERVAL);
        }
        // One initiation every 5 seconds from 0 to 85: abandoned at 90.
        assert_eq!(
            sent,
            (RETRY_GIVE_UP.as_secs() / RETRY_INTERVAL.as_secs()) as usize
        );
        assert!(
            q.events(A)
                .contains(&Event::HandshakeAbandoned { peer: q.keys[B] })
        );
    });
}

#[test]
fn reordered_and_replayed_transport_packets() {
    with_rng(10, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        for n in 0..5 {
            p.send(A, VNI_0, &ip(40, n)).unwrap();
        }
        let mut packets = p.take(B);
        let again = packets[2].clone();
        packets.reverse();
        for d in packets {
            p.recv(B, d).unwrap();
        }
        let expected: Vec<_> = (0..5).rev().map(|n| (VNI_0, ip(40, n))).collect();
        assert_eq!(p.delivered(B), expected);
        assert_eq!(p.recv(B, again), Err(DropReason::Replay));
        assert!(p.take(A).is_empty(), "no reply to a drop");
    });
}

#[test]
fn the_reorder_limit_is_2047_below_the_highest() {
    with_rng(11, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        // The keepalive took counter 0 on queue 0; send on queue 1.
        for _ in 0..2_049 {
            p.send_budget(A, VNI_0, 1, &ip(30, 0), BUDGET).unwrap();
        }
        let mut packets = p.take(B);
        let last = packets.pop().unwrap();
        p.recv(B, last).unwrap();
        // Counter 2,048 is accepted: counter 1 is 2,047 below, counter 0 is
        // 2,048 below.
        let second = packets.remove(1);
        let first = packets.remove(0);
        assert_eq!(p.recv(B, first), Err(DropReason::Replay));
        assert_eq!(p.recv(B, second), Ok(()));
    });
}

#[test]
fn fragments_reassemble_in_any_order() {
    with_rng(12, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        let budget = 100;
        let piece = budget - FRAGMENT_HEADER_LEN;
        // 16 fragments is the most a frame is split into.
        let big = ip(16 * piece, 3);
        p.send_budget(A, VNI_0, 0, &big, budget).unwrap();
        let mut fragments = p.take(B);
        assert_eq!(fragments.len(), 16);
        for f in &fragments {
            assert!(f.len() <= budget + alloy_wire::TRANSPORT_OVERHEAD);
        }
        fragments.swap(0, 15);
        fragments.swap(3, 9);
        let duplicate = fragments[4].clone();
        let last = fragments.pop().unwrap();
        for f in fragments {
            p.recv(B, f).unwrap();
        }
        assert!(p.delivered(B).is_empty());
        p.recv(B, last).unwrap();
        assert_eq!(p.delivered(B), [(VNI_0, big)]);
        // A replayed fragment fails the replay window first.
        assert_eq!(p.recv(B, duplicate), Err(DropReason::Replay));
        // One byte more needs 17 fragments: dropped, nothing sent.
        let too_big = ip(16 * piece + 1, 4);
        assert_eq!(
            p.send_budget(A, VNI_0, 0, &too_big, budget),
            Err(DropReason::TooLarge)
        );
        assert!(p.take(B).is_empty());
        // A budget that carries no fragment data.
        assert_eq!(
            p.send_budget(A, VNI_0, 0, &ip(20, 0), FRAGMENT_HEADER_LEN),
            Err(DropReason::TooLarge)
        );
    });
}

#[test]
fn an_incomplete_reassembly_is_abandoned_after_2_seconds() {
    with_rng(13, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        p.send_budget(A, VNI_0, 0, &ip(300, 1), 100).unwrap();
        let mut fragments = p.take(B);
        let last = fragments.pop().unwrap();
        for f in fragments {
            p.recv(B, f).unwrap();
        }
        p.advance(secs(2));
        assert_eq!(p.nodes[B].drops(DropReason::Reassembly), 1);
        // The last fragment now starts a reassembly of its own.
        p.recv(B, last).unwrap();
        assert!(p.delivered(B).is_empty());
    });
}

#[test]
fn reassembly_holds_4_mib_per_peer() {
    with_rng(14, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        let budget = 60_000;
        let piece = budget - FRAGMENT_HEADER_LEN;
        // Five frames of 16 fragments, each missing its last: 15 * 59,993
        // bytes each, so the fifth passes 4 MiB and drops the first.
        let mut lasts = Vec::new();
        for n in 0..5 {
            p.send_budget(A, VNI_0, 0, &ip(16 * piece, n), budget)
                .unwrap();
            let mut fragments = p.take(B);
            lasts.push(fragments.pop().unwrap());
            for f in fragments {
                p.recv(B, f).unwrap();
            }
        }
        assert_eq!(p.nodes[B].drops(DropReason::Reassembly), 1);
        let second = lasts.remove(1);
        let first = lasts.remove(0);
        p.recv(B, first).unwrap();
        assert!(p.delivered(B).is_empty(), "the first frame was dropped");
        p.recv(B, second).unwrap();
        assert_eq!(p.delivered(B), [(VNI_0, ip(16 * piece, 1))]);
    });
}

#[test]
fn a_fifth_session_discards_the_oldest() {
    with_rng(15, |rng| {
        let mut p = Pair::new(rng);
        for _ in 0..MAX_SESSIONS_PER_PEER {
            p.handshake();
            p.advance(secs(1));
        }
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 4);
        p.events(A);
        // The initiation for a fifth counts as one: the oldest goes first.
        p.connect(A);
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 3);
        assert_eq!(p.events(A), [Event::SessionDiscarded { peer: p.keys[B] }]);
        p.settle();
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 4);
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 4);
        // The newest session carries traffic.
        p.send(A, VNI_0, &ip(40, 1)).unwrap();
        p.send(B, VNI_0, &ip(40, 2)).unwrap();
        p.settle();
        assert_eq!(p.delivered(B).len(), 1);
        assert_eq!(p.delivered(A).len(), 1);
    });
}

#[test]
fn under_load_an_initiation_without_mac2_gets_a_cookie_reply() {
    with_rng(16, |rng| {
        let mut p = Pair::new(rng);
        // At the threshold the node is not under load.
        p.nodes[B].set_handshake_backlog(LOAD_THRESHOLD);
        assert!(!p.nodes[B].is_under_load());
        p.nodes[B].set_handshake_backlog(LOAD_THRESHOLD + 1);
        assert!(p.nodes[B].is_under_load());
        p.connect(A);
        let initiation = p.take(B).remove(0);
        p.recv(B, initiation.clone()).unwrap();
        let reply = p.take(A).remove(0);
        assert!(matches!(
            Message::decode(&reply),
            Ok(Message::CookieReply(_))
        ));
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 0);
        // From another source the cookie differs: another cookie reply.
        let mut copy = initiation.clone();
        p.call(B, |l, cx, out| {
            l.receive(cx, &[192, 0, 2, 1, 0, 1], &mut copy, out)
        })
        .unwrap();
        assert_eq!(p.take(A).len(), 1);
        // a keeps the cookie; its retry carries a valid mac2.
        p.recv(A, reply).unwrap();
        p.advance(RETRY_INTERVAL);
        p.settle();
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 1);
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 1);
    });
}

#[test]
fn removing_a_peer_discards_its_sessions() {
    with_rng(17, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        let peer = p.keys[B];
        p.call(A, |l, _, out| l.remove_peer(&peer, out));
        assert_eq!(p.nodes[A].session_count(&peer), 0);
        assert_eq!(p.send(A, VNI_0, &ip(40, 1)), Err(DropReason::UnknownPeer));
        assert_eq!(p.nodes[A].drops(DropReason::UnknownPeer), 1);
    });
}

/// Every datagram both nodes emit, for one seed.
fn trace(seed: u64) -> Vec<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    with_rng(seed, move |rng| {
        let mut p = Pair::new(rng);
        let mut seen = Vec::new();
        p.connect(A);
        while !p.inflight[A].is_empty() || !p.inflight[B].is_empty() {
            for i in [A, B] {
                for d in p.take(i) {
                    seen.push(d.clone());
                    p.recv(i, d).unwrap();
                }
            }
        }
        p.send(A, VNI_0, &ip(40, 1)).unwrap();
        seen.extend(p.take(B));
        tx.send(seen).unwrap();
    });
    rx.recv().unwrap()
}

#[test]
fn a_seed_fixes_every_byte() {
    assert_eq!(trace(42), trace(42));
    assert_ne!(trace(42), trace(43));
}

/// Hands every datagram in flight to each node in turn until none is left,
/// whatever each `receive` returns.
fn deliver_all(p: &mut Pair<'_>) {
    while !p.inflight[A].is_empty() || !p.inflight[B].is_empty() {
        for i in [A, B] {
            for d in p.take(i) {
                let _ = p.recv(i, d);
            }
        }
    }
}

#[test]
fn crossed_rekeys_keep_traffic_flowing_both_ways() {
    with_rng(104, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        p.advance(REKEY_AFTER);
        // Each node sends before it receives anything: both start a rekey
        // and the two initiations cross.
        p.send(A, VNI_0, &ip(40, 1)).unwrap();
        p.send(B, VNI_0, &ip(40, 2)).unwrap();
        assert!(p.nodes[A].is_pending(&p.keys[B]));
        assert!(p.nodes[B].is_pending(&p.keys[A]));
        deliver_all(&mut p);
        assert_eq!(p.delivered(A), [(VNI_0, ip(40, 2))]);
        assert_eq!(p.delivered(B), [(VNI_0, ip(40, 1))]);
        // Past the 10 seconds, and then every second until the crossed
        // sessions are long discarded, each packet arrives once.
        p.advance(PREVIOUS_RECV_TIME + secs(1));
        for n in 0..200u8 {
            p.send(A, VNI_0, &ip(40, n)).unwrap();
            p.send(B, VNI_0, &ip(41, n)).unwrap();
            deliver_all(&mut p);
            assert_eq!(p.delivered(B), [(VNI_0, ip(40, n))], "a to b, round {n}");
            assert_eq!(p.delivered(A), [(VNI_0, ip(41, n))], "b to a, round {n}");
            p.advance(secs(1));
        }
        assert_eq!(p.nodes[A].drops(DropReason::UnknownIndex), 0);
        assert_eq!(p.nodes[B].drops(DropReason::UnknownIndex), 0);
    });
}

#[test]
fn lost_responses_leave_both_nodes_on_the_previous_session() {
    with_rng(105, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        p.advance(REKEY_AFTER);
        // a's rekey reaches b twice and both responses are lost: b makes two
        // sessions that a never holds.
        p.send(A, VNI_0, &ip(40, 1)).unwrap();
        for _ in 0..2 {
            for d in p.take(B) {
                p.recv(B, d).unwrap();
            }
            p.take(A);
            p.advance(RETRY_INTERVAL);
        }
        p.take(B);
        p.delivered(B);
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 3);
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 1);
        // Long past 10 seconds, a still seals under the session both hold
        // and b still opens it, and the other way.
        p.advance(PREVIOUS_RECV_TIME + secs(5));
        p.take(B);
        p.send(A, VNI_0, &ip(40, 2)).unwrap();
        p.send(B, VNI_0, &ip(40, 3)).unwrap();
        for i in [A, B] {
            for d in p.take(i) {
                p.recv(i, d).unwrap();
            }
        }
        assert_eq!(p.delivered(B), [(VNI_0, ip(40, 2))]);
        assert_eq!(p.delivered(A), [(VNI_0, ip(40, 3))]);
    });
}

#[test]
fn no_key_is_used_past_its_lifetime_without_a_tick() {
    with_rng(101, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        p.send(A, VNI_0, &ip(40, 1)).unwrap();
        let late = p.take(B).remove(0);
        // 180 seconds after the handshake, with no tick in between.
        p.now = p.now.saturating_add(REJECT_AFTER);
        assert_eq!(p.recv(B, late), Err(DropReason::UnknownIndex));
        assert_eq!(p.nodes[B].session_count(&p.keys[A]), 0);
        // a does not seal under the expired session: it holds the packet
        // and starts a handshake.
        assert_eq!(p.send(A, VNI_0, &ip(40, 2)), Ok(Sent::Held));
        assert!(p.nodes[A].is_pending(&p.keys[B]));
        p.settle();
        assert_eq!(p.delivered(B), [(VNI_0, ip(40, 2))]);

        // The previous receive key, 10 seconds after a packet under the
        // newer session showed that the peer moved on, with no tick either.
        let mut q = Pair::new(rng);
        q.handshake();
        q.send(A, VNI_0, &ip(40, 3)).unwrap();
        let old = q.take(B).remove(0);
        q.advance(REKEY_AFTER);
        q.send(A, VNI_0, &ip(40, 4)).unwrap();
        q.settle();
        q.now = q.now.saturating_add(PREVIOUS_RECV_TIME);
        assert_eq!(q.recv(B, old), Err(DropReason::UnknownIndex));
    });
}

#[test]
fn a_replayed_initiation_stays_a_replay_after_the_peer_is_added_again() {
    with_rng(102, |rng| {
        let mut p = Pair::new(rng);
        p.connect(A);
        let initiation = p.take(B).remove(0);
        p.recv(B, initiation.clone()).unwrap();
        p.settle();
        let a = p.keys[A];
        p.call(B, |l, _, out| l.remove_peer(&a, out));
        p.nodes[B].add_peer(a, PresharedKey::from_bytes([7; 32]));
        assert_eq!(p.recv(B, initiation), Err(DropReason::Replay));
        assert_eq!(p.nodes[B].session_count(&a), 0);
        assert!(p.take(A).is_empty(), "no reply to a replay");
        // A fresh initiation, with a higher counter, still makes a session.
        p.connect(A);
        p.settle();
        assert_eq!(p.nodes[B].session_count(&a), 1);
    });
}
