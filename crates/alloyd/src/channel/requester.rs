//! The requester side of the node channel (ARFC 0002 section 3.13),
//! sans-I/O: request ids, resends, one request outstanding, one request of
//! each kind held, and the choice of control node.

use std::time::Duration;

use alloy_crypto::al::PublicKey;
use alloy_rt::Instant;

use super::proto::{self, Body, Kind, NodeMessage};

/// How often an outstanding request is sent again, byte for byte.
pub const RESEND_INTERVAL: Duration = Duration::from_secs(1);

/// The longest time limit of any step, so no request is sent more than this
/// long after it was first sent.
pub const MAX_LIMIT: Duration = Duration::from_secs(10);

/// Where request ids come from: the node's handshake counter (ARFC 0003
/// section 3.2), shared with its AL initiations.
pub trait RequestIds {
    /// The next counter value that no initiation and no request has used,
    /// now used, or `None` when the reserved block is used up and the next
    /// one is not yet durable.
    fn next_counter(&mut self) -> Option<u64>;
}

/// What a request ended with.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The reply with its `request_id`, from the peer it was sent to.
    Reply(Body),
    /// No reply arrived within its time limit, counted from its first send.
    TimedOut,
    /// No peer could be chosen for it, so it was never sent.
    NoPeer,
}

/// Something [`Requester`] asks the caller to do or to know.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Send this encoded node message to `peer`.
    Send {
        /// The peer, by its mesh key.
        peer: PublicKey,
        /// The encoded `NodeMessage`.
        message: Vec<u8>,
    },
    /// A request ended. Whatever asked for a request of its kind while it
    /// was held takes this outcome.
    Ended {
        /// Its kind.
        kind: Kind,
        /// The peer it went to, `None` for [`Outcome::NoPeer`].
        peer: Option<PublicKey>,
        /// How it ended.
        outcome: Outcome,
    },
}

/// What [`Requester::submit`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submitted {
    /// The request waits its turn.
    Queued,
    /// A request of its kind is held, outstanding or waiting, and stands for
    /// it.
    Held,
    /// The body is a reply or a `forwarded`, not a request.
    NotARequest,
}

#[derive(Debug)]
struct Waiting {
    kind: Kind,
    body: Body,
    limit: Duration,
    order: u64,
}

#[derive(Debug)]
struct Outstanding {
    kind: Kind,
    request_id: u64,
    peer: PublicKey,
    bytes: Vec<u8>,
    first_sent: Instant,
    last_sent: Instant,
    limit: Duration,
}

/// The requests one node sends. At most one is outstanding, to any peer;
/// at most one of each kind is held, so at most 6 wait. When the
/// outstanding one ends, a waiting heartbeat goes first, then the others in
/// the order they fell due.
#[derive(Debug, Default)]
pub struct Requester {
    outstanding: Option<Outstanding>,
    waiting: Vec<Waiting>,
    next_order: u64,
}

impl Requester {
    /// A requester with nothing held.
    #[must_use]
    pub fn new() -> Requester {
        Requester::default()
    }

    /// Asks for a request with `body` and the time limit `limit` of the step
    /// that sends it, at most [`MAX_LIMIT`]. It is sent by a later
    /// [`Requester::poll`].
    pub fn submit(&mut self, body: Body, limit: Duration) -> Submitted {
        let Some(kind) = Kind::of(&body) else {
            return Submitted::NotARequest;
        };
        if self.holds(kind) {
            return Submitted::Held;
        }
        self.waiting.push(Waiting {
            kind,
            body,
            limit: limit.min(MAX_LIMIT),
            order: self.next_order,
        });
        self.next_order += 1;
        Submitted::Queued
    }

    /// Whether a request of `kind` is outstanding or waiting.
    #[must_use]
    pub fn holds(&self, kind: Kind) -> bool {
        self.outstanding.as_ref().is_some_and(|o| o.kind == kind)
            || self.waiting.iter().any(|w| w.kind == kind)
    }

    /// The kind of the outstanding request, if any.
    #[must_use]
    pub fn outstanding(&self) -> Option<Kind> {
        self.outstanding.as_ref().map(|o| o.kind)
    }

    /// Ends the outstanding request when its time limit has passed, sends it
    /// again when a resend is due, and starts the next waiting request when
    /// none is outstanding. `peer_for` chooses the peer a request of a kind
    /// goes to when it starts; every resend goes to the same one.
    pub fn poll(
        &mut self,
        now: Instant,
        ids: &mut dyn RequestIds,
        peer_for: &mut dyn FnMut(Kind) -> Option<PublicKey>,
        out: &mut Vec<Action>,
    ) {
        if let Some(o) = &mut self.outstanding {
            if now >= o.first_sent.saturating_add(o.limit) {
                let ended = Action::Ended {
                    kind: o.kind,
                    peer: Some(o.peer),
                    outcome: Outcome::TimedOut,
                };
                self.outstanding = None;
                out.push(ended);
            } else {
                if now >= o.last_sent.saturating_add(RESEND_INTERVAL) {
                    o.last_sent = now;
                    out.push(Action::Send {
                        peer: o.peer,
                        message: o.bytes.clone(),
                    });
                }
                return;
            }
        }
        while let Some(index) = self.next_waiting() {
            let kind = self.waiting[index].kind;
            let Some(peer) = peer_for(kind) else {
                self.waiting.remove(index);
                out.push(Action::Ended {
                    kind,
                    peer: None,
                    outcome: Outcome::NoPeer,
                });
                continue;
            };
            let Some(request_id) = ids.next_counter().and_then(|v| v.checked_add(1)) else {
                return;
            };
            let waiting = self.waiting.remove(index);
            let bytes = proto::encode(&NodeMessage {
                request_id,
                body: Some(waiting.body),
            });
            out.push(Action::Send {
                peer,
                message: bytes.clone(),
            });
            self.outstanding = Some(Outstanding {
                kind,
                request_id,
                peer,
                bytes,
                first_sent: now,
                last_sent: now,
                limit: waiting.limit,
            });
            return;
        }
    }

    /// Takes `message`, received from `from` at `now`, when it is the reply
    /// to the outstanding request: its `request_id`, from the peer the
    /// request went to, of the kind that answers it, and before the request's
    /// time limit, counted from its first send, has passed. Anything else is
    /// dropped: a reply at or after the limit is never taken, whatever order
    /// the caller calls this and [`Requester::poll`] in (T-0111 review F2).
    pub fn receive(
        &mut self,
        from: &PublicKey,
        message: NodeMessage,
        now: Instant,
        out: &mut Vec<Action>,
    ) {
        let Some(o) = &self.outstanding else {
            return;
        };
        if now >= o.first_sent.saturating_add(o.limit) {
            return;
        }
        let Some(body) = message.body else {
            return;
        };
        if message.request_id != o.request_id || o.peer != *from || !o.kind.is_answered_by(&body) {
            return;
        }
        out.push(Action::Ended {
            kind: o.kind,
            peer: Some(o.peer),
            outcome: Outcome::Reply(body),
        });
        self.outstanding = None;
    }

    /// The earliest instant at which [`Requester::poll`] has something to
    /// do for the outstanding request, or `None` when none is outstanding.
    /// A waiting request with none outstanding is due at once.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.outstanding.as_ref().map(|o| {
            o.last_sent
                .saturating_add(RESEND_INTERVAL)
                .min(o.first_sent.saturating_add(o.limit))
        })
    }

    /// Whether a request waits while none is outstanding.
    #[must_use]
    pub fn has_ready(&self) -> bool {
        self.outstanding.is_none() && !self.waiting.is_empty()
    }

    fn next_waiting(&self) -> Option<usize> {
        self.waiting
            .iter()
            .position(|w| w.kind == Kind::NodeStatus)
            .or_else(|| {
                self.waiting
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, w)| w.order)
                    .map(|(i, _)| i)
            })
    }
}

/// The choice of control node of a node with a valid record (ARFC 0002
/// section 3.13): the one that last replied, or, until one has, the first in
/// the view's order; after a request with no reply, or with `UNAVAILABLE`,
/// the next in the view's order, wrapping around. This is all a node keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlChoice {
    nodes: Vec<PublicKey>,
    current: usize,
}

impl ControlChoice {
    /// The choice over the control nodes of a view that the node can reach,
    /// by mesh key, in the view's order.
    #[must_use]
    pub fn new(nodes: Vec<PublicKey>) -> ControlChoice {
        ControlChoice { nodes, current: 0 }
    }

    /// The control node the next request goes to, `None` when there is none.
    #[must_use]
    pub fn current(&self) -> Option<PublicKey> {
        self.nodes.get(self.current).copied()
    }

    /// `peer` replied: it is the one the next request goes to.
    pub fn replied(&mut self, peer: &PublicKey) {
        if let Some(i) = self.nodes.iter().position(|n| n == peer) {
            self.current = i;
        }
    }

    /// The last request got no reply or `UNAVAILABLE`: move to the next.
    pub fn failed(&mut self) {
        if !self.nodes.is_empty() {
            self.current = (self.current + 1) % self.nodes.len();
        }
    }

    /// A later view: keeps the current control node when it is still in it,
    /// else starts from the first.
    pub fn set_view(&mut self, nodes: Vec<PublicKey>) {
        let keep = self.current();
        self.nodes = nodes;
        self.current = 0;
        if let Some(peer) = keep {
            self.replied(&peer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::proto::{
        Done, JoinHello, JoinHelloReply, JoinState, NodeStatus, Renew, decode,
    };
    use super::*;

    struct Counter(u64);

    impl RequestIds for Counter {
        fn next_counter(&mut self) -> Option<u64> {
            let v = self.0;
            self.0 += 1;
            Some(v)
        }
    }

    struct Exhausted;

    impl RequestIds for Exhausted {
        fn next_counter(&mut self) -> Option<u64> {
            None
        }
    }

    const P: PublicKey = PublicKey::from_bytes([1; 32]);
    const Q: PublicKey = PublicKey::from_bytes([2; 32]);

    fn at(secs: u64) -> Instant {
        Instant::from_nanos(secs * 1_000_000_000)
    }

    fn status() -> Body {
        Body::NodeStatus(NodeStatus::default())
    }

    fn renew() -> Body {
        Body::Renew(Renew {
            node_id: vec![0; 16],
            serial: 1,
            signature: vec![0; 64],
        })
    }

    fn poll(r: &mut Requester, now: Instant, ids: &mut dyn RequestIds) -> Vec<Action> {
        let mut out = Vec::new();
        r.poll(now, ids, &mut |_| Some(P), &mut out);
        out
    }

    fn sent_id(action: &Action) -> u64 {
        match action {
            Action::Send { message, .. } => decode(message).unwrap().request_id,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ids_are_the_counter_plus_one_and_resends_are_identical() {
        let mut r = Requester::new();
        let mut ids = Counter(41);
        assert_eq!(
            r.submit(status(), Duration::from_secs(4)),
            Submitted::Queued
        );
        let first = poll(&mut r, at(0), &mut ids);
        assert_eq!(first.len(), 1);
        assert_eq!(sent_id(&first[0]), 42);
        assert!(poll(&mut r, Instant::from_nanos(999_999_999), &mut ids).is_empty());
        let again = poll(&mut r, at(1), &mut ids);
        assert_eq!(again, first);
        assert_eq!(r.next_deadline(), Some(at(2)));
        for secs in 2..4 {
            assert_eq!(poll(&mut r, at(secs), &mut ids), first);
        }
        assert_eq!(
            poll(&mut r, at(4), &mut ids),
            [Action::Ended {
                kind: Kind::NodeStatus,
                peer: Some(P),
                outcome: Outcome::TimedOut
            }]
        );
        assert_eq!(r.outstanding(), None);
    }

    #[test]
    fn only_the_matching_reply_ends_a_request() {
        let mut r = Requester::new();
        let mut ids = Counter(0);
        r.submit(
            Body::JoinHello(JoinHello::default()),
            Duration::from_secs(10),
        );
        poll(&mut r, at(0), &mut ids);
        let reply = |id| NodeMessage {
            request_id: id,
            body: Some(Body::JoinHelloReply(JoinHelloReply::default())),
        };
        let mut out = Vec::new();
        r.receive(&P, reply(2), at(0), &mut out);
        r.receive(&Q, reply(1), at(0), &mut out);
        r.receive(
            &P,
            NodeMessage {
                request_id: 1,
                body: Some(Body::Done(Done::default())),
            },
            at(0),
            &mut out,
        );
        assert!(out.is_empty());
        r.receive(&P, reply(1), at(0), &mut out);
        assert_eq!(
            out,
            [Action::Ended {
                kind: Kind::JoinHello,
                peer: Some(P),
                outcome: Outcome::Reply(Body::JoinHelloReply(JoinHelloReply::default()))
            }]
        );
        out.clear();
        r.receive(&P, reply(1), at(0), &mut out);
        assert!(out.is_empty(), "a second reply is dropped");
    }

    #[test]
    fn a_reply_at_or_after_the_limit_is_not_taken() {
        // T-0111 review F2: a heartbeat with a 4 s limit, polled once at 0,
        // and its reply received at 4 s before any later poll.
        let mut r = Requester::new();
        let mut ids = Counter(0);
        r.submit(status(), Duration::from_secs(4));
        poll(&mut r, at(0), &mut ids);
        let reply = NodeMessage {
            request_id: 1,
            body: Some(Body::NodeStatusReply(proto::NodeStatusReply {
                result: proto::status::OK,
                ..proto::NodeStatusReply::default()
            })),
        };
        let mut out = Vec::new();
        r.receive(&P, reply.clone(), at(4), &mut out);
        assert!(out.is_empty(), "{out:?}");
        assert_eq!(
            poll(&mut r, at(4), &mut ids),
            [Action::Ended {
                kind: Kind::NodeStatus,
                peer: Some(P),
                outcome: Outcome::TimedOut
            }]
        );
        // Just inside the limit, the same reply is taken.
        r.submit(status(), Duration::from_secs(4));
        poll(&mut r, at(5), &mut ids);
        let inside = NodeMessage {
            request_id: 2,
            ..reply
        };
        r.receive(&P, inside, Instant::from_nanos(8_999_999_999), &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn one_outstanding_one_per_kind_and_heartbeats_first() {
        let mut r = Requester::new();
        let mut ids = Counter(0);
        r.submit(renew(), Duration::from_secs(10));
        poll(&mut r, at(0), &mut ids);
        assert_eq!(r.submit(renew(), Duration::from_secs(10)), Submitted::Held);
        assert_eq!(
            r.submit(Body::JoinState(JoinState {}), Duration::from_secs(2)),
            Submitted::Queued
        );
        assert_eq!(
            r.submit(status(), Duration::from_secs(4)),
            Submitted::Queued
        );
        assert_eq!(r.submit(status(), Duration::from_secs(4)), Submitted::Held);
        assert_eq!(
            r.submit(Body::Done(Done::default()), Duration::from_secs(1)),
            Submitted::NotARequest
        );
        let mut out = Vec::new();
        r.receive(
            &P,
            NodeMessage {
                request_id: 1,
                body: Some(Body::JoinReply(Default::default())),
            },
            at(0),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        let next = poll(&mut r, at(1), &mut ids);
        assert_eq!(r.outstanding(), Some(Kind::NodeStatus));
        assert_eq!(sent_id(&next[0]), 2);
        poll(&mut r, at(5), &mut ids);
        poll(&mut r, at(5), &mut ids);
        assert_eq!(r.outstanding(), Some(Kind::JoinState));
    }

    #[test]
    fn a_request_without_a_peer_or_an_id_is_not_sent() {
        let mut r = Requester::new();
        r.submit(status(), Duration::from_secs(4));
        let mut out = Vec::new();
        r.poll(at(0), &mut Counter(0), &mut |_| None, &mut out);
        assert_eq!(
            out,
            [Action::Ended {
                kind: Kind::NodeStatus,
                peer: None,
                outcome: Outcome::NoPeer
            }]
        );
        r.submit(status(), Duration::from_secs(4));
        assert!(poll(&mut r, at(0), &mut Exhausted).is_empty());
        assert!(r.has_ready());
        assert_eq!(poll(&mut r, at(1), &mut Counter(7)).len(), 1);
    }

    #[test]
    fn time_limits_are_capped() {
        let mut r = Requester::new();
        r.submit(status(), Duration::from_secs(60));
        poll(&mut r, at(0), &mut Counter(0));
        for secs in 1..10 {
            assert!(matches!(
                poll(&mut r, at(secs), &mut Counter(0))[..],
                [Action::Send { .. }]
            ));
        }
        assert!(matches!(
            poll(&mut r, at(10), &mut Counter(0))[..],
            [Action::Ended { .. }]
        ));
    }

    #[test]
    fn control_choice_follows_replies_and_failures() {
        let r = PublicKey::from_bytes([3; 32]);
        let mut choice = ControlChoice::new(vec![P, Q, r]);
        assert_eq!(choice.current(), Some(P));
        choice.failed();
        assert_eq!(choice.current(), Some(Q));
        choice.replied(&r);
        assert_eq!(choice.current(), Some(r));
        choice.failed();
        assert_eq!(choice.current(), Some(P));
        choice.set_view(vec![Q, P]);
        assert_eq!(choice.current(), Some(P));
        choice.set_view(vec![Q]);
        assert_eq!(choice.current(), Some(Q));
        let mut none = ControlChoice::new(Vec::new());
        none.failed();
        assert_eq!(none.current(), None);
    }
}
