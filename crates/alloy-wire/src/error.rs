//! Errors returned by the decoders and encoders.

use std::fmt;

use crate::transport::FrameKind;
use crate::types::Vni;

/// Why a byte slice is not a valid message, payload, or frame.
///
/// ARFC 0003 section 6: a receiver drops any message that fails to parse,
/// silently and without reply. The variants exist so that the receiver can count
/// drops by cause; none of them calls for a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodeError {
    /// The input is empty, so it has no first byte: no `type` byte for a
    /// message, no kind for an inner frame.
    Empty,
    /// The `type` byte names no message this crate decodes: a probe (5), a
    /// relay envelope (6), or a reserved value (0, 7 to 255).
    UnknownType(u8),
    /// The `type` byte names a different message than the decoder expects.
    WrongType {
        /// The type the decoder accepts.
        expected: u8,
        /// The type the input carries.
        found: u8,
    },
    /// The `version` byte of a handshake message is not
    /// [`VERSION`](crate::VERSION).
    UnsupportedVersion(u8),
    /// The input is not the exact length its type requires.
    WrongLength {
        /// The required length, in bytes.
        expected: usize,
        /// The length of the input, in bytes.
        found: usize,
    },
    /// The input is shorter than the least its type requires.
    TooShort {
        /// The least length accepted, in bytes.
        minimum: usize,
        /// The length of the input, in bytes.
        found: usize,
    },
    /// A reserved field is not zero.
    NonZeroReserved,
    /// A field that version 1 fixes at zero is not zero: a flag bit, or the
    /// `mac2` of a response.
    NonZeroFixed,
    /// A handshake payload announces a number of queues outside 1 to
    /// [`MAX_QUEUES`](crate::MAX_QUEUES).
    QueueCountOutOfRange(u16),
    /// The kind of an inner frame, the high 4 bits of its first byte, is not
    /// one of 0, 1, 2, 4 or 6.
    UnknownFrameKind(u8),
    /// The first byte of a keepalive, control or fragment frame is not exactly
    /// `0x00`, `0x10` or `0x20`: its low 4 bits are not zero.
    MalformedKindByte(u8),
    /// A keepalive or control frame is carried on a VNI other than 0.
    KindNotCarried {
        /// The kind of the frame.
        kind: FrameKind,
        /// The VNI of the block the packet arrived on.
        vni: Vni,
    },
    /// A reassembled frame is a keepalive or a fragment, which are never split.
    KindNotReassemblable(FrameKind),
    /// A control frame carries a `control type` that version 1 does not define.
    UnknownControlType(u8),
    /// A fragment frame announces a count outside
    /// [`MIN_FRAGMENTS`](crate::MIN_FRAGMENTS) to
    /// [`MAX_FRAGMENTS`](crate::MAX_FRAGMENTS).
    FragmentCountOutOfRange(u8),
    /// A fragment frame carries an index that is not below its count.
    FragmentIndexOutOfRange {
        /// The index carried.
        index: u8,
        /// The count carried.
        count: u8,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty input"),
            Self::UnknownType(ty) => write!(f, "unknown message type {ty}"),
            Self::WrongType { expected, found } => {
                write!(f, "message type {found}, expected {expected}")
            }
            Self::UnsupportedVersion(v) => write!(f, "unsupported version {v}"),
            Self::WrongLength { expected, found } => {
                write!(f, "length {found} bytes, expected {expected}")
            }
            Self::TooShort { minimum, found } => {
                write!(f, "length {found} bytes, at least {minimum} required")
            }
            Self::NonZeroReserved => write!(f, "reserved field is not zero"),
            Self::NonZeroFixed => write!(f, "field fixed at zero in version 1 is not zero"),
            Self::QueueCountOutOfRange(n) => write!(f, "queue count {n} outside 1 to 64"),
            Self::UnknownFrameKind(kind) => write!(f, "unknown inner frame kind {kind}"),
            Self::MalformedKindByte(b) => write!(f, "malformed inner frame first byte {b:#04x}"),
            Self::KindNotCarried { kind, vni } => {
                write!(f, "{kind:?} frame not carried on VNI {}", vni.get())
            }
            Self::KindNotReassemblable(kind) => write!(f, "reassembled frame is a {kind:?}"),
            Self::UnknownControlType(ty) => write!(f, "unknown control type {ty}"),
            Self::FragmentCountOutOfRange(n) => write!(f, "fragment count {n} outside 2 to 16"),
            Self::FragmentIndexOutOfRange { index, count } => {
                write!(f, "fragment index {index} not below count {count}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Why a value could not be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncodeError {
    /// The output buffer is shorter than the encoding. Nothing was written.
    BufferTooSmall {
        /// Length of the encoding, in bytes.
        needed: usize,
        /// Length of the buffer given, in bytes.
        available: usize,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall { needed, available } => {
                write!(f, "buffer of {available} bytes, {needed} needed")
            }
        }
    }
}

impl std::error::Error for EncodeError {}
