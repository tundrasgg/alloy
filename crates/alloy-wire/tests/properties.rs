//! Property tests over the public API of `alloy-wire`.
//!
//! Values are drawn from a seeded generator, so every run checks the same cases
//! and a failure names the seed that produced it.
//!
//! Properties:
//!
//! - Round trip: `decode(encode(x)) == x` for every value the constructors allow.
//! - Canonical encoding: when `decode(b)` succeeds, `encode(decode(b)) == b`.
//! - Truncation: every proper prefix of a valid encoding decodes to an error or
//!   to a different valid value.
//! - Mutation: every single-byte change of a valid encoding decodes to an error
//!   or to a different valid value.
//! - Totality: no input, valid or not, makes a decoder panic.

use alloy_wire::{
    CookieReply, EncodeError, Fragment, FragmentCount, FragmentId, FragmentKind, HandshakeCounter,
    Initiation, InitiationPayload, InnerBody, InnerFrame, Message, PacketCounter, QueueCount,
    QueueId, Response, ResponsePayload, SessionIndex, Transport, TransportHeader, Vni,
};

/// Seeds every property runs on.
const SEEDS: [u64; 8] = [
    0,
    1,
    0x5eed,
    0xdead_beef,
    0x0123_4567_89ab_cdef,
    0xffff_ffff_ffff_ffff,
    42,
    0x8000_0000_0000_0000,
];

/// Values drawn per seed.
const CASES: usize = 16;

/// SplitMix64: a small, well-mixed generator, enough to spread test cases.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `lo..=hi`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }

    fn u8(&mut self) -> u8 {
        self.next() as u8
    }

    fn array<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        out.iter_mut().for_each(|b| *b = self.u8());
        out
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = self.range(0, max as u64) as usize;
        (0..len).map(|_| self.u8()).collect()
    }

    fn queues(&mut self) -> QueueCount {
        QueueCount::new(self.range(1, 64) as u16).unwrap()
    }

    fn initiation(&mut self) -> Initiation {
        Initiation {
            sender_index: SessionIndex(self.next() as u32),
            ephemeral: self.array(),
            encrypted_static: self.array(),
            encrypted_payload: self.array(),
            mac1: self.array(),
            mac2: self.array(),
        }
    }

    fn response(&mut self) -> Response {
        Response {
            sender_index: SessionIndex(self.next() as u32),
            receiver_index: SessionIndex(self.next() as u32),
            ephemeral: self.array(),
            encrypted_payload: self.array(),
            mac1: self.array(),
        }
    }

    fn cookie_reply(&mut self) -> CookieReply {
        CookieReply {
            receiver_index: SessionIndex(self.next() as u32),
            nonce: self.array(),
            encrypted_cookie: self.array(),
        }
    }

    fn initiation_payload(&mut self) -> InitiationPayload {
        InitiationPayload {
            counter: HandshakeCounter(self.next()),
            queues: self.queues(),
        }
    }

    fn response_payload(&mut self) -> ResponsePayload {
        ResponsePayload {
            queues: self.queues(),
        }
    }

    fn transport_header(&mut self) -> TransportHeader {
        TransportHeader {
            queue: QueueId(self.next() as u16),
            receiver_index: SessionIndex(self.next() as u32),
            counter: PacketCounter(self.next()),
        }
    }

    /// Ciphertext of `MIN_CIPHERTEXT_LEN` to `MIN_CIPHERTEXT_LEN + 64` bytes.
    fn ciphertext(&mut self) -> Vec<u8> {
        let len = self.range(0, 64) as usize + Transport::MIN_CIPHERTEXT_LEN;
        (0..len).map(|_| self.u8()).collect()
    }

    fn vni(&mut self) -> Vni {
        Vni::new(self.range(0, u64::from(Vni::MAX.get())) as u32).unwrap()
    }

    /// An inner frame of a random kind whose body borrows from `data`.
    fn inner_frame<'a>(&mut self, data: &'a [u8]) -> InnerFrame<'a> {
        let body = match self.range(0, 4) {
            0 => InnerBody::Keepalive(data),
            1 => InnerBody::Ipv4(data),
            2 => InnerBody::Ipv6(data),
            3 => InnerBody::Control(data),
            _ => {
                let count = FragmentCount::new(self.range(2, 16) as u8).unwrap();
                let index = self.range(0, u64::from(count.get()) - 1) as u8;
                let kind = match self.range(0, 2) {
                    0 => FragmentKind::Ipv4,
                    1 => FragmentKind::Ipv6,
                    _ => FragmentKind::Control,
                };
                let id = FragmentId(self.next() as u32);
                InnerBody::Fragment(Fragment::new(id, index, count, kind, data).unwrap())
            }
        };
        InnerFrame {
            vni: self.vni(),
            body,
        }
    }
}

/// Runs `check` on `CASES` generators for every seed, naming the seed and case
/// on failure.
fn for_all(mut check: impl FnMut(&mut Rng, &str)) {
    for seed in SEEDS {
        let mut rng = Rng(seed);
        for case in 0..CASES {
            let label = format!("seed {seed:#x}, case {case}");
            check(&mut rng, &label);
        }
    }
}

/// Checks that every proper prefix and every single-byte change of `valid`
/// decodes to an error or to a value other than the original, and that every
/// value that decodes re-encodes to the bytes it came from.
///
/// `probe(bytes)` decodes `bytes` and returns `None` on an error, or else
/// whether the value equals the original, and the value's encoding.
fn check_mutations(valid: &[u8], label: &str, probe: impl Fn(&[u8]) -> Option<(bool, Vec<u8>)>) {
    assert_eq!(
        probe(valid),
        Some((true, valid.to_vec())),
        "{label}: original"
    );
    for len in 0..valid.len() {
        let prefix = &valid[..len];
        if let Some((same, encoding)) = probe(prefix) {
            assert!(
                !same,
                "{label}: prefix of {len} bytes decodes to the original"
            );
            assert_eq!(encoding, prefix, "{label}: prefix of {len} bytes");
        }
    }
    let mut changed = valid.to_vec();
    for at in 0..valid.len() {
        for delta in 1..=255u8 {
            changed[at] = valid[at].wrapping_add(delta);
            if let Some((same, encoding)) = probe(&changed) {
                assert!(
                    !same,
                    "{label}: byte {at} plus {delta} decodes to the original"
                );
                assert_eq!(encoding, changed, "{label}: byte {at} plus {delta}");
            }
        }
        changed[at] = valid[at];
    }
}

/// Encodes through `encode(out)` into a buffer of exactly `len` bytes, after
/// checking that a buffer one byte short is refused and left untouched.
fn encode_exact(len: usize, encode: impl Fn(&mut [u8]) -> Result<usize, EncodeError>) -> Vec<u8> {
    let mut short = vec![0xa5; len.saturating_sub(1)];
    if len > 0 {
        assert_eq!(
            encode(&mut short),
            Err(EncodeError::BufferTooSmall {
                needed: len,
                available: len - 1
            })
        );
        assert!(short.iter().all(|&b| b == 0xa5), "a refused encode wrote");
    }
    let mut out = vec![0; len + 3];
    assert_eq!(encode(&mut out), Ok(len));
    assert_eq!(&out[len..], &[0; 3], "encode wrote past its length");
    out.truncate(len);
    out
}

#[test]
fn initiation_properties() {
    for_all(|rng, label| {
        let m = rng.initiation();
        let bytes = encode_exact(Initiation::LEN, |out| m.encode(out));
        assert_eq!(bytes, m.to_bytes());
        assert_eq!(Initiation::decode(&bytes).as_ref(), Ok(&m), "{label}");
        check_mutations(&bytes, label, |b| {
            Initiation::decode(b)
                .ok()
                .map(|v| (v == m, v.to_bytes().to_vec()))
        });
    });
}

#[test]
fn response_properties() {
    for_all(|rng, label| {
        let m = rng.response();
        let bytes = encode_exact(Response::LEN, |out| m.encode(out));
        assert_eq!(bytes, m.to_bytes());
        assert_eq!(Response::decode(&bytes).as_ref(), Ok(&m), "{label}");
        check_mutations(&bytes, label, |b| {
            Response::decode(b)
                .ok()
                .map(|v| (v == m, v.to_bytes().to_vec()))
        });
    });
}

#[test]
fn cookie_reply_properties() {
    for_all(|rng, label| {
        let m = rng.cookie_reply();
        let bytes = encode_exact(CookieReply::LEN, |out| m.encode(out));
        assert_eq!(bytes, m.to_bytes());
        assert_eq!(CookieReply::decode(&bytes).as_ref(), Ok(&m), "{label}");
        check_mutations(&bytes, label, |b| {
            CookieReply::decode(b)
                .ok()
                .map(|v| (v == m, v.to_bytes().to_vec()))
        });
    });
}

#[test]
fn initiation_payload_properties() {
    for_all(|rng, label| {
        let p = rng.initiation_payload();
        let bytes = encode_exact(InitiationPayload::LEN, |out| p.encode(out));
        assert_eq!(bytes, p.to_bytes());
        assert_eq!(InitiationPayload::decode(&bytes), Ok(p), "{label}");
        check_mutations(&bytes, label, |b| {
            InitiationPayload::decode(b)
                .ok()
                .map(|v| (v == p, v.to_bytes().to_vec()))
        });
    });
}

#[test]
fn response_payload_properties() {
    for_all(|rng, label| {
        let p = rng.response_payload();
        let bytes = encode_exact(ResponsePayload::LEN, |out| p.encode(out));
        assert_eq!(bytes, p.to_bytes());
        assert_eq!(ResponsePayload::decode(&bytes), Ok(p), "{label}");
        check_mutations(&bytes, label, |b| {
            ResponsePayload::decode(b)
                .ok()
                .map(|v| (v == p, v.to_bytes().to_vec()))
        });
    });
}

#[test]
fn transport_properties() {
    for_all(|rng, label| {
        let header = rng.transport_header();
        let ciphertext = rng.ciphertext();
        let m = Transport::new(header, &ciphertext).unwrap();
        let bytes = encode_exact(m.encoded_len(), |out| m.encode(out));
        assert_eq!(bytes[..16], header.to_bytes());
        assert_eq!(Transport::decode(&bytes), Ok(m), "{label}");
        check_mutations(&bytes, label, |b| {
            Transport::decode(b).ok().map(|v| {
                let mut out = vec![0; v.encoded_len()];
                v.encode(&mut out).unwrap();
                (v == m, out)
            })
        });
    });
}

#[test]
fn inner_frame_properties() {
    for_all(|rng, label| {
        let data = rng.bytes(48);
        let frame = rng.inner_frame(&data);
        let bytes = encode_exact(frame.encoded_len(), |out| frame.encode(out));
        assert_eq!(InnerFrame::decode(&bytes), Ok(frame), "{label}");
        check_mutations(&bytes, label, |b| {
            InnerFrame::decode(b).ok().map(|v| {
                let mut out = vec![0; v.encoded_len()];
                v.encode(&mut out).unwrap();
                (v == frame, out)
            })
        });
    });
}

#[test]
fn message_properties() {
    for_all(|rng, label| {
        let ciphertext = rng.ciphertext();
        let message = match rng.range(0, 3) {
            0 => Message::Initiation(rng.initiation()),
            1 => Message::Response(rng.response()),
            2 => Message::CookieReply(rng.cookie_reply()),
            _ => Message::Transport(Transport::new(rng.transport_header(), &ciphertext).unwrap()),
        };
        let mut out = [0; 256];
        let len = message.encode(&mut out).unwrap();
        let bytes = &out[..len];
        let decoded = Message::decode(bytes).unwrap();
        assert_eq!(decoded, message, "{label}");
        assert_eq!(decoded.message_type(), message.message_type(), "{label}");
        check_mutations(bytes, label, |b| {
            Message::decode(b).ok().map(|v| {
                let mut out = [0; 256];
                let len = v.encode(&mut out).unwrap();
                (v == message, out[..len].to_vec())
            })
        });
    });
}

/// Arbitrary bytes, biased so that a good share start with a known type byte
/// and version 1 and reach the deeper checks.
fn arbitrary(rng: &mut Rng) -> Vec<u8> {
    let mut bytes = rng.bytes(200);
    if rng.range(0, 3) != 0 {
        if let Some(first) = bytes.first_mut() {
            *first = rng.range(0, 6) as u8;
        }
        if let Some(second) = bytes.get_mut(1) {
            *second = 1;
        }
    }
    bytes
}

#[test]
fn arbitrary_input_never_panics_and_decodes_canonically() {
    let mut rng = Rng(0x00a1_1031);
    for case in 0..20_000 {
        let bytes = arbitrary(&mut rng);
        let label = format!("case {case}, input {bytes:02x?}");
        let mut out = [0; 256];
        if let Ok(m) = Message::decode(&bytes) {
            let len = m.encode(&mut out).unwrap();
            assert_eq!(&out[..len], &bytes[..], "{label}");
        }
        if let Ok(f) = InnerFrame::decode(&bytes) {
            let len = f.encode(&mut out).unwrap();
            assert_eq!(&out[..len], &bytes[..], "{label}");
        }
        if let Ok(p) = InitiationPayload::decode(&bytes) {
            assert_eq!(p.to_bytes()[..], bytes[..], "{label}");
        }
        if let Ok(p) = ResponsePayload::decode(&bytes) {
            assert_eq!(p.to_bytes()[..], bytes[..], "{label}");
        }
    }
}

#[test]
fn every_short_input_is_handled() {
    // Exhaustive over all inputs of up to two bytes.
    let mut inputs = vec![vec![]];
    inputs.extend((0..=255u8).map(|a| vec![a]));
    inputs.extend((0..=255u8).flat_map(|a| (0..=255u8).map(move |b| vec![a, b])));
    for bytes in inputs {
        assert!(Message::decode(&bytes).is_err(), "{bytes:02x?}");
        assert!(InnerFrame::decode(&bytes).is_err(), "{bytes:02x?}");
        assert!(InitiationPayload::decode(&bytes).is_err(), "{bytes:02x?}");
        assert!(ResponsePayload::decode(&bytes).is_err(), "{bytes:02x?}");
    }
}
