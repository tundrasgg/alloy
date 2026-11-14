//! A control node of M11 rung 2, serving (ARFC 0002 sections 3.1 and 3.15,
//! ARFC 0006 sections 2.7, 2.8, and 3.13 to 3.15).
//!
//! [`ControlNode::start`] reads what `init` stored in the data directory,
//! opens the global group on its `native` stores, and serves the API port
//! and the local admin socket over that group. It makes the node's own
//! heartbeat in its own process, on the schedule of ARFC 0006 clause 3.13,
//! with no node message, so the node shows `active` (clause 3.15).

use std::future::poll_fn;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::task::Poll;

use alloy_api::tls::{Protocol, ServerCert, server_config};
use alloy_api::{AdminLimits, ApiPort, ApiServer, Transport};
use alloy_crypto::aead::AeadKey;
use alloy_crypto::al::StaticSecret;
use alloy_crypto::ecdsa::SigningKey as P256Key;
use alloy_crypto::ed25519::{SigningKey, VerifyingKey};
use alloy_crypto::tls::ApiCaFingerprint;
use alloy_id::proto::NodeState as StoredNode;
use alloy_id::{DataDir, NodeRecord, Role, SignedNodeRecord, StoreError};
use alloy_rt::{
    BoxFuture, JoinHandle, Listener, NetError, Protection, RtPath, Runtime, Stream, StreamAddr,
    TaskName, UdpSocket,
};
use alloy_seam::{ImplName, RegistryBuilder, SeamConfig};
use alloy_state::{
    Group, GroupError, Heartbeat, LogStoreSeam, NativeStoreConfig, NodeId as MemberId,
    StateStoreSeam, StatusReply,
};
use tracing::{debug, warn};

use crate::api::{ControlPlane, GroupSlot, Plane};
use crate::channel::heartbeat::{self, HeartbeatResult, Schedule};
use crate::mesh::{Mesh, MeshHandle, reserve_counter};
use crate::nodes::{NodeService, Own};

/// Why a control node could not start or stop.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// A file of the data directory could not be read or written, or is
    /// damaged.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The data directory holds no node of a cluster.
    #[error("the data directory holds no node of a cluster: {0}")]
    NotJoined(&'static str),
    /// The stored record or keys do not decode or do not verify.
    #[error("the stored node is damaged: {0}")]
    Damaged(String),
    /// The node does not hold the `control` role, so it serves no API.
    #[error("the node does not hold the control role")]
    NotControl,
    /// The global group did not open, or could not be written.
    #[error("the global group: {0}")]
    Group(#[from] GroupError),
    /// A store of the global group could not be built.
    #[error("the stores of the global group could not be built: {0}")]
    Stores(String),
    /// A task did not start.
    #[error("a task of the control node did not start: {0}")]
    Spawn(#[from] alloy_rt::SpawnError),
    /// A listener could not be opened.
    #[error("a listener could not be opened: {0}")]
    Listen(#[from] NetError),
}

/// Where a node's data directory is: as the `disk-io` seam names it, and as
/// the operating system does, for the stores that open their files
/// themselves (ARFC 0006 section 2.7).
#[derive(Clone, Debug)]
pub struct NodePaths {
    /// The data directory on the `disk-io` seam.
    pub data: DataDir,
    /// The data directory, as the `disk-io` seam names it.
    pub dir: RtPath,
    /// The data directory's native path.
    pub native: PathBuf,
}

impl NodePaths {
    fn state_dir(&self) -> Result<(RtPath, RtPath), StoreError> {
        let state = self.dir.join("state")?;
        let global = state.join("global")?;
        Ok((state, global))
    }

    /// The native path of the global group's stores.
    fn global_native(&self) -> PathBuf {
        self.native.join("state").join("global")
    }
}

/// Creates `<data-dir>/state/global`, each directory protected as the data
/// directory's own files (ARFC 0006 section 2.7). An existing one is kept.
pub(crate) async fn create_state_dir(
    runtime: &Runtime,
    paths: &NodePaths,
) -> Result<(), NodeError> {
    let (state, global) = paths.state_dir()?;
    for dir in [&state, &global] {
        match runtime
            .disk_io()
            .create_dir_protected(dir, Protection::Private)
            .await
        {
            Ok(()) | Err(alloy_rt::DiskError::AlreadyExists) => {}
            Err(e) => return Err(StoreError::from(e).into()),
        }
    }
    runtime
        .disk_io()
        .sync_dir(&paths.dir)
        .await
        .map_err(StoreError::from)?;
    runtime
        .disk_io()
        .sync_dir(&state)
        .await
        .map_err(StoreError::from)?;
    Ok(())
}

/// Opens the global group on member `me` over the `native` stores in the
/// data directory (ARFC 0006 section 2.7).
pub(crate) async fn open_group(
    runtime: &Runtime,
    paths: &NodePaths,
    me: MemberId,
) -> Result<Group, NodeError> {
    let stores = |e: &dyn std::fmt::Display| NodeError::Stores(e.to_string());
    let mut builder = RegistryBuilder::new();
    alloy_state::register_native(&mut builder).map_err(|e| stores(&e))?;
    let registry = builder.build();
    let dir = native_text(&paths.global_native())?;
    let config = NativeStoreConfig { dir }
        .to_value(1)
        .map_err(|e| stores(&e))?;
    let native = ImplName::new("native").map_err(|e| stores(&e))?;
    let log = registry
        .instantiate::<LogStoreSeam>(runtime, &native, &config)
        .map_err(|e| stores(&e))?;
    let state = registry
        .instantiate::<StateStoreSeam>(runtime, &native, &config)
        .map_err(|e| stores(&e))?;
    Ok(Group::open(runtime, me, log, state).await?)
}

fn native_text(path: &Path) -> Result<String, NodeError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| NodeError::Damaged("the data directory's path is not UTF-8".to_owned()))
}

/// The listeners a control node serves.
#[derive(Debug)]
pub struct Listeners {
    /// The API port: TCP, every local address (ARFC 0008 clause 3.8).
    pub api: Box<dyn Listener>,
    /// The local admin socket (ARFC 0007 clause 3.10.2), when the platform
    /// has one.
    pub admin: Option<Box<dyn Listener>>,
    /// The mesh port: UDP, one socket per address family, on which the
    /// node answers its join service and its nodes' node messages.
    pub mesh: Vec<Box<dyn UdpSocket>>,
}

/// What a control node holds of its cluster, read from its data directory.
struct Stored {
    signed: SignedNodeRecord,
    record: NodeRecord,
    cluster_key: SigningKey,
    mesh_key: StaticSecret,
    cluster_id: [u8; 16],
    at_rest: AeadKey,
    server: ServerCert,
    api_ca_fp: [u8; 16],
}

async fn read_stored(runtime: &Runtime, paths: &NodePaths) -> Result<Stored, NodeError> {
    let opened = paths
        .data
        .open(runtime.rng().as_ref())
        .await?
        .ok_or(NodeError::NotJoined("it holds no at-rest key"))?;
    let state: StoredNode = paths
        .data
        .read_node_state()
        .await?
        .ok_or(NodeError::NotJoined("it holds no node file"))?;
    let damaged = |what: &str| NodeError::Damaged(what.to_owned());
    let cluster_pub: [u8; 32] = state
        .cluster_pub
        .first()
        .and_then(|k| k.as_slice().try_into().ok())
        .ok_or_else(|| damaged("the node file holds no cluster public key"))?;
    let cluster_pub =
        VerifyingKey::from_bytes(&cluster_pub).map_err(|_| damaged("the cluster key"))?;
    let signed = state
        .record
        .as_ref()
        .ok_or(NodeError::NotJoined("the node file holds no record"))?;
    let signed = SignedNodeRecord::from_proto(signed)
        .map_err(|e| NodeError::Damaged(format!("the node record: {e}")))?;
    let record = signed
        .verify(&cluster_pub)
        .map_err(|e| NodeError::Damaged(format!("the node record: {e}")))?;
    // Only a control node holds the cluster key, so the role is checked
    // before it is read: any other joined node is served as a member node.
    if !record.roles.contains(Role::Control) {
        return Err(NodeError::NotControl);
    }
    let cluster_key = <[u8; 32]>::try_from(opened.secrets.cluster_key.as_slice())
        .map(|seed| SigningKey::from_seed(&seed))
        .map_err(|_| damaged("the cluster key"))?;
    if cluster_key.verifying_key().as_bytes() != cluster_pub.as_bytes() {
        return Err(damaged("the cluster key is not the cluster's"));
    }
    let mesh_key = <[u8; 32]>::try_from(opened.secrets.mesh_key.as_slice())
        .map(StaticSecret::from_bytes)
        .map_err(|_| damaged("the mesh key"))?;
    let cluster_id: [u8; 16] = state
        .cluster_id
        .as_slice()
        .try_into()
        .map_err(|_| damaged("the cluster id"))?;
    let key = P256Key::from_pkcs8_der(&opened.secrets.api_server_key)
        .map_err(|_| damaged("the API server key"))?;
    let server = ServerCert::new(
        vec![
            state.api_server_certificate.clone(),
            state.api_ca_certificate.clone(),
        ],
        key,
    )
    .map_err(|e| NodeError::Damaged(format!("the API server certificate: {e}")))?;
    let api_ca_fp = ApiCaFingerprint::of_certificate(&state.api_ca_certificate)
        .ok_or_else(|| damaged("the API CA certificate"))?
        .0;
    Ok(Stored {
        signed,
        record,
        cluster_key,
        mesh_key,
        cluster_id,
        at_rest: AeadKey::from_bytes(opened.key.as_bytes()),
        server,
        api_ca_fp,
    })
}

/// A serving control node. Dropping it stops its tasks; [`ControlNode::stop`]
/// also releases the global group's stores.
pub struct ControlNode {
    runtime: Runtime,
    plane: Arc<ControlPlane>,
    name: String,
    tasks: Vec<JoinHandle<()>>,
}

/// How long [`ControlNode::stop`] waits for the calls in progress to end
/// before it shuts the group down.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

impl std::fmt::Debug for ControlNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlNode")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

fn task(name: &'static str) -> TaskName {
    // Every caller passes a short ASCII literal, which `TaskName::new` accepts.
    TaskName::new(name).expect("a short literal task name is valid")
}

impl ControlNode {
    /// Starts the control node of the data directory at `paths`, serving
    /// the listeners `listen` opens for its record on `transport` (ARFC
    /// 0008 clause 3.8.1).
    ///
    /// # Errors
    ///
    /// [`NodeError`] when the stored node cannot be read, does not hold
    /// `control`, or its group does not open.
    pub async fn start<L>(
        runtime: &Runtime,
        paths: &NodePaths,
        listen: L,
        transport: Transport,
    ) -> Result<ControlNode, NodeError>
    where
        L: FnOnce(&NodeRecord) -> Result<Listeners, NodeError>,
    {
        let stored = read_stored(runtime, paths).await?;
        let listeners = listen(&stored.record)?;
        let me = MemberId(*stored.record.node_id.as_bytes());
        let group = Arc::new(open_group(runtime, paths, me).await?);
        let name = stored.record.name.clone();
        let endpoints = stored
            .record
            .advertise
            .first()
            .map(|ip| SocketAddr::new(*ip, stored.record.api_port))
            .into_iter()
            .collect();
        let control = Arc::new(ControlPlane {
            group: GroupSlot::new(Arc::clone(&group)),
            node: name.clone(),
            at_rest: stored.at_rest,
            cluster_id: stored.cluster_id,
            api_ca_fp: stored.api_ca_fp,
            endpoints,
        });
        let plane = Plane(Arc::clone(&control));
        let server = ApiServer::new(
            runtime.clone(),
            Arc::new(plane.clone()),
            Arc::new(plane.clone()),
            Arc::new(plane),
        );
        let mut node = ControlNode {
            runtime: runtime.clone(),
            plane: control,
            name: name.clone(),
            tasks: Vec::new(),
        };
        let tls = server_config(Arc::clone(runtime.clock()), &stored.server, Protocol::Api)
            .map_err(|e| NodeError::Damaged(format!("the API server certificate: {e}")))?;
        let port = ApiPort::new(runtime.clone(), transport).with_api(server.clone(), tls);
        let api = listeners.api;
        node.tasks
            .push(runtime.spawn(task("api-port"), async move { port.serve(api).await })?);
        if let Some(admin) = listeners.admin {
            node.tasks
                .push(runtime.spawn(task("local-admin"), async move {
                    server
                        .serve_local_admin(admin, AdminLimits::LOCAL_ADMIN)
                        .await;
                })?);
        }
        let counter = reserve_counter(&paths.data).await?;
        let cluster_id = alloy_id::ClusterId::from_bytes(stored.cluster_id);
        let (mesh, inbound) = Mesh::new(
            runtime,
            stored.mesh_key,
            counter,
            paths.data.clone(),
            cluster_id,
            true,
        );
        let (handle, commands) = MeshHandle::channel();
        node.tasks
            .push(runtime.spawn(task("control-mesh"), mesh.run(listeners.mesh, commands))?);
        let service = NodeService::new(
            runtime,
            Arc::clone(&group),
            paths.data.clone(),
            cluster_id,
            stored.cluster_key,
            Own {
                name: name.clone(),
                signed: stored.signed,
                record: stored.record,
            },
            handle,
        );
        node.tasks
            .push(runtime.spawn(task("node-channel"), service.serve(inbound))?);
        let beat = runtime.clone();
        node.tasks
            .push(runtime.spawn(task("heartbeat"), async move {
                own_heartbeats(&beat, &group, &name).await;
            })?);
        Ok(node)
    }

    /// The node's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stops serving and shuts the global group down, releasing its stores.
    ///
    /// # Errors
    ///
    /// [`NodeError::Group`] when the group does not shut down cleanly.
    pub async fn stop(mut self) -> Result<(), NodeError> {
        let tasks = std::mem::take(&mut self.tasks);
        let runtime = self.runtime.clone();
        // From here on a call on a connection still open fails with
        // `Unavailable` and holds no handle on the group.
        let taken = self.plane.group.take();
        drop(self);
        for task in tasks {
            task.cancel();
            // A cancelled task ends with an error; the outcome is not used.
            let _ = task.join().await;
        }
        let Some(mut group) = taken else {
            return Ok(());
        };
        // The calls that took the group before it was taken end within
        // their own time limits.
        let deadline = runtime.clock().now().saturating_add(STOP_GRACE);
        loop {
            match Arc::try_unwrap(group) {
                Ok(group) => return Ok(group.shutdown().await?),
                Err(shared) if runtime.clock().now() < deadline => {
                    group = shared;
                    runtime.sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(_) => {
                    return Err(NodeError::Damaged(
                        "calls still held the group when the stop's grace ended".to_owned(),
                    ));
                }
            }
        }
    }
}

impl Drop for ControlNode {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.cancel();
        }
    }
}

/// The node's own heartbeats (ARFC 0006 clause 3.13): a node that holds
/// `control` makes them in its own process, on the same schedule, and the
/// group accepts them as the leader does (clause 3.14). Runs until
/// cancelled.
async fn own_heartbeats(runtime: &Runtime, group: &Group, name: &str) {
    let clock = runtime.clock();
    let mut schedule = Schedule::new(clock.now());
    loop {
        clock.sleep_until(schedule.due()).await;
        let started = clock.now();
        let addresses = match runtime.net().interfaces(None).await {
            Ok(list) => heartbeat::addresses(&list),
            Err(e) => {
                debug!(reason = %e, "the interfaces could not be read for a heartbeat");
                Vec::new()
            }
        };
        let status = heartbeat::status(&addresses, "");
        let beat = Heartbeat {
            addresses: status.addresses,
            release: String::new(),
        };
        let reply = runtime
            .timeout(
                heartbeat::HEARTBEAT_LIMIT,
                group.accept_heartbeat(name, &beat),
            )
            .await;
        let result = match reply {
            Ok(StatusReply::Ok) => HeartbeatResult::Ok,
            Ok(StatusReply::Refused | StatusReply::ReleaseRefused { .. }) => {
                warn!(
                    node = name,
                    "the control plane refused this node's own heartbeat"
                );
                HeartbeatResult::Refused
            }
            Ok(StatusReply::Unavailable | StatusReply::Invalid) | Err(_) => HeartbeatResult::Failed,
        };
        schedule.ended(result, started, clock.now());
    }
}

/// Two listeners served as one: the API port on IPv4 and on IPv6, which the
/// `os` `net` binds apart (ARFC 0008 clause 3.8: every local address, IPv4
/// and IPv6).
#[derive(Debug)]
pub struct Both(pub Box<dyn Listener>, pub Box<dyn Listener>);

impl Listener for Both {
    fn local_addr(&self) -> StreamAddr {
        self.0.local_addr()
    }

    fn accept(&self) -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>> {
        Box::pin(async move {
            let mut first = self.0.accept();
            let mut second = self.1.accept();
            poll_fn(|cx| {
                if let Poll::Ready(stream) = first.as_mut().poll(cx) {
                    return Poll::Ready(stream);
                }
                second.as_mut().poll(cx)
            })
            .await
        })
    }
}
