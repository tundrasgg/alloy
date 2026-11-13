//! ARFC 0003 section 3.1: the bytes of the initiation (type 1), the response
//! (type 2), the cookie reply (type 3) and the two handshake payloads, and every
//! fixed value and drop rule the section states for them.

use alloy_wire::{
    CookieReply, DecodeError, HandshakeCounter, Initiation, InitiationPayload, MAX_QUEUES, Message,
    MessageType, QueueCount, Response, ResponsePayload, VERSION,
};
use conformance_wire::layout::{
    RESERVED, VERSION as VERSION_AT, cookie_reply, initiation, initiation_payload, response,
    response_payload,
};

use crate::kit::{cat, idx, seq, seq_array};

/// An initiation built from the table of section 3.1: type 1, version 1, zero
/// reserved, `sender_index` 0x1234_5678, then each opaque field counting up
/// from its own start byte, and a non-zero `mac2` as a cookie holder sends.
pub(crate) fn initiation_vector() -> Vec<u8> {
    cat(&[
        &[0x01, 0x01, 0x00, 0x00],
        &[0x78, 0x56, 0x34, 0x12],
        &seq(0x20, 32),
        &seq(0x40, 48),
        &seq(0x70, 32),
        &seq(0xa0, 16),
        &seq(0xc0, 16),
    ])
}

/// A response built from the table of section 3.1: type 2, version 1, zero
/// reserved, `sender_index` 0x89ab_cdef, `receiver_index` 0x1234_5678, opaque
/// fields counting up, and `mac2` zero.
pub(crate) fn response_vector() -> Vec<u8> {
    cat(&[
        &[0x02, 0x01, 0x00, 0x00],
        &[0xef, 0xcd, 0xab, 0x89],
        &[0x78, 0x56, 0x34, 0x12],
        &seq(0x20, 32),
        &seq(0x50, 20),
        &seq(0xa0, 16),
        &[0x00; 16],
    ])
}

/// A cookie reply built from the table of section 3.1: type 3, version 1,
/// zero reserved, `receiver_index` 0x1234_5678, `nonce` and `encrypted cookie`
/// counting up.
pub(crate) fn cookie_reply_vector() -> Vec<u8> {
    cat(&[
        &[0x03, 0x01, 0x00, 0x00],
        &[0x78, 0x56, 0x34, 0x12],
        &seq(0x30, 24),
        &seq(0x60, 32),
    ])
}

/// Every handshake vector with its length.
fn handshake_vectors() -> [(Vec<u8>, usize); 3] {
    [
        (initiation_vector(), initiation::LEN),
        (response_vector(), response::LEN),
        (cookie_reply_vector(), cookie_reply::LEN),
    ]
}

/// Decodes `bytes` with the decoder of its own type.
fn decode_own(bytes: &[u8], len: usize) -> Result<(), DecodeError> {
    match len {
        initiation::LEN => Initiation::decode(bytes).map(drop),
        response::LEN => Response::decode(bytes).map(drop),
        _ => CookieReply::decode(bytes).map(drop),
    }
}

/// Asserts that both the type's decoder and `Message::decode` drop `bytes`.
fn assert_dropped(bytes: &[u8], len: usize, expected: DecodeError) {
    assert_eq!(
        decode_own(bytes, len),
        Err(expected),
        "type decoder kept {bytes:02x?}"
    );
    assert_eq!(
        Message::decode(bytes).map(drop),
        Err(expected),
        "Message::decode kept {bytes:02x?}"
    );
}

#[test]
fn arfc0003_s3_1_initiation_vector_decodes_and_reencodes() {
    let bytes = initiation_vector();
    assert_eq!(bytes.len(), 152);
    let decoded = Initiation::decode(&bytes).expect("the vector is a valid initiation");
    assert_eq!(decoded.sender_index, idx(0x1234_5678));
    assert_eq!(decoded.ephemeral, seq_array::<32>(0x20));
    assert_eq!(decoded.encrypted_static, seq_array::<48>(0x40));
    assert_eq!(decoded.encrypted_payload, seq_array::<32>(0x70));
    assert_eq!(decoded.mac1, seq_array::<16>(0xa0));
    assert_eq!(decoded.mac2, seq_array::<16>(0xc0));
    assert_eq!(decoded.to_bytes().as_slice(), bytes.as_slice());

    let mut out = [0xee; 200];
    assert_eq!(decoded.encode(&mut out), Ok(152));
    assert_eq!(&out[..152], bytes.as_slice());

    let message = Message::decode(&bytes).expect("Message::decode keeps it");
    assert_eq!(message, Message::Initiation(decoded));
    assert_eq!(message.message_type(), MessageType::Initiation);
    assert_eq!(MessageType::Initiation.code(), 1);
    let mut out = [0; 152];
    assert_eq!(message.encode(&mut out), Ok(152));
    assert_eq!(out.as_slice(), bytes.as_slice());
}

#[test]
fn arfc0003_s3_1_response_vector_decodes_and_reencodes() {
    let bytes = response_vector();
    assert_eq!(bytes.len(), 96);
    let decoded = Response::decode(&bytes).expect("the vector is a valid response");
    assert_eq!(decoded.sender_index, idx(0x89ab_cdef));
    assert_eq!(decoded.receiver_index, idx(0x1234_5678));
    assert_eq!(decoded.ephemeral, seq_array::<32>(0x20));
    assert_eq!(decoded.encrypted_payload, seq_array::<20>(0x50));
    assert_eq!(decoded.mac1, seq_array::<16>(0xa0));
    assert_eq!(decoded.to_bytes().as_slice(), bytes.as_slice());

    let mut out = [0xee; 96];
    assert_eq!(decoded.encode(&mut out), Ok(96));
    assert_eq!(out.as_slice(), bytes.as_slice());

    let message = Message::decode(&bytes).expect("Message::decode keeps it");
    assert_eq!(message, Message::Response(decoded));
    assert_eq!(message.message_type(), MessageType::Response);
    assert_eq!(MessageType::Response.code(), 2);
}

#[test]
fn arfc0003_s3_1_cookie_reply_vector_decodes_and_reencodes() {
    let bytes = cookie_reply_vector();
    assert_eq!(bytes.len(), 64);
    let decoded = CookieReply::decode(&bytes).expect("the vector is a valid cookie reply");
    assert_eq!(decoded.receiver_index, idx(0x1234_5678));
    assert_eq!(decoded.nonce, seq_array::<24>(0x30));
    assert_eq!(decoded.encrypted_cookie, seq_array::<32>(0x60));
    assert_eq!(decoded.to_bytes().as_slice(), bytes.as_slice());

    let mut out = [0xee; 64];
    assert_eq!(decoded.encode(&mut out), Ok(64));
    assert_eq!(out.as_slice(), bytes.as_slice());

    let message = Message::decode(&bytes).expect("Message::decode keeps it");
    assert_eq!(message, Message::CookieReply(decoded));
    assert_eq!(message.message_type(), MessageType::CookieReply);
    assert_eq!(MessageType::CookieReply.code(), 3);
}

#[test]
fn arfc0003_s3_1_message_lengths_are_152_96_and_64() {
    assert_eq!(Initiation::LEN, initiation::LEN);
    assert_eq!(Response::LEN, response::LEN);
    assert_eq!(CookieReply::LEN, cookie_reply::LEN);
    assert_eq!(VERSION, 1);
}

#[test]
fn arfc0003_s3_1_noise_bytes_are_the_fields_inside_noise_in_order() {
    assert_eq!(Initiation::NOISE_RANGE, initiation::NOISE);
    assert_eq!(Response::NOISE_RANGE, response::NOISE);

    // In the vectors the Noise bytes are ephemeral || encrypted static ||
    // encrypted payload, and ephemeral || encrypted payload.
    let bytes = initiation_vector();
    let expected = cat(&[&seq(0x20, 32), &seq(0x40, 48), &seq(0x70, 32)]);
    assert_eq!(&bytes[Initiation::NOISE_RANGE], expected.as_slice());
    let bytes = response_vector();
    let expected = cat(&[&seq(0x20, 32), &seq(0x50, 20)]);
    assert_eq!(&bytes[Response::NOISE_RANGE], expected.as_slice());
}

#[test]
fn arfc0003_s3_1_mac1_covers_every_byte_before_it() {
    assert_eq!(Initiation::MAC1_OFFSET, initiation::MAC1.start);
    assert_eq!(Response::MAC1_OFFSET, response::MAC1.start);
    assert_eq!(
        &initiation_vector()[Initiation::MAC1_OFFSET..][..16],
        seq(0xa0, 16).as_slice()
    );
    assert_eq!(
        &response_vector()[Response::MAC1_OFFSET..][..16],
        seq(0xa0, 16).as_slice()
    );
}

#[test]
fn arfc0003_s3_1_mac2_covers_every_byte_before_it_mac1_included() {
    assert_eq!(Initiation::MAC2_OFFSET, initiation::MAC2.start);
    assert_eq!(Response::MAC2_OFFSET, response::MAC2.start);
    assert_eq!(Initiation::MAC2_OFFSET, Initiation::MAC1_OFFSET + 16);
    assert_eq!(Response::MAC2_OFFSET, Response::MAC1_OFFSET + 16);
}

#[test]
fn arfc0003_s3_1_initiation_mac2_is_opaque_zero_or_cookie_mac() {
    // An initiator without a cookie sends 16 zero bytes, one with a cookie
    // sends a MAC: both are initiations, and the codec keeps either unchanged.
    for mac2 in [[0u8; 16], [0xff; 16], seq_array::<16>(0xc0)] {
        let mut bytes = initiation_vector();
        bytes[initiation::MAC2].copy_from_slice(&mac2);
        let decoded = Initiation::decode(&bytes).expect("mac2 is not judged by the codec");
        assert_eq!(decoded.mac2, mac2);
        assert_eq!(decoded.to_bytes().as_slice(), bytes.as_slice());
    }
}

#[test]
fn arfc0003_s3_1_indexes_are_copied_unchanged() {
    // The peer copies sender_index, unchanged, into receiver_index: every u32
    // survives a round trip through each field that carries an index.
    for n in [0, 1, 0x8000_0000, 0xffff_ffc0, u32::MAX] {
        let mut bytes = initiation_vector();
        bytes[initiation::SENDER_INDEX].copy_from_slice(&n.to_le_bytes());
        let i = Initiation::decode(&bytes).expect("any sender_index");
        assert_eq!(i.sender_index, idx(n));
        assert_eq!(i.to_bytes().as_slice(), bytes.as_slice());

        let mut bytes = response_vector();
        bytes[response::SENDER_INDEX].copy_from_slice(&n.to_le_bytes());
        bytes[response::RECEIVER_INDEX].copy_from_slice(&(!n).to_le_bytes());
        let r = Response::decode(&bytes).expect("any indexes");
        assert_eq!((r.sender_index, r.receiver_index), (idx(n), idx(!n)));
        assert_eq!(r.to_bytes().as_slice(), bytes.as_slice());

        let mut bytes = cookie_reply_vector();
        bytes[cookie_reply::RECEIVER_INDEX].copy_from_slice(&n.to_le_bytes());
        let c = CookieReply::decode(&bytes).expect("any receiver_index");
        assert_eq!(c.receiver_index, idx(n));
        assert_eq!(c.to_bytes().as_slice(), bytes.as_slice());
    }
}

#[test]
fn arfc0003_s3_1_drops_handshake_with_version_not_1() {
    for (vector, len) in handshake_vectors() {
        for version in (0..=u8::MAX).filter(|v| *v != 1) {
            let mut bytes = vector.clone();
            bytes[VERSION_AT] = version;
            assert_dropped(&bytes, len, DecodeError::UnsupportedVersion(version));
        }
    }
}

#[test]
fn arfc0003_s3_1_drops_handshake_with_nonzero_header_reserved() {
    for (vector, len) in handshake_vectors() {
        for at in RESERVED {
            for bit in 0..8 {
                let mut bytes = vector.clone();
                bytes[at] = 1 << bit;
                assert_dropped(&bytes, len, DecodeError::NonZeroReserved);
            }
            let mut bytes = vector.clone();
            bytes[at] = 0xff;
            assert_dropped(&bytes, len, DecodeError::NonZeroReserved);
        }
    }
}

#[test]
fn arfc0003_s3_1_drops_handshake_of_wrong_length() {
    for (vector, len) in handshake_vectors() {
        // Truncated: every length from 2 (type and version present) to one
        // short. Lengths 0 and 1 are below the header and also dropped.
        for found in 0..len {
            let bytes = &vector[..found];
            assert!(
                decode_own(bytes, len).is_err(),
                "kept {found} of {len} bytes"
            );
            assert!(
                Message::decode(bytes).is_err(),
                "kept {found} of {len} bytes"
            );
            if found >= 2 {
                assert_dropped(
                    bytes,
                    len,
                    DecodeError::WrongLength {
                        expected: len,
                        found,
                    },
                );
            }
        }
        // Extended: trailing zeros and trailing copies of the message.
        for extra in [1, 2, 15, 16, 64, len, 1_500 - len, 65_535 - len] {
            let mut bytes = vector.clone();
            bytes.resize(len + extra, 0);
            let found = len + extra;
            assert_dropped(
                &bytes,
                len,
                DecodeError::WrongLength {
                    expected: len,
                    found,
                },
            );
            let mut bytes = vector.clone();
            bytes.extend(vector.iter().cycle().take(extra));
            assert_dropped(
                &bytes,
                len,
                DecodeError::WrongLength {
                    expected: len,
                    found,
                },
            );
        }
    }
}

#[test]
fn arfc0003_s3_1_drops_response_with_nonzero_mac2() {
    let vector = response_vector();
    for at in response::MAC2 {
        for bit in 0..8 {
            let mut bytes = vector.clone();
            bytes[at] = 1 << bit;
            assert_dropped(&bytes, response::LEN, DecodeError::NonZeroFixed);
        }
    }
    // A response whose mac2 is a valid-looking MAC is still dropped.
    let mut bytes = vector;
    bytes[response::MAC2].copy_from_slice(&seq(0xc0, 16));
    assert_dropped(&bytes, response::LEN, DecodeError::NonZeroFixed);
}

#[test]
fn arfc0003_s3_1_response_always_carries_zero_mac2() {
    let decoded = Response::decode(&response_vector()).expect("valid response");
    let mut out = [0xee; 96];
    assert_eq!(decoded.encode(&mut out), Ok(96));
    assert_eq!(&out[response::MAC2], &[0; 16]);
    assert_eq!(&decoded.to_bytes()[response::MAC2], &[0; 16]);
}

/// An initiation payload: counter 0x0102_0304_0506_0708, 64 queues.
fn initiation_payload_vector() -> Vec<u8> {
    cat(&[
        &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01],
        &[0x40, 0x00],
        &[0x00, 0x00],
        &[0x00, 0x00, 0x00, 0x00],
    ])
}

/// A response payload: 3 queues.
fn response_payload_vector() -> Vec<u8> {
    vec![0x03, 0x00, 0x00, 0x00]
}

fn queues(n: u16) -> QueueCount {
    QueueCount::new(n).expect("1 to 64 queues")
}

#[test]
fn arfc0003_s3_1_initiation_payload_vector_decodes_and_reencodes() {
    let bytes = initiation_payload_vector();
    assert_eq!(bytes.len(), 16);
    assert_eq!(InitiationPayload::LEN, initiation_payload::LEN);
    let decoded = InitiationPayload::decode(&bytes).expect("valid payload");
    assert_eq!(decoded.counter, HandshakeCounter(0x0102_0304_0506_0708));
    assert_eq!(decoded.queues, queues(64));
    assert_eq!(decoded.to_bytes().as_slice(), bytes.as_slice());
    let built = InitiationPayload {
        counter: HandshakeCounter(0x0102_0304_0506_0708),
        queues: queues(64),
    };
    let mut out = [0xee; 16];
    assert_eq!(built.encode(&mut out), Ok(16));
    assert_eq!(out.as_slice(), bytes.as_slice());
}

#[test]
fn arfc0003_s3_1_response_payload_vector_decodes_and_reencodes() {
    let bytes = response_payload_vector();
    assert_eq!(ResponsePayload::LEN, response_payload::LEN);
    let decoded = ResponsePayload::decode(&bytes).expect("valid payload");
    assert_eq!(decoded.queues, queues(3));
    assert_eq!(decoded.to_bytes().as_slice(), bytes.as_slice());
    let mut out = [0xee; 4];
    assert_eq!(
        ResponsePayload { queues: queues(3) }.encode(&mut out),
        Ok(4)
    );
    assert_eq!(out.as_slice(), bytes.as_slice());
}

#[test]
fn arfc0003_s3_1_payload_encoders_write_zero_flags_and_reserved() {
    for n in 1..=MAX_QUEUES {
        let p = InitiationPayload {
            counter: HandshakeCounter(u64::MAX),
            queues: queues(n),
        }
        .to_bytes();
        assert_eq!(&p[initiation_payload::COUNTER], &[0xff; 8]);
        assert_eq!(&p[initiation_payload::QUEUES], &n.to_le_bytes());
        assert_eq!(&p[initiation_payload::FLAGS], &[0; 2]);
        assert_eq!(&p[initiation_payload::RESERVED], &[0; 4]);
        let r = ResponsePayload { queues: queues(n) }.to_bytes();
        assert_eq!(&r[response_payload::QUEUES], &n.to_le_bytes());
        assert_eq!(&r[response_payload::FLAGS], &[0; 2]);
    }
}

#[test]
fn arfc0003_s3_1_queue_count_is_1_to_64() {
    assert_eq!(MAX_QUEUES, 64);
    assert!(QueueCount::new(0).is_none());
    for n in 1..=64 {
        assert_eq!(QueueCount::new(n).map(QueueCount::get), Some(n));
    }
    for n in [65, 0x100, 0x8000, u16::MAX] {
        assert!(QueueCount::new(n).is_none(), "{n} queues allowed");
    }
}

/// `queues` values outside 1 to 64.
const BAD_QUEUES: [u16; 6] = [0, 65, 0x80, 0x100, 0x4000, u16::MAX];

#[test]
fn arfc0003_s3_1_drops_initiation_payload_with_queues_outside_1_to_64() {
    for n in BAD_QUEUES {
        let mut bytes = initiation_payload_vector();
        bytes[initiation_payload::QUEUES].copy_from_slice(&n.to_le_bytes());
        assert_eq!(
            InitiationPayload::decode(&bytes),
            Err(DecodeError::QueueCountOutOfRange(n))
        );
    }
    for n in [1u16, 64] {
        let mut bytes = initiation_payload_vector();
        bytes[initiation_payload::QUEUES].copy_from_slice(&n.to_le_bytes());
        assert_eq!(
            InitiationPayload::decode(&bytes).map(|p| p.queues),
            Ok(queues(n))
        );
    }
}

#[test]
fn arfc0003_s3_1_drops_initiation_payload_with_a_flag_bit_set() {
    for bit in 0..16 {
        let mut bytes = initiation_payload_vector();
        bytes[initiation_payload::FLAGS].copy_from_slice(&(1u16 << bit).to_le_bytes());
        assert_eq!(
            InitiationPayload::decode(&bytes),
            Err(DecodeError::NonZeroFixed),
            "flag bit {bit} kept"
        );
    }
}

#[test]
fn arfc0003_s3_1_drops_initiation_payload_with_nonzero_reserved() {
    for bit in 0..32 {
        let mut bytes = initiation_payload_vector();
        bytes[initiation_payload::RESERVED].copy_from_slice(&(1u32 << bit).to_le_bytes());
        assert_eq!(
            InitiationPayload::decode(&bytes),
            Err(DecodeError::NonZeroReserved),
            "reserved bit {bit} kept"
        );
    }
}

#[test]
fn arfc0003_s3_1_abandons_response_payload_with_queues_outside_1_to_64() {
    for n in BAD_QUEUES {
        let mut bytes = response_payload_vector();
        bytes[response_payload::QUEUES].copy_from_slice(&n.to_le_bytes());
        assert_eq!(
            ResponsePayload::decode(&bytes),
            Err(DecodeError::QueueCountOutOfRange(n))
        );
    }
}

#[test]
fn arfc0003_s3_1_abandons_response_payload_with_a_flag_bit_set() {
    for bit in 0..16 {
        let mut bytes = response_payload_vector();
        bytes[response_payload::FLAGS].copy_from_slice(&(1u16 << bit).to_le_bytes());
        assert_eq!(
            ResponsePayload::decode(&bytes),
            Err(DecodeError::NonZeroFixed),
            "flag bit {bit} kept"
        );
    }
}

#[test]
fn arfc0003_s3_1_payloads_have_the_sizes_shown() {
    let initiation = initiation_payload_vector();
    for len in (0..64).filter(|l| *l != 16) {
        let mut bytes = initiation.clone();
        bytes.resize(len, 0);
        assert_eq!(
            InitiationPayload::decode(&bytes),
            Err(DecodeError::WrongLength {
                expected: 16,
                found: len
            })
        );
    }
    let response = response_payload_vector();
    for len in (0..64).filter(|l| *l != 4) {
        let mut bytes = response.clone();
        bytes.resize(len, 0);
        assert_eq!(
            ResponsePayload::decode(&bytes),
            Err(DecodeError::WrongLength {
                expected: 4,
                found: len
            })
        );
    }
}
