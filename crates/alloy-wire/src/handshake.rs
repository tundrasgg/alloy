//! Handshake messages and payloads (ARFC 0003 section 3.1).

use crate::codec::{Reader, Writer, exact_header, room};
use crate::error::{DecodeError, EncodeError};
use crate::message::MessageType;
use crate::types::{HandshakeCounter, QueueCount, SessionIndex};
use crate::{KEY_LEN, MAC_LEN, TAG_LEN, VERSION};

/// Writes `type`, `version` and the two zero `reserved` bytes.
fn put_header(w: &mut Writer<'_>, ty: MessageType) {
    w.u8(ty.code());
    w.u8(VERSION);
    w.u16(0);
}

/// Reads the two header `reserved` bytes and checks that they are zero.
fn take_reserved(r: &mut Reader<'_>) -> Result<(), DecodeError> {
    if r.u16()? != 0 {
        return Err(DecodeError::NonZeroReserved);
    }
    Ok(())
}

/// Copies the fixed-size encoding `bytes` into the front of `out`.
fn copy_out<const N: usize>(bytes: &[u8; N], out: &mut [u8]) -> Result<usize, EncodeError> {
    room(out, N)?.copy_from_slice(bytes);
    Ok(N)
}

/// Handshake initiation, type 1, 152 bytes (ARFC 0003 section 3.1).
///
/// `ephemeral`, `encrypted_static`, `encrypted_payload`, `mac1` and `mac2` are
/// opaque here. The decoder checks only the type, the version, the zero
/// header `reserved` bytes and the length; it does not check either MAC.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Initiation {
    /// The index the initiator chose for this handshake.
    pub sender_index: SessionIndex,
    /// The initiator's ephemeral public key.
    pub ephemeral: [u8; KEY_LEN],
    /// The initiator's static public key, sealed, with its tag.
    pub encrypted_static: [u8; KEY_LEN + TAG_LEN],
    /// The sealed [`InitiationPayload`], with its tag.
    pub encrypted_payload: [u8; InitiationPayload::LEN + TAG_LEN],
    /// `MAC(BLAKE2s("alloy/al/mac1/v1" || S), m1)` over the first
    /// [`Initiation::MAC1_OFFSET`] bytes.
    pub mac1: [u8; MAC_LEN],
    /// The cookie MAC over the first [`Initiation::MAC2_OFFSET`] bytes, or
    /// zeros when the initiator holds no cookie.
    pub mac2: [u8; MAC_LEN],
}

impl Initiation {
    /// Length of an initiation, in bytes.
    pub const LEN: usize = 152;

    /// Offset of `mac1`: the bytes `m1` that `mac1` covers are those before it.
    pub const MAC1_OFFSET: usize = 120;

    /// Offset of `mac2`: the bytes `m2` that `mac2` covers are those before it.
    pub const MAC2_OFFSET: usize = Self::MAC1_OFFSET + MAC_LEN;

    /// Offset and length of the Noise message bytes: `ephemeral`,
    /// `encrypted_static` and `encrypted_payload`, in that order.
    pub const NOISE_RANGE: std::ops::Range<usize> = 8..Self::MAC1_OFFSET;

    /// Decodes an initiation.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Empty`], [`DecodeError::WrongType`],
    /// [`DecodeError::UnsupportedVersion`], [`DecodeError::WrongLength`] when
    /// `bytes` is not [`Initiation::LEN`] long, or
    /// [`DecodeError::NonZeroReserved`].
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = exact_header(bytes, MessageType::Initiation.code(), Self::LEN)?;
        take_reserved(&mut r)?;
        Ok(Self {
            sender_index: SessionIndex(r.u32()?),
            ephemeral: r.array()?,
            encrypted_static: r.array()?,
            encrypted_payload: r.array()?,
            mac1: r.array()?,
            mac2: r.array()?,
        })
    }

    /// The encoding of this initiation.
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        let mut w = Writer::new(&mut out);
        put_header(&mut w, MessageType::Initiation);
        w.u32(self.sender_index.0);
        w.put(&self.ephemeral);
        w.put(&self.encrypted_static);
        w.put(&self.encrypted_payload);
        w.put(&self.mac1);
        w.put(&self.mac2);
        out
    }

    /// Writes the encoding to the front of `out` and returns its length,
    /// [`Initiation::LEN`].
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than
    /// [`Initiation::LEN`]; nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        copy_out(&self.to_bytes(), out)
    }
}

/// Handshake response, type 2, 96 bytes (ARFC 0003 section 3.1).
///
/// `mac2` is not a field: a response always carries 16 zero bytes there, the
/// encoder writes them, and the decoder rejects a response where they are not
/// zero. The other byte arrays are opaque; the decoder does not check `mac1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Response {
    /// The index the responder chose for this session.
    pub sender_index: SessionIndex,
    /// The `sender_index` of the initiation answered.
    pub receiver_index: SessionIndex,
    /// The responder's ephemeral public key.
    pub ephemeral: [u8; KEY_LEN],
    /// The sealed [`ResponsePayload`], with its tag.
    pub encrypted_payload: [u8; ResponsePayload::LEN + TAG_LEN],
    /// `MAC(BLAKE2s("alloy/al/mac1/v1" || S), m1)` over the first
    /// [`Response::MAC1_OFFSET`] bytes.
    pub mac1: [u8; MAC_LEN],
}

impl Response {
    /// Length of a response, in bytes.
    pub const LEN: usize = 96;

    /// Offset of `mac1`: the bytes `m1` that `mac1` covers are those before it.
    pub const MAC1_OFFSET: usize = 64;

    /// Offset of `mac2`, which is always 16 zero bytes in a response.
    pub const MAC2_OFFSET: usize = Self::MAC1_OFFSET + MAC_LEN;

    /// Offset and length of the Noise message bytes: `ephemeral` and
    /// `encrypted_payload`, in that order.
    pub const NOISE_RANGE: std::ops::Range<usize> = 12..Self::MAC1_OFFSET;

    /// Decodes a response.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Empty`], [`DecodeError::WrongType`],
    /// [`DecodeError::UnsupportedVersion`], [`DecodeError::WrongLength`] when
    /// `bytes` is not [`Response::LEN`] long, [`DecodeError::NonZeroReserved`],
    /// or [`DecodeError::NonZeroFixed`] when `mac2` is not zero.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = exact_header(bytes, MessageType::Response.code(), Self::LEN)?;
        take_reserved(&mut r)?;
        let response = Self {
            sender_index: SessionIndex(r.u32()?),
            receiver_index: SessionIndex(r.u32()?),
            ephemeral: r.array()?,
            encrypted_payload: r.array()?,
            mac1: r.array()?,
        };
        if r.array::<MAC_LEN>()? != [0; MAC_LEN] {
            return Err(DecodeError::NonZeroFixed);
        }
        Ok(response)
    }

    /// The encoding of this response, with `mac2` zero.
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        let mut w = Writer::new(&mut out);
        put_header(&mut w, MessageType::Response);
        w.u32(self.sender_index.0);
        w.u32(self.receiver_index.0);
        w.put(&self.ephemeral);
        w.put(&self.encrypted_payload);
        w.put(&self.mac1);
        w.put(&[0; MAC_LEN]);
        out
    }

    /// Writes the encoding to the front of `out` and returns its length,
    /// [`Response::LEN`].
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than
    /// [`Response::LEN`]; nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        copy_out(&self.to_bytes(), out)
    }
}

/// Cookie reply, type 3, 64 bytes (ARFC 0003 section 3.1).
///
/// `nonce` and `encrypted_cookie` are opaque here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CookieReply {
    /// The `sender_index` of the initiation answered.
    pub receiver_index: SessionIndex,
    /// The 24-byte XChaCha20-Poly1305 nonce.
    pub nonce: [u8; 24],
    /// The sealed 16-byte cookie, with its tag.
    pub encrypted_cookie: [u8; MAC_LEN + TAG_LEN],
}

impl CookieReply {
    /// Length of a cookie reply, in bytes.
    pub const LEN: usize = 64;

    /// Decodes a cookie reply.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Empty`], [`DecodeError::WrongType`],
    /// [`DecodeError::UnsupportedVersion`], [`DecodeError::WrongLength`] when
    /// `bytes` is not [`CookieReply::LEN`] long, or
    /// [`DecodeError::NonZeroReserved`].
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = exact_header(bytes, MessageType::CookieReply.code(), Self::LEN)?;
        take_reserved(&mut r)?;
        Ok(Self {
            receiver_index: SessionIndex(r.u32()?),
            nonce: r.array()?,
            encrypted_cookie: r.array()?,
        })
    }

    /// The encoding of this cookie reply.
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        let mut w = Writer::new(&mut out);
        put_header(&mut w, MessageType::CookieReply);
        w.u32(self.receiver_index.0);
        w.put(&self.nonce);
        w.put(&self.encrypted_cookie);
        out
    }

    /// Writes the encoding to the front of `out` and returns its length,
    /// [`CookieReply::LEN`].
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than
    /// [`CookieReply::LEN`]; nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        copy_out(&self.to_bytes(), out)
    }
}

/// The plaintext of an initiation's `encrypted_payload`, 16 bytes:
/// `{counter: u64, queues: u16, flags: u16, reserved: u32}` (ARFC 0003 section
/// 3.1).
///
/// `flags` and `reserved` are not fields: version 1 fixes them at zero, the
/// encoder writes zeros, and the decoder rejects a payload where they are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InitiationPayload {
    /// The initiator's handshake counter (ARFC 0003 section 3.2).
    pub counter: HandshakeCounter,
    /// The number of sending queues the initiator will use.
    pub queues: QueueCount,
}

impl InitiationPayload {
    /// Length of the payload plaintext, in bytes.
    pub const LEN: usize = 16;

    /// Decodes a decrypted initiation payload.
    ///
    /// A responder drops the initiation, without reply, when this fails.
    ///
    /// # Errors
    ///
    /// [`DecodeError::WrongLength`] when `bytes` is not
    /// [`InitiationPayload::LEN`] long, [`DecodeError::QueueCountOutOfRange`],
    /// [`DecodeError::NonZeroFixed`] when a flag bit is set, or
    /// [`DecodeError::NonZeroReserved`].
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() != Self::LEN {
            return Err(DecodeError::WrongLength {
                expected: Self::LEN,
                found: bytes.len(),
            });
        }
        let mut r = Reader::new(bytes);
        let counter = HandshakeCounter(r.u64()?);
        let queues = r.u16()?;
        let queues = QueueCount::new(queues).ok_or(DecodeError::QueueCountOutOfRange(queues))?;
        if r.u16()? != 0 {
            return Err(DecodeError::NonZeroFixed);
        }
        if r.u32()? != 0 {
            return Err(DecodeError::NonZeroReserved);
        }
        Ok(Self { counter, queues })
    }

    /// The encoding of this payload, with `flags` and `reserved` zero.
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        let mut w = Writer::new(&mut out);
        w.u64(self.counter.0);
        w.u16(self.queues.get());
        w.u16(0);
        w.u32(0);
        out
    }

    /// Writes the encoding to the front of `out` and returns its length,
    /// [`InitiationPayload::LEN`].
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than
    /// [`InitiationPayload::LEN`]; nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        copy_out(&self.to_bytes(), out)
    }
}

/// The plaintext of a response's `encrypted_payload`, 4 bytes:
/// `{queues: u16, flags: u16}` (ARFC 0003 section 3.1).
///
/// `flags` is not a field: version 1 fixes it at zero, the encoder writes zero,
/// and the decoder rejects a payload where it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResponsePayload {
    /// The number of sending queues the responder will use.
    pub queues: QueueCount,
}

impl ResponsePayload {
    /// Length of the payload plaintext, in bytes.
    pub const LEN: usize = 4;

    /// Decodes a decrypted response payload.
    ///
    /// An initiator abandons the handshake when this fails.
    ///
    /// # Errors
    ///
    /// [`DecodeError::WrongLength`] when `bytes` is not
    /// [`ResponsePayload::LEN`] long, [`DecodeError::QueueCountOutOfRange`], or
    /// [`DecodeError::NonZeroFixed`] when a flag bit is set.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() != Self::LEN {
            return Err(DecodeError::WrongLength {
                expected: Self::LEN,
                found: bytes.len(),
            });
        }
        let mut r = Reader::new(bytes);
        let queues = r.u16()?;
        let queues = QueueCount::new(queues).ok_or(DecodeError::QueueCountOutOfRange(queues))?;
        if r.u16()? != 0 {
            return Err(DecodeError::NonZeroFixed);
        }
        Ok(Self { queues })
    }

    /// The encoding of this payload, with `flags` zero.
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        let mut w = Writer::new(&mut out);
        w.u16(self.queues.get());
        w.u16(0);
        out
    }

    /// Writes the encoding to the front of `out` and returns its length,
    /// [`ResponsePayload::LEN`].
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than
    /// [`ResponsePayload::LEN`]; nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        copy_out(&self.to_bytes(), out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `len` bytes counting up from `start`, so every field is distinct.
    fn ramp<const N: usize>(start: u8) -> [u8; N] {
        let mut out = [0; N];
        for (i, b) in out.iter_mut().enumerate() {
            *b = start.wrapping_add(i as u8);
        }
        out
    }

    fn initiation() -> Initiation {
        Initiation {
            sender_index: SessionIndex(0x0403_0201),
            ephemeral: ramp(0x10),
            encrypted_static: ramp(0x40),
            encrypted_payload: ramp(0x80),
            mac1: ramp(0xa0),
            mac2: ramp(0xc0),
        }
    }

    fn response() -> Response {
        Response {
            sender_index: SessionIndex(0x0403_0201),
            receiver_index: SessionIndex(0x0807_0605),
            ephemeral: ramp(0x10),
            encrypted_payload: ramp(0x40),
            mac1: ramp(0xa0),
        }
    }

    fn cookie_reply() -> CookieReply {
        CookieReply {
            receiver_index: SessionIndex(0x0807_0605),
            nonce: ramp(0x10),
            encrypted_cookie: ramp(0x40),
        }
    }

    #[test]
    fn initiation_layout() {
        let m = initiation();
        let b = m.to_bytes();
        assert_eq!(b.len(), 152);
        assert_eq!(b[..8], [1, 1, 0, 0, 1, 2, 3, 4]);
        assert_eq!(b[8..40], m.ephemeral);
        assert_eq!(b[40..88], m.encrypted_static);
        assert_eq!(b[88..120], m.encrypted_payload);
        assert_eq!(b[120..136], m.mac1);
        assert_eq!(b[136..152], m.mac2);
        assert_eq!(Initiation::MAC1_OFFSET, 120);
        assert_eq!(Initiation::MAC2_OFFSET, 136);
        assert_eq!(Initiation::NOISE_RANGE.len(), 112);
        assert_eq!(Initiation::decode(&b), Ok(m));
    }

    #[test]
    fn response_layout() {
        let m = response();
        let b = m.to_bytes();
        assert_eq!(b.len(), 96);
        assert_eq!(b[..12], [2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(b[12..44], m.ephemeral);
        assert_eq!(b[44..64], m.encrypted_payload);
        assert_eq!(b[64..80], m.mac1);
        assert_eq!(b[80..96], [0; 16]);
        assert_eq!(Response::MAC1_OFFSET, 64);
        assert_eq!(Response::MAC2_OFFSET, 80);
        assert_eq!(Response::NOISE_RANGE.len(), 52);
        assert_eq!(Response::decode(&b), Ok(m));
    }

    #[test]
    fn cookie_reply_layout() {
        let m = cookie_reply();
        let b = m.to_bytes();
        assert_eq!(b.len(), 64);
        assert_eq!(b[..8], [3, 1, 0, 0, 5, 6, 7, 8]);
        assert_eq!(b[8..32], m.nonce);
        assert_eq!(b[32..64], m.encrypted_cookie);
        assert_eq!(CookieReply::decode(&b), Ok(m));
    }

    type Decode = fn(&[u8]) -> Result<(), DecodeError>;

    #[test]
    fn handshake_fixed_values_are_enforced() {
        let messages: [(&[u8], Decode); 3] = [
            (&initiation().to_bytes(), |b| {
                Initiation::decode(b).map(drop)
            }),
            (&response().to_bytes(), |b| Response::decode(b).map(drop)),
            (&cookie_reply().to_bytes(), |b| {
                CookieReply::decode(b).map(drop)
            }),
        ];
        for (valid, decode) in messages {
            let ty = valid[0];
            let mut b = valid.to_vec();
            b[1] = 2;
            assert_eq!(decode(&b), Err(DecodeError::UnsupportedVersion(2)));
            b[1] = 0;
            assert_eq!(decode(&b), Err(DecodeError::UnsupportedVersion(0)));
            for at in [2, 3] {
                let mut b = valid.to_vec();
                b[at] = 0x80;
                assert_eq!(decode(&b), Err(DecodeError::NonZeroReserved));
            }
            let mut b = valid.to_vec();
            b[0] = ty % 3 + 1;
            assert_eq!(
                decode(&b),
                Err(DecodeError::WrongType {
                    expected: ty,
                    found: ty % 3 + 1
                })
            );
            let mut long = valid.to_vec();
            long.push(0);
            let expected = valid.len();
            assert_eq!(
                decode(&long),
                Err(DecodeError::WrongLength {
                    expected,
                    found: expected + 1
                })
            );
            assert_eq!(
                decode(&valid[..expected - 1]),
                Err(DecodeError::WrongLength {
                    expected,
                    found: expected - 1
                })
            );
            assert_eq!(
                decode(&valid[..1]),
                Err(DecodeError::WrongLength { expected, found: 1 })
            );
            assert_eq!(decode(&[]), Err(DecodeError::Empty));
        }
    }

    #[test]
    fn version_is_checked_before_length() {
        assert_eq!(
            Initiation::decode(&[1, 2, 0]),
            Err(DecodeError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn response_mac2_must_be_zero() {
        for at in Response::MAC2_OFFSET..Response::LEN {
            let mut b = response().to_bytes();
            b[at] = 1;
            assert_eq!(Response::decode(&b), Err(DecodeError::NonZeroFixed));
        }
    }

    #[test]
    fn initiation_payload_layout_and_rules() {
        let p = InitiationPayload {
            counter: HandshakeCounter(0x0807_0605_0403_0201),
            queues: QueueCount::new(64).unwrap(),
        };
        let b = p.to_bytes();
        assert_eq!(b, [1, 2, 3, 4, 5, 6, 7, 8, 64, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(InitiationPayload::decode(&b), Ok(p));

        let with = |at: usize, v: u8| {
            let mut b = b;
            b[at] = v;
            InitiationPayload::decode(&b)
        };
        assert_eq!(with(8, 0), Err(DecodeError::QueueCountOutOfRange(0)));
        assert_eq!(with(8, 65), Err(DecodeError::QueueCountOutOfRange(65)));
        assert_eq!(with(9, 1), Err(DecodeError::QueueCountOutOfRange(320)));
        // Flag bit 0, reserved for a hybrid key exchange, and every other bit.
        assert_eq!(with(10, 1), Err(DecodeError::NonZeroFixed));
        assert_eq!(with(11, 0x80), Err(DecodeError::NonZeroFixed));
        for at in 12..16 {
            assert_eq!(with(at, 1), Err(DecodeError::NonZeroReserved));
        }
        assert_eq!(
            InitiationPayload::decode(&b[..15]),
            Err(DecodeError::WrongLength {
                expected: 16,
                found: 15
            })
        );
    }

    #[test]
    fn response_payload_layout_and_rules() {
        let p = ResponsePayload {
            queues: QueueCount::new(1).unwrap(),
        };
        let b = p.to_bytes();
        assert_eq!(b, [1, 0, 0, 0]);
        assert_eq!(ResponsePayload::decode(&b), Ok(p));
        assert_eq!(
            ResponsePayload::decode(&[0, 0, 0, 0]),
            Err(DecodeError::QueueCountOutOfRange(0))
        );
        assert_eq!(
            ResponsePayload::decode(&[65, 0, 0, 0]),
            Err(DecodeError::QueueCountOutOfRange(65))
        );
        assert_eq!(
            ResponsePayload::decode(&[1, 0, 1, 0]),
            Err(DecodeError::NonZeroFixed)
        );
        assert_eq!(
            ResponsePayload::decode(&[1, 0, 0, 0, 0]),
            Err(DecodeError::WrongLength {
                expected: 4,
                found: 5
            })
        );
    }

    #[test]
    fn encode_refuses_a_short_buffer() {
        let mut out = [0; Initiation::LEN - 1];
        assert_eq!(
            initiation().encode(&mut out),
            Err(EncodeError::BufferTooSmall {
                needed: 152,
                available: 151
            })
        );
        let mut out = [0; Initiation::LEN];
        assert_eq!(initiation().encode(&mut out), Ok(Initiation::LEN));
        assert_eq!(out, initiation().to_bytes());
    }
}
