//! One session's state at one node (ARFC 0003 sections 3.3 and 3.4).

use std::collections::BTreeMap;

use alloy_crypto::al::{ProbeKey, PublicKey, RecvKey, SendKey, SessionKeys};
use alloy_rt::Instant;
use alloy_wire::{QueueCount, Vni};

use crate::REJECT_AFTER;
use crate::window::ReplayWindow;

/// A session: its keys, the blocks it holds at this node, the bindings it
/// holds at the peer, and its counters and replay windows.
///
/// Every key erases itself when it is dropped, so discarding a session
/// erases all of its keys at once (section 3.3).
pub(crate) struct Session {
    pub(crate) peer: PublicKey,
    /// When the handshake that made it completed here. Its age is measured
    /// from here and it is discarded 180 seconds later.
    pub(crate) made: Instant,
    /// Whether this node has seen that the peer holds the session (3.6).
    pub(crate) confirmed: bool,
    pub(crate) send: SendKey,
    pub(crate) recv: RecvKey,
    /// Held for the session's lifetime: probes (3.6) are out of this crate.
    pub(crate) _probe_send: ProbeKey,
    pub(crate) _probe_recv: ProbeKey,
    /// The sessions with the same peer that this one follows at this node
    /// (3.3): those confirmed here when this node first sent the handshake
    /// message that makes this one at the peer. Emptied by the first
    /// transport packet accepted under this session, which starts their
    /// 10 seconds, since later packets change nothing.
    pub(crate) follows: Vec<u64>,
    /// When this node discards the session by rule 2 of 3.3: 10 seconds
    /// after it first accepted a transport packet under a session that
    /// follows this one.
    pub(crate) discard_at: Option<Instant>,
    /// When this node first sealed a transport packet under the session.
    /// Every older session with the peer stops qualifying 5 seconds later
    /// (3.4).
    pub(crate) first_sealed: Option<Instant>,
    /// The number of queues the peer sends on: indexes with `q` at or above
    /// it name nothing.
    pub(crate) peer_queues: QueueCount,
    /// The next counter of each of this node's sending queues.
    pub(crate) send_counters: Vec<u64>,
    /// One replay window per sending queue of the peer.
    pub(crate) windows: Vec<ReplayWindow>,
    /// This node's blocks, by the VNI each is bound to. VNI 0 is the block
    /// the handshake reserved.
    pub(crate) local: BTreeMap<Vni, u32>,
    /// The bindings this node holds at the peer: the `base` it seals each
    /// VNI's packets with. VNI 0 is the peer's handshake `sender_index`.
    pub(crate) bindings: BTreeMap<Vni, u32>,
    /// When this node last sent a bind request for each VNI under this
    /// session.
    pub(crate) bind_sent: BTreeMap<Vni, Instant>,
    /// The next `frag_id`; unique per session, so per index.
    pub(crate) next_frag: u64,
}

impl Session {
    /// A session made at `now`, by the initiator when `initiator`, following
    /// the sessions in `follows`.
    pub(crate) fn new(
        peer: PublicKey,
        now: Instant,
        keys: SessionKeys,
        (local_base, peer_base): (u32, u32),
        (ours, theirs): (QueueCount, QueueCount),
        initiator: bool,
        follows: Vec<u64>,
    ) -> Self {
        Self {
            peer,
            made: now,
            // An initiator has processed the response, so it has seen that
            // the peer holds the session (3.6).
            confirmed: initiator,
            send: keys.send,
            recv: keys.recv,
            _probe_send: keys.probe_send,
            _probe_recv: keys.probe_recv,
            follows,
            discard_at: None,
            first_sealed: None,
            peer_queues: theirs,
            send_counters: vec![0; usize::from(ours.get())],
            windows: vec![ReplayWindow::default(); usize::from(theirs.get())],
            local: BTreeMap::from([(Vni::INFRASTRUCTURE, local_base)]),
            bindings: BTreeMap::from([(Vni::INFRASTRUCTURE, peer_base)]),
            bind_sent: BTreeMap::new(),
            next_frag: 0,
        }
    }

    /// Whether the session is past its lifetime at `now`: 180 seconds after
    /// its handshake (rule 1 of 3.3), or 10 seconds after a packet under a
    /// session that follows it (rule 2).
    pub(crate) fn expired(&self, now: Instant) -> bool {
        now >= self.made.saturating_add(REJECT_AFTER) || self.discard_at.is_some_and(|t| now >= t)
    }

    /// The highest next counter over this node's sending queues.
    pub(crate) fn max_counter(&self) -> u64 {
        self.send_counters.iter().copied().max().unwrap_or(0)
    }
}
