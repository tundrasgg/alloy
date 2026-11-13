//! Encoding and decoding of AL messages, the mesh wire protocol of ARFC 0003.
//!
//! This crate covers the messages whose bytes ARFC 0003 sections 3.1, 3.4 and
//! 3.5 fix: the handshake initiation (type 1), response (type 2) and cookie
//! reply (type 3), the transport packet (type 4), and the inner frame carried
//! inside a transport packet, with its keepalive, control, fragment and IP
//! kinds. It also reconstructs a transport packet's full counter from the 32
//! bits on the wire ([`reconstruct_counter`]).
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
mod counter;
mod error;
mod handshake;
mod message;
mod transport;
mod types;

pub use counter::reconstruct_counter;
pub use error::{DecodeError, EncodeError};
pub use handshake::{CookieReply, Initiation, InitiationPayload, Response, ResponsePayload};
pub use message::{Message, MessageType};
pub use transport::{
    Control, Fragment, FrameKind, InnerFrame, IpPacket, IpVersion, Transport, TransportHeader,
};
pub use types::{
    FragmentCount, FragmentId, HandshakeCounter, PacketCounter, QueueCount, SessionIndex, Vni,
    WireCounter,
};

/// The protocol version a handshake message carries in its second byte, and
/// the version of the sessions whose transport packets have type 4.
pub const VERSION: u8 = 1;

/// Length of a MAC field (`mac1`, `mac2`), in bytes.
pub const MAC_LEN: usize = 16;

/// Length of an AEAD tag, in bytes.
pub const TAG_LEN: usize = 16;

/// Length of an X25519 public key, such as the `ephemeral` field, in bytes.
pub const KEY_LEN: usize = 32;

/// Length of the transport header that precedes the ciphertext: type, receiver
/// index and counter, 1 + 4 + 4 bytes (ARFC 0003 section 3.4). Its bytes are
/// the AEAD associated data.
pub const TRANSPORT_HEADER_LEN: usize = 9;

/// Fixed overhead per transport packet, not counting UDP and IP: the header and
/// the AEAD tag, 9 + 16 = 25 bytes (ARFC 0003 section 3.4).
pub const TRANSPORT_OVERHEAD: usize = TRANSPORT_HEADER_LEN + TAG_LEN;

/// Length of a fragment frame before its data: kind, `frag_id`, `index` and
/// `count`, 1 + 4 + 1 + 1 bytes (ARFC 0003 section 3.5).
pub const FRAGMENT_HEADER_LEN: usize = 7;

/// Fewest fragments a frame is split into (ARFC 0003 section 3.5).
pub const MIN_FRAGMENTS: u8 = 2;

/// Most fragments a frame is split into (ARFC 0003 sections 3.5 and 7).
pub const MAX_FRAGMENTS: u8 = 16;

/// Most sending queues a node may announce in a handshake payload (ARFC 0003
/// sections 3.1 and 7).
pub const MAX_QUEUES: u16 = 64;

/// The bound on transport counters: a sender never seals a transport packet
/// with a counter of `COUNTER_LIMIT` (2^31) or more on a queue in a session
/// (ARFC 0003 section 3.4). [`reconstruct_counter`] is exact because of it.
pub const COUNTER_LIMIT: u64 = 1 << 31;
