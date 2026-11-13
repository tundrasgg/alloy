//! One session's state at one node (ARFC 0003 sections 3.3 and 3.4).

use std::collections::BTreeMap;

use alloy_crypto::al::{ProbeKey, PublicKey, RecvKey, SendKey, SessionKeys};
use alloy_rt::Instant;
use alloy_wire::{QueueCount, Vni};

use crate::window::ReplayWindow;

/// A session: its keys, the blocks it holds at this node, the bindings it
/// holds at the peer, and its counters and replay windows.
///
/// Every key erases itself when it is dropped, so discarding a session, or
/// dropping its receive key, erases them (section 3.3).
pub(crate) struct Session {
    pub(crate) peer: PublicKey,
    /// When the handshake that made it completed here. Its age is measured
    /// from here and it is discarded 180 seconds later.
    pub(crate) made: Instant,
    /// Whether this node sent the initiation that made the session.
    pub(crate) initiator: bool,
    /// Whether this node has seen that the peer holds the session (3.6).
    pub(crate) confirmed: bool,
    pub(crate) send: SendKey,
    /// `None` once the 10 seconds after the peer moved to a newer session
    /// are over.
    pub(crate) recv: Option<RecvKey>,
    /// Held for the session's lifetime: probes (3.6) are out of this crate.
    pub(crate) _probe_send: ProbeKey,
    pub(crate) _probe_recv: Option<ProbeKey>,
    /// When `recv` is dropped: 10 seconds after this node first saw that
    /// the peer seals under a newer session (`Link::moved_on`).
    pub(crate) recv_until: Option<Instant>,
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
    pub(crate) fn new(
        peer: PublicKey,
        now: Instant,
        keys: SessionKeys,
        local_base: u32,
        peer_base: u32,
        queues: (QueueCount, QueueCount),
        initiator: bool,
    ) -> Self {
        let (ours, theirs) = queues;
        Self {
            peer,
            made: now,
            initiator,
            // An initiator has processed the response, so it has seen that
            // the peer holds the session (3.6).
            confirmed: initiator,
            send: keys.send,
            recv: Some(keys.recv),
            _probe_send: keys.probe_send,
            _probe_recv: Some(keys.probe_recv),
            recv_until: None,
            peer_queues: theirs,
            send_counters: vec![0; usize::from(ours.get())],
            windows: vec![ReplayWindow::default(); usize::from(theirs.get())],
            local: BTreeMap::from([(Vni::INFRASTRUCTURE, local_base)]),
            bindings: BTreeMap::from([(Vni::INFRASTRUCTURE, peer_base)]),
            bind_sent: BTreeMap::new(),
            next_frag: 0,
        }
    }

    /// Drops the receive keys: the 10 seconds after the peer moved on are
    /// over.
    pub(crate) fn drop_recv(&mut self) {
        self.recv = None;
        self._probe_recv = None;
    }

    /// The highest next counter over this node's sending queues.
    pub(crate) fn max_counter(&self) -> u64 {
        self.send_counters.iter().copied().max().unwrap_or(0)
    }
}
