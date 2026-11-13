//! Writes the seed corpus of every fuzz target to `fuzz/seeds/<target>/`.
//!
//! The seeds are the valid messages of the unit tests in
//! `crates/alloy-wire/src`, one file per message, so that the fuzzer starts
//! from inputs that reach every decoder's deepest checks. Run it with
//! `cargo run --manifest-path fuzz/Cargo.toml --example write_seeds` after a
//! change to the wire format, and commit the result.

// This is repository tooling, not part of Alloy: it writes files, as xtask
// does. The `direct-io` ban covers the crates under `crates/` (ARFC 0005
// clause 3.1.1).
#![allow(
    clippy::disallowed_methods,
    reason = "seed writer is repository tooling, outside the runtime seams"
)]

use std::fs;
use std::io;
use std::path::Path;

use alloy_wire::{
    CookieReply, Fragment, FragmentCount, FragmentId, FragmentKind, HandshakeCounter, Initiation,
    InitiationPayload, InnerBody, InnerFrame, Message, PacketCounter, QueueCount, QueueId,
    Response, ResponsePayload, SessionIndex, Transport, TransportHeader, Vni,
};

/// `N` bytes counting up from `start`, as the unit tests build their fields.
fn ramp<const N: usize>(start: u8) -> [u8; N] {
    let mut out = [0; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = start.wrapping_add(i as u8);
    }
    out
}

/// A queue count; `n` must be in 1 to 64.
fn queues(n: u16) -> QueueCount {
    // Callers pass constants in 1 to 64, the range `QueueCount::new` accepts.
    QueueCount::new(n).expect("queue count in range")
}

fn encode_message(message: &Message<'_>) -> Vec<u8> {
    let mut out = vec![0; 256];
    // Every message of this file is at most 152 bytes long.
    let len = message.encode(&mut out).expect("message fits 256 bytes");
    out.truncate(len);
    out
}

fn encode_frame(frame: &InnerFrame<'_>) -> Vec<u8> {
    let mut out = vec![0; frame.encoded_len()];
    // The buffer is exactly the frame's encoded length.
    frame
        .encode(&mut out)
        .expect("frame fits its encoded length");
    out
}

fn message_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let initiation = Initiation {
        sender_index: SessionIndex(0x0403_0201),
        ephemeral: ramp(0x10),
        encrypted_static: ramp(0x40),
        encrypted_payload: ramp(0x80),
        mac1: ramp(0xa0),
        mac2: ramp(0xc0),
    };
    let response = Response {
        sender_index: SessionIndex(0x0403_0201),
        receiver_index: SessionIndex(0x0807_0605),
        ephemeral: ramp(0x10),
        encrypted_payload: ramp(0x40),
        mac1: ramp(0xa0),
    };
    let cookie_reply = CookieReply {
        receiver_index: SessionIndex(0x0807_0605),
        nonce: ramp(0x10),
        encrypted_cookie: ramp(0x40),
    };
    let header = TransportHeader {
        queue: QueueId(0x0201),
        receiver_index: SessionIndex(0x0605_0403),
        counter: PacketCounter(0x0e0d_0c0b_0a09_0807),
    };
    let shortest: Vec<u8> = (0x40..0x40 + 20).collect();
    let longer: Vec<u8> = (0x40..0x40 + 84).collect();
    // Both ciphertexts are at least `Transport::MIN_CIPHERTEXT_LEN`, 20 bytes.
    let transport = |c| Transport::new(header, c).expect("ciphertext long enough");
    vec![
        (
            "initiation",
            encode_message(&Message::Initiation(initiation)),
        ),
        ("response", encode_message(&Message::Response(response))),
        (
            "cookie_reply",
            encode_message(&Message::CookieReply(cookie_reply)),
        ),
        (
            "transport_shortest",
            encode_message(&Message::Transport(transport(&shortest))),
        ),
        (
            "transport_longer",
            encode_message(&Message::Transport(transport(&longer))),
        ),
    ]
}

fn inner_frame_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let data = [9, 8, 7];
    // 0x12_3456 fits the 24 bits of a VNI.
    let vni = Vni::new(0x0012_3456).expect("VNI fits 24 bits");
    let frame = |body| encode_frame(&InnerFrame { vni, body });
    // 16 is the largest fragment count, and index 15 is below it.
    let fragment = Fragment::new(
        FragmentId(0x0403_0201),
        15,
        FragmentCount::new(16).expect("fragment count in range"),
        FragmentKind::Ipv6,
        &[0xaa, 0xbb],
    )
    .expect("fragment index below count");
    vec![
        ("keepalive", frame(InnerBody::Keepalive(&[]))),
        ("ipv4", frame(InnerBody::Ipv4(&data))),
        ("ipv6", frame(InnerBody::Ipv6(&data))),
        ("control", frame(InnerBody::Control(&data))),
        (
            "fragment",
            encode_frame(&InnerFrame {
                vni: Vni::INFRASTRUCTURE,
                body: InnerBody::Fragment(fragment),
            }),
        ),
    ]
}

fn initiation_payload_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let payload = |queues| {
        InitiationPayload {
            counter: HandshakeCounter(0x0807_0605_0403_0201),
            queues,
        }
        .to_bytes()
        .to_vec()
    };
    vec![
        ("queues_1", payload(queues(1))),
        ("queues_64", payload(queues(64))),
    ]
}

fn response_payload_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let payload = |queues| ResponsePayload { queues }.to_bytes().to_vec();
    vec![
        ("queues_1", payload(queues(1))),
        ("queues_64", payload(queues(64))),
    ]
}

fn main() -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds");
    let targets = [
        ("message", message_seeds()),
        ("inner_frame", inner_frame_seeds()),
        ("initiation_payload", initiation_payload_seeds()),
        ("response_payload", response_payload_seeds()),
    ];
    for (target, seeds) in targets {
        let dir = root.join(target);
        fs::create_dir_all(&dir)?;
        for (name, bytes) in seeds {
            fs::write(dir.join(name), bytes)?;
        }
    }
    Ok(())
}
