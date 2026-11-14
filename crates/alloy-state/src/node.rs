//! The kind `Node` (ARFC 0006 section 2.6), the status messages of section
//! 4.3 as the leader checks them, the soft state they leave (section 2.3),
//! and node state (clause 3.15).
//!
//! A `Node` is a resource of kind [`NODE_KIND`] with cluster scope. Its
//! `spec` and its committed `status` are stored as the messages `NodeSpec`
//! and `NodeStatus` of `alloy.cluster.v1alpha1` (ARFC 0008), with the fields
//! that are soft state or computed when read left unset. Soft state lives in
//! [`Soft`], in memory only, and never goes through the group.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use alloy_proto::cluster::v1alpha1 as pb;
use alloy_rt::{Instant, WallTime};
use prost::Message as _;

use crate::resource::{Resource, ResourceError, Scope, Uid};
use crate::store::NodeId;

/// The kind of node resources.
pub const NODE_KIND: &str = "Node";

/// The longest node name and zone, in bytes (ARFC 0002 section 3.3).
pub const MAX_NODE_NAME: usize = 63;

/// The most advertise addresses a node has (ARFC 0002 section 3.13).
pub const MAX_ADVERTISE: usize = 16;

/// The most addresses one status message holds (section 4.3).
pub const MAX_STATUS_ADDRESSES: usize = 64;

/// The longest release, in bytes (section 4.3).
pub const MAX_RELEASE: usize = 64;

/// How long the control plane counts a node as heard from (clause 3.15): a
/// node last heard from less than this before a read is `active`. It is the
/// lease of a heartbeat's soft state (section 2.3).
pub const HEARD_LEASE: Duration = Duration::from_secs(30);

/// A role a node holds (ARFC 0002 section 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// Accepts AL sessions and relays.
    Anchor,
    /// Runs the control plane.
    Control,
    /// Runs instances.
    Compute,
    /// Holds disks.
    Storage,
}

impl Role {
    fn to_proto(self) -> pb::Role {
        match self {
            Role::Anchor => pb::Role::Anchor,
            Role::Control => pb::Role::Control,
            Role::Compute => pb::Role::Compute,
            Role::Storage => pb::Role::Storage,
        }
    }

    fn from_proto(value: i32) -> Option<Role> {
        match pb::Role::try_from(value).ok()? {
            pb::Role::Anchor => Some(Role::Anchor),
            pb::Role::Control => Some(Role::Control),
            pb::Role::Compute => Some(Role::Compute),
            pb::Role::Storage => Some(Role::Storage),
            pb::Role::Unspecified => None,
        }
    }
}

/// A release as `JoinHello.release` spells it (ARFC 0002 section 3.13):
/// `MAJOR.MINOR.PATCH` with an optional pre-release suffix, by the rules of
/// Semantic Versioning 2.0.0 without build metadata, at most
/// [`MAX_RELEASE`] bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Release(String);

fn numeric_identifier(part: &str) -> bool {
    !part.is_empty()
        && part.bytes().all(|b| b.is_ascii_digit())
        && (part == "0" || !part.starts_with('0'))
}

impl Release {
    /// Parses `text`, or `None` when it is not spelled as a release.
    #[must_use]
    pub fn parse(text: &str) -> Option<Release> {
        if text.len() > MAX_RELEASE {
            return None;
        }
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut numbers = core.split('.');
        for _ in 0..3 {
            if !numbers.next().is_some_and(numeric_identifier) {
                return None;
            }
        }
        if numbers.next().is_some() {
            return None;
        }
        if let Some(pre) = pre {
            let ok = pre.split('.').all(|id| {
                !id.is_empty()
                    && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && (!id.bytes().all(|b| b.is_ascii_digit()) || numeric_identifier(id))
            });
            if !ok {
                return None;
            }
        }
        Some(Release(text.to_owned()))
    }

    /// The release as spelled.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The wanted part of a `Node`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeSpec {
    /// Its roles. Empty for a client node.
    pub roles: BTreeSet<Role>,
    /// Its zone, at most [`MAX_NODE_NAME`] bytes.
    pub zone: String,
    /// Its advertise addresses (ARFC 0002 section 3.10), in record order, at
    /// most [`MAX_ADVERTISE`].
    pub advertise: Vec<IpAddr>,
}

/// The node record a node holds now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordTerms {
    /// The record's serial.
    pub serial: u64,
    /// The end of the record's validity, seconds since the Unix epoch.
    pub not_after: i64,
}

/// The committed part of a `Node`'s status. The rest, `state`, `addresses`,
/// and `last_heartbeat_time`, is in [`NodeView`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeStatus {
    /// Its node id (ARFC 0002 section 2).
    pub node_id: NodeId,
    /// Its current record, `None` for a `pending` node.
    pub record: Option<RecordTerms>,
    /// Whether a heartbeat from it was accepted since its first record was
    /// issued (clause 3.14).
    pub reported: bool,
    /// The leader's wall time when it last became `unreachable`, set and
    /// cleared by the node lifecycle controller (clause 3.16).
    pub unreachable_since: Option<WallTime>,
    /// Its running release (ARFC 0017 clause 3.11), `None` while none is
    /// recorded.
    pub running_release: Option<Release>,
}

impl NodeStatus {
    /// The status of a node with id `node_id` and no record: a `pending`
    /// join request.
    #[must_use]
    pub fn pending(node_id: NodeId) -> NodeStatus {
        NodeStatus {
            node_id,
            record: None,
            reported: false,
            unreachable_since: None,
            running_release: None,
        }
    }
}

/// A `Node` as the group stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Its name: 1 to [`MAX_NODE_NAME`] bytes of `a` to `z`, `0` to `9`, and
    /// `-`.
    pub name: String,
    /// Set by the group at creation.
    pub uid: Uid,
    /// As [`Resource::generation`].
    pub generation: u64,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// As [`Resource::deleting`].
    pub deleting: bool,
    /// What is wanted.
    pub spec: NodeSpec,
    /// What is committed of what is.
    pub status: NodeStatus,
}

/// A node's state, computed when read (clause 3.15).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeState {
    /// A join request waits for approval. No record exists.
    Pending,
    /// A record exists and `reported` is false.
    Joining,
    /// Heard from less than 30 seconds before the read.
    Active,
    /// Heard from 30 seconds or more before the read, or never by this
    /// leader.
    Unreachable,
}

/// A `Node` as a read returns it: the stored node, its state, and the soft
/// state of its latest accepted heartbeat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeView {
    /// The stored node.
    pub node: Node,
    /// Its state at the read.
    pub state: NodeState,
    /// The addresses of the latest heartbeat the leader accepted, empty when
    /// there is none.
    pub addresses: Vec<IpAddr>,
    /// The leader's wall time when it accepted that heartbeat.
    pub last_heartbeat_time: Option<WallTime>,
}

/// Checks a node name.
pub(crate) fn check_name(name: &str) -> Result<(), ResourceError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NODE_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ResourceError::Invalid(
            "a node name must be 1 to 63 bytes of a-z, 0-9, and -",
        ))
    }
}

impl Node {
    /// Checks every field a client sets.
    pub(crate) fn check(&self) -> Result<(), ResourceError> {
        check_name(&self.name)?;
        if self.spec.zone.len() > MAX_NODE_NAME {
            return Err(ResourceError::Invalid("a zone must be at most 63 bytes"));
        }
        if self.spec.advertise.len() > MAX_ADVERTISE {
            return Err(ResourceError::Invalid(
                "a node has at most 16 advertise addresses",
            ));
        }
        Ok(())
    }

    /// The resource that stores this node.
    pub(crate) fn to_resource(&self) -> Resource {
        Resource {
            kind: NODE_KIND.to_owned(),
            uid: self.uid,
            name: self.name.clone(),
            scope: Scope::Cluster,
            generation: self.generation,
            spec: encode_spec(&self.spec),
            status: encode_status(&self.status),
            labels: self.labels.clone(),
            deleting: self.deleting,
        }
    }

    /// The node a stored resource holds, or `None` when it is not one.
    pub(crate) fn from_resource(r: &Resource) -> Option<Node> {
        if r.kind != NODE_KIND || r.scope != Scope::Cluster || check_name(&r.name).is_err() {
            return None;
        }
        Some(Node {
            name: r.name.clone(),
            uid: r.uid,
            generation: r.generation,
            labels: r.labels.clone(),
            deleting: r.deleting,
            spec: decode_spec(&r.spec)?,
            status: decode_status(&r.status)?,
        })
    }
}

fn timestamp(time: WallTime) -> prost_types::Timestamp {
    const NANOS: i64 = 1_000_000_000;
    prost_types::Timestamp {
        seconds: time.0.div_euclid(NANOS),
        // In 0..1e9, so within i32.
        nanos: i32::try_from(time.0.rem_euclid(NANOS)).unwrap_or(0),
    }
}

fn wall_time(ts: &prost_types::Timestamp) -> Option<WallTime> {
    if !(0..1_000_000_000).contains(&ts.nanos) {
        return None;
    }
    let nanos = ts.seconds.checked_mul(1_000_000_000)?;
    nanos.checked_add(i64::from(ts.nanos)).map(WallTime)
}

pub(crate) fn encode_spec(spec: &NodeSpec) -> Vec<u8> {
    pb::NodeSpec {
        roles: spec.roles.iter().map(|r| r.to_proto() as i32).collect(),
        zone: spec.zone.clone(),
        advertise: spec.advertise.iter().map(ToString::to_string).collect(),
    }
    .encode_to_vec()
}

pub(crate) fn decode_spec(bytes: &[u8]) -> Option<NodeSpec> {
    let wire = pb::NodeSpec::decode(bytes).ok()?;
    let mut roles = BTreeSet::new();
    for value in wire.roles {
        let role = Role::from_proto(value)?;
        // Ascending with no repeats, as written.
        if roles.last().is_some_and(|last| *last >= role) {
            return None;
        }
        roles.insert(role);
    }
    let advertise = wire
        .advertise
        .iter()
        .map(|a| a.parse().ok())
        .collect::<Option<Vec<IpAddr>>>()?;
    Some(NodeSpec {
        roles,
        zone: wire.zone,
        advertise,
    })
}

pub(crate) fn encode_status(status: &NodeStatus) -> Vec<u8> {
    pb::NodeStatus {
        node_id: status.node_id.0.to_vec(),
        record_serial: status.record.map_or(0, |r| r.serial),
        record_not_after: status.record.map(|r| prost_types::Timestamp {
            seconds: r.not_after,
            nanos: 0,
        }),
        reported: status.reported,
        unreachable_since: status.unreachable_since.map(timestamp),
        running_release: status
            .running_release
            .as_ref()
            .map_or_else(String::new, |r| r.0.clone()),
        ..pb::NodeStatus::default()
    }
    .encode_to_vec()
}

pub(crate) fn decode_status(bytes: &[u8]) -> Option<NodeStatus> {
    let wire = pb::NodeStatus::decode(bytes).ok()?;
    let record = match (wire.record_not_after, wire.record_serial) {
        (None, 0) => None,
        (None, _) => return None,
        (Some(ts), serial) => {
            if ts.nanos != 0 {
                return None;
            }
            Some(RecordTerms {
                serial,
                not_after: ts.seconds,
            })
        }
    };
    let unreachable_since = match &wire.unreachable_since {
        None => None,
        Some(ts) => Some(wall_time(ts)?),
    };
    let running_release = if wire.running_release.is_empty() {
        None
    } else {
        Some(Release::parse(&wire.running_release)?)
    };
    Some(NodeStatus {
        node_id: NodeId(wire.node_id.try_into().ok()?),
        record,
        reported: wire.reported,
        unreachable_since,
        running_release,
    })
}

/// The status message a node sends (`NodeStatus` of section 4.3), as it
/// came: addresses as bytes, `release` as a string, empty when not set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Heartbeat {
    /// 4 or 16 bytes each, at most [`MAX_STATUS_ADDRESSES`].
    pub addresses: Vec<Vec<u8>>,
    /// Empty, or a [`Release`].
    pub release: String,
}

/// A heartbeat that keeps section 4.3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Checked {
    pub(crate) addresses: Vec<IpAddr>,
    pub(crate) release: Option<Release>,
}

impl Heartbeat {
    /// The heartbeat's typed fields, or `None` when it breaks section 4.3:
    /// more than 64 addresses, an address of another length than 4 or 16,
    /// or a non-empty `release` that is not spelled as a release.
    pub(crate) fn check(&self) -> Option<Checked> {
        if self.addresses.len() > MAX_STATUS_ADDRESSES {
            return None;
        }
        let addresses = self
            .addresses
            .iter()
            .map(|bytes| match bytes.len() {
                4 => <[u8; 4]>::try_from(bytes.as_slice())
                    .ok()
                    .map(|a| IpAddr::V4(Ipv4Addr::from(a))),
                16 => <[u8; 16]>::try_from(bytes.as_slice())
                    .ok()
                    .map(|a| IpAddr::V6(Ipv6Addr::from(a))),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let release = if self.release.is_empty() {
            None
        } else {
            Some(Release::parse(&self.release)?)
        };
        Some(Checked { addresses, release })
    }
}

/// The leader's answer to a heartbeat (`NodeStatusReply` of section 4.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusReply {
    /// Accepted, and every commit it made is applied.
    Ok,
    /// Not answered: this member does not serve the group as its leader, or
    /// a read or commit failed (clause 3.12.2).
    Unavailable,
    /// The message breaks section 4.3. Nothing changed.
    Invalid,
    /// No valid node record holds the session's key. Nothing changed.
    Refused,
    /// The running release write was refused (ARFC 0017 clause 3.11).
    ReleaseRefused {
        /// The finalized release.
        finalized_release: Release,
    },
}

/// When the control plane last heard from a node (clause 3.15): the later of
/// the leader's acceptance of its latest heartbeat, `heartbeat`, and, while
/// `unreachable_since` is unset, the moment the leader began serving the
/// group, `serving_since`.
#[must_use]
pub fn last_heard(
    status: &NodeStatus,
    heartbeat: Option<Instant>,
    serving_since: Option<Instant>,
) -> Option<Instant> {
    let start = serving_since.filter(|_| status.unreachable_since.is_none());
    heartbeat.max(start)
}

/// A node's state at `now` (clause 3.15), given when the control plane
/// `last_heard` from it.
#[must_use]
pub fn node_state(status: &NodeStatus, last_heard: Option<Instant>, now: Instant) -> NodeState {
    if status.record.is_none() {
        return NodeState::Pending;
    }
    if !status.reported {
        return NodeState::Joining;
    }
    match last_heard {
        Some(at) if now.saturating_duration_since(at) < HEARD_LEASE => NodeState::Active,
        _ => NodeState::Unreachable,
    }
}

/// The latest accepted heartbeat of one node.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Heard {
    at: Instant,
    wall: WallTime,
    addresses: Vec<IpAddr>,
}

/// The soft state of the leader's tenure (section 2.3): when it began
/// serving, and the latest accepted heartbeat of each node, by `uid`, so a
/// node created again under the same name starts with none. It holds at
/// most one entry per node the group holds: entries of nodes no longer
/// held are dropped at each read of every node.
#[derive(Debug, Default)]
pub(crate) struct Soft {
    serving_since: Option<Instant>,
    heard: BTreeMap<Uid, Heard>,
}

impl Soft {
    /// Starts a tenure at `now`, holding no heartbeat times (clause 3.15).
    pub(crate) fn serve(&mut self, now: Instant) {
        self.serving_since = Some(now);
        self.heard.clear();
    }

    pub(crate) fn serving(&self) -> bool {
        self.serving_since.is_some()
    }

    /// Records a heartbeat of `uid` accepted at `at` and `wall`, replacing
    /// what was held for it (clause 3.14).
    pub(crate) fn record(&mut self, uid: Uid, at: Instant, wall: WallTime, addresses: Vec<IpAddr>) {
        self.heard.insert(
            uid,
            Heard {
                at,
                wall,
                addresses,
            },
        );
    }

    /// Drops the soft state of every node but those in `held`.
    pub(crate) fn retain(&mut self, held: &BTreeSet<Uid>) {
        self.heard.retain(|uid, _| held.contains(uid));
    }

    /// `node` as a read at `now` returns it.
    pub(crate) fn view(&self, node: Node, now: Instant) -> NodeView {
        let heard = self.heard.get(&node.uid);
        let last = last_heard(&node.status, heard.map(|h| h.at), self.serving_since);
        NodeView {
            state: node_state(&node.status, last, now),
            addresses: heard.map(|h| h.addresses.clone()).unwrap_or_default(),
            last_heartbeat_time: heard.map(|h| h.wall),
            node,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> NodeStatus {
        NodeStatus {
            node_id: NodeId([3; 16]),
            record: Some(RecordTerms {
                serial: 4,
                not_after: 1_800_086_400,
            }),
            reported: true,
            unreachable_since: Some(WallTime(-1_500_000_000)),
            running_release: Release::parse("0.1.0-preview.2"),
        }
    }

    fn secs(s: u64) -> Instant {
        Instant::from_nanos(s * 1_000_000_000)
    }

    #[test]
    fn spec_and_status_round_trip() {
        let spec = NodeSpec {
            roles: BTreeSet::from([Role::Storage, Role::Anchor]),
            zone: "a".to_owned(),
            advertise: vec!["192.0.2.1".parse().unwrap(), "2001:db8::1".parse().unwrap()],
        };
        assert_eq!(decode_spec(&encode_spec(&spec)), Some(spec));
        assert_eq!(
            decode_spec(&encode_spec(&NodeSpec::default())),
            Some(NodeSpec::default())
        );
        let mut s = status();
        assert_eq!(decode_status(&encode_status(&s)), Some(s.clone()));
        s = NodeStatus::pending(NodeId([9; 16]));
        assert_eq!(decode_status(&encode_status(&s)), Some(s));
    }

    #[test]
    fn stored_fields_out_of_range_do_not_decode() {
        let repeated = pb::NodeSpec {
            roles: vec![pb::Role::Control as i32, pb::Role::Anchor as i32],
            ..pb::NodeSpec::default()
        };
        assert_eq!(decode_spec(&repeated.encode_to_vec()), None);
        let unspecified = pb::NodeSpec {
            roles: vec![0],
            ..pb::NodeSpec::default()
        };
        assert_eq!(decode_spec(&unspecified.encode_to_vec()), None);
        let short_id = pb::NodeStatus {
            node_id: vec![1; 15],
            ..pb::NodeStatus::default()
        };
        assert_eq!(decode_status(&short_id.encode_to_vec()), None);
        let serial_without_record = pb::NodeStatus {
            node_id: vec![1; 16],
            record_serial: 2,
            ..pb::NodeStatus::default()
        };
        assert_eq!(decode_status(&serial_without_record.encode_to_vec()), None);
    }

    #[test]
    fn releases_are_spelled_as_semantic_versions() {
        for good in [
            "0.0.1",
            "1.20.3",
            "0.0.1-preview.2",
            "1.0.0-rc-1.0a",
            "10.0.0-0",
        ] {
            assert!(Release::parse(good).is_some(), "{good}");
        }
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.3-",
            "1.2.3-a..b",
            "1.2.3-01",
            "1.2.3+b",
            "v1.2.3",
            "1.2.3-a_b",
        ] {
            assert!(Release::parse(bad).is_none(), "{bad}");
        }
        let long = format!("1.2.3-{}", "a".repeat(59));
        assert_eq!(long.len(), 65);
        assert!(Release::parse(&long).is_none());
        assert!(Release::parse(&long[..64]).is_some());
    }

    #[test]
    fn heartbeats_that_break_section_4_3_are_refused() {
        let ok = Heartbeat {
            addresses: vec![vec![192, 0, 2, 1], vec![0x20; 16]],
            release: "0.1.0".to_owned(),
        };
        let checked = ok.check().unwrap();
        assert_eq!(
            checked.addresses[0],
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))
        );
        assert_eq!(checked.release, Release::parse("0.1.0"));
        let mut many = Heartbeat {
            addresses: vec![vec![1; 4]; 64],
            release: String::new(),
        };
        assert!(many.check().is_some());
        many.addresses.push(vec![1; 4]);
        assert!(many.check().is_none());
        let five = Heartbeat {
            addresses: vec![vec![1; 5]],
            release: String::new(),
        };
        assert!(five.check().is_none());
        let badly_spelled = Heartbeat {
            addresses: Vec::new(),
            release: "latest".to_owned(),
        };
        assert!(badly_spelled.check().is_none());
    }

    #[test]
    fn node_state_follows_clause_3_15() {
        let mut s = status();
        s.unreachable_since = None;
        let at = Some(secs(100));
        assert_eq!(node_state(&s, at, secs(100)), NodeState::Active);
        let edge = Instant::from_nanos(secs(130).as_nanos() - 1);
        assert_eq!(node_state(&s, at, edge), NodeState::Active);
        assert_eq!(node_state(&s, at, secs(130)), NodeState::Unreachable);
        assert_eq!(node_state(&s, None, secs(1)), NodeState::Unreachable);
        s.reported = false;
        assert_eq!(node_state(&s, at, secs(100)), NodeState::Joining);
        assert_eq!(
            node_state(&NodeStatus::pending(NodeId([1; 16])), at, secs(100)),
            NodeState::Pending
        );
    }

    #[test]
    fn a_leader_start_counts_only_while_unreachable_since_is_unset() {
        let mut s = status();
        let start = Some(secs(50));
        assert_eq!(last_heard(&s, None, start), None);
        assert_eq!(last_heard(&s, Some(secs(60)), start), Some(secs(60)));
        s.unreachable_since = None;
        assert_eq!(last_heard(&s, None, start), start);
        assert_eq!(last_heard(&s, Some(secs(40)), start), start);
        assert_eq!(last_heard(&s, Some(secs(60)), start), Some(secs(60)));
    }

    #[test]
    fn names_are_checked() {
        assert!(check_name("linux-1").is_ok());
        assert!(check_name(&"a".repeat(63)).is_ok());
        for bad in ["", "Linux", "a.b", "a_b", "a\0"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_name(&"a".repeat(64)).is_err());
    }
}
