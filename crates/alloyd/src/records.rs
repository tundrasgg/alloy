//! What the control plane keeps of each node besides its `Node` (ARFC 0002
//! section 3.14): its signed record bytes, its facts, the `token_id` that
//! admitted it, and the source address its request came from, as one
//! resource of kind [`RECORD_KIND`] per node, named as the node, in the
//! cluster scope of the global group.
//!
//! The `Node` of ARFC 0006 section 2.6 carries no key, secret, or token id,
//! so these are kept apart from it. A node's mesh and identity keys are
//! those of its record. Both resources are written by the control node's
//! own node channel task only, one write at a time.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

use alloy_crypto::al::PublicKey;
use alloy_crypto::ed25519::VerifyingKey;
use alloy_id::proto::{Facts, SignedNodeRecord as WireRecord};
use alloy_id::{NodeId, NodeRecord, SignedNodeRecord};
use alloy_state::{Group, GroupError, Resource, Scope, StoreError};
use prost::Message;

/// The kind of the resources that hold what the control plane keeps of a
/// node beyond its `Node`.
pub(crate) const RECORD_KIND: &str = "NodeRecord";

/// The stored form, encoded as the resource's spec.
#[derive(Clone, PartialEq, prost::Message)]
struct Stored {
    #[prost(message, optional, tag = "1")]
    record: Option<WireRecord>,
    /// 8 bytes, empty for the node `init` made.
    #[prost(bytes = "vec", tag = "2")]
    token_id: Vec<u8>,
    #[prost(message, optional, tag = "3")]
    facts: Option<Facts>,
    /// 4 or 16 bytes, empty for the node `init` made.
    #[prost(bytes = "vec", tag = "4")]
    source_address: Vec<u8>,
    #[prost(uint32, tag = "5")]
    source_port: u32,
}

/// One node as the control plane keeps it.
#[derive(Clone, Debug)]
pub(crate) struct Enrolled {
    /// The current signed record, as issued.
    pub(crate) signed: SignedNodeRecord,
    /// That record, verified.
    pub(crate) record: NodeRecord,
    /// The token that admitted it, `None` for the node `init` made.
    pub(crate) token_id: Option<[u8; 8]>,
    /// Its facts, as its request gave them.
    pub(crate) facts: Option<Facts>,
    /// Where its request came from.
    pub(crate) source: Option<SocketAddr>,
    /// The resource it was read from, `None` before it is created.
    resource: Option<Resource>,
}

impl Enrolled {
    /// A node not yet stored.
    #[must_use]
    pub(crate) fn new(
        signed: SignedNodeRecord,
        record: NodeRecord,
        token_id: Option<[u8; 8]>,
        facts: Option<Facts>,
        source: Option<SocketAddr>,
    ) -> Enrolled {
        Enrolled {
            signed,
            record,
            token_id,
            facts,
            source,
            resource: None,
        }
    }

    /// Whether `key` is a mesh key of its record.
    #[must_use]
    pub(crate) fn holds_mesh_key(&self, key: &PublicKey) -> bool {
        self.record.mesh_pub.contains(key)
    }

    /// Its node id.
    #[must_use]
    pub(crate) fn node_id(&self) -> NodeId {
        self.record.node_id
    }

    fn spec(&self) -> Vec<u8> {
        let (source_address, source_port) = match self.source {
            Some(addr) => (
                match addr.ip() {
                    IpAddr::V4(v4) => v4.octets().to_vec(),
                    IpAddr::V6(v6) => v6.octets().to_vec(),
                },
                u32::from(addr.port()),
            ),
            None => (Vec::new(), 0),
        };
        Stored {
            record: Some(self.signed.to_proto()),
            token_id: self.token_id.map(|t| t.to_vec()).unwrap_or_default(),
            facts: self.facts.clone(),
            source_address,
            source_port,
        }
        .encode_to_vec()
    }

    fn decode(resource: Resource, cluster_pub: &VerifyingKey) -> Result<Enrolled, GroupError> {
        let damaged = |what: &str| {
            GroupError::Store(StoreError::Corrupt(alloy_seam::Detail::new(&format!(
                "a stored {RECORD_KIND} {what}"
            ))))
        };
        let stored =
            Stored::decode(resource.spec.as_slice()).map_err(|_| damaged("does not decode"))?;
        let wire = stored.record.ok_or_else(|| damaged("holds no record"))?;
        let signed =
            SignedNodeRecord::from_proto(&wire).map_err(|_| damaged("has a bad record"))?;
        let record = signed
            .verify(cluster_pub)
            .map_err(|_| damaged("holds a record the cluster key did not sign"))?;
        if record.name != resource.name {
            return Err(damaged("names another node"));
        }
        let token_id = match stored.token_id.len() {
            0 => None,
            _ => Some(
                <[u8; 8]>::try_from(stored.token_id.as_slice())
                    .map_err(|_| damaged("has a bad token id"))?,
            ),
        };
        let ip = match stored.source_address.len() {
            4 => <[u8; 4]>::try_from(stored.source_address.as_slice())
                .ok()
                .map(IpAddr::from),
            16 => <[u8; 16]>::try_from(stored.source_address.as_slice())
                .ok()
                .map(IpAddr::from),
            _ => None,
        };
        let source = ip.and_then(|ip| {
            u16::try_from(stored.source_port)
                .ok()
                .map(|port| SocketAddr::new(ip, port))
        });
        Ok(Enrolled {
            signed,
            record,
            token_id,
            facts: stored.facts,
            source,
            resource: Some(resource),
        })
    }
}

/// Every node the group keeps a record for, in name order. A resource
/// marked `deleting` is left out.
///
/// # Errors
///
/// [`GroupError`] when the group cannot be read or a stored record is
/// damaged.
pub(crate) async fn list(
    group: &Group,
    cluster_pub: &VerifyingKey,
) -> Result<Vec<Enrolled>, GroupError> {
    group
        .list(RECORD_KIND)
        .await?
        .into_iter()
        .filter(|r| r.scope == Scope::Cluster && !r.deleting)
        .map(|r| Enrolled::decode(r, cluster_pub))
        .collect()
}

/// Stores `node`: creates its resource, or replaces it when it was read
/// from one, and returns it as stored.
///
/// # Errors
///
/// [`GroupError`], [`alloy_state::ResourceError::Conflict`] among them when
/// the resource changed since it was read.
pub(crate) async fn store(
    group: &Group,
    cluster_pub: &VerifyingKey,
    node: &Enrolled,
) -> Result<Enrolled, GroupError> {
    let spec = node.spec();
    let written = match &node.resource {
        None => {
            group
                .create(
                    RECORD_KIND,
                    Scope::Cluster,
                    &node.record.name,
                    spec,
                    BTreeMap::new(),
                )
                .await?
        }
        Some(resource) => {
            let mut next = resource.clone();
            next.spec = spec;
            group.update(next).await?
        }
    };
    Enrolled::decode(written, cluster_pub)
}

/// Deletes the record resource of node `name`, when there is one.
///
/// # Errors
///
/// [`GroupError`].
pub(crate) async fn remove(group: &Group, name: &str) -> Result<(), GroupError> {
    let Some(found) = group.get(RECORD_KIND, &Scope::Cluster, name).await? else {
        return Ok(());
    };
    let marked = if found.deleting {
        found
    } else {
        group
            .delete(RECORD_KIND, &Scope::Cluster, name, found.generation)
            .await?
    };
    group
        .remove(RECORD_KIND, &Scope::Cluster, name, marked.generation)
        .await
}
