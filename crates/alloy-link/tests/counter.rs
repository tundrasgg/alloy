//! Counter draws for node channel request ids (ARFC 0002 section 3.13):
//! every value of the handshake counter is used once, by one initiation or
//! by one draw, across blocks and across a restart (ARFC 0003 section 3.2).

mod common;

use alloy_crypto::al::{INITIATION_NOISE_LEN, PresharedKey, ResponderHandshake, StaticSecret};
use alloy_link::{COUNTER_BLOCK, Config, Ctx, DropReason, Event, Link, Output};
use alloy_rt::{Instant, Rng};
use alloy_wire::{HandshakeCounter, Initiation, InitiationPayload, QueueCount};
use common::{QUEUES, SplitMix, secret, with_rng};

const NOW: Instant = Instant::from_nanos(1_000_000_000);

fn node(start: u64) -> Link {
    Link::new(
        secret(1),
        Config {
            queues: QueueCount::new(QUEUES).unwrap(),
            counter_start: HandshakeCounter(start),
        },
    )
}

/// Responder number `n`, which only decrypts initiations sent to it.
fn responder(n: u32) -> StaticSecret {
    let mut bytes = [0x33; 32];
    bytes[4..8].copy_from_slice(&n.to_le_bytes());
    StaticSecret::from_bytes(bytes)
}

/// What the node under test does with its handshake counter, and what the
/// caller does with the blocks: it persists the end of the highest block it
/// reserved, as `handshake-counter` holds it (ARFC 0002 section 3.14).
struct Node<'r> {
    rng: &'r dyn Rng,
    link: Link,
    /// The end of the highest block reserved.
    persisted: u64,
    next_responder: u32,
    /// Every value used, in order.
    used: Vec<u64>,
}

impl<'r> Node<'r> {
    fn new(rng: &'r dyn Rng, start: u64) -> Self {
        Self {
            rng,
            link: node(start),
            persisted: start + COUNTER_BLOCK,
            next_responder: 0,
            used: Vec::new(),
        }
    }

    /// Reserves the next block when the link asks, unless `reserve` is off.
    fn blocks(&mut self, out: &[Output], reserve: bool) {
        for o in out {
            if let Output::Event(Event::CounterBlockNeeded { next_start }) = o
                && reserve
                && next_start.0 + COUNTER_BLOCK > self.persisted
            {
                self.persisted = next_start.0 + COUNTER_BLOCK;
                self.link.add_counter_block();
            }
        }
    }

    fn draw(&mut self, reserve: bool) -> Option<u64> {
        let mut out = Vec::new();
        let value = self.link.draw_counter(&mut out).map(|c| c.0);
        self.blocks(&out, reserve);
        if let Some(v) = value {
            self.used.push(v);
        }
        value
    }

    /// An initiation to a fresh responder; the counter it carries.
    fn initiate(&mut self, reserve: bool) -> Result<u64, DropReason> {
        let r = responder(self.next_responder);
        self.next_responder += 1;
        self.link.add_peer(r.public_key(), PresharedKey::none());
        let mut out = Vec::new();
        let cx = Ctx {
            now: NOW,
            rng: self.rng,
        };
        let result = self.link.connect(cx, r.public_key(), &mut out);
        self.blocks(&out, reserve);
        result?;
        let datagram = out
            .into_iter()
            .find_map(|o| match o {
                Output::Transmit { datagram, .. } => Some(datagram),
                _ => None,
            })
            .expect("an initiation");

        let mut noise = [0u8; INITIATION_NOISE_LEN];
        noise.copy_from_slice(&datagram[Initiation::NOISE_RANGE]);
        let (_, _, payload) = ResponderHandshake::read_initiation(&r, &noise).unwrap();
        let counter = InitiationPayload::decode(&payload).unwrap().counter.0;
        self.used.push(counter);
        Ok(counter)
    }
}

/// Each value is used once and values only grow.
fn strictly_increasing(used: &[u64]) {
    for pair in used.windows(2) {
        assert!(pair[0] < pair[1], "{} then {}", pair[0], pair[1]);
    }
}

#[test]
fn draws_and_initiations_take_successive_values() {
    with_rng(1, |rng| {
        let mut n = Node::new(rng, 1_000);
        assert_eq!(n.initiate(true), Ok(1_000));
        assert_eq!(n.draw(true), Some(1_001));
        assert_eq!(n.draw(true), Some(1_002));
        assert_eq!(n.initiate(true), Ok(1_003));
        assert_eq!(n.draw(true), Some(1_004));
    });
}

/// Random interleavings of draws and initiations, over several blocks and
/// a restart from the persisted counter: no value is used twice and the
/// values only grow.
#[test]
fn values_never_reused_across_blocks_and_restarts() {
    for seed in 0..4u64 {
        with_rng(10 + seed, move |rng| {
            let mut pick = SplitMix(seed);
            let mut node = Node::new(rng, 0);
            let mut used = Vec::new();
            for life in 0..3 {
                let start = node.persisted - COUNTER_BLOCK;
                // Long runs of draws cross block boundaries; initiations
                // land at random points among them.
                for _ in 0..40 {
                    if pick.chance(25) {
                        node.initiate(true).unwrap();
                    } else {
                        for _ in 0..pick.below(10_000) {
                            node.draw(true).unwrap();
                        }
                    }
                }
                assert!(
                    node.used.last().copied().unwrap_or(0) >= start + COUNTER_BLOCK,
                    "seed {seed}, life {life}: crossed a block"
                );
                // A restart, crash or not: the new process starts at the end
                // of the highest block the old one reserved.
                used.append(&mut node.used);
                let next = node.persisted;
                node = Node::new(rng, next);
            }
            strictly_increasing(&used);
        });
    }
}

#[test]
fn used_up_blocks_stop_draws_and_initiations() {
    with_rng(2, |rng| {
        let mut n = Node::new(rng, 0);
        let mut events = Vec::new();
        for v in 0..COUNTER_BLOCK {
            let mut out = Vec::new();
            assert_eq!(n.link.draw_counter(&mut out), Some(HandshakeCounter(v)));
            events.extend(out);
        }
        // Half the block used: the caller was asked for the next one.
        assert_eq!(
            events,
            vec![Output::Event(Event::CounterBlockNeeded {
                next_start: HandshakeCounter(COUNTER_BLOCK)
            })]
        );
        let mut out = Vec::new();
        assert_eq!(n.link.draw_counter(&mut out), None);
        assert_eq!(
            out,
            vec![Output::Event(Event::CounterBlockNeeded {
                next_start: HandshakeCounter(COUNTER_BLOCK)
            })]
        );
        assert_eq!(n.link.drops(DropReason::NoCounter), 0);
        assert_eq!(n.initiate(false), Err(DropReason::NoCounter));
        // Once the caller reserves the block, both resume at its start.
        n.link.add_counter_block();
        assert_eq!(n.initiate(false), Ok(COUNTER_BLOCK));
        assert_eq!(n.draw(false), Some(COUNTER_BLOCK + 1));
    });
}
