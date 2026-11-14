//! The node messages of ARFC 0002 section 3.13 and the status messages of
//! ARFC 0006 section 4.3: package `alloy.node.v1alpha1`.
//!
//! Field numbers and types are those of the ARFCs. An enum field is declared
//! `int32`, which has the same encoding. The messages that ARFC 0002 shares
//! with the stored data of section 3.14 (`SignedNodeRecord`,
//! `BootstrapView`, `Facts`) are those of `alloy-id`, so both read the same
//! bytes the same way. [`decode`] is the one place a node message is read
//! from the wire: it decodes and checks every bound the ARFCs state.

use std::fmt;

use alloy_id::proto::{BootstrapView, Facts, SignedNodeRecord};
use prost::Message;

/// The largest node message that holds a `forwarded`, in bytes.
pub const MAX_FORWARDED_MESSAGE: usize = 16_512;

/// The largest node message that holds no `forwarded`, in bytes.
pub const MAX_MESSAGE: usize = 16_384;

/// The longest `release` of a `NodeStatus` that keeps ARFC 0006 section
/// 4.3, in bytes. The channel passes a longer one on, for the leader to
/// answer `INVALID` (clause 3.14).
pub const MAX_RELEASE: usize = 64;

/// The longest node name and zone, in bytes.
pub const MAX_NAME: usize = 63;

/// The most advertise addresses a request names.
pub const MAX_ADVERTISE: usize = 16;

/// The most addresses a `NodeStatus` holds.
pub const MAX_STATUS_ADDRESSES: usize = 64;

/// The most records a bootstrap view holds.
pub const MAX_VIEW_NODES: usize = 16;

/// `NodeMessage`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeMessage {
    /// 1 or more for a request and its reply, 0 for a message that holds a
    /// `forwarded`.
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    /// What the message is.
    #[prost(oneof = "Body", tags = "2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 16, 17")]
    pub body: Option<Body>,
}

/// The `body` of a `NodeMessage`.
#[derive(Clone, PartialEq, prost::Oneof)]
pub enum Body {
    /// Node to anchor.
    #[prost(message, tag = "2")]
    JoinHello(JoinHello),
    /// Answers `join_hello`.
    #[prost(message, tag = "3")]
    JoinHelloReply(JoinHelloReply),
    /// Node to anchor.
    #[prost(message, tag = "4")]
    JoinRequest(JoinRequest),
    /// Answers `join_request`, `join_state`, and `renew`.
    #[prost(message, tag = "5")]
    JoinReply(JoinReply),
    /// Node to anchor.
    #[prost(message, tag = "6")]
    JoinState(JoinState),
    /// Node to anchor, or to a control node while its record is valid.
    #[prost(message, tag = "7")]
    Renew(Renew),
    /// Node to anchor.
    #[prost(message, tag = "8")]
    Withdraw(Withdraw),
    /// Node to control node.
    #[prost(message, tag = "9")]
    Leave(Leave),
    /// Answers `withdraw` and `leave`.
    #[prost(message, tag = "10")]
    Done(Done),
    /// Anchor without `control` to control node, and the reply back.
    #[prost(message, boxed, tag = "11")]
    Forwarded(Box<Forwarded>),
    /// Node to control node, never forwarded (ARFC 0006 section 4.3).
    #[prost(message, tag = "16")]
    NodeStatus(NodeStatus),
    /// Answers `node_status`.
    #[prost(message, tag = "17")]
    NodeStatusReply(NodeStatusReply),
}

/// `JoinHello`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct JoinHello {
    /// `MAJOR.MINOR.PATCH` with any pre-release suffix.
    #[prost(string, tag = "1")]
    pub release: String,
}

/// `JoinHelloReply`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct JoinHelloReply {
    /// The refusal or the cluster's proof.
    #[prost(oneof = "HelloResult", tags = "1, 2")]
    pub result: Option<HelloResult>,
}

/// The `result` of a `JoinHelloReply`.
#[derive(Clone, PartialEq, prost::Oneof)]
pub enum HelloResult {
    /// The joiner's release is outside the accepted window.
    #[prost(message, tag = "1")]
    Version(VersionRefusal),
    /// The cluster key and the anchor's record.
    #[prost(message, tag = "2")]
    Cluster(ClusterProof),
}

/// `VersionRefusal`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct VersionRefusal {
    /// The accepted `MAJOR.MINOR` releases.
    #[prost(string, repeated, tag = "1")]
    pub accepted_minors: Vec<String>,
    /// The highest release any node runs.
    #[prost(string, tag = "2")]
    pub highest_release: String,
}

/// `ClusterProof`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ClusterProof {
    /// The current cluster public key, 32 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub cluster_pub: Vec<u8>,
    /// The anchor's own record.
    #[prost(message, optional, tag = "2")]
    pub anchor_record: Option<SignedNodeRecord>,
    /// Empty outside a rotation overlap, else 32 bytes.
    #[prost(bytes = "vec", tag = "3")]
    pub previous_cluster_pub: Vec<u8>,
    /// Empty outside a rotation overlap, else 64 bytes.
    #[prost(bytes = "vec", tag = "4")]
    pub cross_signature: Vec<u8>,
}

/// `JoinRequest`: the encoded body and the two signatures over the join
/// message (ARFC 0002 section 3.3 step 6). It carries no secret.
#[derive(Clone, PartialEq, prost::Message)]
pub struct JoinRequest {
    /// `JoinRequestBody`, encoded, kept as sent.
    #[prost(bytes = "vec", tag = "1")]
    pub body: Vec<u8>,
    /// 64 bytes, by the token key.
    #[prost(bytes = "vec", tag = "2")]
    pub token_signature: Vec<u8>,
    /// 64 bytes, by the node identity key.
    #[prost(bytes = "vec", tag = "3")]
    pub identity_signature: Vec<u8>,
}

/// `JoinRequestBody`. Numbers 2 and 4 are reserved: no node message carries
/// a token's secret, and the mesh key is the session's.
#[derive(Clone, PartialEq, prost::Message)]
pub struct JoinRequestBody {
    /// 8 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub token_id: Vec<u8>,
    /// 32 bytes.
    #[prost(bytes = "vec", tag = "3")]
    pub identity_pub: Vec<u8>,
    /// At most 63 bytes.
    #[prost(string, tag = "5")]
    pub name: String,
    /// Whether `name` is the default the joiner derived.
    #[prost(bool, tag = "6")]
    pub name_is_default: bool,
    /// `Role` values, empty when not given.
    #[prost(int32, repeated, tag = "7")]
    pub roles: Vec<i32>,
    /// At most 63 bytes, empty when not given.
    #[prost(string, tag = "8")]
    pub zone: String,
    /// 4 or 16 bytes each, 0 to 16.
    #[prost(bytes = "vec", repeated, tag = "9")]
    pub advertise: Vec<Vec<u8>>,
    /// The joiner's mesh port.
    #[prost(uint32, tag = "10")]
    pub mesh_port: u32,
    /// The joiner's API port.
    #[prost(uint32, tag = "11")]
    pub api_port: u32,
    /// The machine's facts.
    #[prost(message, optional, tag = "12")]
    pub facts: Option<Facts>,
}

/// `JoinState`: asks for the state of a held request.
#[derive(Clone, PartialEq, prost::Message)]
pub struct JoinState {}

/// `Renew` (ARFC 0002 section 3.5).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Renew {
    /// 16 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub node_id: Vec<u8>,
    /// The serial of the record being renewed.
    #[prost(uint64, tag = "2")]
    pub serial: u64,
    /// 64 bytes.
    #[prost(bytes = "vec", tag = "3")]
    pub signature: Vec<u8>,
}

/// `JoinReply`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct JoinReply {
    /// The record, `pending`, or a refusal.
    #[prost(oneof = "JoinResult", tags = "1, 2, 3")]
    pub result: Option<JoinResult>,
}

/// The `result` of a `JoinReply`.
#[derive(Clone, PartialEq, prost::Oneof)]
pub enum JoinResult {
    /// The node's record and the bootstrap view.
    #[prost(message, tag = "1")]
    Admitted(Admitted),
    /// The request waits for approval.
    #[prost(message, tag = "2")]
    Pending(Pending),
    /// The request is refused.
    #[prost(message, tag = "3")]
    Refusal(Refusal),
}

/// `Admitted`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Admitted {
    /// The node's record.
    #[prost(message, optional, tag = "1")]
    pub record: Option<SignedNodeRecord>,
    /// The bootstrap view.
    #[prost(message, optional, tag = "2")]
    pub view: Option<BootstrapView>,
}

/// `Pending`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Pending {}

/// `RefusalReason` values.
pub mod refusal {
    /// `REFUSAL_REASON_TOKEN_REFUSED`.
    pub const TOKEN_REFUSED: i32 = 1;
    /// `REFUSAL_REASON_KEY_IN_USE`.
    pub const KEY_IN_USE: i32 = 2;
    /// `REFUSAL_REASON_TOKEN_REVOKED`.
    pub const TOKEN_REVOKED: i32 = 3;
    /// `REFUSAL_REASON_TOKEN_EXPIRED`.
    pub const TOKEN_EXPIRED: i32 = 4;
    /// `REFUSAL_REASON_TOKEN_USED_UP`.
    pub const TOKEN_USED_UP: i32 = 5;
    /// `REFUSAL_REASON_PRESET_CONFLICT`.
    pub const PRESET_CONFLICT: i32 = 6;
    /// `REFUSAL_REASON_COMPUTE_UNSUPPORTED`.
    pub const COMPUTE_UNSUPPORTED: i32 = 7;
    /// `REFUSAL_REASON_NAME_TAKEN`.
    pub const NAME_TAKEN: i32 = 8;
    /// `REFUSAL_REASON_BUSY`.
    pub const BUSY: i32 = 9;
    /// `REFUSAL_REASON_REJECTED`.
    pub const REJECTED: i32 = 10;
    /// `REFUSAL_REASON_NODE_UNKNOWN`.
    pub const NODE_UNKNOWN: i32 = 11;
}

/// `Refusal`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Refusal {
    /// A `RefusalReason`.
    #[prost(int32, tag = "1")]
    pub reason: i32,
    /// Seconds since 1970-01-01T00:00:00Z, only for `TOKEN_EXPIRED`.
    #[prost(int64, tag = "2")]
    pub expire_time: i64,
}

/// `Withdraw` (ARFC 0002 section 3.9).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Withdraw {
    /// 64 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub signature: Vec<u8>,
}

/// `Leave` (ARFC 0002 section 3.9).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Leave {
    /// 16 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub node_id: Vec<u8>,
    /// 64 bytes.
    #[prost(bytes = "vec", tag = "2")]
    pub signature: Vec<u8>,
}

/// `DoneCode` values.
pub mod done {
    /// `DONE_CODE_OK`.
    pub const OK: i32 = 1;
    /// `DONE_CODE_CONFLICT`.
    pub const CONFLICT: i32 = 2;
}

/// `Done`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Done {
    /// A `DoneCode`.
    #[prost(int32, tag = "1")]
    pub code: i32,
    /// Why, for a person.
    #[prost(string, tag = "2")]
    pub reason: String,
}

/// `Forwarded`. Number 5 is reserved: origins are mesh keys, not sessions.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Forwarded {
    /// 32 bytes, the key the requester's session authenticated.
    #[prost(bytes = "vec", tag = "1")]
    pub mesh_pub: Vec<u8>,
    /// 4 or 16 bytes, empty in a reply.
    #[prost(bytes = "vec", tag = "2")]
    pub source_address: Vec<u8>,
    /// 0 in a reply.
    #[prost(uint32, tag = "3")]
    pub source_port: u32,
    /// A join service request or its reply, `request_id` 1 or more.
    #[prost(message, optional, tag = "4")]
    pub message: Option<NodeMessage>,
}

/// `NodeStatus` (ARFC 0006 section 4.3).
#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeStatus {
    /// 4 or 16 bytes each, 0 to 64, in the order of ARFC 0006 clause 3.13.
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub addresses: Vec<Vec<u8>>,
    /// Empty, or as `JoinHello.release`, at most 64 bytes.
    #[prost(string, tag = "2")]
    pub release: String,
}

/// `StatusResult` values.
pub mod status {
    /// `STATUS_RESULT_OK`.
    pub const OK: i32 = 1;
    /// `STATUS_RESULT_UNAVAILABLE`.
    pub const UNAVAILABLE: i32 = 2;
    /// `STATUS_RESULT_INVALID`.
    pub const INVALID: i32 = 3;
    /// `STATUS_RESULT_REFUSED`.
    pub const REFUSED: i32 = 4;
    /// `STATUS_RESULT_RELEASE_REFUSED`.
    pub const RELEASE_REFUSED: i32 = 5;
}

/// `NodeStatusReply`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeStatusReply {
    /// A `StatusResult`.
    #[prost(int32, tag = "1")]
    pub result: i32,
    /// Set only with `RELEASE_REFUSED`.
    #[prost(string, tag = "2")]
    pub finalized_release: String,
}

/// Why bytes are not a node message this section accepts. A receiver drops
/// such bytes silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MessageError {
    /// Longer than [`MAX_MESSAGE`], or [`MAX_FORWARDED_MESSAGE`] for one
    /// that holds a `forwarded`.
    #[error("the message is longer than its bound")]
    TooLong,
    /// Not a Protocol Buffers encoding of `NodeMessage`.
    #[error("the message does not decode")]
    Decode,
    /// No `body`, or a field outside the bounds the ARFCs state.
    #[error("a field is missing or outside its bounds")]
    Bounds,
    /// A `request_id` of 0 outside a `forwarded`, a `forwarded` with a
    /// `request_id` other than 0, or a `forwarded` that wraps a `forwarded`
    /// or nothing.
    #[error("the request id or the forwarding is not allowed")]
    Forwarding,
}

/// Decodes and checks one node message.
///
/// # Errors
///
/// [`MessageError`] for bytes over the bound, bytes that do not decode, and
/// a message that breaks a bound of ARFC 0002 section 3.13. A `NodeStatus`
/// that breaks ARFC 0006 section 4.3 decodes, for the leader to answer.
pub fn decode(bytes: &[u8]) -> Result<NodeMessage, MessageError> {
    if bytes.len() > MAX_FORWARDED_MESSAGE {
        return Err(MessageError::TooLong);
    }
    let message = NodeMessage::decode(bytes).map_err(|_| MessageError::Decode)?;
    let forwarded = matches!(message.body, Some(Body::Forwarded(_)));
    if !forwarded && bytes.len() > MAX_MESSAGE {
        return Err(MessageError::TooLong);
    }
    check(&message, true)?;
    Ok(message)
}

/// Encodes `message`. Encoding a message that [`decode`] accepts gives
/// bytes it accepts again.
#[must_use]
pub fn encode(message: &NodeMessage) -> Vec<u8> {
    message.encode_to_vec()
}

fn check(message: &NodeMessage, outer: bool) -> Result<(), MessageError> {
    let body = message.body.as_ref().ok_or(MessageError::Bounds)?;
    match body {
        Body::Forwarded(forwarded) => {
            if !outer || message.request_id != 0 {
                return Err(MessageError::Forwarding);
            }
            return check_forwarded(forwarded);
        }
        _ if message.request_id == 0 => return Err(MessageError::Forwarding),
        _ => {}
    }
    // Strings whose length ARFC 0002 section 3.13 does not bound, the
    // releases among them, are bounded only by the message size, and a
    // `NodeStatus` that breaks ARFC 0006 section 4.3 is passed on, so that
    // the leader can answer it `INVALID` (clause 3.14; T-0111 review F1).
    let ok = match body {
        Body::JoinHello(_) | Body::NodeStatus(_) => true,
        Body::JoinHelloReply(reply) => match &reply.result {
            Some(HelloResult::Version(_)) => true,
            Some(HelloResult::Cluster(proof)) => check_proof(proof),
            None => false,
        },
        Body::JoinRequest(request) => {
            request.token_signature.len() == 64
                && request.identity_signature.len() == 64
                && JoinRequestBody::decode(request.body.as_slice()).is_ok_and(|b| check_body(&b))
        }
        Body::JoinReply(reply) => match &reply.result {
            Some(JoinResult::Admitted(admitted)) => {
                admitted.record.as_ref().is_some_and(check_signed)
                    && admitted.view.as_ref().is_some_and(check_view)
            }
            Some(JoinResult::Pending(_)) => true,
            Some(JoinResult::Refusal(refusal)) => {
                (refusal::TOKEN_REFUSED..=refusal::NODE_UNKNOWN).contains(&refusal.reason)
                    && (refusal.reason == refusal::TOKEN_EXPIRED || refusal.expire_time == 0)
            }
            None => false,
        },
        Body::JoinState(_) => true,
        Body::Renew(renew) => renew.node_id.len() == 16 && renew.signature.len() == 64,
        Body::Withdraw(withdraw) => withdraw.signature.len() == 64,
        Body::Leave(leave) => leave.node_id.len() == 16 && leave.signature.len() == 64,
        Body::Done(done) => (done::OK..=done::CONFLICT).contains(&done.code),
        Body::NodeStatusReply(reply) => {
            (status::OK..=status::RELEASE_REFUSED).contains(&reply.result)
                && (reply.result == status::RELEASE_REFUSED || reply.finalized_release.is_empty())
        }
        Body::Forwarded(_) => false,
    };
    if ok {
        Ok(())
    } else {
        Err(MessageError::Bounds)
    }
}

fn check_forwarded(forwarded: &Forwarded) -> Result<(), MessageError> {
    let wrapped = forwarded.message.as_ref().ok_or(MessageError::Forwarding)?;
    if forwarded.mesh_pub.len() != 32
        || !(forwarded.source_address.is_empty() || is_ip(&forwarded.source_address))
        || forwarded.source_port > u32::from(u16::MAX)
    {
        return Err(MessageError::Bounds);
    }
    check(wrapped, false)
}

fn check_body(body: &JoinRequestBody) -> bool {
    body.token_id.len() == 8
        && body.identity_pub.len() == 32
        && body.name.len() <= MAX_NAME
        && body.zone.len() <= MAX_NAME
        && body.advertise.len() <= MAX_ADVERTISE
        && body.advertise.iter().all(|a| is_ip(a))
        && body.mesh_port <= u32::from(u16::MAX)
        && body.api_port <= u32::from(u16::MAX)
        && body.facts.as_ref().is_none_or(|facts| {
            facts.os.len() <= 64
                && facts.arch.len() <= 32
                && facts.disks.len() <= 32
                && facts.disks.iter().all(|disk| disk.path.len() <= 255)
        })
}

fn check_proof(proof: &ClusterProof) -> bool {
    let overlap = matches!(
        (
            proof.previous_cluster_pub.len(),
            proof.cross_signature.len()
        ),
        (0, 0) | (32, 64)
    );
    proof.cluster_pub.len() == 32
        && overlap
        && proof.anchor_record.as_ref().is_some_and(check_signed)
}

fn check_view(view: &BootstrapView) -> bool {
    view.cluster_pub.len() == 32
        && view.nodes.len() <= MAX_VIEW_NODES
        && view.nodes.iter().all(check_signed)
}

fn check_signed(record: &SignedNodeRecord) -> bool {
    record.signature.len() == 64 && !record.record.is_empty()
}

fn is_ip(bytes: &[u8]) -> bool {
    matches!(bytes.len(), 4 | 16)
}

/// The kind of a request: its `body` field (ARFC 0002 section 3.13).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// `join_hello`.
    JoinHello,
    /// `join_request`.
    JoinRequest,
    /// `join_state`.
    JoinState,
    /// `renew`.
    Renew,
    /// `withdraw`.
    Withdraw,
    /// `leave`.
    Leave,
    /// `node_status`, a heartbeat.
    NodeStatus,
}

impl Kind {
    /// The kind of request `body` is, or `None` for a reply or a
    /// `forwarded`.
    #[must_use]
    pub fn of(body: &Body) -> Option<Kind> {
        Some(match body {
            Body::JoinHello(_) => Kind::JoinHello,
            Body::JoinRequest(_) => Kind::JoinRequest,
            Body::JoinState(_) => Kind::JoinState,
            Body::Renew(_) => Kind::Renew,
            Body::Withdraw(_) => Kind::Withdraw,
            Body::Leave(_) => Kind::Leave,
            Body::NodeStatus(_) => Kind::NodeStatus,
            Body::JoinHelloReply(_)
            | Body::JoinReply(_)
            | Body::Done(_)
            | Body::Forwarded(_)
            | Body::NodeStatusReply(_) => return None,
        })
    }

    /// Whether `body` is the kind of reply that answers a request of this
    /// kind.
    #[must_use]
    pub fn is_answered_by(self, body: &Body) -> bool {
        match self {
            Kind::JoinHello => matches!(body, Body::JoinHelloReply(_)),
            Kind::JoinRequest | Kind::JoinState | Kind::Renew => {
                matches!(body, Body::JoinReply(_))
            }
            Kind::Withdraw | Kind::Leave => matches!(body, Body::Done(_)),
            Kind::NodeStatus => matches!(body, Body::NodeStatusReply(_)),
        }
    }

    /// Whether a request of this kind is a join service request, the only
    /// kind an anchor takes on a session from an unregistered initiator.
    #[must_use]
    pub fn is_join_service(self) -> bool {
        !matches!(self, Kind::Leave | Kind::NodeStatus)
    }
}

impl fmt::Display for Kind {
    /// Writes the `body` field's name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::JoinHello => "join_hello",
            Kind::JoinRequest => "join_request",
            Kind::JoinState => "join_state",
            Kind::Renew => "renew",
            Kind::Withdraw => "withdraw",
            Kind::Leave => "leave",
            Kind::NodeStatus => "node_status",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(request_id: u64) -> NodeMessage {
        NodeMessage {
            request_id,
            body: Some(Body::NodeStatus(NodeStatus {
                addresses: vec![vec![10, 0, 0, 1], vec![0; 16]],
                release: "0.0.1".to_owned(),
            })),
        }
    }

    fn forwarded(inner: NodeMessage) -> NodeMessage {
        NodeMessage {
            request_id: 0,
            body: Some(Body::Forwarded(Box::new(Forwarded {
                mesh_pub: vec![7; 32],
                source_address: vec![192, 0, 2, 1],
                source_port: 7476,
                message: Some(inner),
            }))),
        }
    }

    fn hello(request_id: u64) -> NodeMessage {
        NodeMessage {
            request_id,
            body: Some(Body::JoinHello(JoinHello {
                release: "0.1.0-preview.1".to_owned(),
            })),
        }
    }

    #[test]
    fn round_trips() {
        for message in [status(1), hello(u64::MAX), forwarded(hello(3))] {
            assert_eq!(decode(&encode(&message)), Ok(message));
        }
    }

    #[test]
    fn status_tags_are_16_and_17() {
        // Field 16, wire type 2: the varint 0x82 0x01.
        assert_eq!(&encode(&status(1))[2..4], [0x82, 0x01]);
        let reply = NodeMessage {
            request_id: 1,
            body: Some(Body::NodeStatusReply(NodeStatusReply {
                result: status::OK,
                finalized_release: String::new(),
            })),
        };
        assert_eq!(&encode(&reply)[2..4], [0x8A, 0x01]);
    }

    #[test]
    fn request_ids_and_forwarding_are_checked() {
        assert_eq!(decode(&encode(&status(0))), Err(MessageError::Forwarding));
        let mut outer = forwarded(hello(1));
        outer.request_id = 5;
        assert_eq!(decode(&encode(&outer)), Err(MessageError::Forwarding));
        let nested = forwarded(forwarded(hello(1)));
        assert_eq!(decode(&encode(&nested)), Err(MessageError::Forwarding));
        assert_eq!(
            decode(&encode(&forwarded(hello(0)))),
            Err(MessageError::Forwarding)
        );
        let empty = NodeMessage {
            request_id: 1,
            body: None,
        };
        assert_eq!(decode(&encode(&empty)), Err(MessageError::Bounds));
    }

    #[test]
    fn bounds_are_checked() {
        // A status that breaks ARFC 0006 section 4.3 reaches the leader,
        // which answers it `INVALID`, and a release no bound of ARFC 0002
        // section 3.13 limits is bounded only by the message size (T-0111
        // review F1).
        let mut long = status(1);
        if let Some(Body::NodeStatus(s)) = &mut long.body {
            s.release = "1".repeat(65);
        }
        assert_eq!(decode(&encode(&long)), Ok(long));
        let mut many = status(1);
        if let Some(Body::NodeStatus(s)) = &mut many.body {
            s.addresses = vec![vec![1, 2, 3, 4]; 65];
        }
        assert_eq!(decode(&encode(&many)), Ok(many));
        let mut odd = status(1);
        if let Some(Body::NodeStatus(s)) = &mut odd.body {
            s.addresses = vec![vec![1, 2, 3]];
        }
        assert_eq!(decode(&encode(&odd)), Ok(odd));
        let long_hello = NodeMessage {
            request_id: 1,
            body: Some(Body::JoinHello(JoinHello {
                release: "1".repeat(1_000),
            })),
        };
        assert_eq!(decode(&encode(&long_hello)), Ok(long_hello));
        let bad_request = NodeMessage {
            request_id: 1,
            body: Some(Body::JoinRequest(JoinRequest {
                body: vec![0xFF],
                token_signature: vec![0; 64],
                identity_signature: vec![0; 64],
            })),
        };
        assert_eq!(decode(&encode(&bad_request)), Err(MessageError::Bounds));
    }

    #[test]
    fn length_bounds_depend_on_forwarding() {
        let big = |len| NodeMessage {
            request_id: 1,
            body: Some(Body::Done(Done {
                code: done::OK,
                reason: "r".repeat(len),
            })),
        };
        // 2 bytes of `request_id`, 3 of the body's key and length, and 5 of
        // `code` and the key and length of `reason`.
        let fits = encode(&big(MAX_MESSAGE - 10));
        assert_eq!(fits.len(), MAX_MESSAGE);
        assert!(decode(&fits).is_ok());
        assert_eq!(
            decode(&encode(&big(MAX_MESSAGE - 9))),
            Err(MessageError::TooLong)
        );
        assert_eq!(
            decode(&vec![0; MAX_FORWARDED_MESSAGE + 1]),
            Err(MessageError::TooLong)
        );
    }

    #[test]
    fn decode_is_total() {
        let mut bytes = Vec::new();
        for byte in [0x08, 0x01, 0x12, 0x80, 0xFF, 0x5A, 0x00, 0x82, 0x01] {
            bytes.push(byte);
            let _ = decode(&bytes);
        }
    }

    #[test]
    fn kinds_match_their_replies() {
        let hello = hello(1).body.unwrap();
        assert_eq!(Kind::of(&hello), Some(Kind::JoinHello));
        assert!(
            Kind::JoinHello.is_answered_by(&Body::JoinHelloReply(JoinHelloReply { result: None }))
        );
        assert!(!Kind::Renew.is_answered_by(&Body::Done(Done::default())));
        assert!(!Kind::NodeStatus.is_join_service());
        assert!(!Kind::Leave.is_join_service());
        assert!(Kind::Withdraw.is_join_service());
        assert_eq!(Kind::NodeStatus.to_string(), "node_status");
    }
}
