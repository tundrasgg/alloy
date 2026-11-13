//! The [`Link`] state machine.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use alloy_crypto::al::{
    Cookie, CookieReplyKey, CookieSecret, EphemeralSecret, INITIATION_NOISE_LEN,
    InitiatorHandshake, Mac1Key, PresharedKey, PublicKey, RESPONSE_NOISE_LEN, ResponderHandshake,
    SealError, StaticSecret, verify_mac2,
};
use alloy_rt::{Instant, Rng};
use alloy_wire::{
    COUNTER_LIMIT, Control, CookieReply, FRAGMENT_HEADER_LEN, Fragment, FragmentCount, FragmentId,
    HandshakeCounter, Initiation, InitiationPayload, InnerFrame, IpPacket, MessageType,
    PacketCounter, QueueCount, Response, ResponsePayload, SessionIndex, TAG_LEN,
    TRANSPORT_HEADER_LEN, Transport, TransportHeader, Vni,
};

use crate::reassembly::{NODE_REASSEMBLY_BYTES, PEER_REASSEMBLY_BYTES, Reassembler, Refused};
use crate::session::Session;
use crate::{
    COOKIE_TIME, COUNTER_BLOCK, Config, Ctx, Destination, DropReason, Event, HOLD_TIME,
    LOAD_THRESHOLD, MAX_BINDINGS_PER_SESSION, MAX_BLOCKS_PER_SESSION, MAX_HELD_PER_VNI,
    MAX_SESSIONS_PER_PEER, Outbound, Output, PREVIOUS_RECV_TIME, REJECT_AFTER, REKEY_AFTER,
    REKEY_AFTER_PACKETS, RETRY_GIVE_UP, RETRY_INTERVAL, Sent,
};

/// Indexes per block. This node reserves only blocks whose base is a
/// multiple of 64, so an index's block is its value with the low 6 bits
/// cleared, and two blocks never overlap.
const BLOCK: u32 = 64;

/// What one of this node's blocks is reserved for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockUse {
    /// The block of a pending initiation to this peer.
    Pending(PublicKey),
    /// A session's block, bound to a VNI.
    Session(u64, Vni),
}

/// A peer this link may talk to.
struct Peer {
    psk: PresharedKey,
    /// `mac1` key of messages sent to the peer.
    mac1: Mac1Key,
    /// Opens the cookie replies the peer sends as a responder.
    cookie_key: CookieReplyKey,
    /// The cookie the peer last sent and when it arrived.
    cookie: Option<(Cookie, Instant)>,
    /// The sessions held with the peer, oldest first.
    sessions: Vec<u64>,
    /// Packets waiting for a session, per VNI, oldest first.
    held: BTreeMap<Vni, VecDeque<Held>>,
}

struct Held {
    at: Instant,
    queue: u16,
    budget: usize,
    packet: Vec<u8>,
}

/// An initiation sent and not yet answered.
struct Pending {
    handshake: InitiatorHandshake,
    base: u32,
    mac1: [u8; 16],
    /// When the first initiation of this handshake was sent.
    started: Instant,
    /// When the latest one was sent.
    sent: Instant,
}

/// One node's AL state: handshakes, sessions and their transport (ARFC 0003
/// sections 3.1 to 3.5). See the crate documentation.
pub struct Link {
    local: StaticSecret,
    public: PublicKey,
    /// `mac1` key of messages sent to this node.
    mac1: Mac1Key,
    /// Seals this node's cookie replies.
    cookie_key: CookieReplyKey,
    queues: QueueCount,
    counter_next: u64,
    counter_end: u64,
    /// The current `cookie_secret` and when it was drawn.
    cookie_current: Option<(CookieSecret, Instant)>,
    /// The secret it replaced and when that one stops being accepted.
    cookie_previous: Option<(CookieSecret, Instant)>,
    backlog: usize,
    peers: BTreeMap<PublicKey, Peer>,
    /// The highest handshake counter accepted per initiator static key
    /// (3.2). It outlives [`Link::remove_peer`], so an initiation captured
    /// before a peer is removed is still a replay after it is added again.
    /// One entry per static key ever added as a peer.
    highest: BTreeMap<PublicKey, u64>,
    pending: BTreeMap<PublicKey, Pending>,
    sessions: BTreeMap<u64, Session>,
    blocks: BTreeMap<u32, BlockUse>,
    next_session: u64,
    reassembly: Reassembler,
    local_vnis: BTreeSet<Vni>,
    drops: BTreeMap<DropReason, u64>,
}

/// Shows counts only, never keys.
impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Link")
            .field("public", &self.public)
            .field("peers", &self.peers.len())
            .field("pending", &self.pending.len())
            .field("sessions", &self.sessions.len())
            .finish_non_exhaustive()
    }
}

impl Link {
    /// A link for the node whose static key is `local`, with no peers.
    pub fn new(local: StaticSecret, config: Config) -> Self {
        let public = local.public_key();
        Self {
            mac1: Mac1Key::new(&public),
            cookie_key: CookieReplyKey::new(&public),
            local,
            public,
            queues: config.queues,
            counter_next: config.counter_start.0,
            counter_end: config.counter_start.0.saturating_add(COUNTER_BLOCK),
            cookie_current: None,
            cookie_previous: None,
            backlog: 0,
            peers: BTreeMap::new(),
            highest: BTreeMap::new(),
            pending: BTreeMap::new(),
            sessions: BTreeMap::new(),
            blocks: BTreeMap::new(),
            next_session: 0,
            reassembly: Reassembler::new(PEER_REASSEMBLY_BYTES, NODE_REASSEMBLY_BYTES),
            local_vnis: BTreeSet::new(),
            drops: BTreeMap::new(),
        }
    }

    /// This node's static public key.
    pub fn public_key(&self) -> PublicKey {
        self.public
    }

    /// Lets this link talk to the node whose static key is `peer`, with `psk`
    /// as the pair's pre-shared key ([`PresharedKey::none`] when none is
    /// configured). For a peer already added, replaces the pre-shared key
    /// used by later handshakes.
    pub fn add_peer(&mut self, peer: PublicKey, psk: PresharedKey) {
        if let Some(p) = self.peers.get_mut(&peer) {
            p.psk = psk;
            return;
        }
        self.peers.insert(
            peer,
            Peer {
                psk,
                mac1: Mac1Key::new(&peer),
                cookie_key: CookieReplyKey::new(&peer),
                cookie: None,
                sessions: Vec::new(),
                held: BTreeMap::new(),
            },
        );
    }

    /// Removes `peer`: abandons its pending initiation, discards its
    /// sessions, erasing their keys, and drops its held packets. The highest
    /// handshake counter accepted from it is kept, so a later
    /// [`Link::add_peer`] of the same key still drops replayed initiations.
    pub fn remove_peer(&mut self, peer: &PublicKey, out: &mut Vec<Output>) {
        self.abandon_pending(*peer, out);
        let ids = self.peers.get(peer).map(|p| p.sessions.clone());
        for id in ids.unwrap_or_default() {
            self.discard_session(id, out);
        }
        self.peers.remove(peer);
    }

    /// Declares that a VPC with VNI `vni` has a port on this node, so a peer's
    /// bind request for it is accepted (section 3.4).
    pub fn add_local_vni(&mut self, vni: Vni) {
        self.local_vnis.insert(vni);
    }

    /// Withdraws [`Link::add_local_vni`]. Blocks already bound stay bound for
    /// their sessions' lifetimes.
    pub fn remove_local_vni(&mut self, vni: Vni) {
        self.local_vnis.remove(&vni);
    }

    /// Sets the number of handshake messages the caller has received and not
    /// yet passed to [`Link::receive`]. They count as pending handshakes
    /// toward [`Link::is_under_load`].
    pub fn set_handshake_backlog(&mut self, backlog: usize) {
        self.backlog = backlog;
    }

    /// Whether the node is under load (section 7): more than 1,000 pending
    /// handshakes, counting this link's pending initiations and the backlog
    /// set by [`Link::set_handshake_backlog`].
    pub fn is_under_load(&self) -> bool {
        self.pending.len().saturating_add(self.backlog) > LOAD_THRESHOLD
    }

    /// Extends the handshake counters by one block of [`COUNTER_BLOCK`]
    /// values, after the caller made the block durable (section 3.2,
    /// [`Event::CounterBlockNeeded`]).
    pub fn add_counter_block(&mut self) {
        self.counter_end = self.counter_end.saturating_add(COUNTER_BLOCK);
    }

    /// How many messages and packets were dropped for `reason`.
    pub fn drops(&self, reason: DropReason) -> u64 {
        self.drops.get(&reason).copied().unwrap_or(0)
    }

    /// The number of sessions held with `peer`, pending initiation excluded.
    pub fn session_count(&self, peer: &PublicKey) -> usize {
        self.peers.get(peer).map_or(0, |p| p.sessions.len())
    }

    /// Whether an initiation to `peer` is pending.
    pub fn is_pending(&self, peer: &PublicKey) -> bool {
        self.pending.contains_key(peer)
    }

    /// The earliest instant at which [`Link::tick`] has something to do, or
    /// `None` when no timer runs.
    pub fn next_deadline(&self) -> Option<Instant> {
        let pending = self
            .pending
            .values()
            .map(|p| p.sent.saturating_add(RETRY_INTERVAL));
        let sessions = self.sessions.values().flat_map(|s| {
            [Some(s.made.saturating_add(REJECT_AFTER)), s.recv_until]
                .into_iter()
                .flatten()
        });
        let peers = self.peers.values().flat_map(|p| {
            let cookie = p
                .cookie
                .as_ref()
                .map(|(_, at)| at.saturating_add(COOKIE_TIME));
            let held = p
                .held
                .values()
                .filter_map(|q| q.front().map(|h| h.at.saturating_add(HOLD_TIME)));
            cookie.into_iter().chain(held)
        });
        let secrets = [
            self.cookie_current
                .as_ref()
                .map(|(_, made)| made.saturating_add(COOKIE_TIME)),
            self.cookie_previous.as_ref().map(|(_, until)| *until),
            self.reassembly.deadline(),
        ];
        pending
            .chain(sessions)
            .chain(peers)
            .chain(secrets.into_iter().flatten())
            .min()
    }

    /// Starts a handshake with `peer` now, unless an initiation to it is
    /// pending. A session that results is newer than every session held.
    ///
    /// # Errors
    ///
    /// The [`DropReason`] for which no initiation was sent; it is counted.
    pub fn connect(
        &mut self,
        cx: Ctx<'_>,
        peer: PublicKey,
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        if self.pending.contains_key(&peer) {
            return Ok(());
        }
        let result = self.initiate(cx, peer, cx.now, out);
        self.counted(result)
    }

    /// Handles one datagram received from the underlay. `src` identifies
    /// where it came from, as section 3.1 fixes for the cookie. The datagram
    /// is decrypted in place.
    ///
    /// # Errors
    ///
    /// The [`DropReason`] when the datagram is dropped; it is counted and
    /// nothing is sent in reply.
    pub fn receive(
        &mut self,
        cx: Ctx<'_>,
        src: &[u8],
        datagram: &mut [u8],
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        let result = match datagram.first().copied().and_then(MessageType::from_code) {
            Some(MessageType::Initiation) => self.on_initiation(cx, src, datagram, out),
            Some(MessageType::Response) => self.on_response(cx, datagram, out),
            Some(MessageType::CookieReply) => self.on_cookie_reply(cx, datagram),
            Some(MessageType::Transport) => self.on_transport(cx, datagram, out),
            None => Err(DropReason::UnknownType),
        };
        self.counted(result)
    }

    /// Sends one IPv4 or IPv6 packet to `to.peer` on `to.vni`, under the
    /// session section 3.4 chooses, fragmented to `to.budget` when larger.
    /// When no session qualifies the packet is held, a handshake is started
    /// if no session or initiation exists, and a bind request is sent if the
    /// session lacks a binding for the VNI. A packet sent under a session due
    /// for rekeying starts a handshake.
    ///
    /// # Errors
    ///
    /// The [`DropReason`] when the packet is dropped; it is counted.
    pub fn send(
        &mut self,
        cx: Ctx<'_>,
        to: Outbound,
        packet: &[u8],
        out: &mut Vec<Output>,
    ) -> Result<Sent, DropReason> {
        let result = self.send_packet(cx, to, packet, out);
        self.counted(result)
    }

    /// Runs the timers due at `cx.now`: initiation retries and give-ups,
    /// session discard at 180 seconds, the previous receive key's 10
    /// seconds, cookie and cookie secret lifetimes, held packets and
    /// reassemblies. Key lifetimes do not depend on it: [`Link::send`],
    /// [`Link::receive`] and [`Link::connect`] discard a peer's expired keys
    /// before they use any.
    pub fn tick(&mut self, cx: Ctx<'_>, out: &mut Vec<Output>) {
        let now = cx.now;
        let due: Vec<(PublicKey, Instant)> = self
            .pending
            .iter()
            .filter(|(_, p)| now >= p.sent.saturating_add(RETRY_INTERVAL))
            .map(|(peer, p)| (*peer, p.started))
            .collect();
        for (peer, started) in due {
            if now >= started.saturating_add(RETRY_GIVE_UP) {
                self.abandon_pending(peer, out);
            } else if let Err(reason) = self.initiate(cx, peer, started, out) {
                self.count(reason);
                if self.pending.contains_key(&peer) {
                    self.abandon_pending(peer, out);
                } else {
                    out.push(Output::Event(Event::HandshakeAbandoned { peer }));
                }
            }
        }
        let peers: Vec<PublicKey> = self.peers.keys().copied().collect();
        for peer in peers {
            self.expire_peer(now, peer, out);
        }
        let mut expired = 0;
        for peer in self.peers.values_mut() {
            if peer
                .cookie
                .as_ref()
                .is_some_and(|(_, at)| now >= at.saturating_add(COOKIE_TIME))
            {
                peer.cookie = None;
            }
            for queue in peer.held.values_mut() {
                while queue
                    .front()
                    .is_some_and(|h| now >= h.at.saturating_add(HOLD_TIME))
                {
                    queue.pop_front();
                    expired += 1;
                }
            }
            peer.held.retain(|_, q| !q.is_empty());
        }
        self.count_n(DropReason::Held, expired);
        let abandoned = self.reassembly.expire(now);
        self.count_n(DropReason::Reassembly, abandoned);
        self.retire_cookie_secrets(now);
    }

    fn counted<T>(&mut self, result: Result<T, DropReason>) -> Result<T, DropReason> {
        if let Err(reason) = result {
            self.count(reason);
        }
        result
    }

    /// Counts the drop of an operation the link started itself.
    fn note(&mut self, result: Result<(), DropReason>) {
        if let Err(reason) = result {
            self.count(reason);
        }
    }

    fn count(&mut self, reason: DropReason) {
        self.count_n(reason, 1);
    }

    fn count_n(&mut self, reason: DropReason, n: u64) {
        if n > 0 {
            let c = self.drops.entry(reason).or_default();
            *c = c.saturating_add(n);
        }
    }

    // Handshake (sections 3.1 and 3.2).

    /// Sends a fresh initiation to `peer`, replacing any pending one.
    fn initiate(
        &mut self,
        cx: Ctx<'_>,
        peer: PublicKey,
        started: Instant,
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        if !self.peers.contains_key(&peer) {
            return Err(DropReason::UnknownPeer);
        }
        if self.counter_next >= self.counter_end {
            out.push(Output::Event(Event::CounterBlockNeeded {
                next_start: HandshakeCounter(self.counter_end),
            }));
            return Err(DropReason::NoCounter);
        }
        let start = draw::<4>(cx.rng)?;
        let mut secret = draw::<32>(cx.rng)?;
        let ephemeral = EphemeralSecret::from_bytes(secret);
        wipe(&mut secret);
        if let Some(old) = self.pending.remove(&peer) {
            self.blocks.remove(&old.base);
        }
        self.expire_peer(cx.now, peer, out);
        self.make_room(peer, out);
        let base = self.reserve_block(start, BlockUse::Pending(peer))?;
        let payload = InitiationPayload {
            counter: HandshakeCounter(self.counter_next),
            queues: self.queues,
        }
        .to_bytes();
        let Some(p) = self.peers.get(&peer) else {
            self.blocks.remove(&base);
            return Err(DropReason::UnknownPeer);
        };
        let (handshake, noise) =
            match InitiatorHandshake::write_initiation(&self.local, &peer, ephemeral, &payload) {
                Ok(written) => written,
                Err(_) => {
                    self.blocks.remove(&base);
                    return Err(DropReason::Unauthentic);
                }
            };
        let mut msg = Initiation {
            sender_index: SessionIndex(base),
            ephemeral: [0; 32],
            encrypted_static: [0; 48],
            encrypted_payload: [0; 32],
            mac1: [0; 16],
            mac2: [0; 16],
        };
        msg.ephemeral.copy_from_slice(&noise[..32]);
        msg.encrypted_static.copy_from_slice(&noise[32..80]);
        msg.encrypted_payload
            .copy_from_slice(&noise[80..INITIATION_NOISE_LEN]);
        msg.mac1 = p.mac1.mac1(&msg.to_bytes()[..Initiation::MAC1_OFFSET]);
        if let Some((cookie, _)) = &p.cookie {
            msg.mac2 = cookie.mac2(&msg.to_bytes()[..Initiation::MAC2_OFFSET]);
        }
        self.counter_next += 1;
        if self.counter_next == self.counter_end - COUNTER_BLOCK / 2 {
            out.push(Output::Event(Event::CounterBlockNeeded {
                next_start: HandshakeCounter(self.counter_end),
            }));
        }
        self.pending.insert(
            peer,
            Pending {
                handshake,
                base,
                mac1: msg.mac1,
                started,
                sent: cx.now,
            },
        );
        out.push(Output::Transmit {
            to: Destination::Peer(peer),
            datagram: msg.to_bytes().to_vec(),
        });
        Ok(())
    }

    fn on_initiation(
        &mut self,
        cx: Ctx<'_>,
        src: &[u8],
        bytes: &[u8],
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        let msg = Initiation::decode(bytes).map_err(|_| DropReason::Malformed)?;
        if !self
            .mac1
            .verify_mac1(&bytes[..Initiation::MAC1_OFFSET], &msg.mac1)
        {
            return Err(DropReason::Mac1);
        }
        if self.is_under_load() {
            self.retire_cookie_secrets(cx.now);
            if self.cookie_current.is_none() {
                let mut secret = draw::<32>(cx.rng)?;
                self.cookie_current = Some((CookieSecret::from_bytes(secret), cx.now));
                wipe(&mut secret);
            }
            let previous = self.cookie_previous.as_ref().map(|(s, _)| s);
            let valid = self.cookie_current.as_ref().is_some_and(|(current, _)| {
                verify_mac2(
                    current,
                    previous,
                    src,
                    &bytes[..Initiation::MAC2_OFFSET],
                    &msg.mac2,
                )
            });
            if !valid {
                return self.send_cookie_reply(cx, src, &msg, out);
            }
        }
        let mut noise = [0u8; INITIATION_NOISE_LEN];
        noise.copy_from_slice(&bytes[Initiation::NOISE_RANGE]);
        let (handshake, initiator, payload) =
            ResponderHandshake::read_initiation(&self.local, &noise)
                .map_err(|_| DropReason::Unauthentic)?;
        if !self.peers.contains_key(&initiator) {
            return Err(DropReason::UnknownPeer);
        }
        let payload = InitiationPayload::decode(&payload).map_err(|_| DropReason::Payload)?;
        if self
            .highest
            .get(&initiator)
            .is_some_and(|h| payload.counter.0 <= *h)
        {
            return Err(DropReason::Replay);
        }
        self.highest.insert(initiator, payload.counter.0);
        self.expire_peer(cx.now, initiator, out);
        let start = draw::<4>(cx.rng)?;
        let mut secret = draw::<32>(cx.rng)?;
        let ephemeral = EphemeralSecret::from_bytes(secret);
        wipe(&mut secret);
        self.make_room(initiator, out);
        let id = self.next_session;
        let base = self.reserve_block(start, BlockUse::Session(id, Vni::INFRASTRUCTURE))?;
        let Some(peer) = self.peers.get(&initiator) else {
            self.blocks.remove(&base);
            return Err(DropReason::UnknownPeer);
        };
        let reply = ResponsePayload {
            queues: self.queues,
        }
        .to_bytes();
        let (keys, noise) = match handshake.write_response(ephemeral, &peer.psk, &reply) {
            Ok(written) => written,
            Err(_) => {
                self.blocks.remove(&base);
                return Err(DropReason::Unauthentic);
            }
        };
        let mut response = Response {
            sender_index: SessionIndex(base),
            receiver_index: msg.sender_index,
            ephemeral: [0; 32],
            encrypted_payload: [0; 20],
            mac1: [0; 16],
        };
        response.ephemeral.copy_from_slice(&noise[..32]);
        response
            .encrypted_payload
            .copy_from_slice(&noise[32..RESPONSE_NOISE_LEN]);
        response.mac1 = peer
            .mac1
            .mac1(&response.to_bytes()[..Response::MAC1_OFFSET]);
        out.push(Output::Transmit {
            to: Destination::Source,
            datagram: response.to_bytes().to_vec(),
        });
        let session = Session::new(
            initiator,
            cx.now,
            keys,
            base,
            msg.sender_index.0,
            (self.queues, payload.queues),
            false,
        );
        self.add_session(id, session, false, out);
        self.flush_held(cx.now, initiator, out);
        Ok(())
    }

    fn send_cookie_reply(
        &mut self,
        cx: Ctx<'_>,
        src: &[u8],
        msg: &Initiation,
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        let nonce = draw::<24>(cx.rng)?;
        let Some((secret, _)) = &self.cookie_current else {
            return Err(DropReason::Rng);
        };
        let encrypted_cookie = self
            .cookie_key
            .seal(&nonce, &secret.cookie(src), &msg.mac1)
            .map_err(|_| DropReason::Unauthentic)?;
        let reply = CookieReply {
            receiver_index: msg.sender_index,
            nonce,
            encrypted_cookie,
        };
        out.push(Output::Transmit {
            to: Destination::Source,
            datagram: reply.to_bytes().to_vec(),
        });
        Ok(())
    }

    fn on_response(
        &mut self,
        cx: Ctx<'_>,
        bytes: &[u8],
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        let msg = Response::decode(bytes).map_err(|_| DropReason::Malformed)?;
        if !self
            .mac1
            .verify_mac1(&bytes[..Response::MAC1_OFFSET], &msg.mac1)
        {
            return Err(DropReason::Mac1);
        }
        let peer_key = self.pending_at(msg.receiver_index)?;
        let (Some(peer), Some(pending)) =
            (self.peers.get(&peer_key), self.pending.remove(&peer_key))
        else {
            return Err(DropReason::UnknownIndex);
        };
        let Pending {
            handshake,
            base,
            mac1,
            started,
            sent,
        } = pending;
        let mut noise = [0u8; RESPONSE_NOISE_LEN];
        noise.copy_from_slice(&bytes[Response::NOISE_RANGE]);
        let (keys, payload) = match handshake.read_response(&self.local, &peer.psk, &noise) {
            Ok(read) => read,
            Err(rejected) => {
                let handshake = rejected.handshake;
                let pending = Pending {
                    handshake,
                    base,
                    mac1,
                    started,
                    sent,
                };
                self.pending.insert(peer_key, pending);
                return Err(DropReason::Unauthentic);
            }
        };
        let Ok(payload) = ResponsePayload::decode(&payload) else {
            self.blocks.remove(&base);
            out.push(Output::Event(Event::HandshakeAbandoned { peer: peer_key }));
            return Err(DropReason::Payload);
        };
        self.expire_peer(cx.now, peer_key, out);
        let id = self.next_session;
        self.blocks
            .insert(base, BlockUse::Session(id, Vni::INFRASTRUCTURE));
        let session = Session::new(
            peer_key,
            cx.now,
            keys,
            base,
            msg.sender_index.0,
            (self.queues, payload.queues),
            true,
        );
        self.add_session(id, session, true, out);
        // An initiator seals a keepalive at once, so the responder confirms.
        let keepalive = self.seal_internal(id, InnerFrame::Keepalive, out);
        self.note(keepalive);
        self.flush_held(cx.now, peer_key, out);
        Ok(())
    }

    fn on_cookie_reply(&mut self, cx: Ctx<'_>, bytes: &[u8]) -> Result<(), DropReason> {
        let msg = CookieReply::decode(bytes).map_err(|_| DropReason::Malformed)?;
        let peer_key = self.pending_at(msg.receiver_index)?;
        let (Some(peer), Some(pending)) =
            (self.peers.get_mut(&peer_key), self.pending.get(&peer_key))
        else {
            return Err(DropReason::UnknownIndex);
        };
        let cookie = peer
            .cookie_key
            .open(&msg.nonce, &msg.encrypted_cookie, &pending.mac1)
            .map_err(|_| DropReason::Unauthentic)?;
        peer.cookie = Some((cookie, cx.now));
        Ok(())
    }

    /// The peer whose pending initiation `index` names.
    fn pending_at(&self, index: SessionIndex) -> Result<PublicKey, DropReason> {
        match self.blocks.get(&index.0) {
            Some(BlockUse::Pending(peer)) => Ok(*peer),
            _ => Err(DropReason::UnknownIndex),
        }
    }

    /// Moves the current `cookie_secret` to previous once it is 120 seconds
    /// old, and forgets the previous one 120 seconds after that.
    fn retire_cookie_secrets(&mut self, now: Instant) {
        if self
            .cookie_previous
            .as_ref()
            .is_some_and(|(_, until)| now >= *until)
        {
            self.cookie_previous = None;
        }
        if self
            .cookie_current
            .as_ref()
            .is_some_and(|(_, made)| now >= made.saturating_add(COOKIE_TIME))
            && let Some((secret, made)) = self.cookie_current.take()
        {
            let until = made.saturating_add(COOKIE_TIME + COOKIE_TIME);
            self.cookie_previous = (now < until).then_some((secret, until));
        }
    }

    // Sessions (section 3.3).

    /// Discards the oldest session with `peer`, or its pending initiation,
    /// while one more would make more than 4.
    fn make_room(&mut self, peer: PublicKey, out: &mut Vec<Output>) {
        loop {
            let Some(p) = self.peers.get(&peer) else {
                return;
            };
            let pending = self.pending.get(&peer).map(|p| p.sent);
            if p.sessions.len() + usize::from(pending.is_some()) < MAX_SESSIONS_PER_PEER {
                return;
            }
            let oldest = p
                .sessions
                .first()
                .and_then(|id| self.sessions.get(id).map(|s| (*id, s.made)));
            match (oldest, pending) {
                (Some((_, made)), Some(sent)) if sent < made => self.abandon_pending(peer, out),
                (Some((id, _)), _) => self.discard_session(id, out),
                (None, Some(_)) => self.abandon_pending(peer, out),
                (None, None) => return,
            }
        }
    }

    /// Reserves the first free block at or after `start` rounded down to a
    /// multiple of 64, wrapping.
    fn reserve_block(&mut self, start: [u8; 4], use_: BlockUse) -> Result<u32, DropReason> {
        let first = u32::from_le_bytes(start) & !(BLOCK - 1);
        let mut base = first;
        while self.blocks.contains_key(&base) {
            base = base.wrapping_add(BLOCK);
            if base == first {
                return Err(DropReason::NoFreeBlock);
            }
        }
        self.blocks.insert(base, use_);
        Ok(base)
    }

    /// Adds a session as the newest with its peer. The older sessions keep
    /// their receive keys until [`Link::moved_on`] starts their 10 seconds.
    fn add_session(&mut self, id: u64, session: Session, initiator: bool, out: &mut Vec<Output>) {
        let peer = session.peer;
        self.next_session += 1;
        if let Some(p) = self.peers.get_mut(&peer) {
            p.sessions.push(id);
        }
        self.sessions.insert(id, session);
        out.push(Output::Event(Event::SessionMade { peer, initiator }));
    }

    /// Discards `peer`'s sessions 180 seconds after their handshakes and
    /// drops the receive keys whose 10 seconds are over (section 3.3). Every
    /// call that seals or opens under a session runs this first, so no key
    /// is used past its lifetime whether or not [`Link::tick`] ran at the
    /// deadline.
    fn expire_peer(&mut self, now: Instant, peer: PublicKey, out: &mut Vec<Output>) {
        let expired = |s: &Session| now >= s.made.saturating_add(REJECT_AFTER);
        // Sessions are listed oldest first, so the expired ones lead.
        while let Some(id) = self.peers.get(&peer).and_then(|p| {
            p.sessions
                .first()
                .copied()
                .filter(|id| self.sessions.get(id).is_some_and(expired))
        }) {
            self.discard_session(id, out);
        }
        if let Some(p) = self.peers.get(&peer) {
            for id in &p.sessions {
                if let Some(s) = self.sessions.get_mut(id)
                    && s.recv_until.is_some_and(|until| now >= until)
                {
                    s.drop_recv();
                }
            }
        }
    }

    /// Starts the 10 seconds of section 3.3 on the receive key of each
    /// session with `peer` older than `newer`, once a transport packet
    /// accepted under `newer` shows that the peer no longer seals under it.
    ///
    /// The peer seals under the newest session confirmed at the peer, in the
    /// peer's own order (3.4, 3.6), which can differ from this node's when
    /// two handshakes cross. Of two sessions, the one the peer made later is
    /// the one this node made later too, except for one pair: an older
    /// session the peer initiated and a newer one this node initiated, when
    /// the peer's response and this node's initiation crossed. For that pair
    /// only a packet on VNI 0 other than a bind, which the peer seals under
    /// its newest confirmed session, shows that the peer moved on. A bind is
    /// sealed under the session its request came under, and a packet for
    /// another VNI under an older session while the newer one lacks its
    /// binding, so neither shows it. Until a packet shows it, the older
    /// receive key is kept, at most until the session's 180 seconds.
    fn moved_on(&mut self, now: Instant, peer: PublicKey, newer: u64, on_vni_0: bool) {
        let Some(newer_initiated) = self.sessions.get(&newer).map(|s| s.initiator) else {
            return;
        };
        let Some(p) = self.peers.get(&peer) else {
            return;
        };
        let until = now.saturating_add(PREVIOUS_RECV_TIME);
        for id in p.sessions.iter().take_while(|id| **id != newer) {
            if let Some(s) = self.sessions.get_mut(id) {
                let same_order = s.initiator || !newer_initiated;
                if (same_order || on_vni_0) && s.recv_until.is_none() {
                    s.recv_until = Some(until);
                }
            }
        }
    }

    fn abandon_pending(&mut self, peer: PublicKey, out: &mut Vec<Output>) {
        if let Some(pending) = self.pending.remove(&peer) {
            self.blocks.remove(&pending.base);
            out.push(Output::Event(Event::HandshakeAbandoned { peer }));
        }
    }

    /// Discards a session: its keys are erased as it drops, its blocks freed
    /// and its reassemblies dropped.
    fn discard_session(&mut self, id: u64, out: &mut Vec<Output>) {
        let Some(session) = self.sessions.remove(&id) else {
            return;
        };
        for base in session.local.values() {
            self.blocks.remove(base);
        }
        if let Some(p) = self.peers.get_mut(&session.peer) {
            p.sessions.retain(|s| *s != id);
        }
        self.reassembly.remove_session(id);
        out.push(Output::Event(Event::SessionDiscarded {
            peer: session.peer,
        }));
    }

    // Transport (section 3.4).

    fn on_transport(
        &mut self,
        cx: Ctx<'_>,
        bytes: &mut [u8],
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        let header = Transport::decode(bytes)
            .map_err(|_| DropReason::Malformed)?
            .header();
        let index = header.receiver_index.0;
        let base = index & !(BLOCK - 1);
        let queue = (index - base) as u16;
        let Some(BlockUse::Session(id, vni)) = self.blocks.get(&base).copied() else {
            return Err(DropReason::UnknownIndex);
        };
        let Some(peer) = self.sessions.get(&id).map(|s| s.peer) else {
            return Err(DropReason::UnknownIndex);
        };
        self.expire_peer(cx.now, peer, out);
        let Some(session) = self.sessions.get_mut(&id) else {
            return Err(DropReason::UnknownIndex);
        };
        let (Some(recv), true) = (&session.recv, queue < session.peer_queues.get()) else {
            return Err(DropReason::UnknownIndex);
        };
        let window = usize::from(queue);
        let counter = session.windows[window]
            .check(header.counter)
            .ok_or(DropReason::Replay)?;
        let (head, body) = bytes.split_at_mut(TRANSPORT_HEADER_LEN);
        let mut aad = [0u8; TRANSPORT_HEADER_LEN];
        aad.copy_from_slice(head);
        let frame = recv
            .open_in_place(queue, counter, &aad, body)
            .map_err(|_| DropReason::Unauthentic)?;
        session.windows[window].accept(counter);
        let confirmed = !session.confirmed;
        session.confirmed = true;
        if confirmed {
            out.push(Output::Event(Event::SessionConfirmed { peer }));
        }
        let on_vni_0 = vni == Vni::INFRASTRUCTURE && !is_bind(frame);
        self.moved_on(cx.now, peer, id, on_vni_0);
        let result = self.on_frame(cx, (id, index, vni), peer, frame, out);
        if confirmed {
            self.flush_held(cx.now, peer, out);
        }
        result
    }

    fn on_frame(
        &mut self,
        cx: Ctx<'_>,
        (id, index, vni): (u64, u32, Vni),
        peer: PublicKey,
        frame: &[u8],
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        match InnerFrame::decode(frame, vni).map_err(|_| DropReason::Malformed)? {
            InnerFrame::Keepalive => Ok(()),
            InnerFrame::Control(control) => self.on_control(cx, id, control, out),
            InnerFrame::Ip(packet) => {
                deliver(peer, vni, packet, out);
                Ok(())
            }
            InnerFrame::Fragment(fragment) => {
                let key = (id, index, fragment.id().0);
                let (whole, evicted) = self
                    .reassembly
                    .insert(cx.now, peer, key, &fragment)
                    .map_err(|refused| match refused {
                        Refused::Conflict => DropReason::Fragment,
                        Refused::TooLarge => DropReason::Reassembly,
                    })?;
                self.count_n(DropReason::Reassembly, evicted);
                let Some(whole) = whole else {
                    return Ok(());
                };
                match InnerFrame::decode_reassembled(&whole, vni)
                    .map_err(|_| DropReason::Malformed)?
                {
                    InnerFrame::Control(control) => self.on_control(cx, id, control, out),
                    InnerFrame::Ip(packet) => {
                        deliver(peer, vni, packet, out);
                        Ok(())
                    }
                    InnerFrame::Keepalive | InnerFrame::Fragment(_) => Err(DropReason::Malformed),
                }
            }
        }
    }

    fn on_control(
        &mut self,
        cx: Ctx<'_>,
        id: u64,
        control: Control,
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        match control {
            Control::BindRequest { vni } => {
                if vni == Vni::INFRASTRUCTURE || !self.local_vnis.contains(&vni) {
                    return Err(DropReason::Bind);
                }
                let Some(session) = self.sessions.get(&id) else {
                    return Err(DropReason::UnknownIndex);
                };
                let base = match session.local.get(&vni) {
                    Some(base) => *base,
                    None if session.local.len() >= MAX_BLOCKS_PER_SESSION => {
                        return Err(DropReason::Bind);
                    }
                    None => {
                        let start = draw::<4>(cx.rng)?;
                        let base = self.reserve_block(start, BlockUse::Session(id, vni))?;
                        if let Some(session) = self.sessions.get_mut(&id) {
                            session.local.insert(vni, base);
                        }
                        base
                    }
                };
                let bind = Control::Bind {
                    vni,
                    base: SessionIndex(base),
                };
                self.seal_internal(id, InnerFrame::Control(bind), out)
            }
            Control::Bind { vni, base } => {
                let Some(session) = self.sessions.get_mut(&id) else {
                    return Err(DropReason::UnknownIndex);
                };
                match session.bindings.get(&vni) {
                    Some(held) if *held == base.0 && vni != Vni::INFRASTRUCTURE => Ok(()),
                    Some(_) => Err(DropReason::Bind),
                    None if !session.bind_sent.contains_key(&vni)
                        || session.bindings.len() > MAX_BINDINGS_PER_SESSION =>
                    {
                        Err(DropReason::Bind)
                    }
                    None => {
                        session.bindings.insert(vni, base.0);
                        let peer = session.peer;
                        self.flush_held(cx.now, peer, out);
                        Ok(())
                    }
                }
            }
        }
    }

    fn send_packet(
        &mut self,
        cx: Ctx<'_>,
        to: Outbound,
        packet: &[u8],
        out: &mut Vec<Output>,
    ) -> Result<Sent, DropReason> {
        if !self.peers.contains_key(&to.peer) {
            return Err(DropReason::UnknownPeer);
        }
        if to.queue >= self.queues.get() {
            return Err(DropReason::Queue);
        }
        if IpPacket::new(packet).is_none() {
            return Err(DropReason::NotIp);
        }
        self.expire_peer(cx.now, to.peer, out);
        self.rekey_if_due(cx, to.peer, out);
        self.request_binding(cx.now, to.peer, to.vni, out);
        if let Some(id) = self.choose(&to.peer, to.vni) {
            self.seal_frame(id, to.vni, to.queue, packet, to.budget, out)?;
            return Ok(Sent::Sealed);
        }
        let Some(p) = self.peers.get_mut(&to.peer) else {
            return Err(DropReason::UnknownPeer);
        };
        if !p.held.contains_key(&to.vni) && p.held.len() >= MAX_BLOCKS_PER_SESSION {
            return Err(DropReason::Held);
        }
        let queue = p.held.entry(to.vni).or_default();
        if queue.len() >= MAX_HELD_PER_VNI {
            return Err(DropReason::Held);
        }
        queue.push_back(Held {
            at: cx.now,
            queue: to.queue,
            budget: to.budget,
            packet: packet.to_vec(),
        });
        if p.sessions.is_empty() && !self.pending.contains_key(&to.peer) {
            let started = self.initiate(cx, to.peer, cx.now, out);
            self.note(started);
        }
        Ok(Sent::Held)
    }

    /// Starts a handshake when the newest session with `peer` is 120 seconds
    /// old or has sealed 2^30 packets on a queue, and none is pending.
    fn rekey_if_due(&mut self, cx: Ctx<'_>, peer: PublicKey, out: &mut Vec<Output>) {
        let newest = self
            .peers
            .get(&peer)
            .and_then(|p| p.sessions.last())
            .and_then(|id| self.sessions.get(id));
        let due = newest.is_some_and(|s| {
            cx.now >= s.made.saturating_add(REKEY_AFTER) || s.max_counter() >= REKEY_AFTER_PACKETS
        });
        if due && !self.pending.contains_key(&peer) {
            let started = self.initiate(cx, peer, cx.now, out);
            self.note(started);
        }
    }

    /// The session section 3.4 seals a packet for `vni` to `peer` under: the
    /// newest confirmed one that holds a binding for `vni`, or for VNI 0 when
    /// none is confirmed, the newest.
    fn choose(&self, peer: &PublicKey, vni: Vni) -> Option<u64> {
        let p = self.peers.get(peer)?;
        let mut any_confirmed = false;
        for id in p.sessions.iter().rev() {
            let s = self.sessions.get(id)?;
            any_confirmed |= s.confirmed;
            if s.confirmed && s.bindings.contains_key(&vni) {
                return Some(*id);
            }
        }
        (vni == Vni::INFRASTRUCTURE && !any_confirmed)
            .then(|| p.sessions.last().copied())
            .flatten()
    }

    /// Sends a bind request for `vni` under the session a VNI 0 packet to
    /// `peer` would go under, if it holds no binding for `vni` and none was
    /// sent in the last second.
    fn request_binding(&mut self, now: Instant, peer: PublicKey, vni: Vni, out: &mut Vec<Output>) {
        if vni == Vni::INFRASTRUCTURE {
            return;
        }
        let Some(id) = self.choose(&peer, Vni::INFRASTRUCTURE) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let recent = session
            .bind_sent
            .get(&vni)
            .is_some_and(|at| now < at.saturating_add(HOLD_TIME));
        let full = !session.bind_sent.contains_key(&vni)
            && session.bind_sent.len() >= MAX_BINDINGS_PER_SESSION;
        if session.bindings.contains_key(&vni) || recent || full {
            return;
        }
        session.bind_sent.insert(vni, now);
        let request =
            self.seal_internal(id, InnerFrame::Control(Control::BindRequest { vni }), out);
        self.note(request);
    }

    /// Seals the packets for `vni` held for `peer` for which a session now
    /// qualifies, and asks for the bindings the others wait for.
    fn flush_held(&mut self, now: Instant, peer: PublicKey, out: &mut Vec<Output>) {
        let Some(p) = self.peers.get_mut(&peer) else {
            return;
        };
        let held = std::mem::take(&mut p.held);
        let mut waiting = BTreeMap::new();
        for (vni, mut queue) in held {
            while let (Some(id), Some(_)) = (self.choose(&peer, vni), queue.front()) {
                if let Some(h) = queue.pop_front() {
                    let sealed = self.seal_frame(id, vni, h.queue, &h.packet, h.budget, out);
                    self.note(sealed);
                }
            }
            if !queue.is_empty() {
                waiting.insert(vni, queue);
            }
        }
        let vnis: Vec<Vni> = waiting.keys().copied().collect();
        if let Some(p) = self.peers.get_mut(&peer) {
            p.held = waiting;
        }
        for vni in vnis {
            self.request_binding(now, peer, vni, out);
        }
    }

    /// Seals a keepalive or control frame on VNI 0, queue 0. These frames
    /// are at most 9 bytes and are never fragmented.
    fn seal_internal(
        &mut self,
        id: u64,
        frame: InnerFrame<'_>,
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        let mut buf = [0u8; 9];
        let len = frame.encode(&mut buf).map_err(|_| DropReason::TooLarge)?;
        self.seal(id, Vni::INFRASTRUCTURE, 0, &buf[..len], out)
    }

    /// Seals an IP or control frame, split into fragments when it is larger
    /// than `budget` (section 3.5).
    fn seal_frame(
        &mut self,
        id: u64,
        vni: Vni,
        queue: u16,
        frame: &[u8],
        budget: usize,
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        if frame.len() <= budget {
            return self.seal(id, vni, queue, frame, out);
        }
        let piece = budget
            .checked_sub(FRAGMENT_HEADER_LEN)
            .filter(|p| *p > 0)
            .ok_or(DropReason::TooLarge)?;
        let count = u8::try_from(frame.len().div_ceil(piece))
            .ok()
            .and_then(FragmentCount::new)
            .ok_or(DropReason::TooLarge)?;
        let session = self.sessions.get_mut(&id).ok_or(DropReason::UnknownIndex)?;
        let next = *session
            .send_counters
            .get(usize::from(queue))
            .ok_or(DropReason::Queue)?;
        if next + u64::from(count.get()) > COUNTER_LIMIT {
            return Err(DropReason::CounterExhausted);
        }
        let frag_id = u32::try_from(session.next_frag).map_err(|_| DropReason::TooLarge)?;
        session.next_frag += 1;
        let mut buf = Vec::with_capacity(budget);
        for (index, data) in (0..count.get()).zip(frame.chunks(piece)) {
            let fragment = Fragment::new(FragmentId(frag_id), index, count, data)
                .ok_or(DropReason::TooLarge)?;
            let fragment = InnerFrame::Fragment(fragment);
            buf.resize(fragment.encoded_len(), 0);
            fragment
                .encode(&mut buf)
                .map_err(|_| DropReason::TooLarge)?;
            self.seal(id, vni, queue, &buf, out)?;
        }
        Ok(())
    }

    /// Seals one transport packet carrying `frame` under session `id` for
    /// `vni` on `queue`.
    fn seal(
        &mut self,
        id: u64,
        vni: Vni,
        queue: u16,
        frame: &[u8],
        out: &mut Vec<Output>,
    ) -> Result<(), DropReason> {
        let session = self.sessions.get_mut(&id).ok_or(DropReason::UnknownIndex)?;
        let base = *session.bindings.get(&vni).ok_or(DropReason::Bind)?;
        let next = session
            .send_counters
            .get_mut(usize::from(queue))
            .ok_or(DropReason::Queue)?;
        if *next >= COUNTER_LIMIT {
            return Err(DropReason::CounterExhausted);
        }
        let header = TransportHeader {
            receiver_index: SessionIndex(base.wrapping_add(u32::from(queue))),
            counter: PacketCounter(*next).wire(),
        }
        .to_bytes();
        let mut datagram = vec![0u8; TRANSPORT_HEADER_LEN + frame.len() + TAG_LEN];
        datagram[..TRANSPORT_HEADER_LEN].copy_from_slice(&header);
        datagram[TRANSPORT_HEADER_LEN..TRANSPORT_HEADER_LEN + frame.len()].copy_from_slice(frame);
        session
            .send
            .seal_in_place(queue, *next, &header, &mut datagram[TRANSPORT_HEADER_LEN..])
            .map_err(|e| match e {
                SealError::QueueOutOfRange => DropReason::Queue,
                SealError::BufferTooShort => DropReason::TooLarge,
                _ => DropReason::CounterExhausted,
            })?;
        *next += 1;
        out.push(Output::Transmit {
            to: Destination::Peer(session.peer),
            datagram,
        });
        Ok(())
    }
}

/// Whether `frame` is a bind (section 3.4): the one frame on VNI 0 that is
/// sealed under the session its request came under rather than by session
/// choice.
fn is_bind(frame: &[u8]) -> bool {
    frame.first() == Some(&0x10) && frame.get(1) == Some(&2)
}

fn deliver(peer: PublicKey, vni: Vni, packet: IpPacket<'_>, out: &mut Vec<Output>) {
    out.push(Output::Deliver {
        peer,
        vni,
        packet: packet.bytes().to_vec(),
    });
}

/// `N` bytes from the `alloy-rt` randomness trait.
fn draw<const N: usize>(rng: &dyn Rng) -> Result<[u8; N], DropReason> {
    let mut bytes = [0u8; N];
    rng.fill(&mut bytes).map_err(|_| DropReason::Rng)?;
    Ok(bytes)
}

/// Overwrites this frame's copy of secret bytes once the key type that
/// erases itself holds them. `black_box` keeps the store from being removed
/// as dead.
fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
}

#[cfg(test)]
mod tests;
