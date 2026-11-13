//! Byte layouts of the messages and frames of ARFC 0003 sections 3.1, 3.4 and
//! 3.5, transcribed from the ARFC's tables.
//!
//! Each field is the range of bytes it occupies. The ranges are written out as
//! numbers, not computed, and the unit tests below check that each layout's
//! fields are contiguous, start at 0, and end at the length the ARFC states.

use core::ops::Range;

/// The type byte of every message (ARFC 0003 section 2).
pub const TYPE: usize = 0;

/// The version byte of every handshake message (ARFC 0003 section 2).
pub const VERSION: usize = 1;

/// The `reserved` bytes of a handshake message header (section 3.1).
pub const RESERVED: Range<usize> = 2..4;

/// Initiation, type 1, 152 bytes (ARFC 0003 section 3.1).
pub mod initiation {
    use core::ops::Range;

    /// The type byte value.
    pub const TYPE_VALUE: u8 = 1;
    /// Length of the message.
    pub const LEN: usize = 152;
    /// `sender_index`, 4 bytes.
    pub const SENDER_INDEX: Range<usize> = 4..8;
    /// `ephemeral`, 32 bytes.
    pub const EPHEMERAL: Range<usize> = 8..40;
    /// `encrypted static`, 32 + 16 bytes.
    pub const ENCRYPTED_STATIC: Range<usize> = 40..88;
    /// `encrypted payload`, 16 + 16 bytes.
    pub const ENCRYPTED_PAYLOAD: Range<usize> = 88..120;
    /// `mac1`, 16 bytes.
    pub const MAC1: Range<usize> = 120..136;
    /// `mac2`, 16 bytes.
    pub const MAC2: Range<usize> = 136..152;
    /// The Noise message bytes: `ephemeral`, `encrypted static` and
    /// `encrypted payload`, 112 bytes.
    pub const NOISE: Range<usize> = 8..120;
    /// Every field in wire order, header fields first.
    pub const FIELDS: [Range<usize>; 9] = [
        0..1,
        1..2,
        2..4,
        SENDER_INDEX,
        EPHEMERAL,
        ENCRYPTED_STATIC,
        ENCRYPTED_PAYLOAD,
        MAC1,
        MAC2,
    ];
}

/// Response, type 2, 96 bytes (ARFC 0003 section 3.1).
pub mod response {
    use core::ops::Range;

    /// The type byte value.
    pub const TYPE_VALUE: u8 = 2;
    /// Length of the message.
    pub const LEN: usize = 96;
    /// `sender_index`, 4 bytes.
    pub const SENDER_INDEX: Range<usize> = 4..8;
    /// `receiver_index`, 4 bytes.
    pub const RECEIVER_INDEX: Range<usize> = 8..12;
    /// `ephemeral`, 32 bytes.
    pub const EPHEMERAL: Range<usize> = 12..44;
    /// `encrypted payload`, 4 + 16 bytes.
    pub const ENCRYPTED_PAYLOAD: Range<usize> = 44..64;
    /// `mac1`, 16 bytes.
    pub const MAC1: Range<usize> = 64..80;
    /// `mac2`, 16 bytes, always zero in a response.
    pub const MAC2: Range<usize> = 80..96;
    /// The Noise message bytes: `ephemeral` and `encrypted payload`, 52 bytes.
    pub const NOISE: Range<usize> = 12..64;
    /// Every field in wire order, header fields first.
    pub const FIELDS: [Range<usize>; 9] = [
        0..1,
        1..2,
        2..4,
        SENDER_INDEX,
        RECEIVER_INDEX,
        EPHEMERAL,
        ENCRYPTED_PAYLOAD,
        MAC1,
        MAC2,
    ];
}

/// Cookie reply, type 3, 64 bytes (ARFC 0003 section 3.1).
pub mod cookie_reply {
    use core::ops::Range;

    /// The type byte value.
    pub const TYPE_VALUE: u8 = 3;
    /// Length of the message.
    pub const LEN: usize = 64;
    /// `receiver_index`, 4 bytes.
    pub const RECEIVER_INDEX: Range<usize> = 4..8;
    /// `nonce`, 24 bytes.
    pub const NONCE: Range<usize> = 8..32;
    /// `encrypted cookie`, 16 + 16 bytes.
    pub const ENCRYPTED_COOKIE: Range<usize> = 32..64;
    /// Every field in wire order, header fields first.
    pub const FIELDS: [Range<usize>; 6] =
        [0..1, 1..2, 2..4, RECEIVER_INDEX, NONCE, ENCRYPTED_COOKIE];
}

/// The initiation payload plaintext, `{counter: u64, queues: u16, flags: u16,
/// reserved: u32}`, 16 bytes (ARFC 0003 section 3.1).
pub mod initiation_payload {
    use core::ops::Range;

    /// Length of the plaintext.
    pub const LEN: usize = 16;
    /// `counter`, u64.
    pub const COUNTER: Range<usize> = 0..8;
    /// `queues`, u16.
    pub const QUEUES: Range<usize> = 8..10;
    /// `flags`, u16.
    pub const FLAGS: Range<usize> = 10..12;
    /// `reserved`, u32.
    pub const RESERVED: Range<usize> = 12..16;
    /// Every field in wire order.
    pub const FIELDS: [Range<usize>; 4] = [COUNTER, QUEUES, FLAGS, RESERVED];
}

/// The response payload plaintext, `{queues: u16, flags: u16}`, 4 bytes (ARFC
/// 0003 section 3.1).
pub mod response_payload {
    use core::ops::Range;

    /// Length of the plaintext.
    pub const LEN: usize = 4;
    /// `queues`, u16.
    pub const QUEUES: Range<usize> = 0..2;
    /// `flags`, u16.
    pub const FLAGS: Range<usize> = 2..4;
    /// Every field in wire order.
    pub const FIELDS: [Range<usize>; 2] = [QUEUES, FLAGS];
}

/// Transport, type 4: a 9-byte header, then the ciphertext and its 16-byte tag
/// (ARFC 0003 section 3.4).
pub mod transport {
    use core::ops::Range;

    /// The type byte value: transport under a version 1 session.
    pub const TYPE_VALUE: u8 = 4;
    /// `receiver_index`, 4 bytes.
    pub const RECEIVER_INDEX: Range<usize> = 1..5;
    /// `counter`, the low 32 bits of the packet's counter, 4 bytes.
    pub const COUNTER: Range<usize> = 5..9;
    /// Length of the header, which is the AEAD associated data.
    pub const HEADER_LEN: usize = 9;
    /// Length of the AEAD tag.
    pub const TAG_LEN: usize = 16;
    /// Fixed overhead per packet: header and tag.
    pub const OVERHEAD: usize = 25;
    /// The shortest packet a receiver keeps: header, tag and a 1-byte frame.
    pub const MIN_LEN: usize = 26;
    /// Every header field in wire order.
    pub const HEADER_FIELDS: [Range<usize>; 3] = [0..1, RECEIVER_INDEX, COUNTER];
}

/// Inner frames (ARFC 0003 section 3.4) and the fragment frame (section 3.5).
pub mod frame {
    use core::ops::Range;

    /// The keepalive frame, whole.
    pub const KEEPALIVE: u8 = 0x00;
    /// The first byte of a control frame.
    pub const CONTROL: u8 = 0x10;
    /// The first byte of a fragment frame.
    pub const FRAGMENT: u8 = 0x20;
    /// Control type of a bind request.
    pub const BIND_REQUEST: u8 = 1;
    /// Control type of a bind.
    pub const BIND: u8 = 2;
    /// Length of a bind request frame.
    pub const BIND_REQUEST_LEN: usize = 5;
    /// Length of a bind frame.
    pub const BIND_LEN: usize = 9;
    /// `control type` in a control frame.
    pub const CONTROL_TYPE: usize = 1;
    /// `vni`, 3 bytes, in a bind request or bind.
    pub const CONTROL_VNI: Range<usize> = 2..5;
    /// `base`, 4 bytes, in a bind.
    pub const BIND_BASE: Range<usize> = 5..9;
    /// `frag_id`, 4 bytes, in a fragment frame.
    pub const FRAG_ID: Range<usize> = 1..5;
    /// `index` in a fragment frame.
    pub const FRAG_INDEX: usize = 5;
    /// `count` in a fragment frame.
    pub const FRAG_COUNT: usize = 6;
    /// Length of a fragment frame before its data.
    pub const FRAG_HEADER_LEN: usize = 7;
    /// The shortest fragment frame a receiver keeps: header and 1 byte of data.
    pub const FRAG_MIN_LEN: usize = 8;
    /// Fewest fragments.
    pub const MIN_COUNT: u8 = 2;
    /// Most fragments.
    pub const MAX_COUNT: u8 = 16;
}

/// Most sending queues a handshake payload announces (ARFC 0003 sections 3.1
/// and 7).
pub const MAX_QUEUES: u16 = 64;

/// The largest VNI, 2^24 - 1 (ARFC 0003 section 3.4).
pub const MAX_VNI: u32 = (1 << 24) - 1;

/// A sender never seals a transport packet with a counter at or above this,
/// 2^31 (ARFC 0003 section 3.4).
pub const COUNTER_LIMIT: u64 = 1 << 31;

/// The replay window: a counter more than this far below `H` is dropped (ARFC
/// 0003 section 3.4).
pub const WINDOW_BELOW: u64 = 2_047;

#[cfg(test)]
mod tests {
    use super::*;
    use core::ops::Range;

    fn assert_contiguous(fields: &[Range<usize>], len: usize) {
        let mut at = 0;
        for field in fields {
            assert_eq!(field.start, at, "field {field:?} does not follow byte {at}");
            assert!(field.end > field.start, "field {field:?} is empty");
            at = field.end;
        }
        assert_eq!(at, len, "fields end at {at}, not at {len}");
    }

    #[test]
    fn layouts_are_contiguous_and_have_the_stated_lengths() {
        assert_contiguous(&initiation::FIELDS, initiation::LEN);
        assert_contiguous(&response::FIELDS, response::LEN);
        assert_contiguous(&cookie_reply::FIELDS, cookie_reply::LEN);
        assert_contiguous(&initiation_payload::FIELDS, initiation_payload::LEN);
        assert_contiguous(&response_payload::FIELDS, response_payload::LEN);
        assert_contiguous(&transport::HEADER_FIELDS, transport::HEADER_LEN);
        assert_eq!(
            transport::HEADER_LEN + transport::TAG_LEN,
            transport::OVERHEAD
        );
        assert_eq!(transport::OVERHEAD + 1, transport::MIN_LEN);
        assert_eq!(frame::FRAG_HEADER_LEN + 1, frame::FRAG_MIN_LEN);
    }

    #[test]
    fn field_sizes_match_the_tables() {
        assert_eq!(initiation::EPHEMERAL.len(), 32);
        assert_eq!(initiation::ENCRYPTED_STATIC.len(), 32 + 16);
        assert_eq!(initiation::ENCRYPTED_PAYLOAD.len(), 16 + 16);
        assert_eq!(initiation::NOISE.len(), 112);
        assert_eq!(initiation::NOISE.start, initiation::EPHEMERAL.start);
        assert_eq!(initiation::NOISE.end, initiation::ENCRYPTED_PAYLOAD.end);
        assert_eq!(response::EPHEMERAL.len(), 32);
        assert_eq!(response::ENCRYPTED_PAYLOAD.len(), 4 + 16);
        assert_eq!(response::NOISE.len(), 52);
        assert_eq!(response::NOISE.start, response::EPHEMERAL.start);
        assert_eq!(response::NOISE.end, response::ENCRYPTED_PAYLOAD.end);
        assert_eq!(cookie_reply::NONCE.len(), 24);
        assert_eq!(cookie_reply::ENCRYPTED_COOKIE.len(), 16 + 16);
        assert_eq!(
            initiation::ENCRYPTED_PAYLOAD.len(),
            initiation_payload::LEN + 16
        );
        assert_eq!(
            response::ENCRYPTED_PAYLOAD.len(),
            response_payload::LEN + 16
        );
        assert_eq!(frame::CONTROL_VNI.len(), 3);
        assert_eq!(frame::BIND_BASE.end, frame::BIND_LEN);
        assert_eq!(frame::CONTROL_VNI.end, frame::BIND_REQUEST_LEN);
        assert_eq!(frame::FRAG_COUNT + 1, frame::FRAG_HEADER_LEN);
    }
}
