//! Transport packets and their inner frames (ARFC 0003 sections 3.4 and 3.5).

use crate::codec::{Reader, Writer, room};
use crate::error::{DecodeError, EncodeError};
use crate::message::MessageType;
use crate::types::{FragmentCount, FragmentId, SessionIndex, Vni, WireCounter};
use crate::{FRAGMENT_HEADER_LEN, TAG_LEN, TRANSPORT_HEADER_LEN};

/// The header of a transport packet, type 4, 9 bytes: `type (1)`,
/// `receiver_index (4)`, `counter (4)` (ARFC 0003 section 3.4).
///
/// Its encoding is the associated data of the packet's AEAD. A transport packet
/// carries no `version` byte: type 4 is a transport packet of a version 1
/// session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportHeader {
    /// The index that names the session, the VNI and the sending queue at the
    /// receiver.
    pub receiver_index: SessionIndex,
    /// The low 32 bits of the packet's counter on its queue.
    pub counter: WireCounter,
}

impl TransportHeader {
    /// The encoding of this header: type, receiver index and counter.
    pub fn to_bytes(&self) -> [u8; TRANSPORT_HEADER_LEN] {
        let mut out = [0; TRANSPORT_HEADER_LEN];
        let mut w = Writer::new(&mut out);
        w.u8(MessageType::Transport.code());
        w.u32(self.receiver_index.0);
        w.u32(self.counter.0);
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
/// [`Transport::MIN_CIPHERTEXT_LEN`], a 1-byte frame and the tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transport<'a> {
    header: TransportHeader,
    ciphertext: &'a [u8],
}

impl<'a> Transport<'a> {
    /// The shortest ciphertext a transport packet carries: a 1-byte inner
    /// frame sealed, 1 + [`TAG_LEN`] bytes.
    pub const MIN_CIPHERTEXT_LEN: usize = 1 + TAG_LEN;

    /// The shortest transport packet, 26 bytes. A receiver drops a shorter one
    /// before any cryptographic operation (ARFC 0003 section 3.4).
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
    /// Whether `receiver_index` names a session the receiver holds, and whether
    /// the counter passes the replay window, are session state and not checked
    /// here.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Empty`], [`DecodeError::WrongType`] when the first byte is
    /// not 4, or [`DecodeError::TooShort`] when `bytes` is shorter than
    /// [`Transport::MIN_LEN`].
    pub fn decode(bytes: &'a [u8]) -> Result<Self, DecodeError> {
        let ty = MessageType::Transport.code();
        match bytes.first() {
            None => return Err(DecodeError::Empty),
            Some(&found) if found != ty => {
                return Err(DecodeError::WrongType {
                    expected: ty,
                    found,
                });
            }
            Some(_) => {}
        }
        if bytes.len() < Self::MIN_LEN {
            return Err(DecodeError::TooShort {
                minimum: Self::MIN_LEN,
                found: bytes.len(),
            });
        }
        let mut r = Reader::new(bytes);
        r.u8()?;
        let header = TransportHeader {
            receiver_index: SessionIndex(r.u32()?),
            counter: WireCounter(r.u32()?),
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

/// The kind of an inner frame: the high 4 bits of its first byte (ARFC 0003
/// section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FrameKind {
    /// Kind 0, a keepalive. Carried on VNI 0 only.
    Keepalive,
    /// Kind 1, a control frame. Carried on VNI 0 only.
    Control,
    /// Kind 2, a fragment. Carried on any VNI.
    Fragment,
    /// Kind 4, an IPv4 packet. Carried on any VNI.
    Ipv4,
    /// Kind 6, an IPv6 packet. Carried on any VNI.
    Ipv6,
}

impl FrameKind {
    /// The kind value, the high 4 bits of the frame's first byte.
    pub const fn code(self) -> u8 {
        match self {
            Self::Keepalive => 0,
            Self::Control => 1,
            Self::Fragment => 2,
            Self::Ipv4 => 4,
            Self::Ipv6 => 6,
        }
    }

    /// The kind with value `code`, or `None` when version 1 defines none.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Keepalive),
            1 => Some(Self::Control),
            2 => Some(Self::Fragment),
            4 => Some(Self::Ipv4),
            6 => Some(Self::Ipv6),
            _ => None,
        }
    }

    /// The kind of a frame whose first byte is `first`, or `None` when version
    /// 1 defines none.
    pub const fn of_first_byte(first: u8) -> Option<Self> {
        Self::from_code(first >> 4)
    }

    /// Whether a frame of this kind may be carried on a block bound to `vni`:
    /// keepalives and control frames only on VNI 0, the others on any VNI.
    pub const fn is_carried_on(self, vni: Vni) -> bool {
        match self {
            Self::Keepalive | Self::Control => vni.get() == Vni::INFRASTRUCTURE.get(),
            Self::Fragment | Self::Ipv4 | Self::Ipv6 => true,
        }
    }
}

/// The IP version of an [`IpPacket`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IpVersion {
    /// IPv4: the first 4 bits are 4.
    V4,
    /// IPv6: the first 4 bits are 6.
    V6,
}

/// An IP packet carried as a whole inner frame (ARFC 0003 section 3.4).
///
/// It is at least 1 byte long and its first 4 bits are 4 or 6. AL does not
/// parse it further.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IpPacket<'a>(&'a [u8]);

impl<'a> IpPacket<'a> {
    /// Returns the packet, or `None` when `bytes` is empty or its first 4 bits
    /// are not 4 or 6. A sender drops and counts a packet for a VNI other than
    /// 0 for which this returns `None`.
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        match bytes.first().map(|&b| FrameKind::of_first_byte(b)) {
            Some(Some(FrameKind::Ipv4 | FrameKind::Ipv6)) => Some(Self(bytes)),
            _ => None,
        }
    }

    /// The whole packet.
    pub fn bytes(&self) -> &'a [u8] {
        self.0
    }

    /// The IP version, from the first 4 bits.
    pub fn version(&self) -> IpVersion {
        match self.0.first() {
            Some(b) if b >> 4 == FrameKind::Ipv4.code() => IpVersion::V4,
            _ => IpVersion::V6,
        }
    }
}

/// A control frame, kind 1, carried on VNI 0 only (ARFC 0003 section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Control {
    /// Control type 1, a bind request, 5 bytes: asks the peer to bind `vni` in
    /// the session the request is sealed under.
    BindRequest {
        /// The VNI to bind.
        vni: Vni,
    },
    /// Control type 2, a bind, 9 bytes: the block whose first index is `base`
    /// is bound to `vni` at the sender of the bind.
    Bind {
        /// The VNI bound.
        vni: Vni,
        /// The first index of the block bound to it.
        base: SessionIndex,
    },
}

impl Control {
    /// The `control type` of a bind request.
    pub const TYPE_BIND_REQUEST: u8 = 1;
    /// The `control type` of a bind.
    pub const TYPE_BIND: u8 = 2;

    /// The `control type` byte.
    pub const fn control_type(&self) -> u8 {
        match self {
            Self::BindRequest { .. } => Self::TYPE_BIND_REQUEST,
            Self::Bind { .. } => Self::TYPE_BIND,
        }
    }

    /// The length of the whole control frame, in bytes: 5 or 9.
    pub const fn frame_len(&self) -> usize {
        match self {
            Self::BindRequest { .. } => 5,
            Self::Bind { .. } => 9,
        }
    }

    /// Decodes a control frame whose first byte is already known to be `0x10`.
    ///
    /// Whether the VNI may be bound is session state, not checked here.
    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        r.u8()?;
        let ty = r.u8()?;
        let expected = match ty {
            Self::TYPE_BIND_REQUEST => 5,
            Self::TYPE_BIND => 9,
            other => return Err(DecodeError::UnknownControlType(other)),
        };
        if bytes.len() != expected {
            return Err(DecodeError::WrongLength {
                expected,
                found: bytes.len(),
            });
        }
        let vni = Vni::from_le_bytes(r.array()?);
        Ok(match ty {
            Self::TYPE_BIND_REQUEST => Self::BindRequest { vni },
            _ => Self::Bind {
                vni,
                base: SessionIndex(r.u32()?),
            },
        })
    }

    fn put(&self, w: &mut Writer<'_>) {
        w.u8(InnerFrame::CONTROL_BYTE);
        w.u8(self.control_type());
        match self {
            Self::BindRequest { vni } => w.put(&vni.to_le_bytes()),
            Self::Bind { vni, base } => {
                w.put(&vni.to_le_bytes());
                w.u32(base.0);
            }
        }
    }
}

/// A fragment frame, kind 2: `0x20`, `frag_id (4)`, `index (1)`, `count (1)`,
/// then at least 1 byte of data (ARFC 0003 section 3.5).
///
/// A fragment always has a count of 2 to 16, an index below its count, and
/// data. Reassembly is not this crate's concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fragment<'a> {
    id: FragmentId,
    index: u8,
    count: FragmentCount,
    data: &'a [u8],
}

impl<'a> Fragment<'a> {
    /// The shortest fragment frame, in bytes: the header and 1 byte of data.
    pub const MIN_LEN: usize = FRAGMENT_HEADER_LEN + 1;

    /// Returns a fragment, or `None` when `index` is not below `count` or
    /// `data` is empty.
    pub fn new(id: FragmentId, index: u8, count: FragmentCount, data: &'a [u8]) -> Option<Self> {
        (index < count.get() && !data.is_empty()).then_some(Self {
            id,
            index,
            count,
            data,
        })
    }

    /// The id shared by every fragment of the split frame.
    pub fn id(&self) -> FragmentId {
        self.id
    }

    /// The position of this fragment, from 0, below [`Fragment::count`].
    pub fn index(&self) -> u8 {
        self.index
    }

    /// The number of fragments of the split frame.
    pub fn count(&self) -> FragmentCount {
        self.count
    }

    /// This fragment's piece of the split frame, at least 1 byte.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Decodes a fragment frame whose first byte is already known to be `0x20`.
    fn decode(bytes: &'a [u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::MIN_LEN {
            return Err(DecodeError::TooShort {
                minimum: Self::MIN_LEN,
                found: bytes.len(),
            });
        }
        let mut r = Reader::new(bytes);
        r.u8()?;
        let id = FragmentId(r.u32()?);
        let index = r.u8()?;
        let count = r.u8()?;
        let count = FragmentCount::new(count).ok_or(DecodeError::FragmentCountOutOfRange(count))?;
        if index >= count.get() {
            return Err(DecodeError::FragmentIndexOutOfRange {
                index,
                count: count.get(),
            });
        }
        Ok(Self {
            id,
            index,
            count,
            data: r.rest(),
        })
    }

    fn put(&self, w: &mut Writer<'_>) {
        w.u8(InnerFrame::FRAGMENT_BYTE);
        w.u32(self.id.0);
        w.u8(self.index);
        w.u8(self.count.get());
        w.put(self.data);
    }
}

/// The plaintext of a transport packet: one frame of at least 1 byte, whose
/// kind is the high 4 bits of its first byte (ARFC 0003 section 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InnerFrame<'a> {
    /// Kind 0, a keepalive: the single byte `0x00`.
    Keepalive,
    /// Kind 1, a control frame.
    Control(Control),
    /// Kind 2, a fragment.
    Fragment(Fragment<'a>),
    /// Kind 4 or 6, an IP packet: the whole frame.
    Ip(IpPacket<'a>),
}

impl<'a> InnerFrame<'a> {
    /// The first byte of a keepalive, which is the whole frame.
    pub const KEEPALIVE_BYTE: u8 = 0x00;
    /// The first byte of a control frame.
    pub const CONTROL_BYTE: u8 = 0x10;
    /// The first byte of a fragment frame.
    pub const FRAGMENT_BYTE: u8 = 0x20;

    /// The kind of this frame.
    pub fn kind(&self) -> FrameKind {
        match self {
            Self::Keepalive => FrameKind::Keepalive,
            Self::Control(_) => FrameKind::Control,
            Self::Fragment(_) => FrameKind::Fragment,
            Self::Ip(p) => match p.version() {
                IpVersion::V4 => FrameKind::Ipv4,
                IpVersion::V6 => FrameKind::Ipv6,
            },
        }
    }

    /// Decodes a decrypted frame received on a block bound to `vni`. The frame
    /// borrows from `bytes`.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Empty`]; [`DecodeError::UnknownFrameKind`] for a kind
    /// other than 0, 1, 2, 4 and 6; [`DecodeError::KindNotCarried`] for a
    /// keepalive or control frame on a VNI other than 0;
    /// [`DecodeError::MalformedKindByte`] when a keepalive, control or fragment
    /// frame's first byte is not exactly `0x00`, `0x10` or `0x20`;
    /// [`DecodeError::WrongLength`] for a keepalive longer than 1 byte or a
    /// control frame of the wrong length; [`DecodeError::TooShort`] for a
    /// control frame without its type or a fragment frame shorter than
    /// [`Fragment::MIN_LEN`]; [`DecodeError::UnknownControlType`];
    /// [`DecodeError::FragmentCountOutOfRange`]; and
    /// [`DecodeError::FragmentIndexOutOfRange`].
    pub fn decode(bytes: &'a [u8], vni: Vni) -> Result<Self, DecodeError> {
        let first = *bytes.first().ok_or(DecodeError::Empty)?;
        let kind =
            FrameKind::of_first_byte(first).ok_or(DecodeError::UnknownFrameKind(first >> 4))?;
        if !kind.is_carried_on(vni) {
            return Err(DecodeError::KindNotCarried { kind, vni });
        }
        let exact = |byte: u8| {
            if first == byte {
                Ok(())
            } else {
                Err(DecodeError::MalformedKindByte(first))
            }
        };
        match kind {
            FrameKind::Keepalive => {
                exact(Self::KEEPALIVE_BYTE)?;
                if bytes.len() != 1 {
                    return Err(DecodeError::WrongLength {
                        expected: 1,
                        found: bytes.len(),
                    });
                }
                Ok(Self::Keepalive)
            }
            FrameKind::Control => {
                exact(Self::CONTROL_BYTE)?;
                Control::decode(bytes).map(Self::Control)
            }
            FrameKind::Fragment => {
                exact(Self::FRAGMENT_BYTE)?;
                Fragment::decode(bytes).map(Self::Fragment)
            }
            FrameKind::Ipv4 | FrameKind::Ipv6 => Ok(Self::Ip(IpPacket(bytes))),
        }
    }

    /// Decodes a frame reassembled from fragments received on a block bound to
    /// `vni` (ARFC 0003 section 3.5): as [`InnerFrame::decode`], and the frame
    /// must be an IP packet, or a control frame on VNI 0.
    ///
    /// # Errors
    ///
    /// Those of [`InnerFrame::decode`], and
    /// [`DecodeError::KindNotReassemblable`] for a keepalive or a fragment.
    pub fn decode_reassembled(bytes: &'a [u8], vni: Vni) -> Result<Self, DecodeError> {
        let frame = Self::decode(bytes, vni)?;
        match frame.kind() {
            kind @ (FrameKind::Keepalive | FrameKind::Fragment) => {
                Err(DecodeError::KindNotReassemblable(kind))
            }
            FrameKind::Control | FrameKind::Ipv4 | FrameKind::Ipv6 => Ok(frame),
        }
    }

    /// Length of the encoding, in bytes.
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Keepalive => 1,
            Self::Control(c) => c.frame_len(),
            Self::Fragment(f) => FRAGMENT_HEADER_LEN + f.data.len(),
            Self::Ip(p) => p.0.len(),
        }
    }

    /// Writes the encoding to the front of `out` and returns its length,
    /// [`InnerFrame::encoded_len`].
    ///
    /// The encoder does not know the VNI: sealing a keepalive or control frame
    /// only on VNI 0 is the sender's rule.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than the encoding;
    /// nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let len = self.encoded_len();
        let mut w = Writer::new(room(out, len)?);
        match self {
            Self::Keepalive => w.u8(Self::KEEPALIVE_BYTE),
            Self::Control(c) => c.put(&mut w),
            Self::Fragment(f) => f.put(&mut w),
            Self::Ip(p) => w.put(p.0),
        }
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TRANSPORT_OVERHEAD;

    const VNI_0: Vni = Vni::INFRASTRUCTURE;

    fn vni(n: u32) -> Vni {
        Vni::new(n).unwrap()
    }

    fn header() -> TransportHeader {
        TransportHeader {
            receiver_index: SessionIndex(0x0403_0201),
            counter: WireCounter(0x0807_0605),
        }
    }

    fn encode(frame: &InnerFrame<'_>) -> Vec<u8> {
        let mut out = vec![0; frame.encoded_len()];
        assert_eq!(frame.encode(&mut out), Ok(out.len()));
        out
    }

    #[test]
    fn overhead_is_25_bytes() {
        assert_eq!(TRANSPORT_HEADER_LEN, 9);
        assert_eq!(TRANSPORT_OVERHEAD, 25);
        assert_eq!(Transport::MIN_LEN, 26);
    }

    #[test]
    fn transport_layout() {
        let ciphertext: Vec<u8> = (0x40..0x40 + 17).collect();
        let m = Transport::new(header(), &ciphertext).unwrap();
        let mut out = [0; 64];
        let len = m.encode(&mut out).unwrap();
        assert_eq!(len, 26);
        assert_eq!(out[..9], [4, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(out[9..26], ciphertext[..]);
        assert_eq!(Transport::decode(&out[..len]), Ok(m));
        assert_eq!(header().to_bytes(), out[..9]);
    }

    #[test]
    fn transport_rules() {
        let ciphertext = [0; 16];
        assert_eq!(Transport::new(header(), &ciphertext), None);
        let mut b = header().to_bytes().to_vec();
        b.extend_from_slice(&ciphertext);
        assert_eq!(
            Transport::decode(&b),
            Err(DecodeError::TooShort {
                minimum: 26,
                found: 25
            })
        );
        b.push(0);
        assert!(Transport::decode(&b).is_ok());
        // No version byte: any second byte is part of the receiver index.
        b[1] = 0xff;
        assert!(Transport::decode(&b).is_ok());
        b[0] = 1;
        assert_eq!(
            Transport::decode(&b),
            Err(DecodeError::WrongType {
                expected: 4,
                found: 1
            })
        );
        assert_eq!(Transport::decode(&[]), Err(DecodeError::Empty));
    }

    #[test]
    fn frame_kind_codes() {
        for code in 0..16u8 {
            match FrameKind::from_code(code) {
                Some(kind) => assert_eq!(kind.code(), code),
                None => assert!(![0, 1, 2, 4, 6].contains(&code)),
            }
        }
        for first in [0x30, 0x55, 0x7f, 0x80, 0xff] {
            assert_eq!(
                InnerFrame::decode(&[first], VNI_0),
                Err(DecodeError::UnknownFrameKind(first >> 4))
            );
        }
        assert_eq!(InnerFrame::decode(&[], VNI_0), Err(DecodeError::Empty));
    }

    #[test]
    fn keepalive_layout_and_rules() {
        assert_eq!(encode(&InnerFrame::Keepalive), [0x00]);
        assert_eq!(
            InnerFrame::decode(&[0x00], VNI_0),
            Ok(InnerFrame::Keepalive)
        );
        assert_eq!(
            InnerFrame::decode(&[0x00, 0x00], VNI_0),
            Err(DecodeError::WrongLength {
                expected: 1,
                found: 2
            })
        );
        assert_eq!(
            InnerFrame::decode(&[0x01], VNI_0),
            Err(DecodeError::MalformedKindByte(0x01))
        );
        assert_eq!(
            InnerFrame::decode(&[0x00], vni(7)),
            Err(DecodeError::KindNotCarried {
                kind: FrameKind::Keepalive,
                vni: vni(7)
            })
        );
    }

    #[test]
    fn control_layout() {
        let request = InnerFrame::Control(Control::BindRequest {
            vni: vni(0x0012_3456),
        });
        let bytes = encode(&request);
        assert_eq!(bytes, [0x10, 1, 0x56, 0x34, 0x12]);
        assert_eq!(InnerFrame::decode(&bytes, VNI_0), Ok(request));

        let bind = InnerFrame::Control(Control::Bind {
            vni: vni(0x00ab_cdef),
            base: SessionIndex(0x0403_0201),
        });
        let bytes = encode(&bind);
        assert_eq!(bytes, [0x10, 2, 0xef, 0xcd, 0xab, 1, 2, 3, 4]);
        assert_eq!(InnerFrame::decode(&bytes, VNI_0), Ok(bind));
    }

    #[test]
    fn control_rules() {
        let bind = [0x10, 2, 1, 0, 0, 1, 2, 3, 4];
        assert!(InnerFrame::decode(&bind, VNI_0).is_ok());
        assert_eq!(
            InnerFrame::decode(&bind, vni(1)),
            Err(DecodeError::KindNotCarried {
                kind: FrameKind::Control,
                vni: vni(1)
            })
        );
        assert_eq!(
            InnerFrame::decode(&bind[..8], VNI_0),
            Err(DecodeError::WrongLength {
                expected: 9,
                found: 8
            })
        );
        assert_eq!(
            InnerFrame::decode(&[0x10, 1, 0, 0, 0, 0], VNI_0),
            Err(DecodeError::WrongLength {
                expected: 5,
                found: 6
            })
        );
        for ty in [0, 3, 255] {
            assert_eq!(
                InnerFrame::decode(&[0x10, ty, 0, 0, 0], VNI_0),
                Err(DecodeError::UnknownControlType(ty))
            );
        }
        assert_eq!(
            InnerFrame::decode(&[0x10], VNI_0),
            Err(DecodeError::TooShort {
                minimum: 2,
                found: 1
            })
        );
        assert_eq!(
            InnerFrame::decode(&[0x18, 1, 0, 0, 0], VNI_0),
            Err(DecodeError::MalformedKindByte(0x18))
        );
    }

    #[test]
    fn fragment_layout() {
        let data = [0xaa, 0xbb];
        let count = FragmentCount::new(16).unwrap();
        let f = Fragment::new(FragmentId(0x0403_0201), 15, count, &data).unwrap();
        let frame = InnerFrame::Fragment(f);
        let bytes = encode(&frame);
        assert_eq!(bytes, [0x20, 1, 2, 3, 4, 15, 16, 0xaa, 0xbb]);
        assert_eq!(InnerFrame::decode(&bytes, vni(9)), Ok(frame));
        assert_eq!(InnerFrame::decode(&bytes, VNI_0), Ok(frame));
    }

    #[test]
    fn fragment_rules() {
        let count = FragmentCount::new(2).unwrap();
        assert_eq!(Fragment::new(FragmentId(0), 2, count, &[1]), None);
        assert_eq!(Fragment::new(FragmentId(0), 1, count, &[]), None);
        let valid = [0x20, 0, 0, 0, 0, 1, 2, 0x45];
        assert!(InnerFrame::decode(&valid, VNI_0).is_ok());
        let with = |at: usize, v: u8| {
            let mut b = valid;
            b[at] = v;
            InnerFrame::decode(&b, VNI_0).map(|f| f.kind())
        };
        assert_eq!(with(6, 1), Err(DecodeError::FragmentCountOutOfRange(1)));
        assert_eq!(with(6, 17), Err(DecodeError::FragmentCountOutOfRange(17)));
        assert_eq!(
            with(5, 2),
            Err(DecodeError::FragmentIndexOutOfRange { index: 2, count: 2 })
        );
        assert_eq!(with(0, 0x21), Err(DecodeError::MalformedKindByte(0x21)));
        assert_eq!(
            InnerFrame::decode(&valid[..7], VNI_0),
            Err(DecodeError::TooShort {
                minimum: 8,
                found: 7
            })
        );
    }

    #[test]
    fn ip_frames() {
        let v4 = [0x45, 0, 0, 20];
        let v6 = [0x60, 0, 0, 0];
        for (bytes, kind, version) in [
            (&v4[..], FrameKind::Ipv4, IpVersion::V4),
            (&v6[..], FrameKind::Ipv6, IpVersion::V6),
            (&[0x4f][..], FrameKind::Ipv4, IpVersion::V4),
        ] {
            let p = IpPacket::new(bytes).unwrap();
            assert_eq!(p.version(), version);
            assert_eq!(p.bytes(), bytes);
            let frame = InnerFrame::Ip(p);
            assert_eq!(frame.kind(), kind);
            assert_eq!(encode(&frame), bytes);
            assert_eq!(InnerFrame::decode(bytes, vni(0x00ff_ffff)), Ok(frame));
            assert_eq!(InnerFrame::decode(bytes, VNI_0), Ok(frame));
        }
        for bytes in [
            &[][..],
            &[0x00],
            &[0x10, 1, 0, 0, 0],
            &[0x20],
            &[0x50],
            &[0xf0],
        ] {
            assert_eq!(IpPacket::new(bytes), None);
        }
    }

    #[test]
    fn reassembled_frames() {
        let v4 = [0x45, 1, 2];
        assert!(InnerFrame::decode_reassembled(&v4, vni(3)).is_ok());
        let bind = [0x10, 1, 3, 0, 0];
        assert!(InnerFrame::decode_reassembled(&bind, VNI_0).is_ok());
        assert_eq!(
            InnerFrame::decode_reassembled(&bind, vni(3)),
            Err(DecodeError::KindNotCarried {
                kind: FrameKind::Control,
                vni: vni(3)
            })
        );
        assert_eq!(
            InnerFrame::decode_reassembled(&[0x00], VNI_0),
            Err(DecodeError::KindNotReassemblable(FrameKind::Keepalive))
        );
        let fragment = [0x20, 0, 0, 0, 0, 0, 2, 0x45];
        assert_eq!(
            InnerFrame::decode_reassembled(&fragment, vni(3)),
            Err(DecodeError::KindNotReassemblable(FrameKind::Fragment))
        );
    }
}
