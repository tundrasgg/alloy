//! Stand-in for rand 0.9.5: the thread-local generator, its shortcuts, and the
//! re-exports, at the paths the real crate has them (`src/lib.rs` and
//! `src/rngs/mod.rs` of rand 0.9.5). Nothing here reaches the operating system.

pub use rand_core;
pub use rand_core::{RngCore, SeedableRng, TryRngCore};

/// The generators.
pub mod rngs {
    pub use rand_core::OsRng;

    /// A seedable generator.
    #[derive(Debug, Clone)]
    pub struct StdRng([u8; 32]);

    impl rand_core::SeedableRng for StdRng {
        type Seed = [u8; 32];

        fn from_seed(seed: [u8; 32]) -> Self {
            Self(seed)
        }
    }

    impl rand_core::RngCore for StdRng {
        fn next_u64(&mut self) -> u64 {
            u64::from(self.0[0])
        }
    }

    /// The thread-local generator, seeded from the operating system in the
    /// real crate.
    #[derive(Debug, Clone, Default)]
    pub struct ThreadRng(u64);

    impl rand_core::RngCore for ThreadRng {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(1);
            self.0
        }
    }
}

/// The thread-local generator.
pub fn rng() -> rngs::ThreadRng {
    rngs::ThreadRng::default()
}

/// The thread-local generator, under its 0.8 name.
#[deprecated(since = "0.9.0", note = "renamed to `rng`")]
pub fn thread_rng() -> rngs::ThreadRng {
    rng()
}

/// A value from the thread-local generator.
pub fn random<T: Default>() -> T {
    T::default()
}

/// Values from the thread-local generator.
pub fn random_iter<T: Default>() -> impl Iterator<Item = T> {
    core::iter::repeat_with(T::default)
}

/// A value in `range` from the thread-local generator.
pub fn random_range<T: Default, R>(_range: R) -> T {
    T::default()
}

/// `true` with probability `p`, from the thread-local generator.
pub fn random_bool(p: f64) -> bool {
    p >= 1.0
}

/// `true` with probability `numerator / denominator`, from the thread-local
/// generator.
pub fn random_ratio(numerator: u32, denominator: u32) -> bool {
    numerator >= denominator
}

/// Fills `dest` from the thread-local generator.
pub fn fill<T: ?Sized>(_dest: &mut T) {}
