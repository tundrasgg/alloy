//! Property tests for the configuration value codec of ARFC 0001 section 4.
//!
//! `proptest` runs from fixed seeds with failure persistence off and no
//! time-based limit, so every run checks the same cases and reads neither the
//! clock nor OS entropy (`agent/eng/testing.md`). A failure names its seed.
//!
//! Properties:
//!
//! - Round trip: `ConfigValue::from_bytes(&v.to_bytes()) == Ok(v)` for every
//!   valid value, the empty and the largest payload included.
//! - Totality: every byte string decodes either to a value that re-encodes to
//!   the same bytes or to the typed error section 4 names, checked in its order.

use alloy_seam::{ConfigError, ConfigValue};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestCaseResult, TestRunner};

/// Seeds every property runs on.
const SEEDS: [u64; 4] = [0, 1, 0x5eed, 0xdead_beef];

/// Cases drawn per seed.
const CASES: u32 = 256;

/// The largest payload: the serialized limit less the 2-byte version.
const MAX_PAYLOAD: usize = ConfigValue::MAX_SERIALIZED - 2;

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

/// `len` bytes counting from `start` in steps of `step`: cheap at any length.
fn pattern(len: usize, start: u8, step: u8) -> Vec<u8> {
    (0..len)
        .map(|i| start.wrapping_add(step.wrapping_mul(i as u8)))
        .collect()
}

/// Valid versions, with both ends of the range drawn often.
fn version() -> impl Strategy<Value = u16> {
    prop_oneof![Just(1), Just(u16::MAX), 1..=u16::MAX]
}

/// Valid payloads: empty, short and random, near and at the largest, and any
/// length in between.
fn payload() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(Vec::new()),
        vec(any::<u8>(), 1..=512),
        (any::<u8>(), any::<u8>()).prop_map(|(a, b)| pattern(MAX_PAYLOAD, a, b)),
        (MAX_PAYLOAD - 512..=MAX_PAYLOAD, any::<u8>(), any::<u8>())
            .prop_map(|(len, a, b)| pattern(len, a, b)),
        (0..=MAX_PAYLOAD, any::<u8>(), any::<u8>()).prop_map(|(len, a, b)| pattern(len, a, b)),
    ]
}

/// Byte strings of any shape: short and random, and patterned at lengths around
/// each boundary of section 4, with the version forced to 0 on about half.
fn bytes() -> impl Strategy<Value = Vec<u8>> {
    let len = prop_oneof![
        0..=3_usize,
        ConfigValue::MAX_SERIALIZED - 2..=ConfigValue::MAX_SERIALIZED + 2,
        0..=70_000_usize,
    ];
    prop_oneof![
        vec(any::<u8>(), 0..=64),
        (len, any::<bool>(), any::<u8>(), any::<u8>()).prop_map(|(len, zero, a, b)| {
            let mut bytes = pattern(len, a, b);
            if zero {
                bytes.iter_mut().take(2).for_each(|byte| *byte = 0);
            }
            bytes
        }),
    ]
}

/// Section 4 restated: the first error that applies, in the order `Truncated`,
/// `TooLarge`, `InvalidVersion`, or else the version and the payload.
fn model(bytes: &[u8]) -> Result<(u16, &[u8]), ConfigError> {
    match bytes {
        [] | [_] => Err(ConfigError::Truncated),
        _ if bytes.len() > ConfigValue::MAX_SERIALIZED => {
            Err(ConfigError::TooLarge { len: bytes.len() })
        }
        [0, 0, ..] => Err(ConfigError::InvalidVersion),
        [low, high, payload @ ..] => Ok((u16::from_le_bytes([*low, *high]), payload)),
    }
}

#[test]
fn every_valid_value_round_trips() {
    check((version(), payload()), |(version, payload)| {
        let len = payload.len();
        let value = ConfigValue::new(version, payload);
        prop_assert!(value.is_ok(), "rejected: {value:?}");
        let Ok(value) = value else {
            return Ok(());
        };
        let bytes = value.to_bytes();
        prop_assert_eq!(bytes.len(), 2 + len);
        prop_assert!(bytes.len() <= ConfigValue::MAX_SERIALIZED);
        prop_assert_eq!(&bytes[..2], &version.to_le_bytes()[..]);
        prop_assert_eq!(ConfigValue::from_bytes(&bytes), Ok(value));
        Ok(())
    });
}

#[test]
fn every_byte_string_decodes_to_a_value_or_a_typed_error() {
    check(bytes(), |bytes| {
        match (ConfigValue::from_bytes(&bytes), model(&bytes)) {
            (Ok(value), Ok((version, payload))) => {
                prop_assert_eq!(value.version(), version);
                prop_assert!(value.payload() == payload, "payload differs");
                prop_assert!(value.to_bytes() == bytes, "re-encoding differs");
                prop_assert_eq!(
                    format!("{value:?}"),
                    format!(
                        "ConfigValue {{ version: {version}, len: {} }}",
                        payload.len()
                    )
                );
            }
            (Err(error), Err(expected)) => prop_assert_eq!(error, expected),
            (decoded, expected) => prop_assert!(
                false,
                "decoded {decoded:?}, section 4 gives {:?}",
                expected.map(|(version, payload)| (version, payload.len()))
            ),
        }
        Ok(())
    });
}
