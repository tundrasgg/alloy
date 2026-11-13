//! ARFC 0003 section 3.4, the transport packet: its header, the reserved first
//! byte values, the shortest packet kept, and the counter on the wire and its
//! reconstruction.

use alloy_wire::{
    COUNTER_LIMIT, DecodeError, Message, MessageType, PacketCounter, TAG_LEN, TRANSPORT_HEADER_LEN,
    TRANSPORT_OVERHEAD, Transport, TransportHeader, WireCounter, reconstruct_counter,
};
use conformance_wire::layout::{self, WINDOW_BELOW, transport};
use conformance_wire::model;
use proptest::prelude::*;

use crate::handshake::{cookie_reply_vector, initiation_vector, response_vector};
use crate::kit::{cat, check, check_both_ways, idx, seq};

/// A transport packet built from the table of section 3.4: type 4,
/// `receiver_index` 0x1234_5678, `counter` 0xdead_beef, then a 1-byte frame's
/// ciphertext and its tag, 17 opaque bytes.
fn transport_vector() -> Vec<u8> {
    cat(&[
        &[0x04],
        &[0x78, 0x56, 0x34, 0x12],
        &[0xef, 0xbe, 0xad, 0xde],
        &seq(0x90, 17),
    ])
}

fn header() -> TransportHeader {
    TransportHeader {
        receiver_index: idx(0x1234_5678),
        counter: WireCounter(0xdead_beef),
    }
}

#[test]
fn arfc0003_s3_4_transport_vector_decodes_and_reencodes() {
    let bytes = transport_vector();
    assert_eq!(bytes.len(), 26);
    let decoded = Transport::decode(&bytes).expect("the vector is a valid transport packet");
    assert_eq!(decoded.header(), header());
    assert_eq!(decoded.ciphertext(), seq(0x90, 17).as_slice());
    assert_eq!(decoded.encoded_len(), 26);
    let mut out = [0xee; 64];
    assert_eq!(decoded.encode(&mut out), Ok(26));
    assert_eq!(&out[..26], bytes.as_slice());

    let ciphertext = seq(0x90, 17);
    let built = Transport::new(header(), &ciphertext).expect("17 bytes of ciphertext");
    assert_eq!(built, decoded);

    let message = Message::decode(&bytes).expect("Message::decode keeps it");
    assert_eq!(message, Message::Transport(decoded));
    assert_eq!(message.message_type(), MessageType::Transport);
    assert_eq!(MessageType::Transport.code(), 4);
    let mut out = [0; 26];
    assert_eq!(message.encode(&mut out), Ok(26));
    assert_eq!(out.as_slice(), bytes.as_slice());

    // A longer ciphertext is carried whole.
    let long = cat(&[&bytes[..9], &seq(0x00, 1_400)]);
    let decoded = Transport::decode(&long).expect("a 1,409-byte packet");
    assert_eq!(decoded.ciphertext(), &long[9..]);
    assert_eq!(decoded.encoded_len(), long.len());
}

#[test]
fn arfc0003_s3_4_header_is_the_9_bytes_of_associated_data() {
    assert_eq!(TRANSPORT_HEADER_LEN, transport::HEADER_LEN);
    let bytes = transport_vector();
    assert_eq!(header().to_bytes().as_slice(), &bytes[..9]);
    let mut out = [0xee; 9];
    assert_eq!(header().encode(&mut out), Ok(9));
    assert_eq!(out.as_slice(), &bytes[..9]);
}

#[test]
fn arfc0003_s3_4_transport_has_no_version_byte() {
    // The byte after the type is the low byte of receiver_index, whatever its
    // value: 1 is not read as a version, and 0 or 0xff not refused as one.
    for n in [0x0000_0000, 0x0000_0001, 0x0000_00ff, 0xffff_ff02, u32::MAX] {
        let h = TransportHeader {
            receiver_index: idx(n),
            counter: WireCounter(!n),
        };
        let mut bytes = h.to_bytes().to_vec();
        assert_eq!(bytes[0], 4);
        assert_eq!(&bytes[transport::RECEIVER_INDEX], &n.to_le_bytes());
        assert_eq!(&bytes[transport::COUNTER], &(!n).to_le_bytes());
        bytes.extend(seq(0, 17));
        let decoded = Transport::decode(&bytes).expect("any index and counter");
        assert_eq!(decoded.header(), h);
    }
}

#[test]
fn arfc0003_s3_4_overhead_is_25_bytes() {
    assert_eq!(TAG_LEN, transport::TAG_LEN);
    assert_eq!(TRANSPORT_OVERHEAD, transport::OVERHEAD);
    for n in [1, 2, 100, 1_400, 9_000] {
        let ciphertext = vec![0xab; n + TAG_LEN];
        let packet = Transport::new(header(), &ciphertext).expect("ciphertext of a frame");
        assert_eq!(packet.encoded_len(), n + 25, "an {n}-byte frame");
    }
}

#[test]
fn arfc0003_s3_4_drops_reserved_first_byte() {
    // Reserved first byte values: 0 and 7 to 255. Each is dropped whatever the
    // length, including the lengths of every valid message type.
    let reserved = core::iter::once(0).chain(7..=u8::MAX);
    let bodies = [
        transport_vector(),
        initiation_vector(),
        response_vector(),
        cookie_reply_vector(),
        seq(0, 1),
        seq(0, 1_500),
    ];
    for first in reserved {
        for body in &bodies {
            let mut bytes = body.clone();
            bytes[0] = first;
            assert_eq!(
                Message::decode(&bytes),
                Err(DecodeError::UnknownType(first)),
                "first byte {first} kept at length {}",
                bytes.len()
            );
            assert!(
                Transport::decode(&bytes).is_err(),
                "first byte {first} read as transport"
            );
        }
        assert_eq!(MessageType::from_code(first), None);
    }
}

#[test]
fn arfc0003_s3_4_transport_decoder_takes_type_4_only() {
    for first in (0..=u8::MAX).filter(|b| *b != 4) {
        let mut bytes = transport_vector();
        bytes[0] = first;
        assert_eq!(
            Transport::decode(&bytes),
            Err(DecodeError::WrongType {
                expected: 4,
                found: first
            })
        );
    }
}

#[test]
fn arfc0003_s3_4_drops_transport_shorter_than_26_bytes() {
    assert_eq!(Transport::MIN_LEN, transport::MIN_LEN);
    assert_eq!(Transport::MIN_CIPHERTEXT_LEN, 1 + TAG_LEN);
    let vector = transport_vector();
    assert_eq!(Message::decode(&[]), Err(DecodeError::Empty));
    assert_eq!(Transport::decode(&[]), Err(DecodeError::Empty));
    for found in 1..26 {
        let bytes = &vector[..found];
        let expected = DecodeError::TooShort { minimum: 26, found };
        assert_eq!(
            Transport::decode(bytes),
            Err(expected),
            "kept {found} bytes"
        );
        assert_eq!(Message::decode(bytes), Err(expected), "kept {found} bytes");
    }
    assert!(Transport::decode(&vector[..26]).is_ok());

    for n in 0..17 {
        let ciphertext = vec![0; n];
        assert!(
            Transport::new(header(), &ciphertext).is_none(),
            "built with {n} bytes"
        );
    }
    assert!(Transport::new(header(), &[0; 17]).is_some());
}

#[test]
fn arfc0003_s3_4_counter_field_is_the_low_32_bits() {
    let cases: [(u64, u32); 6] = [
        (0, 0),
        (1, 1),
        (0x7fff_ffff, 0x7fff_ffff),
        (0xffff_ffff, 0xffff_ffff),
        (0x1_0000_0000, 0),
        (0x0123_4567_89ab_cdef, 0x89ab_cdef),
    ];
    for (full, wire) in cases {
        assert_eq!(
            PacketCounter(full).wire(),
            WireCounter(wire),
            "counter {full:#x}"
        );
    }
}

#[test]
fn arfc0003_s3_4_counter_limit_is_2_pow_31() {
    assert_eq!(COUNTER_LIMIT, layout::COUNTER_LIMIT);
    assert_eq!(COUNTER_LIMIT, 0x8000_0000);
}

fn reconstruct(wire: u32, highest: Option<u64>) -> Option<u64> {
    reconstruct_counter(WireCounter(wire), highest.map(PacketCounter)).map(|c| c.0)
}

#[test]
fn arfc0003_s3_4_reconstructs_counter_vectors() {
    // (w, H, c): e is H + 1, or 0 for no H; c = w + k * 2^32 with
    // e - 2^31 < c <= e + 2^31.
    let cases: [(u32, Option<u64>, Option<u64>); 15] = [
        (0, None, Some(0)),
        (5, None, Some(5)),
        (0x7fff_ffff, None, Some(0x7fff_ffff)),
        // e + 2^31 is inside the interval.
        (0x8000_0000, None, Some(0x8000_0000)),
        // e - 2^31 is not, so these are negative.
        (0x8000_0001, None, None),
        (u32::MAX, None, None),
        (10, Some(9), Some(10)),
        (3, Some(2_050), Some(3)),
        (0, Some(0x7fff_fffe), Some(0)),
        // e - 2^31 = 0 is outside the interval, so w = 0 is 2^32.
        (0, Some(0x7fff_ffff), Some(0x1_0000_0000)),
        (0, Some(0x8000_0000), Some(0x1_0000_0000)),
        (1, Some(0x8000_0000), Some(0x1_0000_0001)),
        (0x8000_0001, Some(0xffff_ffff), Some(0x8000_0001)),
        (0x8000_0000, Some(0xffff_ffff), Some(0x1_8000_0000)),
        (0x0000_0005, Some(0x1_0000_0000), Some(0x1_0000_0005)),
    ];
    for (wire, highest, expected) in cases {
        assert_eq!(
            reconstruct(wire, highest),
            expected,
            "w = {wire:#x}, H = {highest:x?}"
        );
        assert_eq!(
            model::reconstruct(wire, highest),
            expected,
            "model disagrees"
        );
    }
}

#[test]
fn arfc0003_s3_4_drops_counter_reconstructed_negative() {
    // With nothing accepted, e is 0 and c is negative for every w above 2^31.
    for wire in [0x8000_0001, 0x8000_0002, 0xc000_0000, 0xffff_fffe, u32::MAX] {
        assert_eq!(reconstruct(wire, None), None, "w = {wire:#x}");
    }
    // With H small, e - 2^31 is negative and the same holds.
    for h in [0, 1, 2_047, 0x7fff_fffc] {
        let e = h + 1;
        let w = (e + 0x8000_0001) as u32;
        assert!(model::reconstruct(w, Some(h)).is_none());
        assert_eq!(reconstruct(w, Some(h)), None, "w = {w:#x}, H = {h}");
    }
}

#[test]
fn arfc0003_s3_4_reconstruction_follows_the_interval_rule() {
    let highest = prop_oneof![
        Just(None),
        any::<u64>().prop_map(Some),
        (0..COUNTER_LIMIT).prop_map(Some),
        (0..0x2_0000_0000u64).prop_map(Some),
        (u64::MAX - 0x1_0000_0000..=u64::MAX).prop_map(Some),
    ];
    check((any::<u32>(), highest), |(wire, highest)| {
        prop_assert_eq!(
            reconstruct(wire, highest),
            model::reconstruct(wire, highest)
        );
        Ok(())
    });
}

#[test]
fn arfc0003_s3_4_reconstruction_is_exact_within_the_window() {
    // Every counter a sender uses is below 2^31, and H is a counter that
    // opened, so also below 2^31. Every true counter not more than 2,047 below
    // H is recovered exactly from its low 32 bits, however far above H it is.
    let case = prop_oneof![
        (0..COUNTER_LIMIT).prop_map(|t| (t, None)),
        (0..COUNTER_LIMIT).prop_flat_map(|h| {
            (h.saturating_sub(WINDOW_BELOW)..COUNTER_LIMIT).prop_map(move |t| (t, Some(h)))
        }),
        (0..COUNTER_LIMIT).prop_flat_map(|h| {
            (h.saturating_sub(WINDOW_BELOW)..=h).prop_map(move |t| (t, Some(h)))
        }),
    ];
    check(case, |(true_counter, highest)| {
        let wire = PacketCounter(true_counter).wire();
        let got = reconstruct_counter(wire, highest.map(PacketCounter));
        prop_assert_eq!(got, Some(PacketCounter(true_counter)));
        Ok(())
    });
}

#[test]
fn arfc0003_s3_4_transport_decoder_accepts_exactly_what_the_spec_allows() {
    let packet = (
        any::<[u8; 9]>(),
        proptest::collection::vec(any::<u8>(), 0..64),
    )
        .prop_map(|(mut head, body)| {
            // Mostly type 4, sometimes any first byte.
            if head[0] % 4 != 0 {
                head[0] = 4;
            }
            cat(&[&head, &body])
        });
    let truncated = (packet, 0usize..80).prop_map(|(bytes, cut)| {
        let keep = bytes.len().min(cut.max(1));
        bytes[..keep].to_vec()
    });
    check_both_ways(truncated, |bytes| {
        let decoded = Transport::decode(&bytes);
        let kept = decoded.is_ok();
        prop_assert_eq!(kept, model::transport_accepts(&bytes));
        if let Ok(t) = decoded {
            prop_assert_eq!(t.header().receiver_index.0, model::u32_at(&bytes, 1));
            prop_assert_eq!(t.header().counter.0, model::u32_at(&bytes, 5));
            prop_assert_eq!(t.ciphertext(), &bytes[9..]);
            let mut out = vec![0; bytes.len()];
            prop_assert_eq!(t.encode(&mut out), Ok(bytes.len()));
            prop_assert_eq!(out, bytes);
        }
        Ok([kept])
    });
}
