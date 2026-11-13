//! The `rng` seam (ARFC 0005 section 2.1 and clause 3.1.5).

use std::fmt;

/// Seam `rng`: random bytes (ARFC 0005 clause 3.1.5).
///
/// The `os` implementation draws from the platform's cryptographically secure
/// generator. The `mem` implementation draws a deterministic stream fixed by
/// the world seed, and its output is not secret.
pub trait Rng: Send + Sync + fmt::Debug + 'static {
    /// Fills all of `dest`. An empty `dest` returns `Ok(())`.
    ///
    /// # Errors
    ///
    /// [`RngError::Unavailable`] when the platform reports that its generator
    /// cannot serve. `dest` is then unspecified. The `mem` implementation never
    /// fails.
    fn fill(&self, dest: &mut [u8]) -> Result<(), RngError>;
}

/// Why [`Rng::fill`] failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RngError {
    /// The platform random generator is unavailable.
    #[error("the platform random generator is unavailable")]
    Unavailable,
}
