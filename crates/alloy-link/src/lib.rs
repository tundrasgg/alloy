//! The AL handshake and session state machines of ARFC 0003 sections 3.1 to
//! 3.5, sans-I/O.
//!
//! A [`Link`] is one node's side of AL: its static key, the peers it may talk
//! to, the handshakes it has pending, the sessions it holds, and their
//! receiver index blocks, bindings, counters, replay windows and
//! reassemblies. It owns no socket, timer or thread. The caller hands it
//! datagrams ([`Link::receive`]), packets to send ([`Link::send`]) and the
//! passing of time ([`Link::tick`]), and carries out the [`Output`]s each call
//! appends: datagrams to transmit, packets to deliver, and events.
//!
//! Every call takes a [`Ctx`]: the current instant of an `alloy-rt` clock and
//! the `alloy-rt` randomness trait, from which the link draws ephemeral keys,
//! cookie secrets, cookie reply nonces and receiver index blocks. Nothing
//! else is read, so a link is deterministic for a given sequence of calls and
//! random bytes. Instants given to one link must never decrease.
//!
//! # Addressing
//!
//! A link does not know addresses or paths (ARFC 0004). A datagram it sends
//! goes either to a peer, named by its static key, by whatever path the
//! caller picks ([`Destination::Peer`]), or back to where the datagram being
//! received came from ([`Destination::Source`]): the response to an
//! initiation and the cookie reply. With each received datagram the caller
//! passes `src`, the bytes section 3.1 fixes for the cookie: the source IP
//! address and port, the stream's remote address and port, or a relay
//! envelope's `src` and `relay_handle`.
//!
//! # What the link does
//!
//! - Handshakes (3.1): `mac1` is checked before any other cryptography;
//!   under load (more than 1,000 pending handshakes, [`Link::is_under_load`])
//!   an initiation without a valid `mac2` gets a cookie reply and no
//!   Diffie-Hellman. An initiation that gets no response is sent again, as a
//!   fresh initiation, every [`RETRY_INTERVAL`] and abandoned after
//!   [`RETRY_GIVE_UP`].
//! - Replay protection (3.2): initiations carry a handshake counter drawn from
//!   blocks of 65,536 values the caller reserves durably; a responder drops
//!   an initiation whose counter is not above the highest it accepted from
//!   that initiator.
//! - Sessions (3.3): at most 4 per peer, pending initiations included, the
//!   oldest discarded first; rekeyed by a new handshake when a packet is sent
//!   under a session 120 seconds old or with 2^30 packets on a queue. A
//!   session is discarded, all its keys at once, 180 seconds after its
//!   handshake, or 10 seconds after the first packet accepted under a
//!   session that follows it: one made by a handshake message this node
//!   sent while the older session was confirmed here, which shows that the
//!   peer moved past the older one. When two rekeys cross, neither new
//!   session follows the other, the nodes may each seal under a different
//!   one, and each keeps both. Every call that seals or opens first discards
//!   the peer's sessions that are past their lifetime, so no key outlives it
//!   whether or not [`Link::tick`] ran on time.
//! - Transport (3.4): receiver index blocks of 64, one per VNI per session,
//!   at most 1,024 per session; bind requests and binds; session choice,
//!   where a session stops qualifying 5 seconds after the first packet
//!   sealed under a newer one; holding up to 16 packets per VNI per peer for
//!   1 second while no session is chosen; counters below 2^31; the
//!   2,048-counter replay window.
//! - Fragmentation (3.5): frames larger than the caller's payload budget are
//!   split into 2 to 16 fragments; reassembly is bounded to 2 seconds, 4 MiB
//!   per peer and 256 MiB per node, oldest dropped first.
//!
//! Every message that fails a check is dropped without reply and counted
//! ([`Link::drops`], section 6). [`Link::receive`] and [`Link::send`] also
//! return the reason.
//!
//! # Peers
//!
//! A link talks only to peers added with [`Link::add_peer`], each with its
//! pre-shared key. An initiation from any other static key is dropped once
//! its static key is decrypted. This bounds the per-initiator replay table
//! of section 3.2 by the static keys ever added as peers: an entry outlives
//! [`Link::remove_peer`], so an initiation captured before a peer was
//! removed stays a replay after it is added again.
//!
//! # Queues
//!
//! A node sends on [`Config::queues`] queues, numbered from 0. The caller
//! names the queue of every packet it sends. Keepalives, bind requests and
//! binds the link generates itself are sealed on queue 0.
//!
//! # Not here
//!
//! Probes, paths and relays (sections 3.6 and 3.7), stream framing (3.8),
//! process handover (3.9), sockets and TUN devices. A session's probe keys
//! are held and erased with the session (3.3).
//!
//! # Binds after a crossed rekey
//!
//! Section 3.4 also forbids binding, and sealing a bind, under a session
//! that no longer qualifies. After a crossed rekey, that drops every bind
//! request the peer sends under the session it initiated, so a VNI first
//! used later is never bound in that direction. This crate binds, and seals
//! the bind, under the session the request came under whether it qualifies
//! or not, until T-0119 settles the rule in the ARFC.

#![forbid(unsafe_code)]

mod link;
mod reassembly;
mod session;
mod window;

use std::fmt;
use std::time::Duration;

use alloy_crypto::al::PublicKey;
use alloy_rt::{Instant, Rng};
use alloy_wire::{HandshakeCounter, QueueCount, Vni};

pub use link::Link;

/// Values in one block of handshake counters (section 3.2). The caller makes
/// one synced write per block.
pub const COUNTER_BLOCK: u64 = 65_536;

/// Sessions a node holds with one peer, pending initiations included
/// (sections 3.4 and 7).
pub const MAX_SESSIONS_PER_PEER: usize = 4;

/// Blocks of 64 receiver indexes a node holds per session: VNI 0 and up to
/// 1,023 others (sections 3.4 and 7).
pub const MAX_BLOCKS_PER_SESSION: usize = 1_024;

/// Bindings for VNIs other than 0 a sender holds per session (section 7).
pub const MAX_BINDINGS_PER_SESSION: usize = 1_023;

/// Packets held per VNI per peer while no session is chosen (section 7).
/// Packets are held for at most [`MAX_BLOCKS_PER_SESSION`] VNIs per peer,
/// since no session binds more.
pub const MAX_HELD_PER_VNI: usize = 16;

/// How long a held packet waits for a session (section 3.4).
pub const HOLD_TIME: Duration = Duration::from_secs(1);

/// Pending handshakes above which a node is under load (section 7).
pub const LOAD_THRESHOLD: usize = 1_000;

/// A session is rekeyed when a packet is sent under it at this age
/// (section 3.3).
pub const REKEY_AFTER: Duration = Duration::from_secs(120);

/// A session is rekeyed when a packet is sent under it after this many
/// packets on one queue (section 3.3).
pub const REKEY_AFTER_PACKETS: u64 = 1 << 30;

/// Session keys are discarded this long after the handshake that made them
/// (section 3.3).
pub const REJECT_AFTER: Duration = Duration::from_secs(180);

/// A session is discarded this long after the first transport packet
/// accepted under a session that follows it (section 3.3, rule 2).
pub const DISCARD_AFTER_FOLLOWED: Duration = Duration::from_secs(10);

/// A session stops qualifying for any message this long after the first
/// transport packet sealed under a newer session with the same peer
/// (section 3.4).
pub const QUALIFY_TIME: Duration = Duration::from_secs(5);

/// Lifetime of a `cookie_secret` as the current one, then again as the
/// previous one; and how long an initiator keeps a cookie (section 3.1).
pub const COOKIE_TIME: Duration = Duration::from_secs(120);

/// How long an initiator waits for a response before it sends a fresh
/// initiation. ARFC 0003 leaves this open (section 9); this crate fixes it.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// How long an initiator keeps retrying a handshake before it abandons it.
/// ARFC 0003 leaves this open (section 9); this crate fixes it.
pub const RETRY_GIVE_UP: Duration = Duration::from_secs(90);

/// How a [`Link`] is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The number of sending queues this node uses, 1 to 64. It is sent in
    /// every handshake payload.
    pub queues: QueueCount,
    /// The first handshake counter of the block the caller has reserved
    /// durably (section 3.2): the link uses `counter_start` up to
    /// `counter_start + COUNTER_BLOCK - 1`.
    pub counter_start: HandshakeCounter,
}

/// The inputs every call takes besides its own: the time and the randomness.
#[derive(Debug, Clone, Copy)]
pub struct Ctx<'a> {
    /// The current instant of the node's `alloy-rt` clock. Never lower than
    /// the instant of an earlier call on the same link.
    pub now: Instant,
    /// The node's `alloy-rt` randomness trait.
    pub rng: &'a dyn Rng,
}

/// Where a packet the caller asks to send goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outbound {
    /// The peer.
    pub peer: PublicKey,
    /// The VNI the packet belongs to: 0 for the infrastructure network.
    pub vni: Vni,
    /// This node's sending queue, below [`Config::queues`].
    pub queue: u16,
    /// The payload budget of the path (section 3.5): the largest inner frame
    /// one transport packet may carry. A larger packet is fragmented.
    pub budget: usize,
}

/// What [`Link::send`] did with a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// Sealed into one or more transport packets, now in the outputs.
    Sealed,
    /// Held until a session is chosen, for at most [`HOLD_TIME`].
    Held,
}

/// Something a call asks the caller to do.
#[derive(Clone, PartialEq, Eq)]
pub enum Output {
    /// Send `datagram` to `to`.
    Transmit {
        /// Where to send it.
        to: Destination,
        /// The whole AL message.
        datagram: Vec<u8>,
    },
    /// A packet received from `peer` on `vni`: hand it to the virtual switch,
    /// or to the infrastructure network for VNI 0.
    Deliver {
        /// The peer it came from.
        peer: PublicKey,
        /// The VNI of the block it arrived on.
        vni: Vni,
        /// The IPv4 or IPv6 packet.
        packet: Vec<u8>,
    },
    /// Something changed.
    Event(Event),
}

/// Shows lengths, never packet bytes.
impl fmt::Debug for Output {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transmit { to, datagram } => f
                .debug_struct("Transmit")
                .field("to", to)
                .field("len", &datagram.len())
                .finish(),
            Self::Deliver { peer, vni, packet } => f
                .debug_struct("Deliver")
                .field("peer", peer)
                .field("vni", vni)
                .field("len", &packet.len())
                .finish(),
            Self::Event(e) => f.debug_tuple("Event").field(e).finish(),
        }
    }
}

/// Where an [`Output::Transmit`] goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// To the peer, by a path the caller chooses.
    Peer(PublicKey),
    /// Back to the source of the datagram passed to the call that produced
    /// this output.
    Source,
}

/// A change the caller may act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A handshake made a session with `peer`.
    SessionMade {
        /// The peer.
        peer: PublicKey,
        /// Whether this node was the initiator.
        initiator: bool,
    },
    /// A responder saw the peer use a session it made (section 3.6).
    SessionConfirmed {
        /// The peer.
        peer: PublicKey,
    },
    /// A session's keys were discarded: at 180 seconds, 10 seconds after a
    /// packet under a session that follows it, for a fifth session, or with
    /// its peer.
    SessionDiscarded {
        /// The peer.
        peer: PublicKey,
    },
    /// A pending initiation ended without a session.
    HandshakeAbandoned {
        /// The peer.
        peer: PublicKey,
    },
    /// Half of the current handshake counter block is used. The caller makes
    /// the next block durable, `next_start` to `next_start + COUNTER_BLOCK -
    /// 1`, then calls [`Link::add_counter_block`].
    CounterBlockNeeded {
        /// The first counter of the next block.
        next_start: HandshakeCounter,
    },
}

/// Why a message or packet was dropped. Each drop is counted
/// ([`Link::drops`]) and gets no reply (section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DropReason {
    /// The first byte is not a message type this crate handles: a reserved
    /// value, a probe (5) or a relay envelope (6).
    UnknownType,
    /// The message does not parse: its length, version or reserved bytes, or
    /// a decrypted inner frame that breaks section 3.4 or 3.5.
    Malformed,
    /// `mac1` does not verify.
    Mac1,
    /// A handshake field, a cookie or a transport packet does not decrypt, or
    /// a Diffie-Hellman result is zero.
    Unauthentic,
    /// An initiation's static key is not a peer of this link, or a packet is
    /// sent to one that is not.
    UnknownPeer,
    /// A decrypted handshake payload breaks section 3.1: `queues` outside 1
    /// to 64, a flag bit or a non-zero `reserved`.
    Payload,
    /// A handshake counter not above the highest accepted (3.2), or a
    /// transport counter negative, more than 2,047 below the highest
    /// accepted, or accepted already (3.4).
    Replay,
    /// A `receiver_index` that names no pending initiation or no index of a
    /// session this node holds.
    UnknownIndex,
    /// No free block of receiver indexes to reserve.
    NoFreeBlock,
    /// The handshake counter block is used up.
    NoCounter,
    /// The `alloy-rt` randomness trait failed.
    Rng,
    /// A bind request this node does not bind, or a bind it does not keep.
    Bind,
    /// A fragment whose count or index conflicts with one held.
    Fragment,
    /// A reassembly abandoned: 2 seconds passed, or a memory bound dropped
    /// it.
    Reassembly,
    /// A packet to send is not an IPv4 or IPv6 packet.
    NotIp,
    /// A sending queue at or above [`Config::queues`].
    Queue,
    /// A frame that needs more than 16 fragments, or a budget that cannot
    /// carry one fragment byte, or no `frag_id` left in the session.
    TooLarge,
    /// The queue's counter reached 2^31 in the session (section 3.4).
    CounterExhausted,
    /// A packet that could not be held, or was held for 1 second.
    Held,
}
