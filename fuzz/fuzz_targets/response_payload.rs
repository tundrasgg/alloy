//! Fuzz target for `ResponsePayload::decode`, the plaintext payload of a
//! handshake response.
//!
//! Checks that decoding never panics, and that a payload that decodes encodes
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

use alloy_wire::ResponsePayload;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(payload) = ResponsePayload::decode(data) else {
        return;
    };
    assert_eq!(
        &payload.to_bytes()[..],
        data,
        "{payload:?} re-encodes to other bytes"
    );
    let mut out = vec![0; data.len()];
    match payload.encode(&mut out) {
        Ok(len) => assert_eq!(&out[..len], data, "{payload:?} encodes to other bytes"),
        Err(err) => panic!("{payload:?} does not encode in {} bytes: {err}", data.len()),
    }
});
