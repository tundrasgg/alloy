//! A model of the byte rules of ARFC 0003 sections 3.1, 3.4 and 3.5, written
//! from the ARFC alone.
//!
//! Each `*_accepts` function says whether a version 1 receiver keeps the given
//! bytes, that is, whether they pass every fixed value, length and drop rule
//! the ARFC states for them. The property tests compare the codec's decoders
//! with these functions on generated and mutated inputs.

use crate::layout::{
    COUNTER_LIMIT, MAX_QUEUES, RESERVED, TYPE, VERSION, cookie_reply, frame, initiation,
    initiation_payload, response, response_payload, transport,
};

/// The little-endian `u16` at `at` (ARFC 0003 section 2).
#[must_use]
pub fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// The little-endian 24-bit integer at `at`, as a VNI is carried.
#[must_use]
pub fn u24_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], 0])
}

/// The little-endian `u32` at `at`.
#[must_use]
pub fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The little-endian `u64` at `at`.
#[must_use]
pub fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut le = [0; 8];
    le.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(le)
}

fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| *b == 0)
}

/// Whether the header of a handshake message is one version 1 keeps: the given
/// type and length, `version` 1, and zero `reserved` bytes (section 3.1).
fn handshake_header_ok(bytes: &[u8], type_value: u8, len: usize) -> bool {
    bytes.len() == len
        && bytes[TYPE] == type_value
        && bytes[VERSION] == 1
        && all_zero(&bytes[RESERVED])
}

/// Whether a receiver keeps an initiation (section 3.1). `mac1`, `mac2` and the
/// Noise bytes are cryptographic and not judged here.
#[must_use]
pub fn initiation_accepts(bytes: &[u8]) -> bool {
    handshake_header_ok(bytes, initiation::TYPE_VALUE, initiation::LEN)
}

/// Whether an initiator keeps a response (section 3.1): the header rules, and
/// `mac2` is 16 zero bytes.
#[must_use]
pub fn response_accepts(bytes: &[u8]) -> bool {
    handshake_header_ok(bytes, response::TYPE_VALUE, response::LEN)
        && all_zero(&bytes[response::MAC2])
}

/// Whether an initiator keeps a cookie reply on its bytes alone (section 3.1).
#[must_use]
pub fn cookie_reply_accepts(bytes: &[u8]) -> bool {
    handshake_header_ok(bytes, cookie_reply::TYPE_VALUE, cookie_reply::LEN)
}

/// Whether a receiver keeps a transport packet on its bytes alone (section
/// 3.4): type 4 and at least 26 bytes. The session, index and replay checks
/// need session state and are not judged here.
#[must_use]
pub fn transport_accepts(bytes: &[u8]) -> bool {
    bytes.len() >= transport::MIN_LEN && bytes[TYPE] == transport::TYPE_VALUE
}

/// Whether a version 1 receiver keeps a message of types 1 to 4 on its bytes
/// alone. Every other first byte is refused: 0 and 7 to 255 are reserved
/// (section 3.4), and types 5 and 6 are not decoded by the codec under test.
#[must_use]
pub fn message_accepts(bytes: &[u8]) -> bool {
    match bytes.first() {
        Some(1) => initiation_accepts(bytes),
        Some(2) => response_accepts(bytes),
        Some(3) => cookie_reply_accepts(bytes),
        Some(4) => transport_accepts(bytes),
        _ => false,
    }
}

/// Whether `queues` is one a handshake payload may announce: 1 to 64.
#[must_use]
pub fn queues_ok(queues: u16) -> bool {
    (1..=MAX_QUEUES).contains(&queues)
}

/// Whether a responder keeps a decrypted initiation payload (section 3.1):
/// 16 bytes, `queues` 1 to 64, every flag bit zero, `reserved` zero.
#[must_use]
pub fn initiation_payload_accepts(bytes: &[u8]) -> bool {
    bytes.len() == initiation_payload::LEN
        && queues_ok(u16_at(bytes, initiation_payload::QUEUES.start))
        && all_zero(&bytes[initiation_payload::FLAGS])
        && all_zero(&bytes[initiation_payload::RESERVED])
}

/// Whether an initiator keeps a decrypted response payload (section 3.1):
/// 4 bytes, `queues` 1 to 64, every flag bit zero.
#[must_use]
pub fn response_payload_accepts(bytes: &[u8]) -> bool {
    bytes.len() == response_payload::LEN
        && queues_ok(u16_at(bytes, response_payload::QUEUES.start))
        && all_zero(&bytes[response_payload::FLAGS])
}

/// The kind of a frame: the high 4 bits of its first byte (section 3.4).
#[must_use]
pub fn kind_of(first: u8) -> u8 {
    first >> 4
}

/// Whether a receiver keeps a decrypted inner frame that arrived on a block
/// bound to `vni` (sections 3.4 and 3.5).
#[must_use]
pub fn frame_accepts(bytes: &[u8], vni: u32) -> bool {
    let Some(&first) = bytes.first() else {
        return false;
    };
    match kind_of(first) {
        0 => vni == 0 && bytes == [frame::KEEPALIVE],
        1 => vni == 0 && first == frame::CONTROL && control_body_ok(bytes),
        2 => first == frame::FRAGMENT && fragment_ok(bytes),
        4 | 6 => true,
        _ => false,
    }
}

fn control_body_ok(bytes: &[u8]) -> bool {
    match bytes.get(frame::CONTROL_TYPE) {
        Some(&frame::BIND_REQUEST) => bytes.len() == frame::BIND_REQUEST_LEN,
        Some(&frame::BIND) => bytes.len() == frame::BIND_LEN,
        _ => false,
    }
}

fn fragment_ok(bytes: &[u8]) -> bool {
    if bytes.len() < frame::FRAG_MIN_LEN {
        return false;
    }
    let count = bytes[frame::FRAG_COUNT];
    let index = bytes[frame::FRAG_INDEX];
    (frame::MIN_COUNT..=frame::MAX_COUNT).contains(&count) && index < count
}

/// Whether a receiver keeps a frame reassembled from fragments received on a
/// block bound to `vni` (section 3.5): a frame it would keep, whose kind is 4
/// or 6, or 1 on VNI 0.
#[must_use]
pub fn reassembled_accepts(bytes: &[u8], vni: u32) -> bool {
    frame_accepts(bytes, vni)
        && match kind_of(bytes[0]) {
            4 | 6 => true,
            1 => vni == 0,
            _ => false,
        }
}

/// The counter reconstruction of section 3.4. With `e` equal to `H + 1`, or 0
/// when nothing was accepted, `c` is the one integer with `c = w + k * 2^32`
/// and `e - 2^31 < c <= e + 2^31`. `None` when `c` is negative, which the
/// receiver drops, or when `c` does not fit in 64 bits.
#[must_use]
pub fn reconstruct(wire: u32, highest: Option<u64>) -> Option<u64> {
    const SPAN: i128 = 1 << 32;
    let half = i128::from(COUNTER_LIMIT);
    let e = highest.map_or(0, |h| i128::from(h) + 1);
    let upper = e + half;
    let c = upper - (upper - i128::from(wire)).rem_euclid(SPAN);
    u64::try_from(c).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstruct_picks_the_value_in_the_half_open_interval() {
        // e = 0: the interval is (-2^31, 2^31].
        assert_eq!(reconstruct(0, None), Some(0));
        assert_eq!(reconstruct(0x8000_0000, None), Some(0x8000_0000));
        assert_eq!(reconstruct(0x8000_0001, None), None);
        assert_eq!(reconstruct(u32::MAX, None), None);
        // e = 2^32: the interval is (2^32 - 2^31, 2^32 + 2^31].
        assert_eq!(reconstruct(0, Some(u64::from(u32::MAX))), Some(1 << 32));
        assert_eq!(
            reconstruct(0x8000_0000, Some(u64::from(u32::MAX))),
            Some(0x1_8000_0000)
        );
        assert_eq!(
            reconstruct(0x8000_0001, Some(u64::from(u32::MAX))),
            Some(0x8000_0001)
        );
        // Past u64::MAX the value does not fit.
        assert_eq!(reconstruct(0, Some(u64::MAX)), None);
    }

    #[test]
    fn frame_model_reads_the_tables() {
        assert!(frame_accepts(&[0x00], 0));
        assert!(!frame_accepts(&[0x00], 1));
        assert!(frame_accepts(&[0x10, 1, 0, 0, 0], 0));
        assert!(frame_accepts(&[0x10, 2, 0, 0, 0, 0, 0, 0, 0], 0));
        assert!(frame_accepts(&[0x20, 0, 0, 0, 0, 0, 2, 9], 7));
        assert!(!frame_accepts(&[0x20, 0, 0, 0, 0, 2, 2, 9], 7));
        assert!(frame_accepts(&[0x45], 7));
        assert!(frame_accepts(&[0x6f], 0));
        assert!(!frame_accepts(&[0x30], 0));
        assert!(!frame_accepts(&[], 0));
    }
}
