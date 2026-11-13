//! Node records: their fields, encoding, cluster key signature, and validity
//! (ARFC 0002 sections 2, 3.5, and 3.13), and the renewal signature of
//! section 3.5.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use alloy_crypto::al;
use alloy_crypto::ed25519::{self, Signature, SigningKey, VerifyingKey};
use alloy_rt::WallTime;
use prost::Message as _;

use crate::ids::{ClusterId, NodeId, Role, Roles};
use crate::proto;

/// The bytes a cluster key signature over a record starts with.
pub const RECORD_CONTEXT: &[u8] = b"alloy/id/node-record/v1";

/// The bytes a renewal signature starts with (section 3.5).
pub const RENEW_CONTEXT: &[u8] = b"alloy-renew-v1";

/// How long a record is valid, in seconds: 24 hours (section 3.5).
pub const RECORD_LIFETIME_SECS: i64 = 86_400;

/// The most advertise addresses a record holds (section 7).
pub const MAX_ADVERTISE: usize = 16;

/// The longest name and zone, in bytes (section 3.3).
pub const MAX_NAME_LEN: usize = 63;

/// Why a record cannot be encoded, decoded, or accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// The bytes are not a `NodeRecord` or `SignedNodeRecord`.
    #[error("the record does not decode")]
    Decode,
    /// A field is out of its range; the string names it.
    #[error("the record's field `{0}` is out of range")]
    Field(&'static str),
    /// The cluster key signature does not verify.
    #[error("the record's signature does not verify")]
    Signature,
    /// The record's `not_after` has passed.
    #[error("the record expired")]
    Expired,
    /// The record names another cluster.
    #[error("the record belongs to another cluster")]
    Cluster,
}

/// A node record whose fields are in range.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NodeRecord {
    /// The cluster.
    pub cluster_id: ClusterId,
    /// The node: the first 16 bytes of `BLAKE3(identity_pub)`.
    pub node_id: NodeId,
    /// 1 to 63 bytes.
    pub name: String,
    /// The node identity key.
    pub identity_pub: VerifyingKey,
    /// The mesh key, or two during a rotation (section 3.6).
    pub mesh_pub: Vec<al::PublicKey>,
    /// Empty for a client node.
    pub roles: Roles,
    /// At most 63 bytes.
    pub zone: String,
    /// The infrastructure address (section 3.4).
    pub infra_addr: Ipv6Addr,
    /// 0 to 16 advertise addresses.
    pub advertise: Vec<IpAddr>,
    /// The UDP port of the AL socket.
    pub mesh_port: u16,
    /// The TCP port of the API and the `stream` underlay.
    pub api_port: u16,
    /// The end of validity, seconds since 1970-01-01T00:00:00Z.
    pub not_after: i64,
    /// Grows with each record issued for the node.
    pub serial: u64,
}

impl NodeRecord {
    /// Encodes the record as `NodeRecord` bytes, after checking its fields
    /// as [`NodeRecord::decode`] does.
    ///
    /// # Errors
    ///
    /// [`RecordError::Field`] for a field out of range.
    pub fn encode(&self) -> Result<Vec<u8>, RecordError> {
        self.check()?;
        let wire = proto::NodeRecord {
            cluster_id: self.cluster_id.as_bytes().to_vec(),
            node_id: self.node_id.as_bytes().to_vec(),
            name: self.name.clone(),
            identity_pub: self.identity_pub.as_bytes().to_vec(),
            mesh_pub: self
                .mesh_pub
                .iter()
                .map(|key| key.as_bytes().to_vec())
                .collect(),
            roles: self.roles.iter().map(|role| role as i32).collect(),
            zone: self.zone.clone(),
            infra_addr: self.infra_addr.octets().to_vec(),
            advertise: self.advertise.iter().map(ip_bytes).collect(),
            mesh_port: u32::from(self.mesh_port),
            api_port: u32::from(self.api_port),
            not_after: self.not_after,
            serial: self.serial,
        };
        Ok(wire.encode_to_vec())
    }

    /// Decodes `NodeRecord` bytes and checks every field: the lengths of
    /// section 3.13, `node_id` derived from `identity_pub`, an identity key
    /// that is a curve point, 1 or 2 mesh keys, roles known, ascending, and
    /// without repeats, a name of 1 to 63 bytes, a zone of at most 63, and
    /// ports that fit 16 bits.
    ///
    /// # Errors
    ///
    /// [`RecordError::Decode`] and [`RecordError::Field`].
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        let wire = proto::NodeRecord::decode(bytes).map_err(|_| RecordError::Decode)?;
        let identity_pub = VerifyingKey::from_bytes(&array(&wire.identity_pub, "identity_pub")?)
            .map_err(|_| RecordError::Field("identity_pub"))?;
        let mut roles = Vec::with_capacity(wire.roles.len());
        for value in &wire.roles {
            let role = Role::from_proto(*value).ok_or(RecordError::Field("roles"))?;
            if roles.last().is_some_and(|last| *last >= role) {
                return Err(RecordError::Field("roles"));
            }
            roles.push(role);
        }
        let record = Self {
            cluster_id: ClusterId::from_bytes(array(&wire.cluster_id, "cluster_id")?),
            node_id: NodeId::from_bytes(array(&wire.node_id, "node_id")?),
            name: wire.name,
            identity_pub,
            mesh_pub: wire
                .mesh_pub
                .iter()
                .map(|key| array(key, "mesh_pub").map(al::PublicKey::from_bytes))
                .collect::<Result<_, _>>()?,
            roles: Roles::of(&roles),
            zone: wire.zone,
            infra_addr: Ipv6Addr::from(array::<16>(&wire.infra_addr, "infra_addr")?),
            advertise: wire
                .advertise
                .iter()
                .map(|bytes| parse_ip(bytes).ok_or(RecordError::Field("advertise")))
                .collect::<Result<_, _>>()?,
            mesh_port: u16::try_from(wire.mesh_port)
                .map_err(|_| RecordError::Field("mesh_port"))?,
            api_port: u16::try_from(wire.api_port).map_err(|_| RecordError::Field("api_port"))?,
            not_after: wire.not_after,
            serial: wire.serial,
        };
        record.check()?;
        Ok(record)
    }

    fn check(&self) -> Result<(), RecordError> {
        if self.node_id != NodeId::of(&self.identity_pub) {
            return Err(RecordError::Field("node_id"));
        }
        if !(1..=2).contains(&self.mesh_pub.len()) {
            return Err(RecordError::Field("mesh_pub"));
        }
        if self.name.is_empty() || self.name.len() > MAX_NAME_LEN {
            return Err(RecordError::Field("name"));
        }
        if self.zone.len() > MAX_NAME_LEN {
            return Err(RecordError::Field("zone"));
        }
        if self.advertise.len() > MAX_ADVERTISE {
            return Err(RecordError::Field("advertise"));
        }
        Ok(())
    }

    /// Whether the record is valid at `now`: before its `not_after`.
    #[must_use]
    pub fn is_valid_at(&self, now: WallTime) -> bool {
        seconds(now) < self.not_after
    }

    /// When the node renews: at half life, 12 hours before `not_after`
    /// (section 3.5).
    #[must_use]
    pub fn renew_at(&self) -> i64 {
        self.not_after.saturating_sub(RECORD_LIFETIME_SECS / 2)
    }
}

/// A node record's bytes as signed, and the cluster key's signature over
/// them. The bytes are kept and forwarded as received, never re-encoded
/// (section 2).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SignedNodeRecord {
    record: Vec<u8>,
    signature: Signature,
}

impl SignedNodeRecord {
    /// Encodes `record` and signs it with the cluster key: Ed25519 over
    /// `alloy/id/node-record/v1` followed by the encoded bytes.
    ///
    /// # Errors
    ///
    /// As [`NodeRecord::encode`].
    pub fn sign(record: &NodeRecord, cluster_key: &SigningKey) -> Result<Self, RecordError> {
        let record = record.encode()?;
        let signature = cluster_key.sign(&signed_bytes(&record));
        Ok(Self { record, signature })
    }

    /// The signed record of a `SignedNodeRecord` message, unverified.
    ///
    /// # Errors
    ///
    /// [`RecordError::Field`] when the signature is not 64 bytes.
    pub fn from_proto(wire: &proto::SignedNodeRecord) -> Result<Self, RecordError> {
        Ok(Self {
            record: wire.record.clone(),
            signature: Signature::from_bytes(array(&wire.signature, "signature")?),
        })
    }

    /// The `SignedNodeRecord` message.
    #[must_use]
    pub fn to_proto(&self) -> proto::SignedNodeRecord {
        proto::SignedNodeRecord {
            record: self.record.clone(),
            signature: self.signature.to_bytes().to_vec(),
        }
    }

    /// The record's bytes, as signed.
    #[must_use]
    pub fn record_bytes(&self) -> &[u8] {
        &self.record
    }

    /// Checks the signature against `cluster_pub`, then decodes the record.
    ///
    /// # Errors
    ///
    /// [`RecordError::Signature`] first, then as [`NodeRecord::decode`].
    pub fn verify(&self, cluster_pub: &VerifyingKey) -> Result<NodeRecord, RecordError> {
        cluster_pub
            .verify(&signed_bytes(&self.record), &self.signature)
            .map_err(|_| RecordError::Signature)?;
        NodeRecord::decode(&self.record)
    }

    /// What a peer checks before it accepts a node: the signature by
    /// `cluster_pub`, the fields, the cluster, and that the record has not
    /// expired at `now` (section 2: "Peers accept a node only by a valid,
    /// unexpired record").
    ///
    /// # Errors
    ///
    /// As [`SignedNodeRecord::verify`], then [`RecordError::Cluster`] and
    /// [`RecordError::Expired`].
    pub fn accept(
        &self,
        cluster_id: &ClusterId,
        cluster_pub: &VerifyingKey,
        now: WallTime,
    ) -> Result<NodeRecord, RecordError> {
        let record = self.verify(cluster_pub)?;
        if record.cluster_id != *cluster_id {
            return Err(RecordError::Cluster);
        }
        if !record.is_valid_at(now) {
            return Err(RecordError::Expired);
        }
        Ok(record)
    }
}

fn signed_bytes(record: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(RECORD_CONTEXT.len() + record.len());
    bytes.extend_from_slice(RECORD_CONTEXT);
    bytes.extend_from_slice(record);
    bytes
}

/// The bytes a `renew` signature covers: `alloy-renew-v1`, `cluster_id`,
/// `node_id`, and `serial` as 8 bytes big endian (section 3.5).
#[must_use]
pub fn renew_message(cluster_id: &ClusterId, node_id: &NodeId, serial: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(RENEW_CONTEXT.len() + 16 + 16 + 8);
    bytes.extend_from_slice(RENEW_CONTEXT);
    bytes.extend_from_slice(cluster_id.as_bytes());
    bytes.extend_from_slice(node_id.as_bytes());
    bytes.extend_from_slice(&serial.to_be_bytes());
    bytes
}

/// Signs a `renew` with the node's identity key.
#[must_use]
pub fn sign_renew(
    identity_key: &SigningKey,
    cluster_id: &ClusterId,
    node_id: &NodeId,
    serial: u64,
) -> Signature {
    identity_key.sign(&renew_message(cluster_id, node_id, serial))
}

/// Checks a `renew` signature against the node's `identity_pub`.
///
/// # Errors
///
/// [`ed25519::Ed25519Error::BadSignature`] when it does not verify.
pub fn verify_renew(
    identity_pub: &VerifyingKey,
    cluster_id: &ClusterId,
    node_id: &NodeId,
    serial: u64,
    signature: &Signature,
) -> Result<(), ed25519::Ed25519Error> {
    identity_pub.verify(&renew_message(cluster_id, node_id, serial), signature)
}

/// Seconds since 1970 of a wall time, rounded down.
pub(crate) fn seconds(time: WallTime) -> i64 {
    time.0.div_euclid(1_000_000_000)
}

fn array<const N: usize>(bytes: &[u8], field: &'static str) -> Result<[u8; N], RecordError> {
    bytes.try_into().map_err(|_| RecordError::Field(field))
}

pub(crate) fn ip_bytes(ip: &IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(ip) => ip.octets().to_vec(),
        IpAddr::V6(ip) => ip.octets().to_vec(),
    }
}

pub(crate) fn parse_ip(bytes: &[u8]) -> Option<IpAddr> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes)
            .ok()
            .map(|b| IpAddr::V4(Ipv4Addr::from(b))),
        16 => <[u8; 16]>::try_from(bytes)
            .ok()
            .map(|b| IpAddr::V6(Ipv6Addr::from(b))),
        _ => None,
    }
}
