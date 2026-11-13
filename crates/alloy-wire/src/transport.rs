//! Transport packets, inner frames and fragment headers (ARFC 0003 sections
//! 3.4 and 3.5).

use crate::codec::{Reader, Writer, min_header, room};
use crate::error::{DecodeError, EncodeError};
use crate::message::MessageType;
use crate::types::{FragmentCount, FragmentId, PacketCounter, QueueId, SessionIndex, Vni};
use crate::{FRAGMENT_HEADER_LEN, INNER_HEADER_LEN, TAG_LEN, TRANSPORT_HEADER_LEN, VERSION};

/// The header of a transport packet, type 4, 16 bytes (ARFC 0003 section 3.4).
///
/// Its encoding is the associated data of the packet's AEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportHeader {
    /// The sender's queue id.
    pub queue: QueueId,
    /// The `sender_index` the receiver chose for this session.
    pub receiver_index: SessionIndex,
    /// The packet counter, per session and per queue.
    pub counter: PacketCounter,
}

impl TransportHeader {
    /// The encoding of this header: type, version, queue, receiver index and
    /// counter.
    pub fn to_bytes(&self) -> [u8; TRANSPORT_HEADER_LEN] {
        let mut out = [0; TRANSPORT_HEADER_LEN];
        let mut w = Writer::new(&mut out);
        w.u8(MessageType::Transport.code());
        w.u8(VERSION);
        w.u16(self.queue.0);
        w.u32(self.receiver_index.0);
        w.u64(self.counter.0);
        out
    }

    /// Writes the encoding to the front of `out` and returns its length,
    /// [`TRANSPORT_HEADER_LEN`].
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than
    /// [`TRANSPORT_HEADER_LEN`]; nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        room(out, TRANSPORT_HEADER_LEN)?.copy_from_slice(&self.to_bytes());
        Ok(TRANSPORT_HEADER_LEN)
    }
}

/// A transport packet, type 4: a [`TransportHeader`] followed by the AEAD
/// ciphertext of an [`InnerFrame`] with its tag (ARFC 0003 section 3.4).
///
/// The ciphertext is opaque here. Its length is at least
/// [`Transport::MIN_CIPHERTEXT_LEN`], the tag plus the inner frame header,
/// since no shorter ciphertext can open to an inner frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transport<'a> {
    header: TransportHeader,
    ciphertext: &'a [u8],
}

impl<'a> Transport<'a> {
    /// The shortest ciphertext a transport packet carries: an empty inner
    /// frame body sealed, [`INNER_HEADER_LEN`] + [`TAG_LEN`] bytes.
    pub const MIN_CIPHERTEXT_LEN: usize = INNER_HEADER_LEN + TAG_LEN;

    /// The shortest transport packet, in bytes.
    pub const MIN_LEN: usize = TRANSPORT_HEADER_LEN + Self::MIN_CIPHERTEXT_LEN;

    /// Returns a packet, or `None` when `ciphertext` is shorter than
    /// [`Transport::MIN_CIPHERTEXT_LEN`].
    pub fn new(header: TransportHeader, ciphertext: &'a [u8]) -> Option<Self> {
        (ciphertext.len() >= Self::MIN_CIPHERTEXT_LEN).then_some(Self { header, ciphertext })
    }

    /// The header.
    pub fn header(&self) -> TransportHeader {
        self.header
    }

    /// The ciphertext with its tag.
    pub fn ciphertext(&self) -> &'a [u8] {
        self.ciphertext
    }

    /// Decodes a transport packet. The ciphertext borrows from `bytes`.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Empty`], [`DecodeError::WrongType`],
    /// [`DecodeError::UnsupportedVersion`], or [`DecodeError::TooShort`] when
    /// `bytes` is shorter than [`Transport::MIN_LEN`].
    pub fn decode(bytes: &'a [u8]) -> Result<Self, DecodeError> {
        let mut r = min_header(bytes, MessageType::Transport.code(), Self::MIN_LEN)?;
        let header = TransportHeader {
            queue: QueueId(r.u16()?),
            receiver_index: SessionIndex(r.u32()?),
            counter: PacketCounter(r.u64()?),
        };
        Ok(Self {
            header,
            ciphertext: r.rest(),
        })
    }

    /// Length of the encoding, in bytes.
    pub fn encoded_len(&self) -> usize {
        TRANSPORT_HEADER_LEN + self.ciphertext.len()
    }

    /// Writes the encoding to the front of `out` and returns its length,
    /// [`Transport::encoded_len`].
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than the encoding;
    /// nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let len = self.encoded_len();
        let mut w = Writer::new(room(out, len)?);
        w.put(&self.header.to_bytes());
        w.put(self.ciphertext);
        Ok(len)
    }
}

/// The kind of the packet a fragment is a piece of (ARFC 0003 section 3.4).
///
/// Only kinds that carry a packet can be fragmented: a keepalive has nothing to
/// split and a fragment is never fragmented again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FragmentKind {
    /// An IPv4 packet, inner kind 1.
    Ipv4,
    /// An IPv6 packet, inner kind 2.
    Ipv6,
    /// A control message, inner kind 3.
    Control,
}

impl FragmentKind {
    /// The kind byte on the wire.
    pub const fn code(self) -> u8 {
        match self {
            Self::Ipv4 => InnerFrame::KIND_IPV4,
            Self::Ipv6 => InnerFrame::KIND_IPV6,
            Self::Control => InnerFrame::KIND_CONTROL,
        }
    }

    /// The kind with wire byte `code`, or `None` when it is not 1, 2 or 3.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            InnerFrame::KIND_IPV4 => Some(Self::Ipv4),
            InnerFrame::KIND_IPV6 => Some(Self::Ipv6),
            InnerFrame::KIND_CONTROL => Some(Self::Control),
            _ => None,
        }
    }
}

/// The body of an inner frame of kind 4: the fragment header, then one piece
/// of the data (ARFC 0003 sections 3.4 and 3.5).
///
/// The header is `frag_id (4), index (1), count (1), inner kind (1), reserved
/// (1)`. A fragment always has a count of 2 to 16 and an index below its count.
/// Reassembly is not this crate's concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fragment<'a> {
    id: FragmentId,
    index: u8,
    count: FragmentCount,
    kind: FragmentKind,
    data: &'a [u8],
}

impl<'a> Fragment<'a> {
    /// Returns a fragment, or `None` when `index` is not below `count`.
    pub fn new(
        id: FragmentId,
        index: u8,
        count: FragmentCount,
        kind: FragmentKind,
        data: &'a [u8],
    ) -> Option<Self> {
        (index < count.get()).then_some(Self {
            id,
            index,
            count,
            kind,
            data,
        })
    }

    /// The id shared by every fragment of the packet.
    pub fn id(&self) -> FragmentId {
        self.id
    }

    /// The position of this fragment, from 0, below [`Fragment::count`].
    pub fn index(&self) -> u8 {
        self.index
    }

    /// The number of fragments of the packet.
    pub fn count(&self) -> FragmentCount {
        self.count
    }

    /// The kind of the reassembled packet.
    pub fn kind(&self) -> FragmentKind {
        self.kind
    }

    /// This fragment's piece of the packet.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Decodes a fragment from the body of an inner frame of kind 4.
    fn decode(body: &'a [u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(body);
        let id = FragmentId(r.u32()?);
        let index = r.u8()?;
        let count = r.u8()?;
        let kind = r.u8()?;
        let reserved = r.u8()?;
        let count = FragmentCount::new(count).ok_or(DecodeError::FragmentCountOutOfRange(count))?;
        if index >= count.get() {
            return Err(DecodeError::FragmentIndexOutOfRange {
                index,
                count: count.get(),
            });
        }
        let kind = FragmentKind::from_code(kind).ok_or(DecodeError::InvalidFragmentKind(kind))?;
        if reserved != 0 {
            return Err(DecodeError::NonZeroReserved);
        }
        Ok(Self {
            id,
            index,
            count,
            kind,
            data: r.rest(),
        })
    }

    fn put(&self, w: &mut Writer<'_>) {
        w.u32(self.id.0);
        w.u8(self.index);
        w.u8(self.count.get());
        w.u8(self.kind.code());
        w.u8(0);
        w.put(self.data);
    }
}

/// The body of an inner frame, by kind (ARFC 0003 section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InnerBody<'a> {
    /// Kind 0, a keepalive. The body bytes, usually none, are kept as sent.
    Keepalive(&'a [u8]),
    /// Kind 1, an IPv4 packet.
    Ipv4(&'a [u8]),
    /// Kind 2, an IPv6 packet.
    Ipv6(&'a [u8]),
    /// Kind 3, a control message.
    Control(&'a [u8]),
    /// Kind 4, a fragment.
    Fragment(Fragment<'a>),
}

impl InnerBody<'_> {
    /// The kind byte on the wire.
    pub const fn kind(&self) -> u8 {
        match self {
            Self::Keepalive(_) => InnerFrame::KIND_KEEPALIVE,
            Self::Ipv4(_) => InnerFrame::KIND_IPV4,
            Self::Ipv6(_) => InnerFrame::KIND_IPV6,
            Self::Control(_) => InnerFrame::KIND_CONTROL,
            Self::Fragment(_) => InnerFrame::KIND_FRAGMENT,
        }
    }

    fn encoded_len(&self) -> usize {
        match self {
            Self::Keepalive(b) | Self::Ipv4(b) | Self::Ipv6(b) | Self::Control(b) => b.len(),
            Self::Fragment(f) => FRAGMENT_HEADER_LEN + f.data.len(),
        }
    }
}

/// The plaintext of a transport packet: `kind (1), vni (3)`, then the body
/// (ARFC 0003 section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InnerFrame<'a> {
    /// The virtual network the body belongs to.
    pub vni: Vni,
    /// The body, by kind.
    pub body: InnerBody<'a>,
}

impl<'a> InnerFrame<'a> {
    /// Kind byte of a keepalive.
    pub const KIND_KEEPALIVE: u8 = 0;
    /// Kind byte of an IPv4 packet.
    pub const KIND_IPV4: u8 = 1;
    /// Kind byte of an IPv6 packet.
    pub const KIND_IPV6: u8 = 2;
    /// Kind byte of a control message.
    pub const KIND_CONTROL: u8 = 3;
    /// Kind byte of a fragment.
    pub const KIND_FRAGMENT: u8 = 4;

    /// Decodes a decrypted inner frame. The body borrows from `bytes`.
    ///
    /// # Errors
    ///
    /// [`DecodeError::TooShort`] when `bytes` is shorter than
    /// [`INNER_HEADER_LEN`], or than [`INNER_HEADER_LEN`] +
    /// [`FRAGMENT_HEADER_LEN`] for a fragment;
    /// [`DecodeError::UnknownFrameKind`]; and for a fragment
    /// [`DecodeError::FragmentCountOutOfRange`],
    /// [`DecodeError::FragmentIndexOutOfRange`],
    /// [`DecodeError::InvalidFragmentKind`], or
    /// [`DecodeError::NonZeroReserved`].
    pub fn decode(bytes: &'a [u8]) -> Result<Self, DecodeError> {
        if bytes.len() < INNER_HEADER_LEN {
            return Err(DecodeError::TooShort {
                minimum: INNER_HEADER_LEN,
                found: bytes.len(),
            });
        }
        let mut r = Reader::new(bytes);
        let kind = r.u8()?;
        let vni = Vni::from_le_bytes(r.array()?);
        let body = r.rest();
        let body = match kind {
            Self::KIND_KEEPALIVE => InnerBody::Keepalive(body),
            Self::KIND_IPV4 => InnerBody::Ipv4(body),
            Self::KIND_IPV6 => InnerBody::Ipv6(body),
            Self::KIND_CONTROL => InnerBody::Control(body),
            Self::KIND_FRAGMENT => {
                if bytes.len() < INNER_HEADER_LEN + FRAGMENT_HEADER_LEN {
                    return Err(DecodeError::TooShort {
                        minimum: INNER_HEADER_LEN + FRAGMENT_HEADER_LEN,
                        found: bytes.len(),
                    });
                }
                InnerBody::Fragment(Fragment::decode(body)?)
            }
            other => return Err(DecodeError::UnknownFrameKind(other)),
        };
        Ok(Self { vni, body })
    }

    /// Length of the encoding, in bytes.
    pub fn encoded_len(&self) -> usize {
        INNER_HEADER_LEN + self.body.encoded_len()
    }

    /// Writes the encoding to the front of `out` and returns its length,
    /// [`InnerFrame::encoded_len`].
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than the encoding;
    /// nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let len = self.encoded_len();
        let mut w = Writer::new(room(out, len)?);
        w.u8(self.body.kind());
        w.put(&self.vni.to_le_bytes());
        match &self.body {
            InnerBody::Keepalive(b)
            | InnerBody::Ipv4(b)
            | InnerBody::Ipv6(b)
            | InnerBody::Control(b) => w.put(b),
            InnerBody::Fragment(f) => f.put(&mut w),
        }
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TRANSPORT_OVERHEAD;

    fn header() -> TransportHeader {
        TransportHeader {
            queue: QueueId(0x0201),
            receiver_index: SessionIndex(0x0605_0403),
            counter: PacketCounter(0x0e0d_0c0b_0a09_0807),
        }
    }

    #[test]
    fn overhead_is_36_bytes() {
        assert_eq!(TRANSPORT_OVERHEAD, 36);
        assert_eq!(Transport::MIN_LEN, TRANSPORT_OVERHEAD);
    }

    #[test]
    fn transport_layout() {
        let ciphertext: Vec<u8> = (0x40..0x40 + 20).collect();
        let m = Transport::new(header(), &ciphertext).unwrap();
        let mut out = [0; 64];
        let len = m.encode(&mut out).unwrap();
        assert_eq!(len, 36);
        assert_eq!(
            out[..16],
            [4, 1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
        assert_eq!(out[16..36], ciphertext[..]);
        assert_eq!(Transport::decode(&out[..len]), Ok(m));
        assert_eq!(header().to_bytes(), out[..16]);
    }

    #[test]
    fn transport_rules() {
        let ciphertext = [0; 19];
        assert_eq!(Transport::new(header(), &ciphertext), None);
        let mut b = header().to_bytes().to_vec();
        b.extend_from_slice(&ciphertext);
        assert_eq!(
            Transport::decode(&b),
            Err(DecodeError::TooShort {
                minimum: 36,
                found: 35
            })
        );
        b.push(0);
        assert!(Transport::decode(&b).is_ok());
        b[1] = 2;
        assert_eq!(
            Transport::decode(&b),
            Err(DecodeError::UnsupportedVersion(2))
        );
        b[1] = 1;
        b[0] = 1;
        assert_eq!(
            Transport::decode(&b),
            Err(DecodeError::WrongType {
                expected: 4,
                found: 1
            })
        );
    }

    #[test]
    fn inner_frame_layout() {
        let data = [9, 8, 7];
        let vni = Vni::new(0x0012_3456).unwrap();
        let cases: [(InnerBody<'_>, u8); 4] = [
            (InnerBody::Keepalive(&[]), 0),
            (InnerBody::Ipv4(&data), 1),
            (InnerBody::Ipv6(&data), 2),
            (InnerBody::Control(&data), 3),
        ];
        for (body, kind) in cases {
            let frame = InnerFrame { vni, body };
            let mut out = [0; 16];
            let len = frame.encode(&mut out).unwrap();
            assert_eq!(out[..4], [kind, 0x56, 0x34, 0x12]);
            assert_eq!(InnerFrame::decode(&out[..len]), Ok(frame));
        }
        assert_eq!(
            InnerFrame::decode(&[5, 0, 0, 0]),
            Err(DecodeError::UnknownFrameKind(5))
        );
        assert_eq!(
            InnerFrame::decode(&[1, 0, 0]),
            Err(DecodeError::TooShort {
                minimum: 4,
                found: 3
            })
        );
    }

    #[test]
    fn fragment_layout() {
        let data = [0xaa, 0xbb];
        let count = FragmentCount::new(16).unwrap();
        let f = Fragment::new(
            FragmentId(0x0403_0201),
            15,
            count,
            FragmentKind::Ipv6,
            &data,
        );
        let frame = InnerFrame {
            vni: Vni::INFRASTRUCTURE,
            body: InnerBody::Fragment(f.unwrap()),
        };
        let mut out = [0; 16];
        let len = frame.encode(&mut out).unwrap();
        assert_eq!(
            out[..len],
            [4, 0, 0, 0, 1, 2, 3, 4, 15, 16, 2, 0, 0xaa, 0xbb]
        );
        assert_eq!(InnerFrame::decode(&out[..len]), Ok(frame));
    }

    #[test]
    fn fragment_rules() {
        let count = FragmentCount::new(2).unwrap();
        assert_eq!(
            Fragment::new(FragmentId(0), 2, count, FragmentKind::Ipv4, &[]),
            None
        );
        let valid = [4, 0, 0, 0, 0, 0, 0, 0, 1, 2, 1, 0];
        assert!(InnerFrame::decode(&valid).is_ok());
        let with = |at: usize, v: u8| {
            let mut b = valid;
            b[at] = v;
            InnerFrame::decode(&b).map(|f| f.vni)
        };
        assert_eq!(with(9, 1), Err(DecodeError::FragmentCountOutOfRange(1)));
        assert_eq!(with(9, 17), Err(DecodeError::FragmentCountOutOfRange(17)));
        assert_eq!(
            with(8, 2),
            Err(DecodeError::FragmentIndexOutOfRange { index: 2, count: 2 })
        );
        assert_eq!(with(10, 0), Err(DecodeError::InvalidFragmentKind(0)));
        assert_eq!(with(10, 4), Err(DecodeError::InvalidFragmentKind(4)));
        assert_eq!(with(11, 1), Err(DecodeError::NonZeroReserved));
        assert_eq!(
            InnerFrame::decode(&valid[..11]),
            Err(DecodeError::TooShort {
                minimum: 12,
                found: 11
            })
        );
    }
}
