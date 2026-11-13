//! Errors returned by the decoders and encoders.

use std::fmt;

/// Why a byte slice is not a valid message, payload, or frame.
///
/// ARFC 0003 section 6: a receiver drops any message that fails to parse,
/// silently and without reply. The variants exist so that the receiver can count
/// drops by cause; none of them calls for a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodeError {
    /// The input is empty, so it has no `type` byte.
    Empty,
    /// The `type` byte names no message this crate decodes.
    UnknownType(u8),
    /// The `type` byte names a different message than the decoder expects.
    WrongType {
        /// The type the decoder accepts.
        expected: u8,
        /// The type the input carries.
        found: u8,
    },
    /// The `version` byte is not [`VERSION`](crate::VERSION).
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
    /// The `kind` of an inner frame is not one of 0 to 4.
    UnknownFrameKind(u8),
    /// A fragment header announces a count outside
    /// [`MIN_FRAGMENTS`](crate::MIN_FRAGMENTS) to
    /// [`MAX_FRAGMENTS`](crate::MAX_FRAGMENTS).
    FragmentCountOutOfRange(u8),
    /// A fragment header carries an index that is not below its count.
    FragmentIndexOutOfRange {
        /// The index carried.
        index: u8,
        /// The count carried.
        count: u8,
    },
    /// A fragment header carries an inner kind that is not IPv4, IPv6, or
    /// control.
    InvalidFragmentKind(u8),
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
            Self::FragmentCountOutOfRange(n) => write!(f, "fragment count {n} outside 2 to 16"),
            Self::FragmentIndexOutOfRange { index, count } => {
                write!(f, "fragment index {index} not below count {count}")
            }
            Self::InvalidFragmentKind(kind) => write!(f, "invalid fragment inner kind {kind}"),
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
