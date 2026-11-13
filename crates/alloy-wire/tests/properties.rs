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
//! - Counter reconstruction: the result matches a brute-force model of the
//!   rule in ARFC 0003 section 3.4, and is exact across the 32-bit wrap and
//!   the 2,048-counter reorder bound.

use alloy_wire::{
    COUNTER_LIMIT, Control, CookieReply, EncodeError, Fragment, FragmentCount, FragmentId,
    HandshakeCounter, Initiation, InitiationPayload, InnerFrame, IpPacket, Message, PacketCounter,
    QueueCount, Response, ResponsePayload, SessionIndex, Transport, TransportHeader, Vni,
    WireCounter, reconstruct_counter,
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
            receiver_index: SessionIndex(self.next() as u32),
            counter: WireCounter(self.next() as u32),
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

    fn control(&mut self) -> Control {
        let vni = self.vni();
        if self.range(0, 1) == 0 {
            Control::BindRequest { vni }
        } else {
            Control::Bind {
                vni,
                base: SessionIndex(self.next() as u32),
            }
        }
    }

    /// An inner frame of a random kind whose variable part borrows from `data`,
    /// with a VNI it may be carried on. `data` must be at least 1 byte and start
    /// with an IPv4 or IPv6 version nibble.
    fn inner_frame<'a>(&mut self, data: &'a [u8]) -> (InnerFrame<'a>, Vni) {
        match self.range(0, 3) {
            0 => (InnerFrame::Keepalive, Vni::INFRASTRUCTURE),
            1 => (InnerFrame::Control(self.control()), Vni::INFRASTRUCTURE),
            2 => {
                let count = FragmentCount::new(self.range(2, 16) as u8).unwrap();
                let index = self.range(0, u64::from(count.get()) - 1) as u8;
                let id = FragmentId(self.next() as u32);
                let fragment = Fragment::new(id, index, count, data).unwrap();
                (InnerFrame::Fragment(fragment), self.vni())
            }
            _ => (InnerFrame::Ip(IpPacket::new(data).unwrap()), self.vni()),
        }
    }

    /// 1 to `max` bytes whose first 4 bits are 4 or 6.
    fn ip_bytes(&mut self, max: usize) -> Vec<u8> {
        let mut data = self.bytes(max - 1);
        let version = if self.range(0, 1) == 0 { 0x40 } else { 0x60 };
        data.insert(0, version | (self.u8() & 0x0f));
        data
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
        assert_eq!(bytes[..9], header.to_bytes());
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
        let data = rng.ip_bytes(48);
        let (frame, vni) = rng.inner_frame(&data);
        let bytes = encode_exact(frame.encoded_len(), |out| frame.encode(out));
        assert_eq!(InnerFrame::decode(&bytes, vni), Ok(frame), "{label}");
        check_mutations(&bytes, label, |b| {
            InnerFrame::decode(b, vni).ok().map(|v| {
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

/// Arbitrary bytes, biased so that a good share start with a frame byte the
/// inner frame decoder accepts and reach its deeper checks.
fn arbitrary_frame(rng: &mut Rng) -> Vec<u8> {
    let mut bytes = rng.bytes(64);
    if rng.range(0, 1) == 0
        && let Some(first) = bytes.first_mut()
    {
        *first = [0x00, 0x10, 0x20, 0x45, 0x60][rng.range(0, 4) as usize];
        if let Some(second) = bytes.get_mut(1) {
            *second = rng.range(0, 3) as u8;
        }
        if let Some(count) = bytes.get_mut(6) {
            *count = rng.range(0, 17) as u8;
        }
    }
    bytes
}

/// Checks that `frame` re-encodes to `bytes`.
fn assert_canonical(frame: &InnerFrame<'_>, bytes: &[u8], label: &str) {
    let mut out = vec![0; frame.encoded_len()];
    assert_eq!(frame.encode(&mut out), Ok(bytes.len()), "{label}");
    assert_eq!(out, bytes, "{label}");
}

#[test]
fn arbitrary_input_never_panics_and_decodes_canonically() {
    let mut rng = Rng(0x00a1_1031);
    let vnis = [Vni::INFRASTRUCTURE, Vni::new(1).unwrap(), Vni::MAX];
    for case in 0..20_000 {
        let bytes = arbitrary(&mut rng);
        let label = format!("case {case}, input {bytes:02x?}");
        let mut out = [0; 256];
        if let Ok(m) = Message::decode(&bytes) {
            let len = m.encode(&mut out).unwrap();
            assert_eq!(&out[..len], &bytes[..], "{label}");
        }
        if let Ok(p) = InitiationPayload::decode(&bytes) {
            assert_eq!(p.to_bytes()[..], bytes[..], "{label}");
        }
        if let Ok(p) = ResponsePayload::decode(&bytes) {
            assert_eq!(p.to_bytes()[..], bytes[..], "{label}");
        }
        let frame = arbitrary_frame(&mut rng);
        let label = format!("case {case}, frame {frame:02x?}");
        for vni in vnis {
            if let Ok(f) = InnerFrame::decode(&frame, vni) {
                assert!(f.kind().is_carried_on(vni), "{label}");
                assert_canonical(&f, &frame, &label);
            }
            if let Ok(f) = InnerFrame::decode_reassembled(&frame, vni) {
                assert_eq!(InnerFrame::decode(&frame, vni), Ok(f), "{label}");
                assert!(
                    !matches!(f, InnerFrame::Keepalive | InnerFrame::Fragment(_)),
                    "{label}"
                );
            }
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
        let label = format!("{bytes:02x?}");
        assert!(Message::decode(&bytes).is_err(), "{label}");
        assert!(InitiationPayload::decode(&bytes).is_err(), "{label}");
        assert!(ResponsePayload::decode(&bytes).is_err(), "{label}");
        // Of the frames this short, only the keepalive on VNI 0 and IP packets
        // are valid.
        let ip = matches!(bytes.first(), Some(b) if b >> 4 == 4 || b >> 4 == 6);
        for vni in [Vni::INFRASTRUCTURE, Vni::MAX] {
            let keepalive = bytes == [0x00] && vni == Vni::INFRASTRUCTURE;
            match InnerFrame::decode(&bytes, vni) {
                Ok(f) => {
                    assert!(ip || keepalive, "{label}");
                    assert_canonical(&f, &bytes, &label);
                }
                Err(_) => assert!(!ip && !keepalive, "{label}"),
            }
        }
    }
}

/// The counter of ARFC 0003 section 3.4 by its definition: the one `c` equal
/// to `wire` modulo 2^32 with `e - 2^31 < c <= e + 2^31`, found by trying every
/// candidate near `e`. Returns it whether or not it fits in 64 bits.
fn model_counter(wire: u32, highest: Option<u64>) -> i128 {
    const SPAN: i128 = 1 << 32;
    let e = highest.map_or(0, |h| i128::from(h) + 1);
    let base = e.div_euclid(SPAN) * SPAN + i128::from(wire);
    let found: Vec<i128> = (-2..=2)
        .map(|k| base + k * SPAN)
        .filter(|&c| e - SPAN / 2 < c && c <= e + SPAN / 2)
        .collect();
    assert_eq!(found.len(), 1, "wire {wire:#x}, highest {highest:?}");
    found[0]
}

/// Checks [`reconstruct_counter`] against [`model_counter`].
fn check_against_model(wire: u32, highest: Option<u64>) {
    let model = model_counter(wire, highest);
    let got = reconstruct_counter(WireCounter(wire), highest.map(PacketCounter));
    let expected = u64::try_from(model).ok().map(PacketCounter);
    assert_eq!(got, expected, "wire {wire:#x}, highest {highest:?}");
}

/// Window positions at and around the edges the rule depends on.
fn edge_positions() -> Vec<Option<u64>> {
    let mut out = vec![None];
    for edge in [
        0,
        2047,
        2048,
        COUNTER_LIMIT / 2,
        COUNTER_LIMIT,
        (1 << 32) - 1,
        1 << 32,
        (1 << 33) + (1 << 31),
        u64::MAX - (1 << 32),
        u64::MAX,
    ] {
        for delta in [-2i64, -1, 0, 1, 2] {
            if let Some(h) = edge.checked_add_signed(delta) {
                out.push(Some(h));
            }
        }
    }
    out
}

#[test]
fn counter_reconstruction_matches_the_model() {
    let mut rng = Rng(0x00c0_0273);
    for highest in edge_positions() {
        // The low 32 bits of e, the expected next counter.
        let e = highest.map_or(0, |h| h.wrapping_add(1) as u32);
        for offset in [0u32, 1, 2, 2047, 2048, 1 << 31, (1 << 31) + 1, u32::MAX] {
            check_against_model(e.wrapping_add(offset), highest);
            check_against_model(e.wrapping_sub(offset), highest);
        }
        for _ in 0..256 {
            check_against_model(rng.next() as u32, highest);
        }
    }
    for _ in 0..20_000 {
        let highest = match rng.range(0, 2) {
            0 => None,
            1 => Some(rng.range(0, COUNTER_LIMIT - 1)),
            _ => Some(rng.next()),
        };
        check_against_model(rng.next() as u32, highest);
    }
}

#[test]
fn counter_reconstruction_is_exact_within_the_window() {
    // Every counter a sender uses is below COUNTER_LIMIT. Every such counter
    // that is not more than 2,047 below the highest accepted, including any
    // above it, reconstructs to itself.
    let mut rng = Rng(0x000e_8ac7);
    let mut highests: Vec<Option<u64>> = edge_positions()
        .into_iter()
        .filter(|h| h.is_none_or(|h| h < COUNTER_LIMIT))
        .collect();
    highests.extend((0..512).map(|_| Some(rng.range(0, COUNTER_LIMIT - 1))));
    for highest in highests {
        let low = highest.map_or(0, |h| h.saturating_sub(2047));
        let mut counters = vec![low, low + 1, COUNTER_LIMIT - 1, COUNTER_LIMIT - 2];
        if let Some(h) = highest {
            counters.extend([h, h + 1]);
            counters.extend((low..=h).filter(|_| rng.range(0, 31) == 0));
        }
        counters.extend((0..64).map(|_| rng.range(low, COUNTER_LIMIT - 1)));
        for c in counters {
            assert_eq!(
                reconstruct_counter(PacketCounter(c).wire(), highest.map(PacketCounter)),
                Some(PacketCounter(c)),
                "counter {c:#x}, highest {highest:?}"
            );
        }
    }
}

#[test]
fn counter_reconstruction_round_trips_across_the_wrap() {
    // For any window position, a counter in the interval around the expected
    // next counter reconstructs to itself, whatever its high 32 bits are.
    let mut rng = Rng(0x0077_4a90);
    for _ in 0..20_000 {
        let h = rng.range(1 << 31, u64::MAX - (1 << 31) - 1);
        let e = h + 1;
        let c = rng.range(e - (1 << 31) + 1, e + (1 << 31));
        assert_eq!(
            reconstruct_counter(PacketCounter(c).wire(), Some(PacketCounter(h))),
            Some(PacketCounter(c)),
            "counter {c:#x}, highest {h:#x}"
        );
    }
}
