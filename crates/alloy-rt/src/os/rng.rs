//! The `os` rng (ARFC 0005 clause 3.1.5): the platform's cryptographically
//! secure generator, through `getrandom`.

use crate::{Rng, RngError};

/// Seam `rng`, implementation `os`.
#[derive(Debug)]
pub(super) struct OsRng;

impl Rng for OsRng {
    fn fill(&self, dest: &mut [u8]) -> Result<(), RngError> {
        if dest.is_empty() {
            return Ok(());
        }
        getrandom::fill(dest).map_err(|_| RngError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_fills_every_byte_and_differs() {
        assert_eq!(OsRng.fill(&mut []), Ok(()));
        // 64 zero bytes from a working generator have probability 2^-512.
        let mut a = [0u8; 64];
        let mut b = [0u8; 64];
        OsRng.fill(&mut a).unwrap();
        OsRng.fill(&mut b).unwrap();
        assert_ne!(a, [0; 64]);
        assert_ne!(a, b);
        let mut big = vec![0u8; 1 << 20];
        OsRng.fill(&mut big).unwrap();
        assert!(big[(1 << 20) - 64..].iter().any(|b| *b != 0));
    }
}
