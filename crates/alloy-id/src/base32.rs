//! Base32 of RFC 4648 section 6, in lower case and without padding, as join
//! tokens use it (ARFC 0002 section 3.2).

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Encodes `bytes`: 8 characters per 5 bytes, the last group cut to the
/// characters that carry bits, no `=`.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u16 = 0;
    let mut bits = 0u32;
    for byte in bytes {
        buffer = (buffer << 8) | u16::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[usize::from((buffer >> bits) & 31)]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(
            ALPHABET[usize::from((buffer << (5 - bits)) & 31)],
        ));
    }
    out
}

/// Decodes the canonical encoding of some bytes: lower case alphabet only,
/// no padding, a length that a whole number of bytes produces, and zero bits
/// after the last byte. `None` for anything else, so each byte string has
/// exactly one encoding that decodes.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    // Lengths mod 8 that encode whole bytes: 0, 2 (1 byte), 4 (2), 5 (3),
    // 7 (4).
    if matches!(text.len() % 8, 1 | 3 | 6) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u16 = 0;
    let mut bits = 0u32;
    for c in text.bytes() {
        let value = match c {
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u16::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
            buffer &= (1 << bits) - 1;
        }
    }
    (buffer == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_rfc_4648_section_10_in_lower_case() {
        for (bytes, text) in [
            ("", ""),
            ("f", "my"),
            ("fo", "mzxq"),
            ("foo", "mzxw6"),
            ("foob", "mzxw6yq"),
            ("fooba", "mzxw6ytb"),
            ("foobar", "mzxw6ytboi"),
        ] {
            assert_eq!(encode(bytes.as_bytes()), text);
            assert_eq!(decode(text).as_deref(), Some(bytes.as_bytes()));
        }
    }

    #[test]
    fn refuses_non_canonical_text() {
        for bad in ["MY", "my=", "m", "mzx", "mzxw6y", "mz", "my1", "m y"] {
            assert_eq!(decode(bad), None, "{bad}");
        }
    }

    proptest::proptest! {
        #[test]
        fn round_trips(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..200)) {
            proptest::prop_assert_eq!(decode(&encode(&bytes)), Some(bytes));
        }
    }
}
