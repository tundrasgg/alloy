//! Seeded random streams.
//!
//! Every source of randomness in a run is its own stream: the scheduler, the
//! scenario's `random_u64`, and, as the world grows, each host, router, and
//! link. A stream is fixed by the run's seed, a label naming its purpose, and
//! the ids that place it (a host and its start, for example), so streams never
//! share values by accident and adding one stream never shifts another.
//!
//! The stream's state is derived by hashing those inputs with BLAKE3 through
//! `alloy-crypto`, and the values come from xoshiro256**. The output is
//! predictable from the seed and is not secret (ARFC 0005 section 8).

/// Domain separation for the state derivation, so that no other use of
/// BLAKE3 in the workspace hashes the same bytes.
const DOMAIN: &[u8] = b"alloy-sim/stream/v1";

/// One seeded stream of 64-bit values: xoshiro256** over a 256-bit state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stream {
    s: [u64; 4],
}

impl Stream {
    /// The stream of `seed` for the purpose `label` at the place `ids`.
    ///
    /// Each input is length-prefixed in the hashed bytes, so two different
    /// inputs never hash the same bytes.
    pub(crate) fn new(seed: u64, label: &[u8], ids: &[u64]) -> Stream {
        let mut hasher = alloy_crypto::Hasher::new();
        hasher
            .update(&(DOMAIN.len() as u64).to_le_bytes())
            .update(DOMAIN)
            .update(&seed.to_le_bytes())
            .update(&(label.len() as u64).to_le_bytes())
            .update(label)
            .update(&(ids.len() as u64).to_le_bytes());
        for id in ids {
            hasher.update(&id.to_le_bytes());
        }
        let digest = hasher.finalize();
        let mut s = [0u64; 4];
        let (words, _) = digest.as_chunks::<8>();
        for (word, bytes) in s.iter_mut().zip(words) {
            *word = u64::from_le_bytes(*bytes);
        }
        Stream::from_state(s)
    }

    /// A stream with the raw state `s`. The all-zero state is a fixed point of
    /// xoshiro, so it is replaced by one with a single bit set.
    fn from_state(mut s: [u64; 4]) -> Stream {
        if s == [0; 4] {
            s[0] = 1;
        }
        Stream { s }
    }

    /// The next value.
    pub(crate) fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// A value from 0 to `n - 1`, or 0 when `n` is 0. Draws exactly one value
    /// from the stream in every case, so the stream's position does not depend
    /// on `n`.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        let wide = u128::from(self.next_u64()) * n as u128;
        // The product of a u64 and a usize shifted right by 64 is below `n`,
        // which is a usize.
        usize::try_from(wide >> 64).unwrap_or(0)
    }

    /// Permutes `items` with a Fisher-Yates shuffle drawn from the stream.
    pub(crate) fn shuffle<T>(&mut self, items: &mut [T]) {
        for last in (1..items.len()).rev() {
            let pick = self.below(last + 1);
            items.swap(last, pick);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first outputs of xoshiro256** from the state 1, 2, 3, 4, as the
    /// reference implementation at https://prng.di.unimi.it/xoshiro256starstar.c
    /// computes them.
    #[test]
    fn xoshiro_matches_the_reference() {
        let mut stream = Stream::from_state([1, 2, 3, 4]);
        let got: Vec<u64> = (0..4).map(|_| stream.next_u64()).collect();
        assert_eq!(got, [11520, 0, 1509978240, 1215971899390074240]);
    }

    #[test]
    fn the_zero_state_is_replaced() {
        let mut stream = Stream::from_state([0; 4]);
        let got: Vec<u64> = (0..8).map(|_| stream.next_u64()).collect();
        assert!(got.iter().any(|v| *v != 0));
    }

    #[test]
    fn streams_are_fixed_by_their_inputs() {
        let a: Vec<u64> = {
            let mut s = Stream::new(1, b"scenario", &[]);
            (0..4).map(|_| s.next_u64()).collect()
        };
        let b: Vec<u64> = {
            let mut s = Stream::new(1, b"scenario", &[]);
            (0..4).map(|_| s.next_u64()).collect()
        };
        assert_eq!(a, b);
    }

    #[test]
    fn different_inputs_give_different_streams() {
        let inputs: [(u64, &[u8], &[u64]); 7] = [
            (1, b"scenario", &[]),
            (2, b"scenario", &[]),
            (1, b"schedule", &[]),
            (1, b"scenario", &[0]),
            (1, b"scenario", &[0, 0]),
            (1, b"scenario", &[1]),
            (1, b"scenari", &[0x6f]),
        ];
        let mut firsts: Vec<[u64; 2]> = inputs
            .iter()
            .map(|(seed, label, ids)| {
                let mut s = Stream::new(*seed, label, ids);
                [s.next_u64(), s.next_u64()]
            })
            .collect();
        firsts.sort_unstable();
        firsts.dedup();
        assert_eq!(firsts.len(), inputs.len());
    }

    #[test]
    fn below_stays_in_range_and_draws_one_value() {
        let mut a = Stream::new(9, b"t", &[]);
        let mut b = a.clone();
        for n in [1usize, 2, 3, 7, 100, usize::MAX] {
            assert!(a.below(n) < n);
            b.next_u64();
        }
        assert_eq!(a.below(0), 0);
        b.next_u64();
        assert_eq!(a, b);
    }

    #[test]
    fn shuffle_permutes() {
        let mut stream = Stream::new(3, b"t", &[]);
        let mut items: Vec<u32> = (0..50).collect();
        stream.shuffle(&mut items);
        assert_ne!(items, (0..50).collect::<Vec<_>>());
        items.sort_unstable();
        assert_eq!(items, (0..50).collect::<Vec<_>>());
    }
}
