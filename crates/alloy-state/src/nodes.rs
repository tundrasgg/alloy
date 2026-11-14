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
    ///
    /// When a node was removed while the heartbeat was accepted, it reads
    /// its node again before it returns, and drops what it recorded when the
    /// group no longer holds that node, so no soft state outlives its node.
    pub async fn accept_heartbeat(&self, name: &str, heartbeat: &Heartbeat) -> StatusReply {
        let mut recorded = None;
        let reply = self.accept(name, heartbeat, &mut recorded).await;
        if let Some((uid, forgotten)) = recorded
            && self.soft_state().forgotten() != forgotten
        {
            let held = match self.get(NODE_KIND, &Scope::Cluster, name).await {
                Ok(Some(found)) => found.uid == uid,
                Ok(None) | Err(_) => false,
            };
            if !held {
                self.soft_state().drop_entry(uid);
            }
        }
        reply
    }

    /// [`Group::accept_heartbeat`] but for the check after it: sets
    /// `recorded` to the `uid` it recorded soft state for and the count of
    /// removals before the read that found the node.
    async fn accept(
        &self,
        name: &str,
        heartbeat: &Heartbeat,
        recorded: &mut Option<(Uid, u64)>,
    ) -> StatusReply {
        if !self.soft_state().serving() {
            return StatusReply::Unavailable;
        }
        let Some(checked) = heartbeat.check() else {
            return StatusReply::Invalid;
        };
        if check_name(name).is_err() {
            return StatusReply::Refused;
        }
        for _ in 0..COMMIT_ATTEMPTS {
            let forgotten = self.soft_state().forgotten();
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
            if recorded.is_none() {
                #[cfg(test)]
                {
                    let pause = self
                        .pause_heartbeat
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take();
                    if let Some((reached, resume)) = pause {
                        // Either half dropped only ends the pause early.
                        let _ = reached.send(());
                        let _ = resume.await;
                    }
                }
                let now = self.now();
                self.soft_state()
                    .record(node.uid, now, wall, checked.addresses.clone());
                *recorded = Some((node.uid, forgotten));
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use alloy_rt::Runtime;
    use alloy_seam::{Detail, ImplName, RegistryBuilder, SeamConfig};
    use alloy_sim::{Failure, HostConfig, RunConfig, RunResult, World, run};

    use crate::{
        Group, GroupError, Heartbeat, LogStoreSeam, MemStoreConfig, NODE_KIND, NodeId, NodeSpec,
        NodeStatus, RecordTerms, ResourceError, Scope, StateStoreSeam, StatusReply, Uid,
        register_mem,
    };

    fn fail(error: impl std::fmt::Debug) -> Failure {
        Failure(Detail::new(&format!("{error:?}")))
    }

    fn check(ok: bool, what: &str) -> Result<(), Failure> {
        if ok {
            Ok(())
        } else {
            Err(Failure(Detail::new(what)))
        }
    }

    fn invalid<T>(result: &Result<T, GroupError>) -> bool {
        matches!(result, Err(GroupError::Resource(ResourceError::Invalid(_))))
    }

    /// A serving one-voter group on `mem` stores on a new simulated host.
    async fn serving(world: &World) -> Result<(Runtime, Group), Failure> {
        let boot = Arc::new(|_| -> alloy_rt::BoxFuture<'static, ()> { Box::pin(async {}) });
        let host = world.add_host(HostConfig::new(boot)).map_err(fail)?;
        let runtime = world.runtime(host).map_err(fail)?;
        let mut builder = RegistryBuilder::new();
        register_mem(&mut builder).map_err(fail)?;
        let registry = builder.build();
        let config = MemStoreConfig {
            name: "global".to_owned(),
            fail_after: 0,
        }
        .to_value(1)
        .map_err(fail)?;
        let mem = ImplName::new("mem").map_err(fail)?;
        let log = registry
            .instantiate::<LogStoreSeam>(&runtime, &mem, &config)
            .map_err(fail)?;
        let state = registry
            .instantiate::<StateStoreSeam>(&runtime, &mem, &config)
            .map_err(fail)?;
        let group = Group::open(&runtime, NodeId([0x11; 16]), log, state)
            .await
            .map_err(fail)?;
        group.initialize().await.map_err(fail)?;
        Ok((runtime, group))
    }

    fn joined(runtime: &Runtime) -> NodeStatus {
        let now = runtime.clock().wall().0.div_euclid(1_000_000_000);
        NodeStatus {
            record: Some(RecordTerms {
                serial: 1,
                not_after: now + 86_400,
            }),
            ..NodeStatus::pending(NodeId([7; 16]))
        }
    }

    fn heartbeat() -> Heartbeat {
        Heartbeat {
            addresses: vec![vec![192, 0, 2, 10]],
            release: String::new(),
        }
    }

    fn heard(group: &Group) -> BTreeSet<Uid> {
        group.soft_state().heard()
    }

    /// Marks node `name` deleting at its current generation, again when a
    /// heartbeat's commit changed it in between, then removes it.
    async fn delete_and_remove(group: &Group, name: &str) -> Result<(), Failure> {
        for _ in 0..5 {
            let node = group
                .get_node(name)
                .await
                .map_err(fail)?
                .ok_or_else(|| fail("the node is missing"))?
                .node;
            let deleted = group
                .delete(NODE_KIND, &Scope::Cluster, name, node.generation)
                .await;
            match deleted {
                Ok(deleting) => {
                    return group
                        .remove(NODE_KIND, &Scope::Cluster, name, deleting.generation)
                        .await
                        .map_err(fail);
                }
                Err(GroupError::Resource(ResourceError::Conflict { .. })) => {}
                Err(other) => return Err(fail(other)),
            }
        }
        Err(fail("the delete kept conflicting"))
    }

    fn failing_seeds<F, Fut>(count: u64, scenario: F) -> Vec<(u64, Option<Detail>)>
    where
        F: Fn(World) -> Fut,
        Fut: Future<Output = Result<(), Failure>> + Send + 'static,
    {
        (0..count)
            .filter_map(|seed| {
                let summary = run(RunConfig::new(seed), &scenario);
                (summary.result != RunResult::Pass).then_some((seed, summary.failure))
            })
            .collect()
    }

    /// T-0114 review F1: the generic resource operations hold a `Node` to
    /// the checks of `create_node` and `update_node`.
    async fn generic_writes(world: World) -> Result<(), Failure> {
        let (runtime, group) = serving(&world).await?;
        let good = group
            .create_node(
                "good",
                NodeSpec::default(),
                joined(&runtime),
                BTreeMap::new(),
            )
            .await
            .map_err(fail)?;
        let spec = good.to_resource().spec;
        let not_a_node = group
            .create(
                NODE_KIND,
                Scope::Cluster,
                "bad",
                vec![0xff, 0xff],
                BTreeMap::new(),
            )
            .await;
        check(
            invalid(&not_a_node),
            "a create whose spec is not a NodeSpec",
        )?;
        let no_status = group
            .create(
                NODE_KIND,
                Scope::Cluster,
                "bare",
                spec.clone(),
                BTreeMap::new(),
            )
            .await;
        check(invalid(&no_status), "a create with no NodeStatus")?;
        let project = Scope::Project {
            org: "o".to_owned(),
            project: "p".to_owned(),
        };
        let projected = group
            .create(NODE_KIND, project, "other", spec, BTreeMap::new())
            .await;
        check(invalid(&projected), "a Node in a project scope")?;
        let mut update = good.to_resource();
        update.status = vec![0x0a, 0x01, 0x00];
        check(
            invalid(&group.update(update).await),
            "an update whose status is not a NodeStatus",
        )?;
        let mut zoned = good.clone();
        zoned.spec.zone = "z".repeat(64);
        check(
            invalid(&group.update(zoned.to_resource()).await),
            "an update that breaks Node::check",
        )?;
        let mut moved = good.clone();
        moved.spec.zone = "z".to_owned();
        let updated = group.update(moved.to_resource()).await.map_err(fail)?;
        check(updated.generation == 2, "a valid generic update is applied")?;
        let nodes = group.list_nodes().await.map_err(fail)?;
        check(
            nodes.len() == 1 && nodes[0].node.spec.zone == "z",
            "list_nodes reads the one node",
        )?;
        group.shutdown().await.map_err(fail)
    }

    #[test]
    fn generic_writes_of_a_node_are_checked() {
        assert_eq!(failing_seeds(10, generic_writes), Vec::new());
    }

    /// T-0114 review F2: removing a node drops its soft state at once, and a
    /// heartbeat that races the removal leaves none behind.
    async fn removal(world: World) -> Result<(), Failure> {
        let (runtime, group) = serving(&world).await?;
        for name in ["n1", "n2"] {
            group
                .create_node(name, NodeSpec::default(), joined(&runtime), BTreeMap::new())
                .await
                .map_err(fail)?;
            check(
                group.accept_heartbeat(name, &heartbeat()).await == StatusReply::Ok,
                "accepted",
            )?;
        }
        check(heard(&group).len() == 2, "two heartbeats held")?;
        delete_and_remove(&group, "n1").await?;
        let n2 = group
            .get_node("n2")
            .await
            .map_err(fail)?
            .ok_or_else(|| fail("n2 is missing"))?;
        check(
            heard(&group) == BTreeSet::from([n2.node.uid]),
            "removing n1 drops its heartbeat",
        )?;
        let (reply, removed) = futures_util::future::join(
            group.accept_heartbeat("n2", &heartbeat()),
            delete_and_remove(&group, "n2"),
        )
        .await;
        removed?;
        check(
            matches!(reply, StatusReply::Ok | StatusReply::Refused),
            "the racing heartbeat is accepted or refused",
        )?;
        check(heard(&group).is_empty(), "no heartbeat outlives its node")?;
        group.shutdown().await.map_err(fail)
    }

    #[test]
    fn removing_a_node_drops_its_soft_state() {
        assert_eq!(failing_seeds(50, removal), Vec::new());
    }

    /// T-0121 review F2: a removal that lands between a heartbeat's read of
    /// its node and its record of soft state, as it can on `os`, leaves no
    /// soft state once the heartbeat returns. Without the re-read of
    /// `accept_heartbeat` the entry outlives its node.
    async fn overtaken(world: World) -> Result<(), Failure> {
        let (runtime, group) = serving(&world).await?;
        group
            .create_node("n1", NodeSpec::default(), joined(&runtime), BTreeMap::new())
            .await
            .map_err(fail)?;
        let (reached_tx, reached) = tokio::sync::oneshot::channel();
        let (resume, resume_rx) = tokio::sync::oneshot::channel();
        *group
            .pause_heartbeat
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((reached_tx, resume_rx));
        let remove = async {
            reached.await.map_err(fail)?;
            delete_and_remove(&group, "n1").await?;
            resume.send(()).map_err(fail)
        };
        let (reply, removed) =
            futures_util::future::join(group.accept_heartbeat("n1", &heartbeat()), remove).await;
        removed?;
        check(
            group.get_node("n1").await.map_err(fail)?.is_none(),
            "the node is removed",
        )?;
        check(
            reply == StatusReply::Unavailable,
            "the heartbeat's commit finds its node gone",
        )?;
        check(heard(&group).is_empty(), "no heartbeat outlives its node")?;
        group.shutdown().await.map_err(fail)
    }

    #[test]
    fn a_removal_between_a_heartbeat_read_and_record_leaves_no_soft_state() {
        assert_eq!(failing_seeds(20, overtaken), Vec::new());
    }
}
