//! Rekeys that cross, and how long a session outlives a rekey (ARFC 0003
//! sections 3.3 and 3.4).
//!
//! Every arrival order in the table of section 3.3, after two crossed
//! initiations between a and b with `O` the session both used before, each
//! with one packet each way every second and no loss on the underlay: every
//! packet must arrive, once, in the second it was sent, through the discard
//! of `O`, the next rekeys and the 180 seconds of the crossed sessions. The
//! row "`ka` before `Rb` and `kb` before `Ra`" cannot happen and has no test:
//! a node seals its first packet under its new session only once it
//! processes the response that makes it. The row "a packet under `O`
//! delayed up to 5 s" runs inside every other row.
//!
//! Then a seeded run of 1,000 crossed rekeys under loss, duplication,
//! reordering and late delivery: once the faults of each rekey stop, no
//! direction stays without delivery longer than [`BOUND`].

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use alloy_link::{
    DISCARD_AFTER_FOLLOWED, DropReason, HOLD_TIME, QUALIFY_TIME, REJECT_AFTER, REKEY_AFTER,
    RETRY_INTERVAL, Sent,
};
use alloy_wire::{MessageType, Vni};
use common::{A, B, Pair, SplitMix, ip, secs, vni, with_rng};

const VNI_0: Vni = Vni::INFRASTRUCTURE;

/// A VNI 0 packet from node `from` carrying `n`, so each delivery can be
/// traced to its send.
fn tagged(from: usize, n: u32) -> Vec<u8> {
    let mut p = vec![0x45; 24];
    p[1] = u8::try_from(from).unwrap();
    p[4..8].copy_from_slice(&n.to_le_bytes());
    p
}

fn type_of(datagram: &[u8]) -> Option<MessageType> {
    datagram.first().copied().and_then(MessageType::from_code)
}

/// The one datagram in flight to node `i`, which must be of type `t`.
fn only(p: &mut Pair<'_>, i: usize, t: MessageType) -> Vec<u8> {
    let mut ds = p.take(i);
    assert_eq!(ds.len(), 1, "one datagram to node {i}");
    let d = ds.remove(0);
    assert_eq!(type_of(&d), Some(t));
    d
}

/// Hands every datagram in flight to node `i` to it; each must be accepted.
fn deliver_to(p: &mut Pair<'_>, i: usize) {
    for d in p.take(i) {
        p.recv(i, d).expect("accepted");
    }
}

/// Each node sends one packet carrying `n` to the other, and everything in
/// flight is delivered: each must arrive, once, now.
fn exchange(p: &mut Pair<'_>, n: u32) {
    assert_eq!(p.send(A, VNI_0, &tagged(A, n)), Ok(Sent::Sealed), "a, {n}");
    assert_eq!(p.send(B, VNI_0, &tagged(B, n)), Ok(Sent::Sealed), "b, {n}");
    p.settle();
    assert_eq!(p.delivered(B), [(VNI_0, tagged(A, n))], "a to b, {n}");
    assert_eq!(p.delivered(A), [(VNI_0, tagged(B, n))], "b to a, {n}");
}

/// Both nodes hold `O`, used both ways, and it is due for a rekey. Each
/// node sends a packet: that starts its rekey, and the packet goes under
/// `O`, held back by the underlay. Returns the pair, each node's
/// initiation, and each node's held packet under `O`.
fn start(rng: &dyn alloy_rt::Rng) -> (Pair<'_>, [Vec<u8>; 2], [Vec<u8>; 2]) {
    let mut p = Pair::new(rng);
    p.handshake();
    exchange(&mut p, 0);
    p.advance(REKEY_AFTER);
    let mut init = [Vec::new(), Vec::new()];
    let mut late = [Vec::new(), Vec::new()];
    for i in [A, B] {
        assert_eq!(p.send(i, VNI_0, &tagged(i, 1)), Ok(Sent::Sealed));
        assert!(p.nodes[i].is_pending(&p.keys[1 - i]));
        for d in p.take(1 - i) {
            match type_of(&d) {
                Some(MessageType::Initiation) => init[i] = d,
                Some(MessageType::Transport) => late[i] = d,
                other => panic!("unexpected {other:?}"),
            }
        }
    }
    (p, init, late)
}

/// The two initiations cross: each node responds to the other's before any
/// response arrives. Returns `Ra`, the response that makes `S_a` at a, and
/// `Rb`, the one that makes `S_b` at b.
fn cross(p: &mut Pair<'_>, init: &[Vec<u8>; 2]) -> (Vec<u8>, Vec<u8>) {
    p.recv(B, init[A].clone()).unwrap();
    p.recv(A, init[B].clone()).unwrap();
    let ra = only(p, A, MessageType::Response);
    let rb = only(p, B, MessageType::Response);
    (ra, rb)
}

/// The traffic after the handshakes settled: one packet each way every
/// second for 400 seconds, each arriving in its second. At 5 seconds the
/// packets under `O` held back at the start arrive and are accepted. At
/// second `kept.0`, the nodes hold `kept.1` sessions.
fn run(p: &mut Pair<'_>, late: [Vec<u8>; 2], kept: (u32, [usize; 2])) {
    for s in 1..=400u32 {
        p.advance(secs(1));
        if s == 5 {
            for i in [A, B] {
                p.recv(1 - i, late[i].clone()).expect("late packet under O");
                assert_eq!(p.delivered(1 - i), [(VNI_0, tagged(i, 1))]);
            }
        }
        if s == kept.0 {
            assert_eq!(p.nodes[A].session_count(&p.keys[B]), kept.1[A], "a");
            assert_eq!(p.nodes[B].session_count(&p.keys[A]), kept.1[B], "b");
        }
        exchange(p, 100 + s);
    }
    for i in [A, B] {
        for reason in [DropReason::UnknownIndex, DropReason::Unauthentic] {
            assert_eq!(p.nodes[i].drops(reason), 0, "node {i}, {reason:?}");
        }
    }
}

#[test]
fn both_responses_before_either_first_packet() {
    with_rng(301, |rng| {
        let (mut p, init, late) = start(rng);
        let (ra, rb) = cross(&mut p, &init);
        p.recv(A, ra).unwrap();
        p.recv(B, rb).unwrap();
        p.settle();
        // a keeps S_b and b keeps S_a; O goes 10 seconds after the first
        // packet under either.
        run(
            &mut p,
            late,
            (DISCARD_AFTER_FOLLOWED.as_secs() as u32 + 1, [2, 2]),
        );
    });
}

/// `Ra`, then `ka`, then `Rb`, `Rb` 3 seconds late, with traffic both ways
/// meanwhile: b seals under `S_a` before it makes `S_b`.
fn ra_then_ka_then_rb(seed: u64, first: usize) {
    with_rng(seed, move |rng| {
        let (mut p, init, late) = start(rng);
        let (ra, rb) = cross(&mut p, &init);
        let (r_first, r_second) = if first == A { (ra, rb) } else { (rb, ra) };
        let second = 1 - first;
        p.recv(first, r_first).unwrap();
        let k_first = only(&mut p, second, MessageType::Transport);
        p.recv(second, k_first).unwrap();
        for n in 0..3 {
            p.advance(secs(1));
            exchange(&mut p, 10 + n);
        }
        p.recv(second, r_second).unwrap();
        deliver_to(&mut p, first);
        p.settle();
        run(
            &mut p,
            late,
            (DISCARD_AFTER_FOLLOWED.as_secs() as u32, [2, 2]),
        );
    });
}

#[test]
fn ra_then_ka_then_rb_late() {
    ra_then_ka_then_rb(302, A);
}

#[test]
fn rb_then_kb_then_ra_late() {
    ra_then_ka_then_rb(303, B);
}

#[test]
fn rb_lost() {
    with_rng(304, |rng| {
        let (mut p, init, late) = start(rng);
        let (ra, _lost) = cross(&mut p, &init);
        p.recv(A, ra).unwrap();
        p.settle();
        // Before b's next initiation, 5 seconds on: a holds O, S_b
        // unconfirmed and S_a; b holds O and S_a.
        run(&mut p, late, (RETRY_INTERVAL.as_secs() as u32 - 1, [3, 2]));
    });
}

/// Not crossed at b: `Rb` reaches b before a's initiation does. `kb`
/// reaches a before `Ra` when `kb_first`, after it otherwise.
fn not_crossed_at_b(seed: u64, kb_first: bool) {
    with_rng(seed, move |rng| {
        let (mut p, init, late) = start(rng);
        p.recv(A, init[B].clone()).unwrap();
        let rb = only(&mut p, B, MessageType::Response);
        p.recv(B, rb).unwrap();
        let kb = only(&mut p, A, MessageType::Transport);
        p.recv(B, init[A].clone()).unwrap();
        let ra = only(&mut p, A, MessageType::Response);
        if kb_first {
            p.recv(A, kb).unwrap();
            p.recv(A, ra).unwrap();
        } else {
            p.recv(A, ra).unwrap();
            p.recv(A, kb).unwrap();
        }
        p.settle();
        // S_a follows S_b and O at b, which keeps only S_a; a keeps S_b,
        // which no session follows at a.
        run(
            &mut p,
            late,
            (DISCARD_AFTER_FOLLOWED.as_secs() as u32 + 1, [2, 1]),
        );
    });
}

#[test]
fn not_crossed_at_b_kb_before_ra() {
    not_crossed_at_b(305, true);
}

#[test]
fn not_crossed_at_b_kb_after_ra() {
    not_crossed_at_b(306, false);
}

#[test]
fn a_silent_peer_leaves_the_previous_session_to_its_180_seconds() {
    with_rng(307, |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        let mut held = Vec::new();
        for n in 0..2 {
            p.send(B, VNI_0, &ip(40, n)).unwrap();
            held.push(only(&mut p, A, MessageType::Transport));
        }
        p.advance(REKEY_AFTER);
        // a rekeys; b answers and confirms the new session, then is silent.
        p.send(A, VNI_0, &ip(40, 9)).unwrap();
        p.settle();
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 2);
        // No packet under a session that follows O has reached a, so a
        // still accepts under O 50 seconds after the rekey.
        p.advance(secs(50));
        assert_eq!(p.recv(A, held.remove(0)), Ok(()));
        // b's first packet under the new session starts O's 10 seconds.
        p.send(B, VNI_0, &ip(40, 10)).unwrap();
        p.settle();
        p.advance(DISCARD_AFTER_FOLLOWED);
        assert_eq!(p.recv(A, held.remove(0)), Err(DropReason::UnknownIndex));
        assert_eq!(p.nodes[A].session_count(&p.keys[B]), 1);

        // Without that packet, O lasts until 180 seconds after its
        // handshake and not a moment more.
        let mut q = Pair::new(rng);
        q.handshake();
        q.send(B, VNI_0, &ip(40, 1)).unwrap();
        let late = only(&mut q, A, MessageType::Transport);
        q.advance(REKEY_AFTER);
        q.send(A, VNI_0, &ip(40, 2)).unwrap();
        q.settle();
        q.advance(REJECT_AFTER - REKEY_AFTER - secs(1));
        assert_eq!(q.nodes[A].session_count(&q.keys[B]), 2);
        q.advance(secs(1));
        assert_eq!(q.nodes[A].session_count(&q.keys[B]), 1);
        assert_eq!(q.recv(A, late), Err(DropReason::UnknownIndex));
    });
}

#[test]
fn a_session_stops_qualifying_5_seconds_after_a_newer_one_is_used() {
    with_rng(308, |rng| {
        let mut p = Pair::new(rng);
        for i in [A, B] {
            p.nodes[i].add_local_vni(vni(7));
        }
        p.handshake();
        assert_eq!(p.send(A, vni(7), &ip(40, 1)), Ok(Sent::Held));
        p.settle();
        assert_eq!(p.delivered(B), [(vni(7), ip(40, 1))]);
        p.advance(REKEY_AFTER);
        p.send(A, VNI_0, &ip(40, 2)).unwrap();
        // a makes the new session and seals its keepalive under it now.
        p.settle();
        assert_eq!(p.delivered(B), [(VNI_0, ip(40, 2))]);
        // The new session has no binding for VNI 7 and b never sees a's
        // requests: VNI 7 goes under O while O qualifies.
        for n in 0..QUALIFY_TIME.as_secs() {
            assert_eq!(p.send(A, vni(7), &ip(40, 3)), Ok(Sent::Sealed), "{n} s");
            p.take(B);
            p.now = p.now.saturating_add(secs(1));
        }
        // 5 seconds after the first packet under the new session, O is
        // used for nothing: the packet waits for the new session's bind.
        assert_eq!(p.send(A, vni(7), &ip(40, 4)), Ok(Sent::Held));
        p.settle();
        assert_eq!(p.delivered(B), [(vni(7), ip(40, 4))]);
    });
}

/// Rekeys in the seeded run.
const ROUNDS: u32 = 1_000;

/// Steps of faulty traffic per rekey, each up to 1 second.
const FAULTY_STEPS: usize = 6;

/// Seconds of clean traffic after each rekey's faults: past the 10 seconds
/// of 3.3 rule 2 that the faulty steps can start.
const CLEAN_SECONDS: u32 = 16;

/// How long a direction may go without delivery once the faults stop: a
/// handshake the faults broke is sent again after [`RETRY_INTERVAL`], and a
/// packet then waits up to [`HOLD_TIME`] for its session.
const BOUND: Duration = Duration::from_secs(RETRY_INTERVAL.as_secs() + HOLD_TIME.as_secs());

/// An underlay that loses 20% of datagrams, duplicates 5%, delivers 10%
/// 1 to 4 steps late, and shuffles the rest of each batch.
struct Underlay {
    rand: SplitMix,
    /// Datagrams delivered late: steps left, receiver, bytes.
    late: Vec<(u64, usize, Vec<u8>)>,
}

impl Underlay {
    fn step(&mut self, p: &mut Pair<'_>, faulty: bool) {
        let mut due = Vec::new();
        self.late.retain_mut(|(steps, to, d)| {
            *steps = steps.saturating_sub(1);
            let now = *steps == 0 || !faulty;
            if now {
                due.push((*to, std::mem::take(d)));
            }
            !now
        });
        for (to, d) in due {
            let _ = p.recv(to, d);
        }
        while !p.inflight[A].is_empty() || !p.inflight[B].is_empty() {
            let mut batch = Vec::new();
            for i in [A, B] {
                for d in p.take(i) {
                    if faulty && self.rand.chance(20) {
                        continue;
                    }
                    if faulty && self.rand.chance(5) {
                        batch.push((i, d.clone()));
                    }
                    if faulty && self.rand.chance(10) {
                        self.late.push((1 + self.rand.below(4), i, d));
                        continue;
                    }
                    batch.push((i, d));
                }
            }
            if faulty {
                for n in (1..batch.len()).rev() {
                    let m = usize::try_from(self.rand.below(n as u64 + 1)).unwrap();
                    batch.swap(n, m);
                }
            }
            for (i, d) in batch {
                let _ = p.recv(i, d);
            }
        }
    }
}

/// Runs the seeded rekeys. Returns the longest time, over all rekeys and
/// both directions, that a direction went without delivery after the
/// faults stopped, and the failures.
fn rekeys_under_faults(seed: u64) -> (u32, Vec<String>) {
    let (tx, rx) = std::sync::mpsc::channel();
    with_rng(seed, move |rng| {
        let mut p = Pair::new(rng);
        p.handshake();
        let mut u = Underlay {
            rand: SplitMix(seed),
            late: Vec::new(),
        };
        let mut next = 0u32;
        let mut seen = BTreeSet::new();
        let mut failures = Vec::new();
        let mut longest = 0;
        for round in 0..ROUNDS {
            // Both nodes rekey at once, so the initiations cross unless the
            // underlay keeps one back past the other's response.
            p.connect(A);
            p.connect(B);
            for _ in 0..FAULTY_STEPS {
                p.advance(Duration::from_millis(u.rand.below(1_000)));
                for i in [A, B] {
                    let _ = p.send(i, VNI_0, &tagged(i, next));
                    next += 1;
                }
                u.step(&mut p, true);
            }
            // The faults stop. Each direction's packet of every second must
            // arrive, except in the first BOUND seconds.
            let mut sent = Vec::new();
            for s in 0..CLEAN_SECONDS {
                for i in [A, B] {
                    let _ = p.send(i, VNI_0, &tagged(i, next));
                    sent.push((s, i, next));
                    next += 1;
                }
                u.step(&mut p, false);
                p.advance(secs(1));
            }
            u.step(&mut p, false);
            for i in [A, B] {
                for (_, packet) in p.delivered(i) {
                    let id = u32::from_le_bytes([packet[4], packet[5], packet[6], packet[7]]);
                    if usize::from(packet[1]) != 1 - i || !seen.insert(id) {
                        failures.push(format!("round {round}: packet {id} twice or astray"));
                    }
                }
            }
            for i in [A, B] {
                let mut gap = 0;
                for (s, from, id) in &sent {
                    if *from != i {
                        continue;
                    }
                    if seen.contains(id) {
                        gap = 0;
                        continue;
                    }
                    gap += 1;
                    longest = longest.max(gap);
                    if u64::from(*s) >= BOUND.as_secs() {
                        failures.push(format!(
                            "round {round}: node {i}'s packet of clean second {s} lost"
                        ));
                    }
                }
            }
        }
        tx.send((longest, failures)).unwrap();
    });
    rx.recv().unwrap()
}

#[test]
fn a_thousand_crossed_rekeys_under_faults_blackhole_nothing() {
    let seed = 309;
    let (longest, failures) = rekeys_under_faults(seed);
    assert!(
        failures.is_empty(),
        "seed {seed}, longest gap {longest} s, {} failures: {:#?}",
        failures.len(),
        &failures[..failures.len().min(20)]
    );
    assert!(
        u64::from(longest) <= BOUND.as_secs(),
        "seed {seed}: {longest} s"
    );
    eprintln!("seed {seed}: {ROUNDS} crossed rekeys, longest gap after the faults {longest} s");
}
