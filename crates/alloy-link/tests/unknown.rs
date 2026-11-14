//! The accept-unknown mode: an anchor takes handshakes from initiators it
//! does not know yet, within the rate limits of ARFC 0002 section 3.3 step
//! 4, and reports them to the caller to admit or drop.

mod common;

use std::time::Duration;

use alloy_crypto::al::{PresharedKey, PublicKey, StaticSecret};
use alloy_link::{
    Ctx, DropReason, Event, Link, Outbound, Output, REJECT_AFTER, UNKNOWN_PER_SOURCE,
    UNKNOWN_TOTAL, UNKNOWN_WINDOW,
};
use alloy_rt::{Instant, Rng};
use alloy_wire::Vni;
use common::{BUDGET, SplitMix, config, ip, secret, with_rng};

/// The anchor's static key.
const ANCHOR: u8 = 2;

/// An IPv4 cookie `src`: 10.0.`net`.`host`, port `port`.
fn v4(net: u8, host: u8, port: u16) -> Vec<u8> {
    let mut src = vec![10, 0, net, host];
    src.extend_from_slice(&port.to_be_bytes());
    src
}

/// An IPv6 cookie `src`: fd00::`host`, port `port`.
fn v6(host: u8, port: u16) -> Vec<u8> {
    let mut src = vec![0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, host];
    src.extend_from_slice(&port.to_be_bytes());
    src
}

/// An anchor that accepts unknown initiators, with no peers.
fn anchor() -> Link {
    let mut link = Link::new(secret(ANCHOR), config());
    link.set_accept_unknown(true);
    link
}

/// Joiner number `n`: its own static key, and the anchor as its one peer
/// with no pre-shared key, as a joiner holds it from a token.
fn joiner(n: u32) -> Link {
    let mut bytes = [0x5a; 32];
    bytes[4..8].copy_from_slice(&n.to_le_bytes());
    let mut link = Link::new(StaticSecret::from_bytes(bytes), config());
    link.add_peer(secret(ANCHOR).public_key(), PresharedKey::none());
    link
}

fn cx(rng: &dyn Rng, now: Instant) -> Ctx<'_> {
    Ctx { now, rng }
}

fn datagrams(out: Vec<Output>) -> Vec<Vec<u8>> {
    out.into_iter()
        .filter_map(|o| match o {
            Output::Transmit { datagram, .. } => Some(datagram),
            _ => None,
        })
        .collect()
}

fn events(out: &[Output]) -> Vec<Event> {
    out.iter()
        .filter_map(|o| match o {
            Output::Event(e) => Some(*e),
            _ => None,
        })
        .collect()
}

/// The initiation `j` sends to the anchor now.
fn initiation(rng: &dyn Rng, now: Instant, j: &mut Link) -> Vec<u8> {
    let mut out = Vec::new();
    let anchor = secret(ANCHOR).public_key();
    j.connect(cx(rng, now), anchor, &mut out).unwrap();
    datagrams(out).remove(0)
}

/// Hands `datagram` from `src` to `link`.
fn offer(
    rng: &dyn Rng,
    now: Instant,
    link: &mut Link,
    src: &[u8],
    mut datagram: Vec<u8>,
) -> (Result<(), DropReason>, Vec<Output>) {
    let mut out = Vec::new();
    let result = link.receive(cx(rng, now), src, &mut datagram, &mut out);
    (result, out)
}

/// A fresh joiner `n` initiates from `src`; the anchor's verdict.
fn try_join(
    rng: &dyn Rng,
    now: Instant,
    anchor: &mut Link,
    n: u32,
    src: &[u8],
) -> Result<(), DropReason> {
    let mut j = joiner(n);
    let init = initiation(rng, now, &mut j);
    offer(rng, now, anchor, src, init).0
}

const T0: Instant = Instant::from_nanos(1_000_000_000);

fn at(base: Instant, by: Duration) -> Instant {
    base.saturating_add(by)
}

#[test]
fn unknown_initiator_dropped_when_mode_is_off() {
    with_rng(1, |rng| {
        let mut a = Link::new(secret(ANCHOR), config());
        assert_eq!(
            try_join(rng, T0, &mut a, 1, &v4(0, 1, 9)),
            Err(DropReason::UnknownPeer)
        );
        assert_eq!(a.drops(DropReason::UnknownPeer), 1);

        // Turned off again, later unknown initiations are dropped.
        a.set_accept_unknown(true);
        assert_eq!(try_join(rng, T0, &mut a, 2, &v4(0, 1, 9)), Ok(()));
        a.set_accept_unknown(false);
        assert_eq!(
            try_join(rng, T0, &mut a, 3, &v4(0, 1, 9)),
            Err(DropReason::UnknownPeer)
        );
    });
}

#[test]
fn unknown_initiator_gets_a_session_and_is_reported() {
    with_rng(2, |rng| {
        let mut a = anchor();
        let mut j = joiner(7);
        let jk = j.public_key();
        let ak = a.public_key();
        let src = v4(0, 7, 4000);

        let init = initiation(rng, T0, &mut j);
        let (result, out) = offer(rng, T0, &mut a, &src, init);
        assert_eq!(result, Ok(()));
        assert_eq!(
            events(&out),
            vec![
                Event::SessionMade {
                    peer: jk,
                    initiator: false
                },
                Event::UnknownInitiator { peer: jk },
            ]
        );
        assert!(a.is_unknown(&jk));
        assert!(!a.is_admitted(&jk));

        // The handshake is otherwise unchanged: the joiner completes it and
        // its packets reach the anchor.
        let response = datagrams(out).remove(0);
        let (result, out) = offer(rng, T0, &mut j, &[], response);
        assert_eq!(result, Ok(()));
        for d in datagrams(out) {
            assert_eq!(offer(rng, T0, &mut a, &src, d).0, Ok(()));
        }
        let to_anchor = Outbound {
            peer: ak,
            vni: Vni::INFRASTRUCTURE,
            queue: 0,
            budget: BUDGET,
        };
        let mut out = Vec::new();
        j.send(cx(rng, T0), to_anchor, &ip(60, 3), &mut out)
            .unwrap();
        let (result, out) = offer(rng, T0, &mut a, &src, datagrams(out).remove(0));
        assert_eq!(result, Ok(()));
        assert!(out.iter().any(|o| matches!(
            o,
            Output::Deliver { peer, packet, .. } if *peer == jk && *packet == ip(60, 3)
        )));

        // The caller admits it: it becomes a peer like any other and keeps
        // its session.
        a.add_peer(jk, PresharedKey::none());
        assert!(a.is_admitted(&jk));
        assert!(!a.is_unknown(&jk));
        assert_eq!(a.session_count(&jk), 1);
        let to_joiner = Outbound {
            peer: jk,
            ..to_anchor
        };
        let mut out = Vec::new();
        a.send(cx(rng, T0), to_joiner, &ip(60, 4), &mut out)
            .unwrap();
        let (result, out) = offer(rng, T0, &mut j, &[], datagrams(out).remove(0));
        assert_eq!(result, Ok(()));
        assert!(out.iter().any(|o| matches!(o, Output::Deliver { .. })));

        // An admitted peer is no longer counted against the limits: its
        // later initiations pass even with the source's budget spent, one
        // of it by its own first initiation.
        for n in 0..(UNKNOWN_PER_SOURCE - 1) as u32 {
            assert_eq!(try_join(rng, T0, &mut a, 100 + n, &src), Ok(()));
        }
        assert_eq!(
            try_join(rng, T0, &mut a, 200, &src),
            Err(DropReason::RateLimited)
        );
        let later = at(T0, Duration::from_millis(10));
        let mut out = Vec::new();
        j.connect(cx(rng, later), ak, &mut out).unwrap();
        let rekey = datagrams(out).remove(0);
        let (result, out) = offer(rng, later, &mut a, &src, rekey);
        assert_eq!(result, Ok(()));
        assert!(!events(&out).contains(&Event::UnknownInitiator { peer: jk }));
    });
}

#[test]
fn dropped_unknown_initiator_is_forgotten_and_its_replays_stay_replays() {
    with_rng(3, |rng| {
        let mut a = anchor();
        let mut j = joiner(8);
        let jk = j.public_key();
        let src = v4(0, 8, 4000);
        let init = initiation(rng, T0, &mut j);
        assert_eq!(offer(rng, T0, &mut a, &src, init.clone()).0, Ok(()));

        // A replay of an accepted initiation is dropped, and does not spend
        // the source's budget.
        assert_eq!(
            offer(rng, T0, &mut a, &src, init.clone()).0,
            Err(DropReason::Replay)
        );
        // One accepted, one replay: nine more fit in the window.
        for n in 0..(UNKNOWN_PER_SOURCE - 1) as u32 {
            assert_eq!(try_join(rng, T0, &mut a, 300 + n, &src), Ok(()));
        }
        assert_eq!(
            try_join(rng, T0, &mut a, 400, &src),
            Err(DropReason::RateLimited)
        );

        let mut out = Vec::new();
        a.remove_peer(&jk, &mut out);
        assert!(events(&out).contains(&Event::SessionDiscarded { peer: jk }));
        assert!(!a.is_unknown(&jk));
        assert_eq!(a.session_count(&jk), 0);
        // With the budget back, the replay is still a replay.
        let later = at(T0, UNKNOWN_WINDOW);
        assert_eq!(
            offer(rng, later, &mut a, &src, init).0,
            Err(DropReason::Replay)
        );
        assert!(!a.is_unknown(&jk));
    });
}

#[test]
fn unknown_initiator_without_a_session_is_forgotten() {
    with_rng(4, |rng| {
        let mut a = anchor();
        let mut j = joiner(9);
        let jk = j.public_key();
        let init = initiation(rng, T0, &mut j);
        assert_eq!(offer(rng, T0, &mut a, &v4(0, 9, 1), init).0, Ok(()));
        assert!(a.is_unknown(&jk));

        let mut out = Vec::new();
        a.tick(cx(rng, at(T0, REJECT_AFTER)), &mut out);
        assert!(events(&out).contains(&Event::SessionDiscarded { peer: jk }));
        assert!(!a.is_unknown(&jk));

        // Packets to it are dropped as to any key that is not a peer.
        let to = Outbound {
            peer: jk,
            vni: Vni::INFRASTRUCTURE,
            queue: 0,
            budget: BUDGET,
        };
        let mut out = Vec::new();
        let now = at(T0, REJECT_AFTER);
        assert_eq!(
            a.send(cx(rng, now), to, &ip(40, 1), &mut out),
            Err(DropReason::UnknownPeer)
        );
    });
}

#[test]
fn per_source_limit() {
    with_rng(5, |rng| {
        let mut a = anchor();
        // Ten from one address, each from another port, are accepted.
        for n in 0..UNKNOWN_PER_SOURCE as u32 {
            let src = v4(1, 1, 1000 + n as u16);
            assert_eq!(try_join(rng, T0, &mut a, n, &src), Ok(()), "initiation {n}");
        }
        // The eleventh in the same second is not, whatever its port.
        assert_eq!(
            try_join(rng, T0, &mut a, 50, &v4(1, 1, 9)),
            Err(DropReason::RateLimited)
        );
        let edge = at(T0, UNKNOWN_WINDOW - Duration::from_nanos(1));
        assert_eq!(
            try_join(rng, edge, &mut a, 51, &v4(1, 1, 9)),
            Err(DropReason::RateLimited)
        );
        assert_eq!(a.drops(DropReason::RateLimited), 2);
        // Another address is not limited by it, an IPv6 one included.
        assert_eq!(try_join(rng, edge, &mut a, 52, &v4(1, 2, 9)), Ok(()));
        assert_eq!(try_join(rng, edge, &mut a, 53, &v6(1, 9)), Ok(()));
        // A second after the first ten, the address has its budget again.
        let next = at(T0, UNKNOWN_WINDOW);
        for n in 0..UNKNOWN_PER_SOURCE as u32 {
            assert_eq!(try_join(rng, next, &mut a, 60 + n, &v4(1, 1, 2)), Ok(()));
        }
        assert_eq!(
            try_join(rng, next, &mut a, 80, &v4(1, 1, 2)),
            Err(DropReason::RateLimited)
        );
    });
}

#[test]
fn total_limit() {
    with_rng(6, |rng| {
        let mut a = anchor();
        let sources = UNKNOWN_TOTAL / UNKNOWN_PER_SOURCE + 1;
        let mut n = 0u32;
        let mut accepted = 0;
        for s in 0..sources {
            for _ in 0..UNKNOWN_PER_SOURCE {
                let src = v4(2, s as u8, 7);
                let result = try_join(rng, T0, &mut a, n, &src);
                n += 1;
                if accepted < UNKNOWN_TOTAL {
                    assert_eq!(result, Ok(()), "initiation {n}");
                    accepted += 1;
                } else {
                    assert_eq!(result, Err(DropReason::RateLimited), "initiation {n}");
                }
            }
        }
        assert_eq!(a.drops(DropReason::RateLimited), UNKNOWN_PER_SOURCE as u64);
        // A fresh source is refused too while the window is full.
        let edge = at(T0, UNKNOWN_WINDOW - Duration::from_nanos(1));
        assert_eq!(
            try_join(rng, edge, &mut a, n, &v6(2, 7)),
            Err(DropReason::RateLimited)
        );
        // Known peers are not limited.
        let mut peer = joiner(n + 1);
        a.add_peer(peer.public_key(), PresharedKey::none());
        let init = initiation(rng, edge, &mut peer);
        assert_eq!(offer(rng, edge, &mut a, &v4(2, 0, 7), init).0, Ok(()));
        // The window moves on.
        let next = at(T0, UNKNOWN_WINDOW);
        assert_eq!(try_join(rng, next, &mut a, n + 2, &v6(2, 7)), Ok(()));
    });
}

/// The limits as a model: the times and sources of accepted initiations.
struct Model(Vec<(Instant, Vec<u8>)>);

/// What the model does with one initiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Accepted,
    SourceFull,
    TotalFull,
}

impl Model {
    fn accept(&mut self, now: Instant, source: &[u8]) -> Verdict {
        self.0.retain(|(t, _)| now < at(*t, UNKNOWN_WINDOW));
        if self.0.len() >= UNKNOWN_TOTAL {
            return Verdict::TotalFull;
        }
        if self.0.iter().filter(|(_, s)| s == source).count() >= UNKNOWN_PER_SOURCE {
            return Verdict::SourceFull;
        }
        self.0.push((now, source.to_vec()));
        Verdict::Accepted
    }
}

/// Bursts of initiations from a few busy addresses and many quiet ones,
/// IPv4 and IPv6, on random ports: the anchor accepts exactly what a
/// sliding-window model accepts, and both limits are reached.
#[test]
fn limits_match_a_model() {
    for seed in 0..8u64 {
        with_rng(100 + seed, move |rng| {
            let mut pick = SplitMix(seed);
            let mut a = anchor();
            let mut model = Model(Vec::new());
            let mut now = T0;
            let mut seen = [0usize; 3];
            for n in 0..600u32 {
                if !pick.chance(95) {
                    now = at(now, Duration::from_millis(pick.below(400)));
                }
                let host = if pick.chance(40) {
                    pick.below(3)
                } else {
                    pick.below(60)
                } as u8;
                let port = pick.below(65_536) as u16;
                let (src, address) = if pick.chance(30) {
                    let src = v6(host, port);
                    let address = src[..16].to_vec();
                    (src, address)
                } else {
                    let src = v4(3, host, port);
                    let address = src[..4].to_vec();
                    (src, address)
                };
                let verdict = model.accept(now, &address);
                let got = try_join(rng, now, &mut a, n, &src);
                let want = match verdict {
                    Verdict::Accepted => Ok(()),
                    Verdict::SourceFull | Verdict::TotalFull => Err(DropReason::RateLimited),
                };
                assert_eq!(got, want, "seed {seed}, initiation {n}, {verdict:?}");
                seen[verdict as usize] += 1;
            }
            assert!(seen.iter().all(|&s| s > 0), "seed {seed}: {seen:?}");
        });
    }
}

/// A key the anchor knows only as an unknown initiator is counted on its
/// rekeys too, and reported for each session.
#[test]
fn rekeys_of_an_unknown_initiator_count() {
    with_rng(7, |rng| {
        let mut a = anchor();
        let mut j = joiner(10);
        let jk: PublicKey = j.public_key();
        let ak = a.public_key();
        let src = v4(4, 1, 1);
        let mut now = T0;
        for round in 0..UNKNOWN_PER_SOURCE {
            let mut out = Vec::new();
            j.connect(cx(rng, now), ak, &mut out).unwrap();
            let (result, got) = offer(rng, now, &mut a, &src, datagrams(out).remove(0));
            assert_eq!(result, Ok(()), "round {round}");
            assert!(events(&got).contains(&Event::UnknownInitiator { peer: jk }));
            let (result, _) = offer(rng, now, &mut j, &[], datagrams(got).remove(0));
            assert_eq!(result, Ok(()));
            now = at(now, Duration::from_millis(1));
        }
        let mut out = Vec::new();
        j.connect(cx(rng, now), ak, &mut out).unwrap();
        let (result, _) = offer(rng, now, &mut a, &src, datagrams(out).remove(0));
        assert_eq!(result, Err(DropReason::RateLimited));
        assert!(a.is_unknown(&jk));
    });
}
