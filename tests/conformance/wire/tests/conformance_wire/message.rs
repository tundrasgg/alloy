//! Properties over every message type and payload of ARFC 0003 sections 3.1
//! and 3.4: on generated and mutated bytes, each decoder keeps exactly what the
//! model of the ARFC keeps, reads each field from its offset in the table, and
//! encodes a kept value back to the same bytes.

use alloy_wire::{
    CookieReply, Initiation, InitiationPayload, Message, Response, ResponsePayload, Transport,
};
use conformance_wire::layout::{cookie_reply, initiation, response};
use conformance_wire::model;
use proptest::prelude::*;

use crate::handshake::{cookie_reply_vector, initiation_vector, response_vector};
use crate::kit::{cat, check_both_ways};

/// A message of types 1 to 4 built from random field values with every fixed
/// value right, then up to three mutations: a byte set, a truncation, or bytes
/// appended. A mutation that sets a byte favours the header bytes and the
/// response's `mac2`, where the drop rules are.
fn mutated_message() -> impl Strategy<Value = Vec<u8>> {
    let fields = (0u8..4, proptest::collection::vec(any::<u8>(), 152));
    let valid = fields.prop_map(|(kind, random)| {
        let mut bytes = match kind {
            0 => initiation_vector(),
            1 => response_vector(),
            2 => cookie_reply_vector(),
            _ => cat(&[&[0x04], &random[..60]]),
        };
        // Randomize everything but the header, and mac2 of a response.
        let keep_zero = if kind == 1 { response::MAC2 } else { 0..0 };
        let skip = if kind == 3 { 1 } else { 4 };
        for (at, byte) in bytes.iter_mut().enumerate().skip(skip) {
            if !keep_zero.contains(&at) {
                *byte = random[at];
            }
        }
        bytes
    });
    let position = prop_oneof![0usize..4, 80usize..96, any::<usize>()];
    let value = prop_oneof![Just(0u8), Just(1), Just(2), Just(4), any::<u8>()];
    let mutation = prop_oneof![
        3 => (position, value).prop_map(|(at, b)| (0u8, at, b)),
        1 => (any::<usize>(), any::<u8>()).prop_map(|(at, b)| (1u8, at, b)),
        1 => (any::<usize>(), any::<u8>()).prop_map(|(at, b)| (2u8, at, b)),
    ];
    (valid, proptest::collection::vec(mutation, 0..=3)).prop_map(|(mut bytes, mutations)| {
        for (op, at, b) in mutations {
            match op {
                0 if !bytes.is_empty() => {
                    let at = at % bytes.len();
                    bytes[at] = b;
                }
                1 => bytes.truncate(at % (bytes.len() + 1)),
                _ => bytes.extend(core::iter::repeat_n(b, at % 17 + 1)),
            }
        }
        bytes
    })
}

fn same<const N: usize>(field: &[u8; N], bytes: &[u8], range: core::ops::Range<usize>) -> bool {
    field.as_slice() == &bytes[range]
}

/// Checks a kept message's fields against the table offsets and that it
/// encodes back to `bytes`.
fn check_fields(message: &Message<'_>, bytes: &[u8]) -> Result<(), TestCaseError> {
    match message {
        Message::Initiation(m) => {
            prop_assert_eq!(m.sender_index.0, model::u32_at(bytes, 4));
            prop_assert!(same(&m.ephemeral, bytes, initiation::EPHEMERAL));
            prop_assert!(same(
                &m.encrypted_static,
                bytes,
                initiation::ENCRYPTED_STATIC
            ));
            prop_assert!(same(
                &m.encrypted_payload,
                bytes,
                initiation::ENCRYPTED_PAYLOAD
            ));
            prop_assert!(same(&m.mac1, bytes, initiation::MAC1));
            prop_assert!(same(&m.mac2, bytes, initiation::MAC2));
            prop_assert_eq!(&m.to_bytes()[..], bytes);
        }
        Message::Response(m) => {
            prop_assert_eq!(m.sender_index.0, model::u32_at(bytes, 4));
            prop_assert_eq!(m.receiver_index.0, model::u32_at(bytes, 8));
            prop_assert!(same(&m.ephemeral, bytes, response::EPHEMERAL));
            prop_assert!(same(
                &m.encrypted_payload,
                bytes,
                response::ENCRYPTED_PAYLOAD
            ));
            prop_assert!(same(&m.mac1, bytes, response::MAC1));
            prop_assert_eq!(&m.to_bytes()[..], bytes);
        }
        Message::CookieReply(m) => {
            prop_assert_eq!(m.receiver_index.0, model::u32_at(bytes, 4));
            prop_assert!(same(&m.nonce, bytes, cookie_reply::NONCE));
            prop_assert!(same(
                &m.encrypted_cookie,
                bytes,
                cookie_reply::ENCRYPTED_COOKIE
            ));
            prop_assert_eq!(&m.to_bytes()[..], bytes);
        }
        Message::Transport(t) => {
            prop_assert_eq!(t.header().receiver_index.0, model::u32_at(bytes, 1));
            prop_assert_eq!(t.header().counter.0, model::u32_at(bytes, 5));
            prop_assert_eq!(t.ciphertext(), &bytes[9..]);
        }
    }
    let mut out = vec![0; bytes.len()];
    prop_assert_eq!(message.encode(&mut out), Ok(bytes.len()));
    prop_assert_eq!(out.as_slice(), bytes);
    Ok(())
}

#[test]
fn arfc0003_s3_1_handshake_decoders_accept_exactly_what_the_spec_allows() {
    check_both_ways(mutated_message(), |bytes| {
        let initiation = Initiation::decode(&bytes).is_ok();
        let response = Response::decode(&bytes).is_ok();
        let cookie_reply = CookieReply::decode(&bytes).is_ok();
        prop_assert_eq!(initiation, model::initiation_accepts(&bytes));
        prop_assert_eq!(response, model::response_accepts(&bytes));
        prop_assert_eq!(cookie_reply, model::cookie_reply_accepts(&bytes));
        Ok([initiation, response, cookie_reply])
    });
}

#[test]
fn arfc0003_s3_4_message_decoder_accepts_exactly_what_the_spec_allows() {
    check_both_ways(mutated_message(), |bytes| {
        let decoded = Message::decode(&bytes);
        prop_assert_eq!(
            decoded.is_ok(),
            model::message_accepts(&bytes),
            "{:02x?}: {:?}",
            bytes,
            decoded
        );
        let transport = Transport::decode(&bytes).is_ok();
        prop_assert_eq!(transport, model::transport_accepts(&bytes));
        if let Ok(message) = &decoded {
            prop_assert_eq!(message.message_type().code(), bytes[0]);
            check_fields(message, &bytes)?;
        }
        Ok([decoded.is_ok(), transport])
    });
}

/// A payload plaintext: random bytes with `queues` mostly in range and
/// `flags` and `reserved` mostly zero, of length 16 or 4 or nearby.
fn payload() -> impl Strategy<Value = Vec<u8>> {
    let queues = prop_oneof![1u16..=64, Just(0), Just(65), any::<u16>()];
    let zeroish =
        prop_oneof![3 => Just(0u32), 1 => (0..32u32).prop_map(|b| 1 << b), 1 => any::<u32>()];
    let len = prop_oneof![4 => Just(16usize), 4 => Just(4usize), 1 => 0usize..24];
    (any::<u64>(), queues, zeroish.clone(), zeroish, len).prop_map(
        |(counter, queues, flags, reserved, len)| {
            let mut bytes = if len <= 4 {
                cat(&[&queues.to_le_bytes(), &(flags as u16).to_le_bytes()])
            } else {
                cat(&[
                    &counter.to_le_bytes(),
                    &queues.to_le_bytes(),
                    &(flags as u16).to_le_bytes(),
                    &reserved.to_le_bytes(),
                ])
            };
            bytes.resize(len, 0);
            bytes
        },
    )
}

#[test]
fn arfc0003_s3_1_payload_decoders_accept_exactly_what_the_spec_allows() {
    check_both_ways(payload(), |bytes| {
        let initiation = InitiationPayload::decode(&bytes);
        prop_assert_eq!(
            initiation.is_ok(),
            model::initiation_payload_accepts(&bytes),
            "{:02x?}: {:?}",
            bytes,
            initiation
        );
        if let Ok(p) = initiation {
            prop_assert_eq!(p.counter.0, model::u64_at(&bytes, 0));
            prop_assert_eq!(p.queues.get(), model::u16_at(&bytes, 8));
            prop_assert_eq!(p.to_bytes().to_vec(), bytes.clone());
        }
        let response = ResponsePayload::decode(&bytes);
        prop_assert_eq!(
            response.is_ok(),
            model::response_payload_accepts(&bytes),
            "{:02x?}: {:?}",
            bytes,
            response
        );
        if let Ok(p) = response {
            prop_assert_eq!(p.queues.get(), model::u16_at(&bytes, 0));
            prop_assert_eq!(p.to_bytes().to_vec(), bytes.clone());
        }
        Ok([initiation.is_ok(), response.is_ok()])
    });
}
