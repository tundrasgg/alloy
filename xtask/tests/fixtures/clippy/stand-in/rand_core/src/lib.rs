//! Stand-in for rand_core 0.9.5, with its `os_rng` feature: the traits and the
//! operating system generator at the paths the real crate has them (`src/lib.rs`
//! and `src/os.rs` of rand_core 0.9.5). Nothing here reaches the operating
//! system: the stand-in `getrandom` always fails.

/// An infallible generator.
pub trait RngCore {
    /// The next random `u64`.
    fn next_u64(&mut self) -> u64;
}

/// A fallible generator.
pub trait TryRngCore {
    /// Why a draw failed.
    type Error;
    /// The next random `u64`, or why there is none.
    fn try_next_u64(&mut self) -> Result<u64, Self::Error>;
}

/// A generator built from a seed.
pub trait SeedableRng: Sized {
    /// The seed.
    type Seed: Default + AsMut<[u8]>;

    /// Builds the generator from `seed`.
    fn from_seed(seed: Self::Seed) -> Self;

    /// Seeds from the operating system, in the real crate, and panics when it
    /// cannot.
    fn from_os_rng() -> Self {
        match Self::try_from_os_rng() {
            Ok(rng) => rng,
            Err(err) => panic!("from_os_rng failed: {err}"),
        }
    }

    /// Seeds from the operating system, in the real crate.
    fn try_from_os_rng() -> Result<Self, getrandom::Error> {
        let mut seed = Self::Seed::default();
        getrandom::fill(seed.as_mut())?;
        Ok(Self::from_seed(seed))
    }
}

/// The operating system generator, in the real crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsRng;

/// Why [`OsRng`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsError(getrandom::Error);

impl TryRngCore for OsRng {
    type Error = OsError;

    fn try_next_u64(&mut self) -> Result<u64, OsError> {
        getrandom::u64().map_err(OsError)
    }
}
