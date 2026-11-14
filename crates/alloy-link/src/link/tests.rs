//! Rules that a conforming peer never triggers, checked on the link's
//! internals: inner frame drops, counter and binding limits, counter blocks
//! and cookie secret lifetimes.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use alloy_rt::BoxFuture;
use alloy_sim::{Boot, HostConfig, RunConfig, RunResult, run};

use super::*;

const A: usize = 0;
const B: usize = 1;

fn with_rng(seed: u64, body: impl FnOnce(&dyn Rng) + Send + 'static) {
    let summary = run(RunConfig::new(seed), move |world| async move {
        let boot: Boot = Arc::new(|_| -> BoxFuture<'static, ()> { Box::pin(async {}) });
        let host = world.add_host(HostConfig::new(boot)).expect("a host");
        let runtime = world.runtime(host).expect("its runtime");
        body(runtime.rng().as_ref());
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass, "seed {seed}");
}

fn vni(n: u32) -> Vni {
    Vni::new(n).unwrap()
}

fn link(n: u8) -> Link {
    Link::new(
        StaticSecret::from_bytes([n; 32]),
        Config {
            queues: QueueCount::new(4).unwrap(),
            counter_start: HandshakeCounter(0),
        },
    )
}

/// Two links with a session between them, a the initiator.
struct Two<'r> {
    rng: &'r dyn Rng,
    now: Instant,
    links: [Link; 2],
    keys: [PublicKey; 2],
}

impl<'r> Two<'r> {
    fn new(rng: &'r dyn Rng) -> Self {
        let mut links = [link(1), link(2)];
        let keys = [links[A].public_key(), links[B].public_key()];
        links[A].add_peer(keys[B], PresharedKey::none());
        links[B].add_peer(keys[A], PresharedKey::none());
        let mut two = Self {
            rng,
            now: Instant::from_nanos(1),
            links,
            keys,
        };
        let mut out = Vec::new();
        let cx = two.cx();
        two.links[A].connect(cx, keys[B], &mut out).unwrap();
        two.pump(A, out).unwrap();
        two
    }

    fn cx(&self) -> Ctx<'r> {
        Ctx {
            now: self.now,
            rng: self.rng,
        }
    }

    /// The newest session of node `i`.
    fn id(&self, i: usize) -> u64 {
        *self.links[i].peers[&self.keys[1 - i]]
            .sessions
            .last()
            .unwrap()
    }

    fn session(&mut self, i: usize) -> &mut Session {
        let id = self.id(i);
        self.links[i].sessions.get_mut(&id).unwrap()
    }

    /// Delivers the datagrams in `out`, produced by node `from`, and every
    /// reply, until none is left. Returns the last drop, if any.
    fn pump(&mut self, from: usize, out: Vec<Output>) -> Result<(), DropReason> {
        let mut queue: VecDeque<(usize, Vec<u8>)> = VecDeque::new();
        let mut last = Ok(());
        let route = |from: usize, out: Vec<Output>, queue: &mut VecDeque<_>| {
            for o in out {
                if let Output::Transmit { datagram, .. } = o {
                    queue.push_back((1 - from, datagram));
                }
            }
        };
        route(from, out, &mut queue);
        while let Some((to, mut datagram)) = queue.pop_front() {
            let mut out = Vec::new();
            let cx = self.cx();
            last = self.links[to].receive(cx, &[], &mut datagram, &mut out);
            route(to, out, &mut queue);
        }
        last
    }

    /// Seals `frame` from a to b under a's newest session on `vni`, as a
    /// peer that breaks the rules would, and hands it to b.
    fn forge(&mut self, v: Vni, frame: &[u8]) -> Result<(), DropReason> {
        let id = self.id(A);
        let mut out = Vec::new();
        let now = self.now;
        self.links[A]
            .seal(now, (id, v, 0), frame, &mut out)
            .unwrap();
        self.pump(A, out)
    }
}

#[test]
fn inner_frame_drop_rules() {
    with_rng(31, |rng| {
        let mut t = Two::new(rng);
        // Bind VNI 7 at b and give a the binding, without a's request.
        t.links[B].add_local_vni(vni(7));
        let (b_id, cx) = (t.id(B), t.cx());
        let request = Control::BindRequest { vni: vni(7) };
        t.links[B]
            .on_control(cx, b_id, request, &mut Vec::new())
            .unwrap();
        let base = t.session(B).local[&vni(7)];
        t.session(A).bindings.insert(vni(7), base);

        let zero = Vni::INFRASTRUCTURE;
        let bad: [(Vni, &[u8]); 14] = [
            (zero, &[0x30]),
            (zero, &[0x80]),
            (zero, &[0x00, 0x00]),
            (zero, &[0x01]),
            (zero, &[0x10]),
            (zero, &[0x10, 3, 0, 0, 0]),
            (zero, &[0x10, 1, 7, 0]),
            (zero, &[0x10, 2, 7, 0, 0, 0, 0, 0]),
            (zero, &[0x20, 0, 0, 0, 0, 0, 2]),
            (zero, &[0x20, 0, 0, 0, 0, 0, 1, 9]),
            (zero, &[0x20, 0, 0, 0, 0, 2, 2, 9]),
            (zero, &[0x21, 0, 0, 0, 0, 0, 2, 9]),
            (vni(7), &[0x00]),
            (vni(7), &[0x10, 1, 7, 0, 0]),
        ];
        for (v, frame) in bad {
            assert_eq!(t.forge(v, frame), Err(DropReason::Malformed), "{frame:?}");
        }
        // A reassembled frame must be IP, or control on VNI 0.
        let split: [(Vni, &[u8], &[u8]); 3] = [
            // A keepalive, which is never split.
            (
                zero,
                &[0x20, 1, 0, 0, 0, 0, 2, 0x00],
                &[0x20, 1, 0, 0, 0, 1, 2, 0x00],
            ),
            // A fragment, which is never split again.
            (
                zero,
                &[0x20, 2, 0, 0, 0, 0, 2, 0x20, 0, 0, 0],
                &[0x20, 2, 0, 0, 0, 1, 2, 0, 0, 2, 9],
            ),
            // A control frame on a VNI other than 0.
            (
                vni(7),
                &[0x20, 3, 0, 0, 0, 0, 2, 0x10, 1],
                &[0x20, 3, 0, 0, 0, 1, 2, 7, 0, 0],
            ),
        ];
        for (v, first, second) in split {
            assert_eq!(t.forge(v, first), Ok(()));
            assert_eq!(t.forge(v, second), Err(DropReason::Malformed));
        }
        // A fragment whose count or index conflicts with one held.
        assert_eq!(t.forge(zero, &[0x20, 4, 0, 0, 0, 0, 2, 0x45]), Ok(()));
        assert_eq!(
            t.forge(zero, &[0x20, 4, 0, 0, 0, 1, 3, 0x45]),
            Err(DropReason::Fragment)
        );
        assert_eq!(
            t.forge(zero, &[0x20, 4, 0, 0, 0, 0, 2, 0x46]),
            Err(DropReason::Fragment)
        );
        // A control frame reassembled on VNI 0 is handled: a bind request
        // for VNI 7, answered with a bind.
        assert_eq!(t.forge(zero, &[0x20, 5, 0, 0, 0, 0, 2, 0x10, 1]), Ok(()));
        let before = t.session(B).send_counters[0];
        // a holds that binding already: the bind repeats it.
        assert_eq!(t.forge(zero, &[0x20, 5, 0, 0, 0, 1, 2, 7, 0, 0]), Ok(()));
        assert_eq!(
            t.session(B).send_counters[0],
            before + 1,
            "b sealed the bind"
        );
        assert_eq!(t.links[A].drops(DropReason::Bind), 0);
    });
}

#[test]
fn transport_counters_stay_below_2_31() {
    with_rng(32, |rng| {
        let mut t = Two::new(rng);
        let to = Outbound {
            peer: t.keys[B],
            vni: Vni::INFRASTRUCTURE,
            queue: 1,
            budget: 1_400,
        };
        let packet = [0x45; 40];
        t.session(A).send_counters[1] = COUNTER_LIMIT - 2;
        // A frame of two fragments needs counters 2^31 - 2 and 2^31 - 1. The
        // send also starts a rekey, left pending here.
        let fragmented = Outbound { budget: 30, ..to };
        let mut out = Vec::new();
        let cx = t.cx();
        assert_eq!(
            t.links[A].send(cx, fragmented, &packet, &mut out),
            Ok(Sent::Sealed)
        );
        let transport: Vec<Vec<u8>> = out
            .into_iter()
            .filter_map(|o| match o {
                Output::Transmit { datagram, .. } if datagram[0] == 4 => Some(datagram),
                _ => None,
            })
            .collect();
        assert_eq!(transport.len(), 2);
        let mut got = Vec::new();
        for mut d in transport {
            assert_eq!(t.links[B].receive(cx, &[], &mut d, &mut got), Ok(()));
        }
        assert!(matches!(&got[..], [Output::Deliver { packet: p, .. }] if p[..] == packet));
        let mut out = Vec::new();
        assert_eq!(
            t.links[A].send(cx, to, &packet, &mut out),
            Err(DropReason::CounterExhausted)
        );
        assert!(out.is_empty());
        t.session(A).send_counters[1] = COUNTER_LIMIT - 1;
        assert_eq!(
            t.links[A].send(cx, fragmented, &packet, &mut out),
            Err(DropReason::CounterExhausted)
        );
        assert_eq!(t.links[A].drops(DropReason::CounterExhausted), 2);
    });
}

#[test]
fn a_session_rekeys_at_2_30_packets_on_a_queue() {
    with_rng(33, |rng| {
        let mut t = Two::new(rng);
        let to = Outbound {
            peer: t.keys[B],
            vni: Vni::INFRASTRUCTURE,
            queue: 2,
            budget: 1_400,
        };
        let cx = t.cx();
        t.session(A).send_counters[2] = REKEY_AFTER_PACKETS - 1;
        t.links[A]
            .send(cx, to, &[0x45; 40], &mut Vec::new())
            .unwrap();
        assert!(!t.links[A].is_pending(&t.keys[B]));
        t.links[A]
            .send(cx, to, &[0x45; 40], &mut Vec::new())
            .unwrap();
        assert!(t.links[A].is_pending(&t.keys[B]));
    });
}

#[test]
fn handshake_counters_come_in_durable_blocks() {
    with_rng(34, |rng| {
        let mut a = link(1);
        let b = StaticSecret::from_bytes([2; 32]).public_key();
        a.add_peer(b, PresharedKey::none());
        let cx = Ctx {
            now: Instant::ZERO,
            rng,
        };
        a.counter_next = COUNTER_BLOCK / 2 - 1;
        let mut out = Vec::new();
        a.connect(cx, b, &mut out).unwrap();
        let needed = Output::Event(Event::CounterBlockNeeded {
            next_start: HandshakeCounter(COUNTER_BLOCK),
        });
        assert!(out.contains(&needed));

        a.pending.clear();
        a.counter_next = COUNTER_BLOCK;
        let mut out = Vec::new();
        assert_eq!(a.connect(cx, b, &mut out), Err(DropReason::NoCounter));
        assert_eq!(out, [needed]);
        a.add_counter_block();
        assert_eq!(a.connect(cx, b, &mut Vec::new()), Ok(()));
        assert_eq!(a.counter_next, COUNTER_BLOCK + 1);
    });
}

#[test]
fn a_session_binds_at_most_1024_blocks() {
    with_rng(35, |rng| {
        let mut t = Two::new(rng);
        let max = MAX_BLOCKS_PER_SESSION as u32;
        for n in 1..=max + 1 {
            t.links[B].add_local_vni(vni(n));
        }
        let (id, cx) = (t.id(B), t.cx());
        let request = |link: &mut Link, n: u32| {
            let control = Control::BindRequest { vni: vni(n) };
            link.on_control(cx, id, control, &mut Vec::new())
        };
        for n in 1..max {
            assert_eq!(request(&mut t.links[B], n), Ok(()));
        }
        assert_eq!(t.session(B).local.len(), MAX_BLOCKS_PER_SESSION);
        assert_eq!(request(&mut t.links[B], max), Err(DropReason::Bind));
        // A VNI already bound keeps its block.
        let base = t.session(B).local[&vni(5)];
        assert_eq!(request(&mut t.links[B], 5), Ok(()));
        assert_eq!(t.session(B).local[&vni(5)], base);
        // VNI 0, and a VNI without a port here.
        t.links[B].remove_local_vni(vni(9));
        let control = Control::BindRequest {
            vni: Vni::INFRASTRUCTURE,
        };
        assert_eq!(
            t.links[B].on_control(cx, id, control, &mut Vec::new()),
            Err(DropReason::Bind)
        );
        assert_eq!(request(&mut t.links[B], 9), Err(DropReason::Bind));
    });
}

#[test]
fn a_session_that_no_longer_qualifies_binds_nothing() {
    with_rng(37, |rng| {
        let mut t = Two::new(rng);
        t.links[B].add_local_vni(vni(5));
        let old = t.id(B);
        let cx = t.cx();
        let mut out = Vec::new();
        t.links[A].connect(cx, t.keys[B], &mut out).unwrap();
        t.pump(A, out).unwrap();
        let newer = t.id(B);
        assert_ne!(old, newer);
        let sealed = t.now;
        t.session(B).first_sealed = Some(sealed);
        let request = |t: &mut Two<'_>, after: Duration| {
            let cx = Ctx {
                now: sealed.saturating_add(after),
                rng: t.rng,
            };
            let mut out = Vec::new();
            let control = Control::BindRequest { vni: vni(5) };
            let got = t.links[B].on_control(cx, old, control, &mut out);
            (got, out.len())
        };
        // Section 3.4: 5 s after b first sealed under the newer session, the
        // older one no longer qualifies: the request is dropped, nothing is
        // bound and no bind is sealed.
        assert_eq!(request(&mut t, QUALIFY_TIME), (Err(DropReason::Bind), 0));
        assert!(!t.links[B].sessions[&old].local.contains_key(&vni(5)));
        assert_eq!(
            request(&mut t, QUALIFY_TIME - Duration::from_millis(1)),
            (Ok(()), 1)
        );
        assert!(t.links[B].sessions[&old].local.contains_key(&vni(5)));
        // The newer session still qualifies and binds.
        let cx = Ctx {
            now: sealed.saturating_add(QUALIFY_TIME),
            rng: t.rng,
        };
        let control = Control::BindRequest { vni: vni(5) };
        assert_eq!(
            t.links[B].on_control(cx, newer, control, &mut Vec::new()),
            Ok(())
        );
    });
}

#[test]
fn a_sender_keeps_only_binds_it_asked_for() {
    with_rng(36, |rng| {
        let mut t = Two::new(rng);
        let (id, cx) = (t.id(A), t.cx());
        let bind = |n: u32, base: u32| Control::Bind {
            vni: Vni::new(n).unwrap(),
            base: SessionIndex(base),
        };
        let handle = |link: &mut Link, control| link.on_control(cx, id, control, &mut Vec::new());
        let now = t.now;
        assert_eq!(handle(&mut t.links[A], bind(5, 640)), Err(DropReason::Bind));
        t.session(A).bind_sent.insert(vni(5), now);
        assert_eq!(handle(&mut t.links[A], bind(5, 640)), Ok(()));
        assert_eq!(handle(&mut t.links[A], bind(5, 640)), Ok(()), "a repeat");
        assert_eq!(handle(&mut t.links[A], bind(5, 704)), Err(DropReason::Bind));
        assert_eq!(handle(&mut t.links[A], bind(0, 0)), Err(DropReason::Bind));
        // 1,023 bindings for VNIs other than 0 is the most a sender holds.
        let session = t.session(A);
        for n in 6..=MAX_BINDINGS_PER_SESSION as u32 + 4 {
            session.bindings.insert(vni(n), n << 6);
        }
        session.bind_sent.insert(vni(4_000), now);
        assert_eq!(t.session(A).bindings.len(), MAX_BINDINGS_PER_SESSION + 1);
        assert_eq!(
            handle(&mut t.links[A], bind(4_000, 0)),
            Err(DropReason::Bind)
        );
        t.session(A).bindings.remove(&vni(6));
        assert_eq!(handle(&mut t.links[A], bind(4_000, 0)), Ok(()));
    });
}

#[test]
fn cookie_secrets_live_120_seconds_then_120_more_as_previous() {
    let mut a = link(1);
    let t0 = Instant::from_nanos(5);
    let at = |s: u64| t0.saturating_add(Duration::from_secs(s));
    a.cookie_current = Some((CookieSecret::from_bytes([3; 32]), t0));
    a.retire_cookie_secrets(at(119));
    assert!(a.cookie_current.is_some() && a.cookie_previous.is_none());
    a.retire_cookie_secrets(at(120));
    assert!(a.cookie_current.is_none());
    assert_eq!(
        a.cookie_previous.as_ref().map(|(_, until)| *until),
        Some(at(240))
    );
    a.retire_cookie_secrets(at(239));
    assert!(a.cookie_previous.is_some());
    a.retire_cookie_secrets(at(240));
    assert!(a.cookie_previous.is_none());
    // Retired late, a secret is never accepted past its 240 seconds.
    a.cookie_current = Some((CookieSecret::from_bytes([4; 32]), t0));
    a.retire_cookie_secrets(at(250));
    assert!(a.cookie_current.is_none() && a.cookie_previous.is_none());
}

#[test]
fn the_next_deadline_is_the_earliest_timer() {
    with_rng(37, |rng| {
        let mut t = Two::new(rng);
        let made = t.now;
        assert_eq!(
            t.links[A].next_deadline(),
            Some(made.saturating_add(REJECT_AFTER))
        );
        t.now = t.now.saturating_add(Duration::from_secs(3));
        let (cx, peer) = (t.cx(), t.keys[B]);
        t.links[A].connect(cx, peer, &mut Vec::new()).unwrap();
        assert_eq!(
            t.links[A].next_deadline(),
            Some(t.now.saturating_add(RETRY_INTERVAL))
        );
    });
}

/// The replay entries of keys never added as peers are bounded: the least
/// recently accepted goes first, a key accepted again moves to the back, and
/// a key admitted with `add_peer` is kept for good.
#[test]
fn unknown_replay_entries_are_bounded() {
    let mut a = link(1);
    let key = |n: u32| {
        let mut bytes = [0; 32];
        bytes[..4].copy_from_slice(&n.to_le_bytes());
        PublicKey::from_bytes(bytes)
    };
    let accept = |a: &mut Link, n: u32| {
        a.highest.insert(key(n), u64::from(n));
        a.note_transient(key(n));
    };
    let cap = UNKNOWN_REPLAY_ENTRIES as u32;
    for n in 0..cap {
        accept(&mut a, n);
    }
    a.add_peer(key(1), PresharedKey::none());
    accept(&mut a, 0);
    accept(&mut a, cap);
    assert_eq!(a.transient.len(), UNKNOWN_REPLAY_ENTRIES);
    assert!(a.highest.contains_key(&key(2)));
    accept(&mut a, cap + 1);
    assert_eq!(a.transient.len(), UNKNOWN_REPLAY_ENTRIES);
    assert_eq!(a.transient.len(), a.transient_order.len());
    // Key 2 was the least recent: 0 moved to the back, 1 was admitted.
    assert!(!a.highest.contains_key(&key(2)));
    assert!(a.highest.contains_key(&key(0)));
    assert!(a.highest.contains_key(&key(1)));
    assert!(a.highest.contains_key(&key(cap + 1)));
    for n in cap + 2..cap + 10 {
        accept(&mut a, n);
    }
    assert_eq!(a.transient.len(), UNKNOWN_REPLAY_ENTRIES);
    assert!(!a.highest.contains_key(&key(10)));
    assert!(a.highest.contains_key(&key(11)));
    assert!(
        a.highest.contains_key(&key(1)),
        "an admitted key is never evicted"
    );
    assert!(a.highest.contains_key(&key(0)));
}

/// A key that was a peer, was removed, and then had an unknown initiation
/// accepted keeps its replay entry through a flood of unknown keys past
/// [`UNKNOWN_REPLAY_ENTRIES`]: initiations accepted from it stay replays.
#[test]
fn a_key_once_a_peer_keeps_its_replay_entry() {
    with_rng(7, |rng| {
        let mut a = link(1);
        let mut k = link(3);
        let (ak, kk) = (a.public_key(), k.public_key());
        k.add_peer(ak, PresharedKey::none());
        a.add_peer(kk, PresharedKey::none());
        a.set_accept_unknown(true);
        let cx = Ctx {
            now: Instant::from_nanos(1),
            rng,
        };
        let initiation = |k: &mut Link| {
            let mut out = Vec::new();
            k.remove_peer(&ak, &mut out);
            k.add_peer(ak, PresharedKey::none());
            k.connect(cx, ak, &mut out).unwrap();
            out.into_iter()
                .find_map(|o| match o {
                    Output::Transmit { datagram, .. } => Some(datagram),
                    _ => None,
                })
                .unwrap()
        };
        let offer = |a: &mut Link, mut datagram: Vec<u8>| {
            a.receive(cx, &[], &mut datagram, &mut Vec::new())
        };
        let first = initiation(&mut k);
        assert_eq!(offer(&mut a, first.clone()), Ok(()));
        a.remove_peer(&kk, &mut Vec::new());
        let second = initiation(&mut k);
        assert_eq!(offer(&mut a, second.clone()), Ok(()));
        assert!(a.is_unknown(&kk));

        // Accepted unknown initiations from fresh keys, as `on_initiation`
        // records them, until every older transient entry is evicted.
        for n in 0..=UNKNOWN_REPLAY_ENTRIES as u32 {
            let mut bytes = [0xee; 32];
            bytes[..4].copy_from_slice(&n.to_le_bytes());
            let fresh = PublicKey::from_bytes(bytes);
            a.highest.insert(fresh, u64::from(n));
            a.note_transient(fresh);
        }
        assert_eq!(a.transient.len(), UNKNOWN_REPLAY_ENTRIES);
        assert_eq!(offer(&mut a, first), Err(DropReason::Replay));
        assert_eq!(offer(&mut a, second), Err(DropReason::Replay));
    });
}
