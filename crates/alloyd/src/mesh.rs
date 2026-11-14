//! A node's AL sessions over the underlay, and the node channel on them
//! (ARFC 0002 sections 3.13 and 3.15, ARFC 0003).
//!
//! [`Mesh`] drives one [`Link`] of `alloy-link` over the node's UDP sockets
//! on its mesh port: it hands the link every datagram that arrives and the
//! passing of time, sends what the link transmits, makes the handshake
//! counter blocks the link asks for durable before it uses them, and carries
//! node messages as the IPv6 frames of section 3.13 on VNI 0. Requests go
//! out through one [`Requester`]; requests that arrive go to whoever reads
//! [`Inbound`].
//!
//! # Where a datagram goes
//!
//! A peer is reached by one of two paths. A node dials a peer at an address
//! it was given: a joiner an anchor of its token, a node a home of its view.
//! An anchor answers a peer only at the endpoint the section 3.15 rule of
//! rung 2 gives: the source endpoint of the handshake initiation it answered
//! to make the peer's latest session, and only once it has accepted a
//! transport packet under that session from that same endpoint. A session
//! confirmed from another endpoint gets nothing, and no packet changes the
//! endpoint later; a peer whose endpoint changes is reached again only on a
//! session it opens from its new one. No reflection request is sent or
//! answered.
//!
//! # Join service
//!
//! An anchor turns on accept-unknown ([`Link::set_accept_unknown`]), so
//! initiators that are not peers yet make sessions within the link's rate
//! limits. On such a session it takes only the join service requests
//! (`join_hello`, `join_request`, `join_state`, `renew`, `withdraw`), at
//! most [`JOIN_SERVICE_PER_SESSION`] new request per second per session and
//! [`JOIN_SERVICE_TOTAL`] per second over all of them, and drops every other
//! message silently. A peer it admits with [`Command::Admit`] is from then
//! on a registered node.
//!
//! # This version
//!
//! UDP only: the `stream` underlay of ARFC 0004 section 3.6 is not built,
//! so a session is never made over TCP. No `forwarded` is sent or taken,
//! since in rung 2 every anchor holds `control` (section 3.13, This
//! version).

use std::collections::{BTreeMap, VecDeque};
use std::future::poll_fn;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use alloy_crypto::al::{PresharedKey, PublicKey, StaticSecret};
use alloy_id::{ClusterId, DataDir, StoreError};
use alloy_link::{COUNTER_BLOCK, Config, Ctx, Destination, Event, Link, Outbound, Output};
use alloy_rt::{Datagram, Instant, NetError, RecvSlot, Rng, Runtime, UdpOptions, UdpSocket};
use alloy_wire::{HandshakeCounter, QueueCount, Vni};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::channel::frame;
use crate::channel::proto::{self, Body, Kind, NodeMessage};
use crate::channel::requester::{Action, ControlChoice, Outcome, RequestIds, Requester, Submitted};

/// The payload budget of an underlay path (ARFC 0003 section 3.5): the
/// largest inner frame one transport packet carries. A larger node message
/// is split by AL. It keeps every datagram within the IPv6 minimum MTU of
/// 1,280 bytes with the AL, UDP, and IP headers.
pub(crate) const PATH_BUDGET: usize = 1_120;

/// New join service requests taken per second from one session of an
/// unregistered initiator (ARFC 0002 section 3.13).
pub(crate) const JOIN_SERVICE_PER_SESSION: usize = 1;

/// New join service requests taken per second over all sessions of
/// unregistered initiators (ARFC 0002 section 3.13).
pub(crate) const JOIN_SERVICE_TOTAL: usize = 100;

/// Receive slots per socket, and their size: a datagram of AL over a path
/// within [`PATH_BUDGET`] always fits.
const SLOTS: usize = 16;
const SLOT_LEN: usize = 2_048;

/// The bound of each channel between the mesh task and its users. A
/// message to a full channel is dropped; the node channel's resends carry
/// what it held.
const CHANNEL: usize = 1_024;

/// The longest the task sleeps, so that it notices a closed command channel
/// and the rate windows age even when nothing else is due.
const MAX_SLEEP: Duration = Duration::from_secs(1);

/// The pause after a socket fails to receive, so that a broken socket does
/// not spin the task.
const RECEIVE_ERROR_PAUSE: Duration = Duration::from_millis(100);

/// The most sessions of unregistered initiators whose last new request the
/// rate limit keeps. Past it, entries older than the window are dropped.
const RATE_ENTRIES: usize = 4_096;

/// The window of the join service rate limits.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// Where a request goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// This peer, by its mesh key.
    Peer(PublicKey),
    /// The control node the choice of ARFC 0002 section 3.13 gives.
    Control,
}

/// How a request ended.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ended {
    /// The peer it went to, `None` when it was never sent.
    pub(crate) peer: Option<PublicKey>,
    /// Its outcome.
    pub(crate) outcome: Outcome,
}

/// What the users of a [`Mesh`] ask of it.
#[derive(Debug)]
pub(crate) enum Command {
    /// Make `peer` a peer of the link, reached at `addr`, and start a
    /// handshake with it.
    Dial {
        /// Its mesh key.
        peer: PublicKey,
        /// Its address and mesh port.
        addr: SocketAddr,
    },
    /// Remove `peer`: its sessions, its pending handshake, and its path.
    Forget {
        /// Its mesh key.
        peer: PublicKey,
    },
    /// Admit `peer`, an unknown initiator or a node known from the
    /// cluster's state, as a registered peer.
    Admit {
        /// Its mesh key.
        peer: PublicKey,
    },
    /// Send `message`, an encoded node message that answers a request, to
    /// `peer`.
    Reply {
        /// Its mesh key.
        peer: PublicKey,
        /// The encoded `NodeMessage`.
        message: Vec<u8>,
    },
    /// Send a request with `body` and the time limit `limit` to `target`,
    /// and tell `done` how it ended. A request of a kind already held takes
    /// that one's outcome.
    Request {
        /// The request's body.
        body: Body,
        /// The time limit of the step that sends it.
        limit: Duration,
        /// Where it goes.
        target: Target,
        /// Where its outcome goes.
        done: oneshot::Sender<Ended>,
    },
    /// The control nodes [`Target::Control`] chooses from, in the view's
    /// order.
    SetControl(Vec<PublicKey>),
}

/// What a [`Mesh`] tells its user.
#[derive(Debug)]
pub(crate) enum Inbound {
    /// A session with `peer` was made.
    SessionMade {
        /// Its mesh key.
        peer: PublicKey,
    },
    /// A handshake with `peer` ended without a session.
    Abandoned {
        /// Its mesh key.
        peer: PublicKey,
    },
    /// A request that `peer` sent.
    Request {
        /// The mesh key its session authenticated.
        peer: PublicKey,
        /// Whether `peer` is a registered peer, not an unknown initiator.
        registered: bool,
        /// The source of the datagram that carried it.
        source: Option<SocketAddr>,
        /// The request.
        message: NodeMessage,
    },
}

/// How a peer is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    /// At an address this node was given.
    Dial(SocketAddr),
    /// At the endpoint of the initiation of its latest session, once that
    /// session is confirmed from it (ARFC 0002 section 3.15).
    Answer {
        endpoint: SocketAddr,
        confirmed: bool,
    },
}

/// The requests of one kind held, and who waits for them.
#[derive(Debug)]
struct Waiting {
    target: Target,
    done: Vec<oneshot::Sender<Ended>>,
}

/// The join service's rate limits on sessions of unregistered initiators.
#[derive(Debug, Default)]
struct JoinRate {
    per_session: BTreeMap<PublicKey, (u64, Instant)>,
    taken: VecDeque<Instant>,
}

impl JoinRate {
    /// Whether a request with `request_id` from `peer` at `now` is taken: a
    /// resend of the last one taken always is, a new one only within the
    /// limits.
    fn take(&mut self, peer: PublicKey, request_id: u64, now: Instant) -> bool {
        while self
            .taken
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= RATE_WINDOW)
        {
            self.taken.pop_front();
        }
        if let Some((last, at)) = self.per_session.get(&peer) {
            if request_id <= *last {
                return request_id == *last;
            }
            if now.saturating_duration_since(*at) < RATE_WINDOW / JOIN_SERVICE_PER_SESSION as u32 {
                return false;
            }
        }
        if self.taken.len() >= JOIN_SERVICE_TOTAL {
            return false;
        }
        if self.per_session.len() >= RATE_ENTRIES {
            self.per_session
                .retain(|_, (_, at)| now.saturating_duration_since(*at) < RATE_WINDOW);
            if self.per_session.len() >= RATE_ENTRIES {
                return false;
            }
        }
        self.per_session.insert(peer, (request_id, now));
        self.taken.push_back(now);
        true
    }
}

/// Request ids drawn from the link's handshake counter.
struct Draws<'a> {
    link: &'a mut Link,
    out: &'a mut Vec<Output>,
}

impl RequestIds for Draws<'_> {
    fn next_counter(&mut self) -> Option<u64> {
        self.link.draw_counter(self.out).map(|c| c.0)
    }
}

/// Reserves the next block of handshake counter values durably (ARFC 0003
/// section 3.2, ARFC 0002 section 3.14) and returns its first value.
///
/// # Errors
///
/// [`StoreError`] when `handshake-counter` cannot be read or written.
pub(crate) async fn reserve_counter(data: &DataDir) -> Result<u64, StoreError> {
    let start = data.read_handshake_counter().await?.unwrap_or(0);
    data.write_handshake_counter(start.saturating_add(COUNTER_BLOCK))
        .await?;
    Ok(start)
}

/// Binds the mesh port `port` on every local IPv4 and IPv6 address. IPv6 is
/// left out, with a log line, where the host refuses it.
///
/// # Errors
///
/// The [`NetError`] of the IPv4 bind.
pub(crate) fn bind(runtime: &Runtime, port: u16) -> Result<Vec<Box<dyn UdpSocket>>, NetError> {
    let net = runtime.net();
    let v4 = net.bind_udp(
        SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port)),
        UdpOptions::default(),
    )?;
    let mut sockets = vec![v4];
    match net.bind_udp(
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
        UdpOptions::default(),
    ) {
        Ok(v6) => sockets.push(v6),
        Err(e) => debug!(reason = %e, "the mesh port is not bound on IPv6"),
    }
    Ok(sockets)
}

/// The bytes that identify a datagram's source for the cookie of ARFC 0003
/// section 3.1: the source address and port.
fn cookie_source(src: SocketAddr) -> Vec<u8> {
    let mut bytes = match src.ip() {
        std::net::IpAddr::V4(v4) => v4.octets().to_vec(),
        std::net::IpAddr::V6(v6) => v6.octets().to_vec(),
    };
    bytes.extend_from_slice(&src.port().to_be_bytes());
    bytes
}

/// One node's AL link and node channel. See the module documentation.
pub(crate) struct Mesh {
    runtime: Runtime,
    rng: Arc<dyn Rng>,
    link: Link,
    data: DataDir,
    cluster_id: ClusterId,
    own_infra: Ipv6Addr,
    answer_unknown: bool,
    paths: BTreeMap<PublicKey, Path>,
    requester: Requester,
    waiters: BTreeMap<Kind, Waiting>,
    control: ControlChoice,
    inbound: mpsc::Sender<Inbound>,
    rate: JoinRate,
    counter_due: Option<HandshakeCounter>,
}

impl std::fmt::Debug for Mesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mesh")
            .field("own_infra", &self.own_infra)
            .finish_non_exhaustive()
    }
}

/// What woke the mesh task.
enum Woke {
    Command(Command),
    Closed,
    Received(usize, Result<usize, NetError>),
    Timer,
}

impl Mesh {
    /// A mesh for the node with mesh key `key` in cluster `cluster_id`,
    /// whose link uses the handshake counters from `counter_start`, which
    /// [`reserve_counter`] made durable, and writes later blocks to `data`.
    /// An anchor passes `answer_unknown` to take sessions from unregistered
    /// initiators. Returns it with the receiver of its [`Inbound`].
    #[must_use]
    pub(crate) fn new(
        runtime: &Runtime,
        key: StaticSecret,
        counter_start: u64,
        data: DataDir,
        cluster_id: ClusterId,
        answer_unknown: bool,
    ) -> (Mesh, mpsc::Receiver<Inbound>) {
        let own_infra = cluster_id.infra_addr(&key.public_key());
        let config = Config {
            // 1 is within the 1 to 64 queues `QueueCount::new` accepts.
            queues: QueueCount::new(1).expect("1 queue is a valid queue count"),
            counter_start: HandshakeCounter(counter_start),
        };
        let mut link = Link::new(key, config);
        link.set_accept_unknown(answer_unknown);
        let (inbound, receiver) = mpsc::channel(CHANNEL);
        let mesh = Mesh {
            runtime: runtime.clone(),
            rng: Arc::clone(runtime.rng()),
            link,
            data,
            cluster_id,
            own_infra,
            answer_unknown,
            paths: BTreeMap::new(),
            requester: Requester::new(),
            waiters: BTreeMap::new(),
            control: ControlChoice::new(Vec::new()),
            inbound,
            rate: JoinRate::default(),
            counter_due: None,
        };
        (mesh, receiver)
    }

    /// Runs the mesh over `sockets`, taking [`Command`]s from `commands`,
    /// until that channel closes or the task is cancelled.
    pub(crate) async fn run(
        mut self,
        sockets: Vec<Box<dyn UdpSocket>>,
        mut commands: mpsc::Receiver<Command>,
    ) {
        let mut slots: Vec<Vec<RecvSlot>> = sockets
            .iter()
            .map(|_| {
                (0..SLOTS)
                    .filter_map(|_| RecvSlot::new(SLOT_LEN).ok())
                    .collect()
            })
            .collect();
        let clock = Arc::clone(self.runtime.clock());
        loop {
            let deadline = self.deadline(clock.now());
            let woke = {
                let mut sleep = clock.sleep_until(deadline);
                let mut receives: Vec<_> = sockets
                    .iter()
                    .zip(slots.iter_mut())
                    .map(|(socket, slots)| socket.recv(slots))
                    .collect();
                poll_fn(|cx| {
                    match commands.poll_recv(cx) {
                        Poll::Ready(Some(command)) => return Poll::Ready(Woke::Command(command)),
                        Poll::Ready(None) => return Poll::Ready(Woke::Closed),
                        Poll::Pending => {}
                    }
                    for (i, receive) in receives.iter_mut().enumerate() {
                        if let Poll::Ready(result) = receive.as_mut().poll(cx) {
                            return Poll::Ready(Woke::Received(i, result));
                        }
                    }
                    sleep.as_mut().poll(cx).map(|()| Woke::Timer)
                })
                .await
            };
            let now = clock.now();
            let mut tx = Vec::new();
            match woke {
                Woke::Closed => return,
                Woke::Command(command) => {
                    let mut out = Vec::new();
                    self.command(now, command, &mut out);
                    self.process(now, None, out, Vec::new(), &mut tx);
                }
                Woke::Received(i, Ok(n)) => {
                    for slot in slots[i].iter_mut().take(n) {
                        if slot.truncated {
                            continue;
                        }
                        let src = slot.src;
                        let mut out = Vec::new();
                        let cx = Ctx {
                            now,
                            rng: self.rng.as_ref(),
                        };
                        if let Err(reason) = self.link.receive(
                            cx,
                            &cookie_source(src),
                            &mut slot.buf[..slot.len],
                            &mut out,
                        ) {
                            debug!(?reason, "a datagram was dropped");
                        }
                        self.process(now, Some(src), out, Vec::new(), &mut tx);
                    }
                }
                Woke::Received(_, Err(e)) => {
                    warn!(reason = %e, "the mesh socket could not receive");
                    self.runtime.sleep(RECEIVE_ERROR_PAUSE).await;
                }
                Woke::Timer => {}
            }
            let now = clock.now();
            if self.link.next_deadline().is_some_and(|due| due <= now) {
                let mut out = Vec::new();
                let cx = Ctx {
                    now,
                    rng: self.rng.as_ref(),
                };
                self.link.tick(cx, &mut out);
                self.process(now, None, out, Vec::new(), &mut tx);
            }
            let mut out = Vec::new();
            let actions = self.poll_requests(now, &mut out);
            self.process(now, None, out, actions, &mut tx);
            send_all(&sockets, &tx).await;
            if let Some(next) = self.counter_due.take() {
                match self
                    .data
                    .write_handshake_counter(next.0.saturating_add(COUNTER_BLOCK))
                    .await
                {
                    Ok(()) => self.link.add_counter_block(),
                    Err(e) => warn!(
                        reason = %e,
                        "the next block of handshake counters could not be made durable"
                    ),
                }
            }
        }
    }

    fn deadline(&self, now: Instant) -> Instant {
        let mut deadline = now.saturating_add(MAX_SLEEP);
        if let Some(due) = self.link.next_deadline() {
            deadline = deadline.min(due);
        }
        if let Some(due) = self.requester.next_deadline() {
            deadline = deadline.min(due);
        }
        if self.requester.has_ready() {
            deadline = now;
        }
        deadline
    }

    fn command(&mut self, now: Instant, command: Command, out: &mut Vec<Output>) {
        match command {
            Command::Dial { peer, addr } => {
                self.link.add_peer(peer, PresharedKey::none());
                self.paths.insert(peer, Path::Dial(addr));
                let rng = Arc::clone(&self.rng);
                let cx = Ctx {
                    now,
                    rng: rng.as_ref(),
                };
                if let Err(reason) = self.link.connect(cx, peer, out) {
                    debug!(?reason, "no handshake was started");
                }
            }
            Command::Forget { peer } => {
                self.link.remove_peer(&peer, out);
                self.paths.remove(&peer);
            }
            Command::Admit { peer } => self.link.add_peer(peer, PresharedKey::none()),
            Command::Reply { peer, message } => self.seal(now, peer, &message, out),
            Command::Request {
                body,
                limit,
                target,
                done,
            } => {
                let Some(kind) = Kind::of(&body) else {
                    return;
                };
                match self.requester.submit(body, limit) {
                    Submitted::Queued => {
                        self.waiters.insert(
                            kind,
                            Waiting {
                                target,
                                done: vec![done],
                            },
                        );
                    }
                    Submitted::Held => {
                        if let Some(waiting) = self.waiters.get_mut(&kind) {
                            waiting.done.push(done);
                        }
                    }
                    Submitted::NotARequest => {}
                }
            }
            Command::SetControl(nodes) => self.control.set_view(nodes),
        }
    }

    fn poll_requests(&mut self, now: Instant, out: &mut Vec<Output>) -> Vec<Action> {
        let Mesh {
            requester,
            link,
            waiters,
            control,
            ..
        } = self;
        let mut actions = Vec::new();
        let mut ids = Draws { link, out };
        let mut peer_for = |kind: Kind| match waiters.get(&kind).map(|w| w.target) {
            Some(Target::Peer(peer)) => Some(peer),
            Some(Target::Control) => control.current(),
            None => None,
        };
        requester.poll(now, &mut ids, &mut peer_for, &mut actions);
        actions
    }

    /// Carries out the link's outputs and the requester's actions, and what
    /// they lead to, until nothing is left. `src` is the source of the
    /// datagram being received, `None` when none is.
    fn process(
        &mut self,
        now: Instant,
        src: Option<SocketAddr>,
        mut out: Vec<Output>,
        mut actions: Vec<Action>,
        tx: &mut Vec<(SocketAddr, Vec<u8>)>,
    ) {
        while !out.is_empty() || !actions.is_empty() {
            for output in std::mem::take(&mut out) {
                match output {
                    Output::Transmit { to, datagram } => {
                        let addr = match to {
                            Destination::Source => src,
                            Destination::Peer(peer) => self.address_of(&peer),
                        };
                        if let Some(addr) = addr {
                            tx.push((addr, datagram));
                        }
                    }
                    Output::Deliver { peer, vni, packet } => {
                        if vni == Vni::INFRASTRUCTURE {
                            self.deliver(now, src, peer, &packet, &mut actions);
                        }
                    }
                    Output::Event(event) => self.event(src, event),
                }
            }
            for action in std::mem::take(&mut actions) {
                match action {
                    Action::Send { peer, message } => self.seal(now, peer, &message, &mut out),
                    Action::Ended {
                        kind,
                        peer,
                        outcome,
                    } => self.ended(now, kind, peer, outcome, &mut out),
                }
            }
        }
    }

    fn address_of(&self, peer: &PublicKey) -> Option<SocketAddr> {
        match self.paths.get(peer)? {
            Path::Dial(addr) => Some(*addr),
            Path::Answer {
                endpoint,
                confirmed: true,
            } => Some(*endpoint),
            Path::Answer { .. } => None,
        }
    }

    fn event(&mut self, src: Option<SocketAddr>, event: Event) {
        match event {
            Event::SessionMade { peer, initiator } => {
                if !initiator
                    && !matches!(self.paths.get(&peer), Some(Path::Dial(_)))
                    && let Some(endpoint) = src
                {
                    self.paths.insert(
                        peer,
                        Path::Answer {
                            endpoint,
                            confirmed: false,
                        },
                    );
                }
                let _ = self.inbound.try_send(Inbound::SessionMade { peer });
            }
            Event::SessionConfirmed { peer } => {
                if let Some(Path::Answer {
                    endpoint,
                    confirmed,
                }) = self.paths.get_mut(&peer)
                    && Some(*endpoint) == src
                {
                    *confirmed = true;
                }
            }
            Event::SessionDiscarded { peer } => {
                if self.link.session_count(&peer) == 0
                    && matches!(self.paths.get(&peer), Some(Path::Answer { .. }))
                {
                    self.paths.remove(&peer);
                }
            }
            Event::HandshakeAbandoned { peer } => {
                let _ = self.inbound.try_send(Inbound::Abandoned { peer });
            }
            Event::UnknownInitiator { .. } => {}
            Event::CounterBlockNeeded { next_start } => self.counter_due = Some(next_start),
        }
    }

    /// Seals the node message `message` to `peer` as the frame of section
    /// 3.13.
    fn seal(&mut self, now: Instant, peer: PublicKey, message: &[u8], out: &mut Vec<Output>) {
        let dst = self.cluster_id.infra_addr(&peer);
        let Ok(packet) = frame::encode(self.own_infra, dst, message) else {
            return;
        };
        let to = Outbound {
            peer,
            vni: Vni::INFRASTRUCTURE,
            queue: 0,
            budget: PATH_BUDGET,
        };
        let rng = Arc::clone(&self.rng);
        let cx = Ctx {
            now,
            rng: rng.as_ref(),
        };
        if let Err(reason) = self.link.send(cx, to, &packet, out) {
            debug!(?reason, "a node message was not sent");
        }
    }

    /// Takes a packet delivered on VNI 0 from `peer` as a node message, when
    /// the frame and the message pass section 3.13.
    fn deliver(
        &mut self,
        now: Instant,
        src: Option<SocketAddr>,
        peer: PublicKey,
        packet: &[u8],
        actions: &mut Vec<Action>,
    ) {
        let from = self.cluster_id.infra_addr(&peer);
        let Ok(payload) = frame::accept(packet, from, self.own_infra) else {
            return;
        };
        let Ok(message) = proto::decode(payload) else {
            return;
        };
        let Some(body) = &message.body else {
            return;
        };
        match Kind::of(body) {
            None => {
                if !matches!(body, Body::Forwarded(_)) {
                    self.requester.receive(&peer, message, now, actions);
                }
            }
            Some(kind) => {
                let registered = self.link.is_admitted(&peer);
                if !registered
                    && (!self.answer_unknown
                        || !kind.is_join_service()
                        || !self.rate.take(peer, message.request_id, now))
                {
                    return;
                }
                let _ = self.inbound.try_send(Inbound::Request {
                    peer,
                    registered,
                    source: src,
                    message,
                });
            }
        }
    }

    fn ended(
        &mut self,
        now: Instant,
        kind: Kind,
        peer: Option<PublicKey>,
        outcome: Outcome,
        out: &mut Vec<Output>,
    ) {
        let Some(waiting) = self.waiters.remove(&kind) else {
            return;
        };
        if waiting.target == Target::Control {
            match (&outcome, peer) {
                (Outcome::Reply(Body::NodeStatusReply(reply)), _)
                    if reply.result == proto::status::UNAVAILABLE =>
                {
                    self.control.failed();
                }
                (Outcome::Reply(_), Some(peer)) => self.control.replied(&peer),
                _ => self.control.failed(),
            }
        }
        if outcome == Outcome::TimedOut
            && let Some(peer) = peer
            && matches!(self.paths.get(&peer), Some(Path::Dial(_)))
        {
            // No reply within the limit: the peer may hold the session no
            // longer, as after a restart, so a new one is made.
            let rng = Arc::clone(&self.rng);
            let cx = Ctx {
                now,
                rng: rng.as_ref(),
            };
            if let Err(reason) = self.link.connect(cx, peer, out) {
                debug!(?reason, "no new handshake was started");
            }
        }
        for done in waiting.done {
            let _ = done.send(Ended {
                peer,
                outcome: outcome.clone(),
            });
        }
    }
}

/// Sends each datagram from the socket of its address family.
async fn send_all(sockets: &[Box<dyn UdpSocket>], tx: &[(SocketAddr, Vec<u8>)]) {
    for (dst, payload) in tx {
        let Some(socket) = sockets
            .iter()
            .find(|s| s.local_addr().is_ipv4() == dst.is_ipv4())
        else {
            debug!(%dst, "no mesh socket of the destination's address family");
            continue;
        };
        let batch = [Datagram {
            dst: *dst,
            src_ip: None,
            payload,
        }];
        let sent = socket.send(&batch).await;
        if let Some(e) = sent.error {
            debug!(%dst, reason = %e, "a datagram was not sent");
        }
    }
}

/// A running mesh: the handle its users send [`Command`]s through.
#[derive(Clone, Debug)]
pub(crate) struct MeshHandle {
    commands: mpsc::Sender<Command>,
}

impl MeshHandle {
    /// A handle over `commands`, and the receiver the mesh reads.
    #[must_use]
    pub(crate) fn channel() -> (MeshHandle, mpsc::Receiver<Command>) {
        let (commands, receiver) = mpsc::channel(CHANNEL);
        (MeshHandle { commands }, receiver)
    }

    /// Sends `command`, waiting while the channel is full. `false` when the
    /// mesh has stopped.
    pub(crate) async fn send(&self, command: Command) -> bool {
        self.commands.send(command).await.is_ok()
    }

    /// Sends a request and waits until it ends.
    pub(crate) async fn request(&self, body: Body, limit: Duration, target: Target) -> Ended {
        let stopped = Ended {
            peer: None,
            outcome: Outcome::NoPeer,
        };
        let (done, ended) = oneshot::channel();
        let command = Command::Request {
            body,
            limit,
            target,
            done,
        };
        if !self.send(command).await {
            return stopped;
        }
        ended.await.unwrap_or(stopped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_join_service_takes_one_new_request_per_second_per_session() {
        let mut rate = JoinRate::default();
        let peer = PublicKey::from_bytes([1; 32]);
        let t0 = Instant::from_nanos(1_000_000_000);
        assert!(rate.take(peer, 5, t0));
        // A resend of the request taken is taken again, a lower id never.
        assert!(rate.take(peer, 5, t0));
        assert!(!rate.take(peer, 4, t0));
        // A new one within the second is not.
        assert!(!rate.take(peer, 6, t0.saturating_add(Duration::from_millis(500))));
        assert!(rate.take(peer, 6, t0.saturating_add(RATE_WINDOW)));
    }

    #[test]
    fn the_join_service_takes_at_most_100_new_requests_per_second() {
        let mut rate = JoinRate::default();
        let t0 = Instant::from_nanos(1_000_000_000);
        for n in 0..JOIN_SERVICE_TOTAL {
            let mut key = [0u8; 32];
            key[..8].copy_from_slice(&(n as u64).to_be_bytes());
            assert!(rate.take(PublicKey::from_bytes(key), 1, t0));
        }
        let late = PublicKey::from_bytes([0xff; 32]);
        assert!(!rate.take(late, 1, t0));
        assert!(rate.take(late, 1, t0.saturating_add(RATE_WINDOW)));
    }

    #[test]
    fn the_cookie_source_is_the_address_and_port() {
        let v4: SocketAddr = "10.0.0.1:7476".parse().unwrap();
        assert_eq!(cookie_source(v4), [10, 0, 0, 1, 0x1d, 0x34]);
        let v6: SocketAddr = "[::1]:1".parse().unwrap();
        let bytes = cookie_source(v6);
        assert_eq!(bytes.len(), 18);
        assert_eq!(&bytes[15..], [1, 0, 1]);
    }
}
