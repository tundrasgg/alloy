//! SHA-256 and HMAC-SHA-256, for request signing compatibility: the SigV4
//! signatures of the native API (ARFC 0008 clause 3.7.1,
//! `agent/eng/crypto.md`, primitives table).
//!
//! These are the only uses. Alloy's own constructions hash with BLAKE3 and
//! BLAKE2s; SHA-256 here only reproduces what SigV4 names.

use hmac::{KeyInit, Mac};
use sha2::Digest;

/// The length of a SHA-256 digest and of an HMAC-SHA-256 tag.
pub const LEN: usize = 32;

/// The SHA-256 digest of `input` (FIPS 180-4).
#[must_use]
pub fn sha256(input: &[u8]) -> [u8; LEN] {
    sha2::Sha256::digest(input).into()
}

/// HMAC-SHA-256 of `message` under `key` (RFC 2104 with SHA-256). Any key
/// length is accepted, as RFC 2104 section 3 allows.
#[must_use]
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; LEN] {
    // `new_from_slice` refuses no key length for HMAC: a key longer than the
    // block is hashed, a shorter one padded (RFC 2104 section 3).
    let Ok(mut mac) = <hmac::Hmac<sha2::Sha256> as KeyInit>::new_from_slice(key) else {
        return [0; LEN];
    };
    mac.update(message);
    mac.finalize().into_bytes().into()
}

/// Whether `a` and `b` are equal, compared in constant time for equal
/// lengths (`agent/eng/crypto.md` rule 3). Unequal lengths compare unequal.
#[must_use]
pub fn equal(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// FIPS 180-4 example "abc", and the empty input.
    #[test]
    fn sha256_matches_the_standard_examples() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// RFC 4231 test cases 1 and 2.
    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn equal_compares_contents_and_lengths() {
        assert!(equal(b"abc", b"abc"));
        assert!(!equal(b"abc", b"abd"));
        assert!(!equal(b"abc", b"ab"));
    }
}
