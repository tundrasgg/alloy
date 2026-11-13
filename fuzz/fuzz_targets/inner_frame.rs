//! Fuzz target for `InnerFrame::decode`, the inner frame of a transport
//! packet, fragment header included.
//!
//! Checks that decoding never panics, and that a frame that decodes encodes
//! back to exactly the bytes it was decoded from.

#![no_main]
// The fuzz harness is test tooling, not part of Alloy. `fuzz_target!` expands to
// libFuzzer glue that opens a `std::fs::File` when `RUST_LIBFUZZER_DEBUG_PATH`
// is set. The `direct-io` ban covers the crates under `crates/` (ARFC 0005
// clause 3.1.1).
#![allow(
    clippy::disallowed_types,
    reason = "libFuzzer glue from `fuzz_target!`, outside the runtime seams"
)]

use alloy_wire::InnerFrame;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(frame) = InnerFrame::decode(data) else {
        return;
    };
    assert_eq!(
        frame.encoded_len(),
        data.len(),
        "{frame:?} has an encoded length other than its input's"
    );
    let mut out = vec![0; data.len()];
    match frame.encode(&mut out) {
        Ok(len) => assert_eq!(&out[..len], data, "{frame:?} re-encodes to other bytes"),
        Err(err) => panic!(
            "{frame:?} does not re-encode in {} bytes: {err}",
            data.len()
        ),
    }
});
