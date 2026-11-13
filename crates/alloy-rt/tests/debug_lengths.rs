//! Property test: `Datagram` and `RecvSlot` `Debug` output holds lengths only,
//! whatever the bytes, addresses, and flags (ARFC 0005 section 2.1, I-22).
//!
//! `proptest` runs from fixed seeds with failure persistence off and no
//! time-based limit, so every run checks the same cases and reads neither the
//! clock nor OS entropy (`agent/eng/testing.md`). A failure names its seed.

use std::net::{IpAddr, SocketAddr};

use alloy_rt::{Datagram, RecvSlot};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestCaseResult, TestRunner};

/// Seeds every property runs on.
const SEEDS: [u64; 4] = [0, 1, 0x5eed, 0xdead_beef];

/// Cases drawn per seed.
const CASES: u32 = 256;

/// A runner seeded with `seed` whose outcome depends on nothing else: no
/// clock-bounded shrinking, no failure files, and every limit set here rather
/// than read from `PROPTEST_*` environment variables.
fn runner(seed: u64) -> TestRunner {
    TestRunner::new(Config {
        cases: CASES,
        max_local_rejects: 65_536,
        max_global_rejects: 1_024,
        max_flat_map_regens: 1_000_000,
        failure_persistence: None,
        max_shrink_time: 0,
        max_shrink_iters: 4_096,
        max_default_size_range: 100,
        verbose: 0,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(seed),
        ..Config::default()
    })
}

/// Runs `test` over `strategy` on every seed and panics with the seed and the
/// minimal failing input on the first failure.
fn check<S: Strategy>(strategy: S, test: impl Fn(S::Value) -> TestCaseResult) {
    for seed in SEEDS {
        if let Err(error) = runner(seed).run(&strategy, &test) {
            panic!("seed {seed:#x}: {error}");
        }
    }
}

#[test]
fn datagram_debug_prints_the_payload_length_only() {
    let strategy = (
        any::<SocketAddr>(),
        any::<Option<IpAddr>>(),
        vec(any::<u8>(), 0..=2_048),
    );
    check(strategy, |(dst, src_ip, payload)| {
        let datagram = Datagram {
            dst,
            src_ip,
            payload: &payload,
        };
        prop_assert_eq!(
            format!("{datagram:?}"),
            format!("Datagram {{ len: {} }}", payload.len())
        );
        Ok(())
    });
}

#[test]
fn recv_slot_debug_prints_the_capacity_and_length_only() {
    let strategy = (
        0..=RecvSlot::MAX_CAPACITY,
        any::<usize>(),
        any::<bool>(),
        any::<SocketAddr>(),
        any::<IpAddr>(),
        any::<u8>(),
    );
    check(strategy, |(capacity, len, truncated, src, dst_ip, fill)| {
        let len = len % (capacity + 1);
        let slot = RecvSlot {
            buf: vec![fill; capacity].into_boxed_slice(),
            len,
            truncated,
            src,
            dst_ip,
        };
        prop_assert_eq!(
            format!("{slot:?}"),
            format!("RecvSlot {{ capacity: {capacity}, len: {len} }}")
        );
        Ok(())
    });
}
