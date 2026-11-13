//! Message types and decoding by the `type` byte.

use crate::error::{DecodeError, EncodeError};
use crate::handshake::{CookieReply, Initiation, Response};
use crate::transport::Transport;

/// The message types this crate decodes, by their `type` byte (ARFC 0003
/// sections 3.1 and 3.4).
///
/// Probe messages (type 5), relay envelopes (type 6) and stream framing are
/// outside the scope of T-0054, which built this crate; a message of type 5 or
/// 6 decodes to [`DecodeError::UnknownType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MessageType {
    /// Handshake initiation, type 1.
    Initiation,
    /// Handshake response, type 2.
    Response,
    /// Cookie reply, type 3.
    CookieReply,
    /// Transport, type 4.
    Transport,
}

impl MessageType {
    /// The `type` byte on the wire.
    pub const fn code(self) -> u8 {
        match self {
            Self::Initiation => 1,
            Self::Response => 2,
            Self::CookieReply => 3,
            Self::Transport => 4,
        }
    }

    /// The type with `type` byte `code`, or `None` when this crate does not
    /// decode it.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Initiation),
            2 => Some(Self::Response),
            3 => Some(Self::CookieReply),
            4 => Some(Self::Transport),
            _ => None,
        }
    }
}

/// A decoded message of any type this crate decodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Message<'a> {
    /// Handshake initiation, type 1.
    Initiation(Initiation),
    /// Handshake response, type 2.
    Response(Response),
    /// Cookie reply, type 3.
    CookieReply(CookieReply),
    /// Transport, type 4. The ciphertext borrows from the input.
    Transport(Transport<'a>),
}

impl<'a> Message<'a> {
    /// Decodes a message by its `type` byte.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Empty`], [`DecodeError::UnknownType`] for a type this
    /// crate does not decode, or any error of the type's own decoder.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, DecodeError> {
        let ty = bytes.first().ok_or(DecodeError::Empty)?;
        match MessageType::from_code(*ty) {
            Some(MessageType::Initiation) => Initiation::decode(bytes).map(Self::Initiation),
            Some(MessageType::Response) => Response::decode(bytes).map(Self::Response),
            Some(MessageType::CookieReply) => CookieReply::decode(bytes).map(Self::CookieReply),
            Some(MessageType::Transport) => Transport::decode(bytes).map(Self::Transport),
            None => Err(DecodeError::UnknownType(*ty)),
        }
    }

    /// Writes the encoding to the front of `out` and returns its length.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] when `out` is shorter than the encoding;
    /// nothing is written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        match self {
            Self::Initiation(m) => m.encode(out),
            Self::Response(m) => m.encode(out),
            Self::CookieReply(m) => m.encode(out),
            Self::Transport(m) => m.encode(out),
        }
    }

    /// The type of this message.
    pub fn message_type(&self) -> MessageType {
        match self {
            Self::Initiation(_) => MessageType::Initiation,
            Self::Response(_) => MessageType::Response,
            Self::CookieReply(_) => MessageType::CookieReply,
            Self::Transport(_) => MessageType::Transport,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_codes() {
        for code in 0..=255u8 {
            match MessageType::from_code(code) {
                Some(ty) => assert_eq!(ty.code(), code),
                None => assert!(!(1..=4).contains(&code)),
            }
        }
    }

    #[test]
    fn unknown_types_are_refused() {
        assert_eq!(Message::decode(&[]), Err(DecodeError::Empty));
        for ty in [0, 5, 6, 255] {
            assert_eq!(
                Message::decode(&[ty, 1, 0, 0]),
                Err(DecodeError::UnknownType(ty))
            );
        }
    }
}
