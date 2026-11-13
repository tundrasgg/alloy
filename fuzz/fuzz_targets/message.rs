//! Fuzz target for `Message::decode`, which dispatches on the type byte to the
//! initiation, response, cookie reply and transport decoders.
//!
//! Checks that decoding never panics, and that a message that decodes encodes
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

use alloy_wire::Message;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(message) = Message::decode(data) else {
        return;
    };
    // A canonical encoding is exactly as long as the input, so a buffer of that
    // length must hold it.
    let mut out = vec![0; data.len()];
    match message.encode(&mut out) {
        Ok(len) => assert_eq!(&out[..len], data, "{message:?} re-encodes to other bytes"),
        Err(err) => panic!(
            "{message:?} does not re-encode in {} bytes: {err}",
            data.len()
        ),
    }
});
