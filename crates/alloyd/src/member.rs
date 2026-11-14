//! A joined node that does not hold `control`, serving (ARFC 0002 sections
//! 3.5, 3.11, and 3.13, ARFC 0006 clause 3.13), as M11 rung 2 runs it: a
//! Linux node or a client node.
//!
//! [`MemberNode::start`] reads what `join` stored, opens an AL session from
//! its mesh port to each home of its bootstrap view, and sends its
//! heartbeats and renewals to a control node over them as node messages.
//! In rung 2 a node reaches only those of its homes that hold `control`
//! (ARFC 0002 section 3.13, This version), so those are its homes, in the
//! view's order.
//!
//! - Heartbeats follow the schedule of ARFC 0006 clause 3.13 while the
//!   node's record has not expired by its own clock.
//! - The node renews its record at half life, and after a renewal that gets
//!   no reply, again 60 seconds after it ended (section 3.5). A record it
//!   is issued whose serial is above its own replaces its own, in its node
//!   file. On `node_unknown` it stops renewing and logs that the machine is
//!   no longer a node of the cluster.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use alloy_crypto::al::{PublicKey, StaticSecret};
use alloy_crypto::ed25519::{SigningKey, VerifyingKey};
use alloy_id::proto::NodeState as StoredNode;
use alloy_id::record::sign_renew;
use alloy_id::{ClusterId, DataDir, NodeRecord, Role, SignedNodeRecord};
use alloy_rt::{JoinHandle, Runtime, TaskName, UdpSocket};
use tracing::{debug, error, info, warn};

use crate::channel::heartbeat::{self, HeartbeatResult, Schedule};
use crate::channel::proto::{Body, JoinReply, JoinResult, Renew, refusal};
use crate::channel::requester::Outcome;
use crate::control::{NodeError, NodePaths};
use crate::mesh::{Command, Mesh, MeshHandle, Target, reserve_counter};

/// The time limit of a `renew` (ARFC 0002 section 3.5).
pub(crate) const RENEW_LIMIT: Duration = Duration::from_secs(10);

/// The wait after a `renew` that got no reply, counted from its end.
pub(crate) const RENEW_RETRY: Duration = Duration::from_secs(60);

/// The longest the renewal task sleeps before it reads the wall clock
/// again, so that a computer that was asleep renews soon after it wakes.
const RENEW_CHECK: Duration = Duration::from_secs(60);

fn task(name: &'static str) -> TaskName {
    // Every caller passes a short ASCII literal, which `TaskName::new` accepts.
    TaskName::new(name).expect("a short literal task name is valid")
}

fn seconds(time: alloy_rt::WallTime) -> i64 {
    time.0.div_euclid(1_000_000_000)
}

/// What the node holds of itself.
struct Member {
    cluster_id: ClusterId,
    cluster_pub: VerifyingKey,
    identity: SigningKey,
    signed: SignedNodeRecord,
    record: NodeRecord,
}

/// The node's current record, shared by its tasks.
type Current = Arc<Mutex<(SignedNodeRecord, NodeRecord)>>;

/// A serving node that does not hold `control`. Dropping it stops its
/// tasks.
pub struct MemberNode {
    name: String,
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for MemberNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemberNode")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

async fn read_member(
    runtime: &Runtime,
    paths: &NodePaths,
) -> Result<(Member, StaticSecret, StoredNode), NodeError> {
    let opened = paths
        .data
        .open(runtime.rng().as_ref())
        .await?
        .ok_or(NodeError::NotJoined("it holds no at-rest key"))?;
    let state = paths
        .data
        .read_node_state()
        .await?
        .ok_or(NodeError::NotJoined("it holds no node file"))?;
    let damaged = |what: &str| NodeError::Damaged(what.to_owned());
    let cluster_pub = state
        .cluster_pub
        .first()
        .and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok())
        .and_then(|k| VerifyingKey::from_bytes(&k).ok())
        .ok_or_else(|| damaged("the node file holds no cluster public key"))?;
    let cluster_id = <[u8; 16]>::try_from(state.cluster_id.as_slice())
        .map(ClusterId::from_bytes)
        .map_err(|_| damaged("the cluster id"))?;
    let signed = state
        .record
        .as_ref()
        .ok_or(NodeError::NotJoined("the node file holds no record"))
        .and_then(|r| {
            SignedNodeRecord::from_proto(r).map_err(|e| damaged(&format!("the node record: {e}")))
        })?;
    // An expired record is kept: the node renews it (section 3.5).
    let record = signed
        .verify(&cluster_pub)
        .map_err(|e| damaged(&format!("the node record: {e}")))?;
    let identity = <[u8; 32]>::try_from(opened.secrets.identity_key.as_slice())
        .map(|seed| SigningKey::from_seed(&seed))
        .map_err(|_| damaged("the identity key"))?;
    let mesh = <[u8; 32]>::try_from(opened.secrets.mesh_key.as_slice())
        .map(StaticSecret::from_bytes)
        .map_err(|_| damaged("the mesh key"))?;
    if record.mesh_pub.first() != Some(&mesh.public_key()) {
        return Err(damaged("the record names another mesh key"));
    }
    Ok((
        Member {
            cluster_id,
            cluster_pub,
            identity,
            signed,
            record,
        },
        mesh,
        state,
    ))
}

/// The homes a node of rung 2 reaches: the nodes of its view that hold
/// `anchor` and `control`, in the view's order, by mesh key and the first
/// advertise address with the mesh port. Its own record is left out.
fn homes(state: &StoredNode, member: &Member) -> Vec<(PublicKey, SocketAddr)> {
    let Some(view) = &state.view else {
        return Vec::new();
    };
    view.nodes
        .iter()
        .filter_map(|n| SignedNodeRecord::from_proto(n).ok())
        .filter_map(|s| s.verify(&member.cluster_pub).ok())
        .filter(|r| {
            r.node_id != member.record.node_id
                && r.roles.contains(Role::Anchor)
                && r.roles.contains(Role::Control)
        })
        .filter_map(|r| {
            let key = *r.mesh_pub.first()?;
            let ip = *r.advertise.first()?;
            Some((key, SocketAddr::new(ip, r.mesh_port)))
        })
        .collect()
}

impl MemberNode {
    /// Starts the node of the data directory at `paths`, over the sockets
    /// `bind` binds to the mesh port of its record.
    ///
    /// # Errors
    ///
    /// [`NodeError`] when the stored node cannot be read, holds `control`
    /// and so is a [`ControlNode`](crate::ControlNode), or `bind` fails.
    pub async fn start<B>(
        runtime: &Runtime,
        paths: &NodePaths,
        bind: B,
    ) -> Result<MemberNode, NodeError>
    where
        B: FnOnce(&NodeRecord) -> Result<Vec<Box<dyn UdpSocket>>, NodeError>,
    {
        let (member, mesh_key, state) = read_member(runtime, paths).await?;
        if member.record.roles.contains(Role::Control) {
            return Err(NodeError::Damaged(
                "a node that holds control is served as a control node".to_owned(),
            ));
        }
        let sockets = bind(&member.record)?;
        let homes = homes(&state, &member);
        if homes.is_empty() {
            warn!(
                "no home of this node's view holds control, so it cannot reach the control plane"
            );
        }
        let counter = reserve_counter(&paths.data).await?;
        let (mesh, _inbound) = Mesh::new(
            runtime,
            mesh_key,
            counter,
            paths.data.clone(),
            member.cluster_id,
            false,
        );
        let (handle, commands) = MeshHandle::channel();
        let name = member.record.name.clone();
        let mut node = MemberNode {
            name: name.clone(),
            tasks: Vec::new(),
        };
        node.tasks
            .push(runtime.spawn(task("member-mesh"), mesh.run(sockets, commands))?);
        for (peer, addr) in &homes {
            handle
                .send(Command::Dial {
                    peer: *peer,
                    addr: *addr,
                })
                .await;
        }
        handle
            .send(Command::SetControl(homes.iter().map(|(k, _)| *k).collect()))
            .await;
        let current: Current = Arc::new(Mutex::new((member.signed.clone(), member.record.clone())));
        let beats = (runtime.clone(), handle.clone(), Arc::clone(&current));
        node.tasks
            .push(runtime.spawn(task("heartbeat"), async move {
                heartbeats(&beats.0, &beats.1, &beats.2).await;
            })?);
        let renewal = Renewal {
            runtime: runtime.clone(),
            mesh: handle,
            data: paths.data.clone(),
            member,
            current,
        };
        node.tasks
            .push(runtime.spawn(task("renewal"), renewal.run())?);
        info!(node = %name, homes = homes.len(), "serving as a node");
        Ok(node)
    }

    /// The node's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stops the node's tasks and waits until each has ended, which closes
    /// its sockets.
    pub async fn stop(mut self) {
        for task in std::mem::take(&mut self.tasks) {
            task.cancel();
            // A cancelled task ends with an error; the outcome is not used.
            let _ = task.join().await;
        }
    }
}

impl Drop for MemberNode {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.cancel();
        }
    }
}

/// The node's heartbeats (ARFC 0006 clause 3.13), while its record has not
/// expired by its own clock. Runs until cancelled.
async fn heartbeats(runtime: &Runtime, mesh: &MeshHandle, current: &Current) {
    let clock = runtime.clock();
    let mut schedule = Schedule::new(clock.now());
    loop {
        clock.sleep_until(schedule.due()).await;
        let valid = current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .is_valid_at(clock.wall());
        if !valid {
            runtime.sleep(heartbeat::FIRST_FAILURE_DELAY).await;
            continue;
        }
        let started = clock.now();
        let addresses = match runtime.net().interfaces(None).await {
            Ok(list) => heartbeat::addresses(&list),
            Err(e) => {
                debug!(reason = %e, "the interfaces could not be read for a heartbeat");
                Vec::new()
            }
        };
        let status = heartbeat::status(&addresses, "");
        let ended = mesh
            .request(
                Body::NodeStatus(status),
                heartbeat::HEARTBEAT_LIMIT,
                Target::Control,
            )
            .await;
        let result = match &ended.outcome {
            Outcome::Reply(Body::NodeStatusReply(reply)) => HeartbeatResult::of(reply),
            _ => HeartbeatResult::Failed,
        };
        if result == HeartbeatResult::Refused {
            warn!("the control plane refused this node's heartbeat");
        }
        schedule.ended(result, started, clock.now());
    }
}

/// The node's renewals (section 3.5).
struct Renewal {
    runtime: Runtime,
    mesh: MeshHandle,
    data: DataDir,
    member: Member,
    current: Current,
}

impl Renewal {
    async fn run(mut self) {
        let clock = Arc::clone(self.runtime.clock());
        loop {
            let now = seconds(clock.wall());
            let due = self.member.record.renew_at();
            if now < due {
                let wait = Duration::from_secs(u64::try_from(due - now).unwrap_or(0));
                self.runtime.sleep(wait.min(RENEW_CHECK)).await;
                continue;
            }
            let signature = sign_renew(
                &self.member.identity,
                &self.member.cluster_id,
                &self.member.record.node_id,
                self.member.record.serial,
            );
            let renew = Body::Renew(Renew {
                node_id: self.member.record.node_id.as_bytes().to_vec(),
                serial: self.member.record.serial,
                signature: signature.to_bytes().to_vec(),
            });
            let ended = self.mesh.request(renew, RENEW_LIMIT, Target::Control).await;
            match ended.outcome {
                Outcome::Reply(Body::JoinReply(JoinReply {
                    result: Some(JoinResult::Admitted(admitted)),
                })) => {
                    let taken = admitted
                        .record
                        .as_ref()
                        .is_some_and(|r| self.take(r).is_some());
                    if taken {
                        self.write().await;
                    } else {
                        self.runtime.sleep(RENEW_RETRY).await;
                    }
                }
                Outcome::Reply(Body::JoinReply(JoinReply {
                    result: Some(JoinResult::Refusal(refused)),
                })) if refused.reason == refusal::NODE_UNKNOWN => {
                    error!(
                        "this machine is no longer a node of the cluster: run alloy node leave \
                         --force, then alloy join with a new token, to make it one again"
                    );
                    return;
                }
                _ => {
                    if !self.member.record.is_valid_at(clock.wall()) {
                        warn!("this node's record expired and could not be renewed");
                    }
                    self.runtime.sleep(RENEW_RETRY).await;
                }
            }
        }
    }

    /// Takes `record` as the node's record when it is the cluster's, names
    /// this node, and has a serial above its own.
    fn take(&mut self, record: &alloy_id::proto::SignedNodeRecord) -> Option<()> {
        let signed = SignedNodeRecord::from_proto(record).ok()?;
        let decoded = signed.verify(&self.member.cluster_pub).ok()?;
        if decoded.node_id != self.member.record.node_id
            || decoded.mesh_pub != self.member.record.mesh_pub
            || decoded.serial <= self.member.record.serial
        {
            return None;
        }
        info!(serial = decoded.serial, "this node's record was renewed");
        self.member.signed = signed.clone();
        self.member.record = decoded.clone();
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = (signed, decoded);
        Some(())
    }

    /// Writes the current record to the node file.
    async fn write(&self) {
        match self.data.read_node_state().await {
            Ok(Some(mut state)) => {
                state.record = Some(self.member.signed.to_proto());
                if let Err(e) = self.data.write_node_state(&state).await {
                    warn!(reason = %e, "the renewed record could not be written to the node file");
                }
            }
            Ok(None) => {}
            Err(e) => warn!(reason = %e, "the node file could not be read"),
        }
    }
}
