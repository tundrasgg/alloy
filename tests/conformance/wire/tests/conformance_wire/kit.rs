//! What the test files share: byte builders, the seeded property runner, and
//! small constructors.
//!
//! `proptest` runs from fixed seeds with failure persistence off and no
//! time-based limit, so every run checks the same cases and reads neither the
//! clock nor OS entropy (`agent/eng/testing.md`). A failure names its seed.

use core::cell::Cell;

use alloy_wire::{SessionIndex, Vni};
use proptest::strategy::Strategy;
use proptest::test_runner::{
    Config, RngAlgorithm, RngSeed, TestCaseError, TestCaseResult, TestRunner,
};

/// Seeds every property runs on.
pub(crate) const SEEDS: [u64; 4] = [0, 1, 0x5eed, 0xdead_beef];

/// Cases drawn per seed.
const CASES: u32 = 512;

/// `len` bytes counting up from `start`, wrapping. Every field of a vector is
/// filled this way from a different start, so a field read from the wrong
/// offset, or with its bytes reversed, does not compare equal.
pub(crate) fn seq(start: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| start.wrapping_add(i as u8)).collect()
}

/// `seq` as a fixed-size array.
pub(crate) fn seq_array<const N: usize>(start: u8) -> [u8; N] {
    let mut out = [0; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = start.wrapping_add(i as u8);
    }
    out
}

/// The concatenation of `parts`.
pub(crate) fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// A VNI known to be in range.
pub(crate) fn vni(n: u32) -> Vni {
    Vni::new(n).expect("the VNI fits in 24 bits")
}

/// VNI 0, the infrastructure network.
pub(crate) fn vni0() -> Vni {
    vni(0)
}

/// VNIs other than 0 the tests use, including the largest.
pub(crate) fn tenant_vnis() -> [Vni; 3] {
    [vni(1), vni(0x12_3456), vni((1 << 24) - 1)]
}

/// An index.
pub(crate) fn idx(n: u32) -> SessionIndex {
    SessionIndex(n)
}

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
pub(crate) fn check<S: Strategy>(strategy: S, test: impl Fn(S::Value) -> TestCaseResult) {
    for seed in SEEDS {
        if let Err(error) = runner(seed).run(&strategy, &test) {
            panic!("seed {seed:#x}: {error}");
        }
    }
}

/// Fewest cases, over all seeds, that must fall on each side of every decision
/// a property reports, so that a property cannot pass by never reaching one.
const MIN_EACH_SIDE: usize = 64;

/// Runs `test` like [`check`]. Each case also reports `N` decisions, such as
/// whether a decoder kept its input. Panics unless every decision came out
/// both ways on at least `MIN_EACH_SIDE` cases.
pub(crate) fn check_both_ways<S: Strategy, const N: usize>(
    strategy: S,
    test: impl Fn(S::Value) -> Result<[bool; N], TestCaseError>,
) {
    let kept: [Cell<usize>; N] = core::array::from_fn(|_| Cell::new(0));
    let dropped: [Cell<usize>; N] = core::array::from_fn(|_| Cell::new(0));
    check(strategy, |value| {
        for (i, outcome) in test(value)?.into_iter().enumerate() {
            let side = if outcome { &kept[i] } else { &dropped[i] };
            side.set(side.get() + 1);
        }
        Ok(())
    });
    for i in 0..N {
        let (k, d) = (kept[i].get(), dropped[i].get());
        assert!(
            k >= MIN_EACH_SIDE && d >= MIN_EACH_SIDE,
            "decision {i} came out kept {k} and dropped {d} times"
        );
    }
}
