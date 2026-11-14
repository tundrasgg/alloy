//! The port's counts as a plain state machine, and the gate on the
//! simulator's clock, so that every time bound is checked without a
//! scheduling margin.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use alloy_rt::{BoxFuture, Runtime};
use alloy_sim::{Boot, Failure, HostConfig, RunConfig, RunResult, run};
use proptest::prelude::*;

use super::*;
use crate::server::first;

fn limits() -> ConnLimits {
    ConnLimits {
        unproven: 4,
        unproven_per_source: 2,
        api: 2,
        api_per_principal: 1,
        stream: 1,
        ..ConnLimits::API_PORT
    }
}

fn ip(addr: impl Into<IpAddr>) -> Source {
    Source::of(Some(StreamAddr::Ip(SocketAddr::new(addr.into(), 443))))
}

fn v4(a: u8, b: u8, c: u8, d: u8) -> Source {
    ip(Ipv4Addr::new(a, b, c, d))
}

fn alice() -> Principal {
    Principal::Authenticated("alice".to_owned())
}

fn bob() -> Principal {
    Principal::Authenticated("bob".to_owned())
}

#[test]
fn a_source_is_an_ipv4_address_or_an_ipv6_64_and_its_network_a_24_or_48() {
    let a = v4(192, 0, 2, 1);
    assert_ne!(a, v4(192, 0, 2, 2));
    assert_eq!(a, ip(Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped()));
    assert_eq!(a.network(), v4(192, 0, 2, 200).network());
    assert_ne!(a.network(), v4(192, 0, 3, 1).network());
    let v6 =
        |net: u16, sub: u16, last: u16| ip(Ipv6Addr::new(0x2001, 0xdb8, net, sub, 0, 0, 0, last));
    assert_eq!(v6(0, 1, 1), v6(0, 1, 2));
    assert_ne!(v6(0, 1, 1), v6(0, 2, 1));
    assert_eq!(v6(0, 1, 1).network(), v6(0, 2, 1).network());
    assert_ne!(v6(0, 1, 1).network(), v6(1, 1, 1).network());
    assert_eq!(Source::of(None), Source::Other);
    assert_eq!(Source::Other.network(), Network::Other);
}

#[test]
fn a_source_holds_at_most_its_share_of_unproven_places() {
    let pools = Pools::new(limits());
    let a = v4(192, 0, 2, 1);
    let a1 = pools.admit(a).expect("a1");
    let _a2 = pools.admit(a).expect("a2");
    assert!(pools.admit(a).is_none(), "the share of a is 2");
    drop(a1);
    let _a3 = pools.admit(a).expect("a share comes back on drop");
    assert_eq!(pools.open().unproven, 2);
}

#[test]
fn when_full_the_heaviest_network_gives_up_its_oldest_place() {
    let pools = Pools::new(limits());
    // Network 198.51.100.0/24 holds three places, from two sources.
    let x1 = pools.admit(v4(198, 51, 100, 1)).expect("x1");
    let y1 = pools.admit(v4(198, 51, 100, 2)).expect("y1");
    let x2 = pools.admit(v4(198, 51, 100, 1)).expect("x2");
    let friend = pools.admit(v4(192, 0, 2, 7)).expect("friend");
    assert_eq!(pools.open().unproven, 4);

    // A newcomer from a third network takes the oldest place of the
    // heaviest source, 198.51.100.1, in the heaviest network.
    let newcomer = pools.admit(v4(203, 0, 113, 9)).expect("newcomer");
    assert!(x1.evicted().fired());
    assert!(!y1.evicted().fired() && !x2.evicted().fired() && !friend.evicted().fired());
    assert_eq!(pools.open().unproven, 4);
    assert_eq!(pools.open().networks, 3);

    // The evicted place counts nowhere, and cannot be proven.
    let mut x1 = x1;
    assert_eq!(x1.promote(Proof::Api(alice())), Err(Refused::Gone));
    drop(x1);
    assert_eq!(pools.open().unproven, 4);

    // A newcomer from the heaviest network takes the place of its own
    // source's oldest connection, the newcomer counted.
    let again = pools.admit(v4(198, 51, 100, 2)).expect("again");
    assert!(y1.evicted().fired());
    assert!(!x2.evicted().fired() && !friend.evicted().fired() && !newcomer.evicted().fired());
    drop((y1, x2, friend, newcomer, again));
    assert_eq!(pools.open(), Open::default(), "no entry is left");
}

#[test]
fn ties_go_to_the_oldest_connection_never_the_newcomer() {
    let pools = Pools::new(limits());
    let places: Vec<Place> = (1..=4)
        .map(|n| pools.admit(v4(198, 51, n, 1)).expect("one per network"))
        .collect();
    let newcomer = pools.admit(v4(192, 0, 2, 7)).expect("newcomer");
    assert!(places[0].evicted().fired());
    assert!(places[1..].iter().all(|p| !p.evicted().fired()));
    assert!(!newcomer.evicted().fired());
}

#[test]
fn proving_moves_a_place_within_each_bound() {
    let pools = Pools::new(limits());
    let mut a = pools.admit(v4(192, 0, 2, 1)).expect("a");
    let mut b = pools.admit(v4(192, 0, 2, 2)).expect("b");
    let mut c = pools.admit(v4(192, 0, 2, 3)).expect("c");
    let mut d = pools.admit(v4(192, 0, 2, 4)).expect("d");
    assert_eq!(a.promote(Proof::Api(alice())), Ok(()));
    assert_eq!(
        a.promote(Proof::Stream),
        Err(Refused::Gone),
        "only unproven moves"
    );
    assert_eq!(
        b.promote(Proof::Api(alice())),
        Err(Refused::Full),
        "alice's share is 1"
    );
    assert_eq!(b.promote(Proof::Api(bob())), Ok(()));
    assert_eq!(
        c.promote(Proof::Api(Principal::Authenticated("carol".to_owned()))),
        Err(Refused::Full),
        "the API count is 2"
    );
    assert_eq!(c.promote(Proof::Stream), Ok(()));
    assert_eq!(d.promote(Proof::Stream), Err(Refused::Full));
    let open = pools.open();
    assert_eq!(
        (open.unproven, open.api, open.principals, open.stream),
        (1, 2, 2, 1)
    );
    drop((a, b, c, d));
    assert_eq!(pools.open(), Open::default());
}

/// One step of the model test.
#[derive(Clone, Debug)]
enum Step {
    /// Admit a connection from source `0..8`, in network `source / 3`.
    Admit(u8),
    /// Drop the place at this index of the live places, if any.
    Drop(usize),
    /// Prove the place at this index as principal `0..3`, or as `stream`
    /// for 3.
    Prove(usize, u8),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (0u8..8).prop_map(Step::Admit),
        1 => (0usize..16).prop_map(Step::Drop),
        1 => ((0usize..16), (0u8..4)).prop_map(|(i, p)| Step::Prove(i, p)),
    ]
}

fn source(n: u8) -> Source {
    v4(198, 51, n / 3, n)
}

proptest! {
    /// Against a model: every count stays within its bound, and a place is
    /// given up only by the heaviest network, its heaviest source, and that
    /// source's oldest connection, the newcomer counted.
    #[test]
    fn the_counts_follow_the_model(steps in prop::collection::vec(step(), 1..120)) {
        let limits = ConnLimits {
            unproven: 5,
            unproven_per_source: 2,
            api: 3,
            api_per_principal: 2,
            stream: 2,
            ..ConnLimits::API_PORT
        };
        let pools = Pools::new(limits);
        // Live places with their source, `None` once proven, and whether
        // they were evicted.
        let mut live: Vec<(Place, Option<Source>)> = Vec::new();
        for step in steps {
            match step {
                Step::Admit(n) => {
                    let s = source(n);
                    let unproven: Vec<Source> = live
                        .iter()
                        .filter(|(p, s)| s.is_some() && !p.evicted().fired())
                        .filter_map(|(_, s)| *s)
                        .collect();
                    let of_source = unproven.iter().filter(|&&x| x == s).count();
                    let admitted = pools.admit(s);
                    if of_source >= limits.unproven_per_source {
                        prop_assert!(admitted.is_none());
                        continue;
                    }
                    let mut all = unproven.clone();
                    all.push(s);
                    let net = |x: Source| all.iter().filter(|y| y.network() == x.network()).count();
                    let src = |x: Source| all.iter().filter(|&&y| y == x).count();
                    let most_net = all.iter().map(|&x| net(x)).max().unwrap_or(0);
                    let newly_evicted: Vec<Source> = live
                        .iter()
                        .filter(|(p, s)| s.is_some() && p.evicted().fired())
                        .filter_map(|(_, s)| *s)
                        .collect();
                    // Places evicted before this step were removed below, so
                    // any fired now fired in this step.
                    match admitted {
                        Some(place) => {
                            if unproven.len() < limits.unproven {
                                prop_assert!(newly_evicted.is_empty());
                            } else {
                                prop_assert_eq!(newly_evicted.len(), 1);
                                let victim = newly_evicted[0];
                                prop_assert_eq!(net(victim), most_net);
                                let in_net = all.iter().filter(|y| y.network() == victim.network());
                                let most_src = in_net.map(|&y| src(y)).max().unwrap_or(0);
                                prop_assert_eq!(src(victim), most_src);
                            }
                            live.push((place, Some(s)));
                        }
                        None => prop_assert!(false, "a newcomer under its share is placed"),
                    }
                    live.retain(|(p, s)| s.is_none() || !p.evicted().fired());
                }
                Step::Drop(i) => {
                    if i < live.len() {
                        live.remove(i);
                    }
                }
                Step::Prove(i, p) => {
                    let Some((place, held)) = live.get_mut(i) else { continue };
                    let proof = if p == 3 {
                        Proof::Stream
                    } else {
                        Proof::Api(Principal::Authenticated(format!("p{p}")))
                    };
                    if place.promote(proof).is_ok() {
                        *held = None;
                    }
                }
            }
            let open = pools.open();
            let unproven = live.iter().filter(|(_, s)| s.is_some()).count();
            prop_assert_eq!(open.unproven, unproven);
            prop_assert!(open.unproven <= limits.unproven);
            prop_assert!(open.api <= limits.api);
            prop_assert!(open.stream <= limits.stream);
            prop_assert_eq!(open.api + open.stream + open.unproven, live.len());
        }
        drop(live);
        prop_assert_eq!(pools.open(), Open::default());
    }
}

/// Runs `body` on the simulator with one host's runtime.
fn sim<F, Fut>(body: F)
where
    F: FnOnce(Runtime) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let summary = run(RunConfig::new(1), move |world| async move {
        let boot: Boot = Arc::new(|_| -> BoxFuture<'static, ()> { Box::pin(async {}) });
        let host = world.add_host(HostConfig::new(boot)).expect("a host");
        let runtime = world.runtime(host).expect("its runtime");
        body(runtime).await;
        Ok::<(), Failure>(())
    });
    assert_eq!(summary.result, RunResult::Pass, "{summary:?}");
}

/// Whether `gate` closes within `within`.
async fn closes_within(runtime: &Runtime, gate: &Gate, within: Duration) -> bool {
    first(gate.closed(), runtime.sleep(within)).await.is_some()
}

fn gate(runtime: &Runtime, pools: &Arc<Pools>, limits: &ConnLimits) -> Arc<Gate> {
    let place = pools.admit(v4(192, 0, 2, 1)).expect("place");
    let clock = Arc::clone(runtime.clock());
    let accepted = clock.now();
    Arc::new(Gate::unproven(clock, place, accepted, limits))
}

fn timed() -> ConnLimits {
    ConnLimits {
        prove: Duration::from_secs(20),
        rejected: 4,
        call: Duration::from_secs(30),
        idle: Duration::from_secs(60),
        ..limits()
    }
}

#[test]
fn an_unproven_connection_closes_at_its_rejected_bound_or_prove() {
    sim(|rt| async move {
        let limits = timed();
        let pools = Pools::new(limits);
        let g = gate(&rt, &pools, &limits);
        for _ in 0..3 {
            g.reject();
        }
        assert!(g.admit());
        g.reject();
        assert!(!g.admit(), "the fourth rejected call closes it");
        assert!(closes_within(&rt, &g, Duration::ZERO).await);
        assert_eq!(g.begin(&alice()).map(|_| ()), Err(NotStarted::Closing));

        // Rejected calls spaced out do not keep a connection past `prove`.
        let g = gate(&rt, &pools, &limits);
        for _ in 0..3 {
            assert!(!closes_within(&rt, &g, Duration::from_secs(6)).await);
            g.reject();
        }
        assert!(closes_within(&rt, &g, Duration::from_secs(3)).await);
        assert!(!g.admit());
    });
}

#[test]
fn calls_in_progress_or_closer_than_idle_keep_a_connection() {
    sim(|rt| async move {
        let limits = timed();
        let pools = Pools::new(limits);
        let g = gate(&rt, &pools, &limits);
        drop(g.begin(&alice()).expect("proves"));
        assert_eq!(pools.open().api, 1);
        // Calls 50 s apart, each under `idle` after the last, keep it.
        for _ in 0..4 {
            assert!(!closes_within(&rt, &g, Duration::from_secs(50)).await);
            drop(g.begin(&alice()).expect("call"));
        }
        // A call in progress keeps it open up to `call`, and then `idle`.
        let call = g.begin(&alice()).expect("long call");
        assert!(!closes_within(&rt, &g, Duration::from_secs(89)).await);
        assert!(closes_within(&rt, &g, Duration::from_secs(2)).await);
        drop(call);
        assert!(!g.admit());
        drop(g);
        assert_eq!(pools.open(), Open::default());
    });
}

#[test]
fn an_idle_proven_connection_closes_and_takes_no_call() {
    sim(|rt| async move {
        let limits = timed();
        let pools = Pools::new(limits);
        let g = gate(&rt, &pools, &limits);
        drop(g.begin(&alice()).expect("proves"));
        assert!(!closes_within(&rt, &g, Duration::from_secs(59)).await);
        rt.sleep(Duration::from_secs(1)).await;
        // Due, though no one has waited on `closed` since: the next call is
        // refused unread.
        assert!(!g.admit());
        assert_eq!(g.begin(&alice()).map(|_| ()), Err(NotStarted::Closing));
    });
}

#[test]
fn a_principal_holds_at_most_its_share_of_api_places() {
    sim(|rt| async move {
        let limits = timed();
        let pools = Pools::new(limits);
        let a = gate(&rt, &pools, &limits);
        let _a = a.begin(&alice()).expect("alice");
        let b = gate(&rt, &pools, &limits);
        assert_eq!(b.begin(&alice()).map(|_| ()), Err(NotStarted::Full));
        // Refused, the connection is still unproven and may be proven by
        // another principal.
        let _b = b.begin(&bob()).expect("bob");
        assert_eq!(pools.open().principals, 2);
    });
}

#[test]
fn an_evicted_connection_closes() {
    sim(|rt| async move {
        let limits = ConnLimits {
            unproven_per_source: 4,
            ..timed()
        };
        let pools = Pools::new(limits);
        let g = gate(&rt, &pools, &limits);
        let _rest: Vec<Place> = (0..3)
            .map(|_| pools.admit(v4(192, 0, 2, 1)).expect("same source"))
            .collect();
        assert!(!closes_within(&rt, &g, Duration::from_secs(1)).await);
        let _newcomer = pools.admit(v4(203, 0, 113, 1)).expect("newcomer");
        assert!(closes_within(&rt, &g, Duration::ZERO).await);
        assert_eq!(g.begin(&alice()).map(|_| ()), Err(NotStarted::Closing));
    });
}

#[test]
fn a_stream_connection_is_closed_unless_proven_in_time() {
    sim(|rt| async move {
        let limits = timed();
        let pools = Pools::new(limits);
        let late = gate(&rt, &pools, &limits);
        assert!(!closes_within(&rt, &late, Duration::from_secs(19)).await);
        assert!(closes_within(&rt, &late, Duration::from_secs(2)).await);
        assert!(!late.prove_stream());
        drop(late);

        let g = gate(&rt, &pools, &limits);
        assert!(g.prove_stream());
        assert!(g.prove_stream(), "proving twice changes nothing");
        assert_eq!(pools.open().stream, 1);
        assert!(!closes_within(&rt, &g, Duration::from_secs(3_000)).await);

        // The `stream` count is full: the next one is closed.
        let full = gate(&rt, &pools, &limits);
        assert!(!full.prove_stream());
        assert!(closes_within(&rt, &full, Duration::ZERO).await);
        drop((g, full));
        assert_eq!(pools.open(), Open::default());
    });
}
