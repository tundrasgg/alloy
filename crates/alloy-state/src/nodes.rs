//! The node operations of a group (ARFC 0006 section 2.6): nodes created,
//! updated, and read through the resource operations, heartbeats accepted
//! as clause 3.14 says, and state computed at each read as clause 3.15
//! says.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{MutexGuard, PoisonError};

use alloy_rt::{Instant, WallTime};
use alloy_seam::Detail;

use crate::group::{Group, GroupError};
use crate::node::{
    Heartbeat, NODE_KIND, Node, NodeSpec, NodeStatus, NodeView, Soft, StatusReply, check_name,
};
use crate::resource::{Resource, ResourceError, Scope, Uid};
use crate::store::StoreError;

/// How many times a heartbeat's commit is tried when another write changed
/// the node in between.
const COMMIT_ATTEMPTS: usize = 3;

fn decode_node(resource: &Resource) -> Result<Node, GroupError> {
    Node::from_resource(resource).ok_or_else(|| {
        StoreError::Corrupt(Detail::new("a stored node does not decode as a Node")).into()
    })
}

/// Whole seconds since the Unix epoch, rounded down.
fn seconds(time: WallTime) -> i64 {
    time.0.div_euclid(1_000_000_000)
}

impl Group {
    fn soft_state(&self) -> MutexGuard<'_, Soft> {
        self.soft.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now(&self) -> Instant {
        self.scope.runtime.clock().now()
    }

    /// Creates the node `name` with a new random `uid` and generation 1.
    ///
    /// # Errors
    ///
    /// [`ResourceError::Invalid`] for a name, zone, or advertise list out of
    /// range (ARFC 0002 section 3.3), and as [`Group::create`].
    pub async fn create_node(
        &self,
        name: &str,
        spec: NodeSpec,
        status: NodeStatus,
        labels: BTreeMap<String, String>,
    ) -> Result<Node, GroupError> {
        let draft = Node {
            name: name.to_owned(),
            uid: Uid([0; 16]),
            generation: 1,
            labels,
            deleting: false,
            spec,
            status,
        };
        draft.check()?;
        let Resource {
            spec,
            status,
            labels,
            ..
        } = draft.to_resource();
        let target = crate::resource::Target {
            kind: NODE_KIND.to_owned(),
            scope: Scope::Cluster,
            name: name.to_owned(),
        };
        let created = self
            .create_with_status(target, spec, status, labels)
            .await?;
        decode_node(&created)
    }

    /// Replaces the spec, committed status, and labels of node `node.name`
    /// when its generation is `node.generation`, and returns it with its
    /// generation one more (clause 3.5).
    ///
    /// # Errors
    ///
    /// [`ResourceError::Invalid`] as for [`Group::create_node`], and as
    /// [`Group::update`].
    pub async fn update_node(&self, node: &Node) -> Result<Node, GroupError> {
        node.check()?;
        let updated = self.update(node.to_resource()).await?;
        decode_node(&updated)
    }

    /// Reads node `name`, linearizably, with its state at the read and its
    /// soft state.
    ///
    /// # Errors
    ///
    /// [`ResourceError::Invalid`] for a name out of range,
    /// [`GroupError::Store`] for a stored node that does not decode, and as
    /// [`Group::get`].
    pub async fn get_node(&self, name: &str) -> Result<Option<NodeView>, GroupError> {
        check_name(name)?;
        let Some(found) = self.get(NODE_KIND, &Scope::Cluster, name).await? else {
            return Ok(None);
        };
        let node = decode_node(&found)?;
        let now = self.now();
        Ok(Some(self.soft_state().view(node, now)))
    }

    /// Reads every node, linearizably, in name order, each with its state at
    /// the read and its soft state. Drops the soft state of nodes the group
    /// no longer holds.
    ///
    /// # Errors
    ///
    /// As [`Group::get_node`].
    pub async fn list_nodes(&self) -> Result<Vec<NodeView>, GroupError> {
        let nodes = self
            .list(NODE_KIND)
            .await?
            .iter()
            .map(decode_node)
            .collect::<Result<Vec<_>, _>>()?;
        let now = self.now();
        let mut soft = self.soft_state();
        soft.retain(&nodes.iter().map(|n| n.uid).collect::<BTreeSet<_>>());
        Ok(nodes.into_iter().map(|n| soft.view(n, now)).collect())
    }

    /// Accepts a heartbeat from node `name` as the leader of the global group
    /// (clause 3.14). `name` is the node whose record holds the mesh key that
    /// the heartbeat's AL session authenticated.
    ///
    /// Replies [`StatusReply::Unavailable`] while this member does not serve
    /// the group, [`StatusReply::Invalid`] when the message breaks section
    /// 4.3, and [`StatusReply::Refused`] when the group holds no node `name`,
    /// or it has no record, is being deleted, or its record has expired by
    /// the wall clock; each changes nothing. Otherwise it records the
    /// heartbeat as soft state, replacing what it held for the node, commits
    /// `reported` true when it was false and `release` as the running
    /// release when it is set and differs, and replies [`StatusReply::Ok`]
    /// once those commits are applied, or [`StatusReply::Unavailable`] when
    /// one fails (clause 3.12.2). A heartbeat that changes neither commits
    /// nothing.
    pub async fn accept_heartbeat(&self, name: &str, heartbeat: &Heartbeat) -> StatusReply {
        if !self.soft_state().serving() {
            return StatusReply::Unavailable;
        }
        let Some(checked) = heartbeat.check() else {
            return StatusReply::Invalid;
        };
        if check_name(name).is_err() {
            return StatusReply::Refused;
        }
        let mut recorded = false;
        for _ in 0..COMMIT_ATTEMPTS {
            let node = match self.get(NODE_KIND, &Scope::Cluster, name).await {
                Ok(Some(found)) => match Node::from_resource(&found) {
                    Some(node) => node,
                    None => return StatusReply::Unavailable,
                },
                Ok(None) => return StatusReply::Refused,
                Err(_) => return StatusReply::Unavailable,
            };
            let wall = self.scope.runtime.clock().wall();
            let valid = node
                .status
                .record
                .is_some_and(|record| seconds(wall) < record.not_after);
            if node.deleting || !valid {
                return StatusReply::Refused;
            }
            if !recorded {
                let now = self.now();
                self.soft_state()
                    .record(node.uid, now, wall, checked.addresses.clone());
                recorded = true;
            }
            let mut next = node.clone();
            next.status.reported = true;
            if let Some(release) = &checked.release {
                next.status.running_release = Some(release.clone());
            }
            if next.status == node.status {
                return StatusReply::Ok;
            }
            match self.update_node(&next).await {
                Ok(_) => return StatusReply::Ok,
                Err(GroupError::Resource(ResourceError::Conflict { .. })) => {}
                Err(_) => return StatusReply::Unavailable,
            }
        }
        StatusReply::Unavailable
    }
}
