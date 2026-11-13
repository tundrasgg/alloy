//! Encoding and decoding of AL messages, the mesh wire protocol of ARFC 0003.
//!
//! This crate covers the messages whose bytes ARFC 0003 sections 3.1, 3.4 and
//! 3.5 fix: the handshake initiation (type 1), response (type 2) and cookie
//! reply (type 3), the transport packet (type 4), the inner frame carried inside
//! a transport packet, and the fragment header of an inner frame of kind 4.
//!
//! It is sans-I/O and performs no cryptography. Ephemeral keys, ciphertexts,
//! MACs, nonces and AEAD tags are opaque byte arrays: computing and checking
//! them belongs to the handshake and session state machines. What this crate
//! does enforce is every fixed value and length the ARFC states, so that a
//! decoded value is always one the protocol allows.
//!
//! Every decoder is total: it returns a [`DecodeError`] for any input it does
//! not accept and never panics. Every encoder writes into a caller buffer and
//! returns an [`EncodeError`] when the buffer is too small. A value that decodes
//! encodes back to the same bytes, and every value the public constructors allow
//! encodes to bytes that decode back to it.
//!
//! All integers on the wire are little endian (ARFC 0003 section 2).

#![forbid(unsafe_code)]

mod codec;
mod error;
mod handshake;
mod message;
mod transport;
mod types;

pub use error::{DecodeError, EncodeError};
pub use handshake::{CookieReply, Initiation, InitiationPayload, Response, ResponsePayload};
pub use message::{Message, MessageType};
pub use transport::{Fragment, FragmentKind, InnerBody, InnerFrame, Transport, TransportHeader};
pub use types::{
    FragmentCount, FragmentId, HandshakeCounter, PacketCounter, QueueCount, QueueId, SessionIndex,
    Vni,
};

/// The protocol version every message of this crate carries in its second byte.
pub const VERSION: u8 = 1;

/// Length of a MAC field (`mac1`, `mac2`), in bytes.
pub const MAC_LEN: usize = 16;

/// Length of an AEAD tag, in bytes.
pub const TAG_LEN: usize = 16;

/// Length of an X25519 public key, such as the `ephemeral` field, in bytes.
pub const KEY_LEN: usize = 32;

/// Length of the transport header that precedes the ciphertext (ARFC 0003
/// section 3.4): type, version, queue, receiver index and counter.
pub const TRANSPORT_HEADER_LEN: usize = 16;

/// Length of the inner frame header: kind and VNI (ARFC 0003 section 3.4).
pub const INNER_HEADER_LEN: usize = 4;

/// Fixed overhead per transport packet, not counting UDP and IP: header, inner
/// frame header and AEAD tag, 16 + 4 + 16 bytes (ARFC 0003 section 3.4).
pub const TRANSPORT_OVERHEAD: usize = TRANSPORT_HEADER_LEN + INNER_HEADER_LEN + TAG_LEN;

/// Length of the fragment header at the start of the body of an inner frame of
/// kind 4: fragment id, index, count, inner kind and reserved (ARFC 0003
/// section 3.4).
pub const FRAGMENT_HEADER_LEN: usize = 8;

/// Fewest fragments a fragmented packet is split into (ARFC 0003 section 3.5).
pub const MIN_FRAGMENTS: u8 = 2;

/// Most fragments a fragmented packet is split into (ARFC 0003 sections 3.5
/// and 7).
pub const MAX_FRAGMENTS: u8 = 16;

/// Most sending queues a node may announce in a handshake payload (ARFC 0003
/// sections 3.1 and 7).
pub const MAX_QUEUES: u16 = 64;
