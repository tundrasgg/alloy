//! ARFC 0003 section 3.5, the fragment frame: its 7-byte header, the bounds on
//! `count` and `index`, the shortest fragment kept, and what a reassembled
//! frame may be.

use alloy_wire::{
    Control, DecodeError, FRAGMENT_HEADER_LEN, Fragment, FragmentCount, FragmentId, FrameKind,
    InnerFrame, IpPacket, MAX_FRAGMENTS, MIN_FRAGMENTS,
};
use conformance_wire::layout::frame;
use conformance_wire::model;
use proptest::prelude::*;

use crate::frame::{any_vni, check_kept, mutated_frame};
use crate::kit::{cat, check_both_ways, seq, tenant_vnis, vni, vni0};

fn count(n: u8) -> FragmentCount {
    FragmentCount::new(n).expect("2 to 16 fragments")
}

/// A fragment built from the table of section 3.5: 0x20, `frag_id`
/// 0xcafe_babe, `index` 1, `count` 3, then 3 bytes of data.
fn fragment_vector() -> Vec<u8> {
    vec![0x20, 0xbe, 0xba, 0xfe, 0xca, 0x01, 0x03, 0x45, 0x00, 0x11]
}

#[test]
fn arfc0003_s3_5_fragment_vector_decodes_and_reencodes() {
    let bytes = fragment_vector();
    let data = [0x45, 0x00, 0x11];
    let expected = Fragment::new(FragmentId(0xcafe_babe), 1, count(3), &data).expect("valid");
    for on in [vni0(), vni(1), vni((1 << 24) - 1)] {
        let decoded = InnerFrame::decode(&bytes, on).expect("a fragment on any VNI");
        assert_eq!(decoded, InnerFrame::Fragment(expected));
        assert_eq!(decoded.kind(), FrameKind::Fragment);
        let InnerFrame::Fragment(f) = decoded else {
            panic!("not a fragment: {decoded:?}");
        };
        assert_eq!(f.id(), FragmentId(0xcafe_babe));
        assert_eq!(f.index(), 1);
        assert_eq!(f.count(), count(3));
        assert_eq!(f.data(), data.as_slice());
        let mut out = [0xee; 16];
        assert_eq!(decoded.encode(&mut out), Ok(bytes.len()));
        assert_eq!(&out[..bytes.len()], bytes.as_slice());
    }
    assert_eq!(InnerFrame::FRAGMENT_BYTE, frame::FRAGMENT);
}

#[test]
fn arfc0003_s3_5_fragment_header_is_7_bytes() {
    assert_eq!(FRAGMENT_HEADER_LEN, frame::FRAG_HEADER_LEN);
    assert_eq!(Fragment::MIN_LEN, frame::FRAG_MIN_LEN);
    for len in [1, 2, 100, 1_400] {
        let data = vec![0x60; len];
        let f = Fragment::new(FragmentId(7), 0, count(2), &data).expect("valid");
        assert_eq!(InnerFrame::Fragment(f).encoded_len(), len + 7);
    }
}

#[test]
fn arfc0003_s3_5_count_is_2_to_16() {
    assert_eq!(MIN_FRAGMENTS, frame::MIN_COUNT);
    assert_eq!(MAX_FRAGMENTS, frame::MAX_COUNT);
    for n in 0..=u8::MAX {
        let allowed = (2..=16).contains(&n);
        assert_eq!(FragmentCount::new(n).is_some(), allowed, "count {n}");
    }
}

#[test]
fn arfc0003_s3_5_accepts_every_count_and_index_below_it() {
    for n in 2..=16u8 {
        for index in 0..n {
            let bytes = cat(&[&[0x20, 0x01, 0x02, 0x03, 0x04, index, n], &seq(0x40, 5)]);
            for on in [vni0(), vni(77)] {
                let decoded = InnerFrame::decode(&bytes, on).expect("a valid fragment");
                let InnerFrame::Fragment(f) = decoded else {
                    panic!("not a fragment: {decoded:?}");
                };
                assert_eq!((f.index(), f.count().get()), (index, n));
                assert_eq!(f.id(), FragmentId(0x0403_0201));
            }
            let data = [0x40];
            assert!(Fragment::new(FragmentId(0), index, count(n), &data).is_some());
        }
    }
}

#[test]
fn arfc0003_s3_5_drops_a_fragment_shorter_than_8_bytes() {
    let full = fragment_vector();
    for len in 1..8 {
        let bytes = &full[..len];
        for on in [vni0(), vni(9)] {
            assert_eq!(
                InnerFrame::decode(bytes, on),
                Err(DecodeError::TooShort {
                    minimum: 8,
                    found: len
                }),
                "kept {len} bytes"
            );
        }
    }
    // A full header with no data.
    assert!(Fragment::new(FragmentId(1), 0, count(2), &[]).is_none());
}

#[test]
fn arfc0003_s3_5_drops_a_fragment_whose_first_byte_is_not_0x20() {
    for first in 0x21..=0x2f {
        let mut bytes = fragment_vector();
        bytes[0] = first;
        for on in [vni0(), vni(9)] {
            assert_eq!(
                InnerFrame::decode(&bytes, on),
                Err(DecodeError::MalformedKindByte(first))
            );
        }
    }
}

#[test]
fn arfc0003_s3_5_drops_a_fragment_whose_count_is_outside_2_to_16() {
    for n in [0, 1, 17, 18, 32, 0x80, u8::MAX] {
        let mut bytes = fragment_vector();
        bytes[frame::FRAG_INDEX] = 0;
        bytes[frame::FRAG_COUNT] = n;
        assert_eq!(
            InnerFrame::decode(&bytes, vni0()),
            Err(DecodeError::FragmentCountOutOfRange(n)),
            "count {n} kept"
        );
    }
}

#[test]
fn arfc0003_s3_5_drops_a_fragment_whose_index_is_not_below_count() {
    for n in 2..=16u8 {
        for index in n..=u8::MAX {
            let mut bytes = fragment_vector();
            bytes[frame::FRAG_INDEX] = index;
            bytes[frame::FRAG_COUNT] = n;
            assert_eq!(
                InnerFrame::decode(&bytes, vni0()),
                Err(DecodeError::FragmentIndexOutOfRange { index, count: n }),
                "index {index} of {n} kept"
            );
            let data = [0x45];
            assert!(Fragment::new(FragmentId(0), index, count(n), &data).is_none());
        }
    }
}

/// Splits `frame` into `n` fragments with `frag_id` `id` and returns each
/// fragment frame's encoding.
fn split(frame: &[u8], id: u32, n: u8) -> Vec<Vec<u8>> {
    // n pieces of at least 1 byte: the first `len % n` get one byte more.
    let (base, extra) = (frame.len() / usize::from(n), frame.len() % usize::from(n));
    let mut pieces = Vec::new();
    let mut rest = frame;
    for i in 0..usize::from(n) {
        let (piece, tail) = rest.split_at(base + usize::from(i < extra));
        pieces.push(piece);
        rest = tail;
    }
    pieces
        .into_iter()
        .enumerate()
        .map(|(index, data)| {
            let index = u8::try_from(index).expect("at most 16 fragments");
            let f = Fragment::new(FragmentId(id), index, count(n), data).expect("valid fragment");
            let frame = InnerFrame::Fragment(f);
            let mut out = vec![0; frame.encoded_len()];
            assert_eq!(frame.encode(&mut out), Ok(out.len()));
            out
        })
        .collect()
}

#[test]
fn arfc0003_s3_5_split_frame_is_the_data_concatenated_in_index_order() {
    let ip = cat(&[&[0x45, 0x00, 0x05, 0xdc], &seq(0x00, 1_496)]);
    let bind = [0x10, 0x02, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x89];
    for (original, on) in [(&ip[..], vni(5)), (&ip[..], vni0()), (&bind[..], vni0())] {
        for n in 2..=16u8 {
            if usize::from(n) > original.len() {
                continue;
            }
            let parts = split(original, 0x0102_0304, n);
            // Received out of order: last first.
            let mut held: Vec<(u8, Vec<u8>)> = Vec::new();
            for bytes in parts.iter().rev() {
                assert_eq!(bytes[0], 0x20);
                let decoded = InnerFrame::decode(bytes, on).expect("each fragment is kept");
                let InnerFrame::Fragment(f) = decoded else {
                    panic!("not a fragment: {decoded:?}");
                };
                assert_eq!(f.count().get(), u8::try_from(parts.len()).expect("<= 16"));
                held.push((f.index(), f.data().to_vec()));
            }
            held.sort_by_key(|(index, _)| *index);
            let joined: Vec<u8> = held.into_iter().flat_map(|(_, data)| data).collect();
            assert_eq!(joined.as_slice(), original);
            let frame = InnerFrame::decode_reassembled(&joined, on).expect("reassembled frame");
            let mut out = vec![0; original.len()];
            assert_eq!(frame.encode(&mut out), Ok(original.len()));
            assert_eq!(out.as_slice(), original);
        }
    }
}

#[test]
fn arfc0003_s3_5_drops_a_reassembled_frame_that_is_not_ip_or_control_on_vni_0() {
    let ipv4 = [0x45, 0x00, 0x00, 0x14];
    let ipv6 = [0x60, 0x00, 0x00, 0x00];
    let request = [0x10, 0x01, 0x01, 0x00, 0x00];
    let fragment = fragment_vector();

    for on in [vni0(), vni(3), vni((1 << 24) - 1)] {
        // IP is kept on any VNI.
        for ip in [&ipv4[..], &ipv6[..]] {
            let packet = IpPacket::new(ip).expect("IP");
            assert_eq!(
                InnerFrame::decode_reassembled(ip, on),
                Ok(InnerFrame::Ip(packet))
            );
        }
        // A keepalive and a fragment are never split, so never reassembled.
        assert!(InnerFrame::decode_reassembled(&[0x00], on).is_err());
        assert!(InnerFrame::decode_reassembled(&fragment, on).is_err());
    }
    assert_eq!(
        InnerFrame::decode_reassembled(&[0x00], vni0()),
        Err(DecodeError::KindNotReassemblable(FrameKind::Keepalive))
    );
    assert_eq!(
        InnerFrame::decode_reassembled(&fragment, vni0()),
        Err(DecodeError::KindNotReassemblable(FrameKind::Fragment))
    );
    // A control frame only on VNI 0.
    assert_eq!(
        InnerFrame::decode_reassembled(&request, vni0()),
        Ok(InnerFrame::Control(Control::BindRequest { vni: vni(1) }))
    );
    for on in tenant_vnis() {
        assert!(InnerFrame::decode_reassembled(&request, on).is_err());
    }
}

#[test]
fn arfc0003_s3_5_reassembled_decoder_accepts_exactly_what_the_spec_allows() {
    check_both_ways((mutated_frame(), any_vni()), |(bytes, n)| {
        let decoded = InnerFrame::decode_reassembled(&bytes, vni(n));
        prop_assert_eq!(
            decoded.is_ok(),
            model::reassembled_accepts(&bytes, n),
            "{:02x?} on VNI {}: {:?}",
            bytes,
            n,
            decoded
        );
        if let Ok(frame) = decoded {
            check_kept(frame, &bytes)?;
        }
        Ok([decoded.is_ok()])
    });
}
