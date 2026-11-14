//! The kind `JoinToken` (ARFC 0006 section 2.9): what the control plane
//! keeps of a join token (ARFC 0002 sections 3.2 and 3.14), and no secret.
//!
//! A `JoinToken` is a resource of kind [`JOIN_TOKEN_KIND`] with cluster
//! scope, named `token-` and the 16 lower case hexadecimal digits of its
//! `token_id`. Its `spec` and `status` are this module's Protocol Buffers
//! messages; no method serves the kind yet, so their field numbers are this
//! crate's choice (ARFC 0008 clause 3.1).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use alloy_rt::WallTime;
use prost::Message as _;

use crate::node::{MAX_NODE_NAME, Role, timestamp, wall_time};
use crate::resource::{Resource, ResourceError, Scope, Uid};

/// The kind of join token resources.
pub const JOIN_TOKEN_KIND: &str = "JoinToken";

/// A join token's 8-byte public lookup id (ARFC 0002 section 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenId(pub [u8; 8]);

impl TokenId {
    /// The name of the token's resource: `token-` and 16 lower case
    /// hexadecimal digits.
    #[must_use]
    pub fn resource_name(&self) -> String {
        format!("token-{}", hex(&self.0))
    }
}

/// Lower case hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        // Writing to a String does not fail.
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The wanted part of a `JoinToken`, fixed when the token is created except
/// `revoked`.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinTokenSpec {
    /// The token's public lookup id.
    pub token_id: TokenId,
    /// The token key's Ed25519 public key.
    pub token_pub: [u8; 32],
    /// The 16 bytes of the cluster key fingerprint the token carries.
    pub cluster_key_fp: [u8; 16],
    /// The wall time at which its time limit ends.
    pub expire_time: WallTime,
    /// Its use limit, 0 for unlimited.
    pub use_limit: u64,
    /// Its preset roles, empty for none.
    pub preset_roles: BTreeSet<Role>,
    /// Its preset zone, empty for none, at most [`MAX_NODE_NAME`] bytes.
    pub preset_zone: String,
    /// Whether it is revoked.
    pub revoked: bool,
}

/// Shows every field; none is secret.
impl fmt::Debug for JoinTokenSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinTokenSpec")
            .field("token_id", &hex(&self.token_id.0))
            .field("token_pub", &hex(&self.token_pub))
            .field("cluster_key_fp", &hex(&self.cluster_key_fp))
            .field("expire_time", &self.expire_time)
            .field("use_limit", &self.use_limit)
            .field("preset_roles", &self.preset_roles)
            .field("preset_zone", &self.preset_zone)
            .field("revoked", &self.revoked)
            .finish()
    }
}

/// A `JoinToken` as the group stores it. Its name follows from
/// `spec.token_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinTokenRecord {
    /// Set by the group at creation.
    pub uid: Uid,
    /// As [`Resource::generation`].
    pub generation: u64,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// As [`Resource::deleting`].
    pub deleting: bool,
    /// What is wanted.
    pub spec: JoinTokenSpec,
    /// The uses counted so far (ARFC 0002 section 3.3), committed state.
    pub uses: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
struct SpecPb {
    #[prost(bytes = "vec", tag = "1")]
    token_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    token_pub: Vec<u8>,
    #[prost(bytes = "vec", tag = "3")]
    cluster_key_fp: Vec<u8>,
    #[prost(message, optional, tag = "4")]
    expire_time: Option<prost_types::Timestamp>,
    #[prost(uint64, tag = "5")]
    use_limit: u64,
    #[prost(int32, repeated, tag = "6")]
    preset_roles: Vec<i32>,
    #[prost(string, tag = "7")]
    preset_zone: String,
    #[prost(bool, tag = "8")]
    revoked: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
struct StatusPb {
    #[prost(uint64, tag = "1")]
    uses: u64,
}

pub(crate) fn encode_spec(spec: &JoinTokenSpec) -> Vec<u8> {
    SpecPb {
        token_id: spec.token_id.0.to_vec(),
        token_pub: spec.token_pub.to_vec(),
        cluster_key_fp: spec.cluster_key_fp.to_vec(),
        expire_time: Some(timestamp(spec.expire_time)),
        use_limit: spec.use_limit,
        preset_roles: spec
            .preset_roles
            .iter()
            .map(|r| r.to_proto() as i32)
            .collect(),
        preset_zone: spec.preset_zone.clone(),
        revoked: spec.revoked,
    }
    .encode_to_vec()
}

pub(crate) fn decode_spec(bytes: &[u8]) -> Option<JoinTokenSpec> {
    let wire = SpecPb::decode(bytes).ok()?;
    let mut preset_roles = BTreeSet::new();
    for value in wire.preset_roles {
        let role = Role::from_proto(value)?;
        // Ascending with no repeats, as written.
        if preset_roles.last().is_some_and(|last| *last >= role) {
            return None;
        }
        preset_roles.insert(role);
    }
    Some(JoinTokenSpec {
        token_id: TokenId(wire.token_id.try_into().ok()?),
        token_pub: wire.token_pub.try_into().ok()?,
        cluster_key_fp: wire.cluster_key_fp.try_into().ok()?,
        expire_time: wall_time(wire.expire_time.as_ref()?)?,
        use_limit: wire.use_limit,
        preset_roles,
        preset_zone: wire.preset_zone,
        revoked: wire.revoked,
    })
}

fn encode_status(uses: u64) -> Vec<u8> {
    StatusPb { uses }.encode_to_vec()
}

fn decode_status(bytes: &[u8]) -> Option<u64> {
    StatusPb::decode(bytes).ok().map(|s| s.uses)
}

impl JoinTokenSpec {
    /// Checks every field a client sets: the preset zone is at most
    /// [`MAX_NODE_NAME`] bytes, as a node's zone (ARFC 0002 section 3.3).
    pub(crate) fn check(&self) -> Result<(), ResourceError> {
        if self.preset_zone.len() > MAX_NODE_NAME {
            return Err(ResourceError::Invalid(
                "a preset zone must be at most 63 bytes",
            ));
        }
        Ok(())
    }
}

impl JoinTokenRecord {
    /// The name of its resource.
    #[must_use]
    pub fn name(&self) -> String {
        self.spec.token_id.resource_name()
    }

    /// The resource that stores this token.
    pub(crate) fn to_resource(&self) -> Resource {
        Resource {
            kind: JOIN_TOKEN_KIND.to_owned(),
            uid: self.uid,
            name: self.name(),
            scope: Scope::Cluster,
            generation: self.generation,
            spec: encode_spec(&self.spec),
            status: encode_status(self.uses),
            labels: self.labels.clone(),
            deleting: self.deleting,
        }
    }

    /// The token a stored resource holds, or `None` when it is not one: of
    /// another kind or scope, named otherwise than its `token_id` says, or
    /// with a spec or status that does not decode or check.
    pub(crate) fn from_resource(r: &Resource) -> Option<JoinTokenRecord> {
        if r.kind != JOIN_TOKEN_KIND || r.scope != Scope::Cluster {
            return None;
        }
        let spec = decode_spec(&r.spec)?;
        spec.check().ok()?;
        if r.name != spec.token_id.resource_name() {
            return None;
        }
        Some(JoinTokenRecord {
            uid: r.uid,
            generation: r.generation,
            labels: r.labels.clone(),
            deleting: r.deleting,
            spec,
            uses: decode_status(&r.status)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn spec() -> JoinTokenSpec {
        JoinTokenSpec {
            token_id: TokenId([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]),
            token_pub: [7; 32],
            cluster_key_fp: [9; 16],
            expire_time: WallTime(1_800_000_000_123_456_789),
            use_limit: 3,
            preset_roles: BTreeSet::from([Role::Compute, Role::Anchor]),
            preset_zone: "z1".to_owned(),
            revoked: false,
        }
    }

    #[test]
    fn names_follow_the_token_id() {
        assert_eq!(spec().token_id.resource_name(), "token-0123456789abcdef");
    }

    #[test]
    fn a_token_round_trips_through_its_resource() {
        let record = JoinTokenRecord {
            uid: Uid([4; 16]),
            generation: 2,
            labels: BTreeMap::new(),
            deleting: false,
            spec: spec(),
            uses: 5,
        };
        let resource = record.to_resource();
        assert_eq!(resource.name, "token-0123456789abcdef");
        assert_eq!(JoinTokenRecord::from_resource(&resource), Some(record));
        let mut unlimited = spec();
        unlimited.use_limit = 0;
        unlimited.preset_roles.clear();
        unlimited.preset_zone.clear();
        unlimited.revoked = true;
        assert_eq!(
            decode_spec(&encode_spec(&unlimited)),
            Some(unlimited.clone())
        );
    }

    #[test]
    fn stored_tokens_out_of_range_do_not_decode() {
        let good = JoinTokenRecord {
            uid: Uid([4; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            spec: spec(),
            uses: 0,
        }
        .to_resource();
        let mut renamed = good.clone();
        renamed.name = "token-0000000000000000".to_owned();
        assert_eq!(JoinTokenRecord::from_resource(&renamed), None);
        let mut scoped = good.clone();
        scoped.scope = Scope::Org("default".to_owned());
        assert_eq!(JoinTokenRecord::from_resource(&scoped), None);
        let mut wire = SpecPb::decode(good.spec.as_slice()).unwrap();
        wire.token_pub.pop();
        assert_eq!(decode_spec(&wire.encode_to_vec()), None);
        let mut wire = SpecPb::decode(good.spec.as_slice()).unwrap();
        wire.expire_time = None;
        assert_eq!(decode_spec(&wire.encode_to_vec()), None);
        let mut wire = SpecPb::decode(good.spec.as_slice()).unwrap();
        wire.preset_roles = vec![Role::Anchor.to_proto() as i32; 2];
        assert_eq!(decode_spec(&wire.encode_to_vec()), None);
        let mut zoned = spec();
        zoned.preset_zone = "z".repeat(64);
        assert!(zoned.check().is_err());
        let mut bad = good;
        bad.spec = encode_spec(&zoned);
        assert_eq!(JoinTokenRecord::from_resource(&bad), None);
    }
}
