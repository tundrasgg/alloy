//! ARFC 0003 section 3.4, the inner frame: its kinds, the bytes of the
//! keepalive, control and IP frames, the VNIs each kind is carried on, and the
//! control frame rules.

use alloy_wire::{
    Control, DecodeError, FrameKind, InnerFrame, IpPacket, IpVersion, SessionIndex, Vni,
};
use conformance_wire::layout::{MAX_VNI, frame};
use conformance_wire::model;
use proptest::prelude::*;

use crate::kit::{cat, check_both_ways, idx, seq, tenant_vnis, vni, vni0};

/// Every VNI the frame tests decode on: 0 and the tenant VNIs.
fn all_vnis() -> [Vni; 4] {
    let [a, b, c] = tenant_vnis();
    [vni0(), a, b, c]
}

/// Decodes `bytes` on `vni` and re-encodes the result; returns the frame.
fn round_trip(bytes: &[u8], on: Vni) -> InnerFrame<'_> {
    let decoded = InnerFrame::decode(bytes, on)
        .unwrap_or_else(|e| panic!("{bytes:02x?} on VNI {} dropped: {e}", on.get()));
    assert_eq!(decoded.encoded_len(), bytes.len());
    let mut out = vec![0xee; bytes.len() + 8];
    assert_eq!(decoded.encode(&mut out), Ok(bytes.len()));
    assert_eq!(&out[..bytes.len()], bytes);
    decoded
}

#[test]
fn arfc0003_s3_4_keepalive_vector() {
    let decoded = round_trip(&[0x00], vni0());
    assert_eq!(decoded, InnerFrame::Keepalive);
    assert_eq!(decoded.kind(), FrameKind::Keepalive);
    assert_eq!(InnerFrame::KEEPALIVE_BYTE, frame::KEEPALIVE);
}

#[test]
fn arfc0003_s3_4_bind_request_vector() {
    // 0x10, control type 1, vni 0xabcdef little endian.
    let bytes = [0x10, 0x01, 0xef, 0xcd, 0xab];
    let decoded = round_trip(&bytes, vni0());
    let expected = Control::BindRequest {
        vni: vni(0xab_cdef),
    };
    assert_eq!(decoded, InnerFrame::Control(expected));
    assert_eq!(decoded.kind(), FrameKind::Control);
    assert_eq!(expected.control_type(), frame::BIND_REQUEST);
    assert_eq!(expected.frame_len(), frame::BIND_REQUEST_LEN);
    assert_eq!(Control::TYPE_BIND_REQUEST, 1);
    assert_eq!(InnerFrame::CONTROL_BYTE, frame::CONTROL);
}

#[test]
fn arfc0003_s3_4_bind_vector() {
    // 0x10, control type 2, vni 0x123456, base 0x89abcdef, little endian.
    let bytes = [0x10, 0x02, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x89];
    let decoded = round_trip(&bytes, vni0());
    let expected = Control::Bind {
        vni: vni(0x12_3456),
        base: idx(0x89ab_cdef),
    };
    assert_eq!(decoded, InnerFrame::Control(expected));
    assert_eq!(expected.control_type(), frame::BIND);
    assert_eq!(expected.frame_len(), frame::BIND_LEN);
    assert_eq!(Control::TYPE_BIND, 2);
}

#[test]
fn arfc0003_s3_4_control_carries_every_24_bit_vni_and_32_bit_base() {
    for (v, base) in [
        (0, 0),
        (1, 1),
        (MAX_VNI, u32::MAX),
        (0x80_0000, 0x8000_0000),
    ] {
        let le = v.to_le_bytes();
        let (a, b, c) = (le[0], le[1], le[2]);
        assert_eq!(le[3], 0, "the VNI fits in 3 bytes");
        let request = [0x10, 0x01, a, b, c];
        assert_eq!(
            round_trip(&request, vni0()),
            InnerFrame::Control(Control::BindRequest { vni: vni(v) })
        );
        let bind = cat(&[&[0x10, 0x02, a, b, c], &base.to_le_bytes()]);
        assert_eq!(
            round_trip(&bind, vni0()),
            InnerFrame::Control(Control::Bind {
                vni: vni(v),
                base: SessionIndex(base),
            })
        );
    }
}

/// An IPv4 header, 20 bytes, as the virtual switch hands it to AL.
fn ipv4_vector() -> Vec<u8> {
    cat(&[
        &[
            0x45, 0x00, 0x00, 0x14, 0x00, 0x01, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00,
        ],
        &[10, 0, 0, 1, 10, 0, 0, 2],
    ])
}

/// An IPv6 header, 40 bytes.
fn ipv6_vector() -> Vec<u8> {
    cat(&[
        &[0x60, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3b, 0x40],
        &seq(0xf0, 16),
        &seq(0x10, 16),
    ])
}

#[test]
fn arfc0003_s3_4_ip_frame_vectors_are_the_whole_frame() {
    for (bytes, version, kind) in [
        (ipv4_vector(), IpVersion::V4, FrameKind::Ipv4),
        (ipv6_vector(), IpVersion::V6, FrameKind::Ipv6),
    ] {
        for on in all_vnis() {
            let decoded = round_trip(&bytes, on);
            let InnerFrame::Ip(packet) = decoded else {
                panic!("{bytes:02x?} is not read as IP: {decoded:?}");
            };
            assert_eq!(packet.bytes(), bytes.as_slice());
            assert_eq!(packet.version(), version);
            assert_eq!(decoded.kind(), kind);
        }
    }
}

#[test]
fn arfc0003_s3_4_ip_frame_is_not_parsed_further() {
    // Any frame whose first 4 bits are 4 or 6 is an IP packet, whatever
    // follows: AL reads only the version field.
    for first in (0x40..=0x4f).chain(0x60..=0x6f) {
        for len in [1, 2, 19, 20, 1_500, 65_535] {
            let bytes = cat(&[&[first], &vec![0xff; len - 1]]);
            for on in [vni0(), vni(MAX_VNI)] {
                let decoded = round_trip(&bytes, on);
                let InnerFrame::Ip(packet) = decoded else {
                    panic!("first byte {first:#04x} not read as IP");
                };
                assert_eq!(packet.bytes().len(), len);
            }
        }
    }
}

#[test]
fn arfc0003_s3_4_sender_refuses_a_non_ip_packet_for_a_tenant_vni() {
    // A packet handed to AL for a VNI other than 0 whose first 4 bits are not
    // 4 or 6 is dropped: it cannot be made into an IP frame.
    assert!(IpPacket::new(&[]).is_none());
    for first in 0..=u8::MAX {
        let bytes = [first, 0x00, 0x00, 0x14];
        let built = IpPacket::new(&bytes);
        let ip = matches!(first >> 4, 4 | 6);
        assert_eq!(built.is_some(), ip, "first byte {first:#04x}");
        if let Some(packet) = built {
            assert_eq!(packet.bytes(), bytes.as_slice());
        }
    }
}

#[test]
fn arfc0003_s3_4_frame_kind_is_the_high_4_bits() {
    let kinds = [
        (0, FrameKind::Keepalive),
        (1, FrameKind::Control),
        (2, FrameKind::Fragment),
        (4, FrameKind::Ipv4),
        (6, FrameKind::Ipv6),
    ];
    for (code, kind) in kinds {
        assert_eq!(kind.code(), code);
        assert_eq!(FrameKind::from_code(code), Some(kind));
    }
    for code in [3, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 0xff] {
        assert_eq!(FrameKind::from_code(code), None, "kind {code}");
    }
    for first in 0..=u8::MAX {
        let expected = kinds
            .iter()
            .find(|(c, _)| *c == first >> 4)
            .map(|(_, k)| *k);
        assert_eq!(
            FrameKind::of_first_byte(first),
            expected,
            "first byte {first:#04x}"
        );
    }
}

#[test]
fn arfc0003_s3_4_kinds_are_carried_on_the_vnis_in_the_table() {
    for on in all_vnis() {
        let infra = on.get() == 0;
        assert_eq!(FrameKind::Keepalive.is_carried_on(on), infra);
        assert_eq!(FrameKind::Control.is_carried_on(on), infra);
        assert!(FrameKind::Fragment.is_carried_on(on));
        assert!(FrameKind::Ipv4.is_carried_on(on));
        assert!(FrameKind::Ipv6.is_carried_on(on));
    }
}

#[test]
fn arfc0003_s3_4_vni_is_24_bits() {
    assert_eq!(Vni::INFRASTRUCTURE.get(), 0);
    assert_eq!(Vni::MAX.get(), MAX_VNI);
    assert_eq!(Vni::new(MAX_VNI).map(Vni::get), Some(MAX_VNI));
    for n in [1 << 24, (1 << 24) + 1, u32::MAX] {
        assert!(Vni::new(n).is_none(), "VNI {n:#x} allowed");
    }
}

#[test]
fn arfc0003_s3_4_drops_an_empty_frame() {
    for on in all_vnis() {
        assert_eq!(InnerFrame::decode(&[], on), Err(DecodeError::Empty));
    }
}

#[test]
fn arfc0003_s3_4_drops_a_frame_kind_not_in_the_table() {
    for kind in [3u8, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
        for low in 0..16 {
            let first = (kind << 4) | low;
            for len in [1, 2, 5, 9, 64] {
                let bytes = cat(&[&[first], &vec![0; len - 1]]);
                for on in all_vnis() {
                    assert_eq!(
                        InnerFrame::decode(&bytes, on),
                        Err(DecodeError::UnknownFrameKind(kind)),
                        "first byte {first:#04x}, length {len}"
                    );
                }
            }
        }
    }
}

#[test]
fn arfc0003_s3_4_drops_a_keepalive_on_a_tenant_vni() {
    for on in tenant_vnis() {
        assert_eq!(
            InnerFrame::decode(&[0x00], on),
            Err(DecodeError::KindNotCarried {
                kind: FrameKind::Keepalive,
                vni: on
            })
        );
    }
}

#[test]
fn arfc0003_s3_4_drops_a_control_frame_on_a_tenant_vni() {
    let request = [0x10, 0x01, 0x01, 0x00, 0x00];
    let bind = [0x10, 0x02, 0x01, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00];
    for on in tenant_vnis() {
        for bytes in [&request[..], &bind[..]] {
            assert_eq!(
                InnerFrame::decode(bytes, on),
                Err(DecodeError::KindNotCarried {
                    kind: FrameKind::Control,
                    vni: on
                })
            );
        }
    }
}

#[test]
fn arfc0003_s3_4_drops_a_keepalive_that_is_not_exactly_0x00() {
    // A kind 0 first byte with low bits set.
    for first in 0x01..=0x0f {
        assert_eq!(
            InnerFrame::decode(&[first], vni0()),
            Err(DecodeError::MalformedKindByte(first))
        );
    }
    // A 0x00 followed by more bytes.
    for len in [2, 3, 17, 1_500] {
        let bytes = vec![0; len];
        assert_eq!(
            InnerFrame::decode(&bytes, vni0()),
            Err(DecodeError::WrongLength {
                expected: 1,
                found: len
            })
        );
    }
}

#[test]
fn arfc0003_s3_4_drops_a_control_frame_whose_first_byte_is_not_0x10() {
    for first in 0x11..=0x1f {
        for body in [
            &[0x01, 0x01, 0x00, 0x00][..],
            &[0x02, 0x01, 0, 0, 1, 0, 0, 0][..],
        ] {
            let bytes = cat(&[&[first], body]);
            assert_eq!(
                InnerFrame::decode(&bytes, vni0()),
                Err(DecodeError::MalformedKindByte(first))
            );
        }
    }
}

#[test]
fn arfc0003_s3_4_drops_a_control_frame_of_an_undefined_type() {
    for control_type in (0..=u8::MAX).filter(|t| !matches!(t, 1 | 2)) {
        for len in [2, 5, 9, 32] {
            let mut bytes = vec![0; len];
            bytes[0] = 0x10;
            bytes[1] = control_type;
            assert_eq!(
                InnerFrame::decode(&bytes, vni0()),
                Err(DecodeError::UnknownControlType(control_type)),
                "control type {control_type}, length {len}"
            );
        }
    }
}

#[test]
fn arfc0003_s3_4_drops_a_control_frame_without_its_type() {
    assert_eq!(
        InnerFrame::decode(&[0x10], vni0()),
        Err(DecodeError::TooShort {
            minimum: 2,
            found: 1
        })
    );
}

#[test]
fn arfc0003_s3_4_drops_a_bind_request_not_5_bytes_long() {
    let full = [0x10, 0x01, 0x01, 0x00, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
    for len in (2..=full.len()).filter(|l| *l != 5) {
        assert_eq!(
            InnerFrame::decode(&full[..len], vni0()),
            Err(DecodeError::WrongLength {
                expected: 5,
                found: len
            })
        );
    }
}

#[test]
fn arfc0003_s3_4_drops_a_bind_not_9_bytes_long() {
    let full = [
        0x10, 0x02, 0x01, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0xaa, 0xbb, 0xcc,
    ];
    for len in (2..=full.len()).filter(|l| *l != 9) {
        assert_eq!(
            InnerFrame::decode(&full[..len], vni0()),
            Err(DecodeError::WrongLength {
                expected: 9,
                found: len
            })
        );
    }
}

/// Frames to start mutating from: one of each kind, valid on VNI 0.
fn seed_frames() -> Vec<Vec<u8>> {
    vec![
        vec![0x00],
        vec![0x10, 0x01, 0x01, 0x00, 0x00],
        vec![0x10, 0x02, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x89],
        vec![0x20, 0xbe, 0xba, 0xfe, 0xca, 0x01, 0x03, 0x45, 0x00],
        vec![0x20, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x10, 0x60],
        ipv4_vector(),
        ipv6_vector(),
    ]
}

/// A frame drawn from the seed frames with up to three mutations: a byte set,
/// a truncation, or bytes appended. Most draws stay close to a valid frame, so
/// both outcomes are covered.
pub(crate) fn mutated_frame() -> impl Strategy<Value = Vec<u8>> {
    let mutation = prop_oneof![
        (any::<usize>(), any::<u8>()).prop_map(|(at, b)| (0u8, at, b)),
        (any::<usize>(), any::<u8>()).prop_map(|(at, b)| (1u8, at, b)),
        (any::<usize>(), any::<u8>()).prop_map(|(at, b)| (2u8, at, b)),
    ];
    let start = prop_oneof![
        proptest::sample::select(seed_frames()),
        proptest::collection::vec(any::<u8>(), 0..24),
    ];
    (start, proptest::collection::vec(mutation, 0..=3)).prop_map(|(mut bytes, mutations)| {
        for (op, at, b) in mutations {
            match op {
                0 if !bytes.is_empty() => {
                    let at = at % bytes.len();
                    bytes[at] = b;
                }
                1 => bytes.truncate(at % (bytes.len() + 1)),
                _ => bytes.extend(core::iter::repeat_n(b, at % 9 + 1)),
            }
        }
        bytes
    })
}

/// A VNI: 0 half the time, otherwise a tenant VNI.
pub(crate) fn any_vni() -> impl Strategy<Value = u32> {
    prop_oneof![Just(0), Just(1), Just(MAX_VNI), 0..=MAX_VNI]
}

/// Checks the fields of a kept frame against the bytes, and that it encodes
/// back to them.
pub(crate) fn check_kept(frame: InnerFrame<'_>, bytes: &[u8]) -> Result<(), TestCaseError> {
    match frame {
        InnerFrame::Keepalive => prop_assert_eq!(bytes, &[0x00][..]),
        InnerFrame::Control(Control::BindRequest { vni }) => {
            prop_assert_eq!(vni.get(), model::u24_at(bytes, 2));
        }
        InnerFrame::Control(Control::Bind { vni, base }) => {
            prop_assert_eq!(vni.get(), model::u24_at(bytes, 2));
            prop_assert_eq!(base.0, model::u32_at(bytes, 5));
        }
        InnerFrame::Fragment(f) => {
            prop_assert_eq!(f.id().0, model::u32_at(bytes, 1));
            prop_assert_eq!(f.index(), bytes[5]);
            prop_assert_eq!(f.count().get(), bytes[6]);
            prop_assert_eq!(f.data(), &bytes[7..]);
        }
        InnerFrame::Ip(p) => prop_assert_eq!(p.bytes(), bytes),
    }
    prop_assert_eq!(frame.encoded_len(), bytes.len());
    let mut out = vec![0; bytes.len()];
    prop_assert_eq!(frame.encode(&mut out), Ok(bytes.len()));
    prop_assert_eq!(out.as_slice(), bytes);
    Ok(())
}

#[test]
fn arfc0003_s3_4_frame_decoder_accepts_exactly_what_the_spec_allows() {
    check_both_ways((mutated_frame(), any_vni()), |(bytes, n)| {
        let decoded = InnerFrame::decode(&bytes, vni(n));
        prop_assert_eq!(
            decoded.is_ok(),
            model::frame_accepts(&bytes, n),
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
