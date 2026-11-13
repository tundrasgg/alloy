//! Fuzz target for `InnerFrame::decode` and `InnerFrame::decode_reassembled`,
//! the inner frame of a transport packet, fragment header included.
//!
//! The first 3 bytes of the input are the VNI the frame arrives on, little
//! endian; the rest is the frame. Checks that neither decoder panics, that a
//! frame that decodes has a kind carried on that VNI and encodes back to exactly
//! the bytes it was decoded from, and that `decode_reassembled` returns what
//! `decode` returns, except that it refuses a keepalive and a fragment.

#![no_main]
// The fuzz harness is test tooling, not part of Alloy. `fuzz_target!` expands to
// libFuzzer glue that opens a `std::fs::File` when `RUST_LIBFUZZER_DEBUG_PATH`
// is set. The `direct-io` ban covers the crates under `crates/` (ARFC 0005
// clause 3.1.1).
#![allow(
    clippy::disallowed_types,
    reason = "libFuzzer glue from `fuzz_target!`, outside the runtime seams"
)]

use alloy_wire::{DecodeError, FrameKind, InnerFrame, Vni};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&[a, b, c], frame_bytes)) = data.split_first_chunk::<3>() else {
        return;
    };
    // Three bytes never exceed 24 bits.
    let vni = Vni::new(u32::from_le_bytes([a, b, c, 0])).expect("3 bytes fit a VNI");

    let decoded = InnerFrame::decode(frame_bytes, vni);
    let reassembled = InnerFrame::decode_reassembled(frame_bytes, vni);
    match decoded {
        Ok(frame) => match frame.kind() {
            kind @ (FrameKind::Keepalive | FrameKind::Fragment) => assert_eq!(
                reassembled,
                Err(DecodeError::KindNotReassemblable(kind)),
                "decode_reassembled accepts {frame:?}"
            ),
            FrameKind::Control | FrameKind::Ipv4 | FrameKind::Ipv6 => assert_eq!(
                reassembled,
                Ok(frame),
                "decode_reassembled differs from decode"
            ),
        },
        Err(err) => assert_eq!(
            reassembled,
            Err(err),
            "decode_reassembled differs from decode"
        ),
    }
    let Ok(frame) = decoded else {
        return;
    };

    assert!(
        frame.kind().is_carried_on(vni),
        "{frame:?} decodes on {vni:?}, which does not carry its kind"
    );
    assert_eq!(
        frame.encoded_len(),
        frame_bytes.len(),
        "{frame:?} has an encoded length other than its input's"
    );
    let mut out = vec![0; frame_bytes.len()];
    match frame.encode(&mut out) {
        Ok(len) => assert_eq!(
            &out[..len],
            frame_bytes,
            "{frame:?} re-encodes to other bytes"
        ),
        Err(err) => panic!(
            "{frame:?} does not re-encode in {} bytes: {err}",
            frame_bytes.len()
        ),
    }
});
