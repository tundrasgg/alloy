//! Two links driven against each other in memory, with randomness from the
//! simulator's `mem` generator.

#![allow(
    dead_code,
    reason = "each test file uses a different part of the harness"
)]

use std::sync::Arc;
use std::time::Duration;

use alloy_crypto::al::{PresharedKey, PublicKey, StaticSecret};
use alloy_link::{Config, Ctx, DropReason, Event, Link, Outbound, Output, Sent};
use alloy_rt::{BoxFuture, Instant, Rng};
use alloy_sim::{Boot, HostConfig, RunConfig, RunResult, run};
use alloy_wire::{HandshakeCounter, Message, QueueCount, Vni};

/// Node a, the usual initiator.
pub(crate) const A: usize = 0;
/// Node b, the usual responder.
pub(crate) const B: usize = 1;

/// The cookie `src` of each node's datagrams: an IPv4 address and port.
pub(crate) const SRC: [&[u8]; 2] = [&[10, 0, 0, 1, 0x1f, 0x90], &[10, 0, 0, 2, 0x1f, 0x90]];

/// Sending queues of both nodes.
pub(crate) const QUEUES: u16 = 4;

/// A payload budget no test packet reaches unless it means to.
pub(crate) const BUDGET: usize = 1_400;

/// Runs `body` with the `mem` generator of a simulated host seeded by
/// `seed`, and fails if it panics.
pub(crate) fn with_rng(seed: u64, body: impl FnOnce(&dyn Rng) + Send + 'static) {
    let summary = run(RunConfig::new(seed), move |world| async move {
        let boot: Boot = Arc::new(|_| -> BoxFuture<'static, ()> { Box::pin(async {}) });
        let host = world.add_host(HostConfig::new(boot)).expect("a host");
        let runtime = world.runtime(host).expect("its runtime");
        body(runtime.rng().as_ref());
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass, "seed {seed}");
}

/// The static key of node `n`.
pub(crate) fn secret(n: u8) -> StaticSecret {
    StaticSecret::from_bytes([n; 32])
}

pub(crate) fn config() -> Config {
    Config {
        queues: QueueCount::new(QUEUES).unwrap(),
        counter_start: HandshakeCounter(0),
    }
}

/// An IPv4 packet of `len` bytes, filled with `fill` after its first byte.
pub(crate) fn ip(len: usize, fill: u8) -> Vec<u8> {
    let mut p = vec![fill; len];
    p[0] = 0x45;
    p
}

pub(crate) fn vni(n: u32) -> Vni {
    Vni::new(n).unwrap()
}

pub(crate) fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

/// A test's own choices, apart from the links' randomness: SplitMix64,
/// seeded by the test.
pub(crate) struct SplitMix(pub(crate) u64);

impl SplitMix {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// True with probability `percent` in 100.
    pub(crate) fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Two links that are each other's peers, the datagrams in flight to each,
/// and what each delivered or reported.
pub(crate) struct Pair<'r> {
    pub(crate) rng: &'r dyn Rng,
    pub(crate) now: Instant,
    pub(crate) nodes: [Link; 2],
    pub(crate) keys: [PublicKey; 2],
    pub(crate) inflight: [Vec<Vec<u8>>; 2],
    pub(crate) got: [Vec<Output>; 2],
}

impl<'r> Pair<'r> {
    pub(crate) fn new(rng: &'r dyn Rng) -> Self {
        Self::with_psk(rng, [7; 32])
    }

    pub(crate) fn with_psk(rng: &'r dyn Rng, psk: [u8; 32]) -> Self {
        let mut a = Link::new(secret(1), config());
        let mut b = Link::new(secret(2), config());
        let keys = [a.public_key(), b.public_key()];
        a.add_peer(keys[B], PresharedKey::from_bytes(psk));
        b.add_peer(keys[A], PresharedKey::from_bytes(psk));
        Self {
            rng,
            now: Instant::from_nanos(1_000_000_000),
            nodes: [a, b],
            keys,
            inflight: [Vec::new(), Vec::new()],
            got: [Vec::new(), Vec::new()],
        }
    }

    pub(crate) fn cx(&self) -> Ctx<'r> {
        Ctx {
            now: self.now,
            rng: self.rng,
        }
    }

    /// Calls `f` on node `i` and routes its outputs: every datagram, which
    /// must decode as an AL message, to the other node; the rest to `got`.
    pub(crate) fn call<T>(
        &mut self,
        i: usize,
        f: impl FnOnce(&mut Link, Ctx<'r>, &mut Vec<Output>) -> T,
    ) -> T {
        let mut out = Vec::new();
        let cx = self.cx();
        let result = f(&mut self.nodes[i], cx, &mut out);
        for output in out {
            match output {
                Output::Transmit { datagram, .. } => {
                    assert!(Message::decode(&datagram).is_ok(), "an AL message");
                    self.inflight[1 - i].push(datagram);
                }
                other => self.got[i].push(other),
            }
        }
        result
    }

    pub(crate) fn connect(&mut self, i: usize) {
        let peer = self.keys[1 - i];
        self.call(i, |l, cx, out| l.connect(cx, peer, out)).unwrap();
    }

    pub(crate) fn send(&mut self, i: usize, v: Vni, packet: &[u8]) -> Result<Sent, DropReason> {
        self.send_budget(i, v, 0, packet, BUDGET)
    }

    pub(crate) fn send_budget(
        &mut self,
        i: usize,
        v: Vni,
        queue: u16,
        packet: &[u8],
        budget: usize,
    ) -> Result<Sent, DropReason> {
        let to = Outbound {
            peer: self.keys[1 - i],
            vni: v,
            queue,
            budget,
        };
        self.call(i, |l, cx, out| l.send(cx, to, packet, out))
    }

    /// Hands `datagram` to node `i`, from the other node's source.
    pub(crate) fn recv(&mut self, i: usize, mut datagram: Vec<u8>) -> Result<(), DropReason> {
        let src = SRC[1 - i];
        self.call(i, |l, cx, out| l.receive(cx, src, &mut datagram, out))
    }

    /// Takes the datagrams in flight to node `i`.
    pub(crate) fn take(&mut self, i: usize) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.inflight[i])
    }

    /// Delivers every datagram in flight, both ways, until none is left. Each
    /// must be accepted.
    pub(crate) fn settle(&mut self) {
        while !self.inflight[A].is_empty() || !self.inflight[B].is_empty() {
            for i in [A, B] {
                for d in self.take(i) {
                    self.recv(i, d).expect("accepted");
                }
            }
        }
    }

    /// Advances the clock and ticks both nodes.
    pub(crate) fn advance(&mut self, by: Duration) {
        self.now = self.now.saturating_add(by);
        for i in [A, B] {
            self.call(i, |l, cx, out| l.tick(cx, out));
        }
    }

    /// The packets node `i` delivered since the last call, with their VNIs.
    pub(crate) fn delivered(&mut self, i: usize) -> Vec<(Vni, Vec<u8>)> {
        let mut packets = Vec::new();
        self.got[i].retain(|o| match o {
            Output::Deliver { vni, packet, .. } => {
                packets.push((*vni, packet.clone()));
                false
            }
            _ => true,
        });
        packets
    }

    /// The events node `i` reported since the last call.
    pub(crate) fn events(&mut self, i: usize) -> Vec<Event> {
        let mut events = Vec::new();
        self.got[i].retain(|o| match o {
            Output::Event(e) => {
                events.push(*e);
                false
            }
            _ => true,
        });
        events
    }

    /// a connects to b and both settle: one session each, confirmed at b.
    pub(crate) fn handshake(&mut self) {
        self.connect(A);
        self.settle();
    }
}
