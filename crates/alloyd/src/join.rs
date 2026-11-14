//! `join`, the steps that make a machine a node of a cluster that `init`
//! made (ARFC 0002 sections 3.3, 3.11, 3.14, and 6), as M11 rung 2 keeps
//! them (section 3.15).
//!
//! [`join`] is the interface `alloy join` calls, in its own process, as
//! root, once its checks of section 3.8 pass and it has derived the name,
//! the advertise addresses, and the machine's facts (step 1). It writes the
//! join request, the token included, to the data directory, with the local
//! join state `joining` (step 2), then carries out steps 3 to 7 itself:
//! it opens an AL session from the mesh port to an anchor of the token as
//! an unregistered initiator, checks the anchor's reply to `join_hello`,
//! sends `join_request`, and stores the record it is issued, the local join
//! state then `joined`. The CLI then enables and starts `alloyd`, which
//! serves from the stored data ([`MemberNode`](crate::MemberNode)).
//!
//! [`resume`] is the same attempt from the request already stored, which
//! `alloyd` runs when it starts in local join state `joining` (section 6).
//!
//! A failed attempt leaves only the node keys, the handshake counter, and
//! the host file, with the local join state `unjoined`, so the next attempt
//! reuses the keys and its initiations are not dropped as replays.
//!
//! In rung 2 no reflection request is sent (section 3.15), no request is
//! `pending`, and every address is tried over UDP only (see
//! [`mesh`](crate::mesh)).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use alloy_crypto::Zeroizing;
use alloy_crypto::al::{PublicKey, StaticSecret};
use alloy_crypto::ed25519::{Signature, SigningKey, VerifyingKey};
use alloy_id::keys::{generate_ed25519, generate_x25519};
use alloy_id::proto::{BootstrapView, Facts, NodeSecrets, NodeState as StoredNode};
use alloy_id::store::{HOST_FILE, Opened};
use alloy_id::token::join_message;
use alloy_id::{
    ClusterId, ClusterKeyFp, IdError, JoinToken, NodeRecord, Roles, SignedNodeRecord, StoreError,
};
use alloy_rt::{NetError, Runtime, TaskName, UdpSocket};
use prost::Message;
use tokio::sync::mpsc;
use tracing::info;

use crate::channel::proto::{
    self, Body, ClusterProof, HelloResult, JoinHello, JoinHelloReply, JoinReply, JoinRequest,
    JoinRequestBody, JoinResult, refusal,
};
use crate::channel::requester::Outcome;
use crate::control::NodePaths;
use crate::data_dir::{JoinState, parse_host_file};
use crate::mesh::{Command, Inbound, Mesh, MeshHandle, Target, reserve_counter};

/// The longest steps 3 to 5 take over every address of every anchor in
/// the token, and the longest the joiner waits for the answer to step 6.
pub const STEP_LIMIT: Duration = Duration::from_secs(10);

/// The context of the cross signature of a cluster key rotation (section
/// 3.6).
const CROSS_CONTEXT: &[u8] = b"alloy/id/cluster-key/v1";

/// What `join` is told: what the CLI's flags and step 1 chose.
#[derive(Clone)]
pub struct JoinOptions {
    /// The join token, an `alyt_` string.
    pub token: Zeroizing<String>,
    /// The node's name: by the rule of section 3.3, or as `--name` gave it.
    pub name: String,
    /// Whether `name` is the default the hostname rule derived, so the
    /// control plane may add a suffix when it is in use.
    pub name_is_default: bool,
    /// The roles `--roles` named, `None` when the flag was not given. The
    /// control plane then takes the token's preset roles, or none on
    /// Windows, or `compute` and `storage`, without `compute` when the
    /// facts show no hardware virtualization.
    pub roles: Option<Roles>,
    /// The zone `--zone` named, `None` when the flag was not given.
    pub zone: Option<String>,
    /// The advertise addresses, 0 to 16.
    pub advertise: Vec<IpAddr>,
    /// The UDP port of the node's AL socket, which `join` binds.
    pub mesh_port: u16,
    /// The TCP port of the API, used only while the node holds `anchor` or
    /// `control`.
    pub api_port: u16,
    /// The machine's facts: `os` is `linux` or `windows`, `arch`, cores,
    /// memory, the virtualization check of section 3.8, and the disks.
    pub facts: Facts,
}

impl std::fmt::Debug for JoinOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinOptions")
            .field("name", &self.name)
            .field("name_is_default", &self.name_is_default)
            .field("roles", &self.roles)
            .field("zone", &self.zone)
            .field("advertise", &self.advertise)
            .field("mesh_port", &self.mesh_port)
            .field("api_port", &self.api_port)
            .finish_non_exhaustive()
    }
}

/// What a successful `join` made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Joined {
    /// The node's name, which the control plane may have given a suffix.
    pub node: String,
    /// Its roles, none for a client node.
    pub roles: Roles,
    /// Its zone.
    pub zone: String,
}

/// A refusal of section 3.3, which the control plane sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// An unknown token, or a signature that does not verify.
    TokenRefused,
    /// The node's identity or mesh key belongs to another node or request.
    KeyInUse,
    /// The token is revoked.
    TokenRevoked,
    /// The token is past its time limit, given in seconds since the Unix
    /// epoch.
    TokenExpired {
        /// When it expired.
        expire_time: i64,
    },
    /// The token is at its use limit.
    TokenUsedUp,
    /// The roles or zone differ from the token's preset.
    PresetConflict,
    /// The roles include `compute` and the facts show no hardware
    /// virtualization.
    ComputeUnsupported,
    /// A name given with `--name` is in use.
    NameTaken,
    /// 1,000 requests are pending.
    Busy,
    /// A cloud admin rejected the pending request.
    Rejected,
    /// The node is unknown, as a renewal's refusal says.
    NodeUnknown,
}

impl Refusal {
    fn of(reply: &proto::Refusal) -> Refusal {
        match reply.reason {
            refusal::KEY_IN_USE => Refusal::KeyInUse,
            refusal::TOKEN_REVOKED => Refusal::TokenRevoked,
            refusal::TOKEN_EXPIRED => Refusal::TokenExpired {
                expire_time: reply.expire_time,
            },
            refusal::TOKEN_USED_UP => Refusal::TokenUsedUp,
            refusal::PRESET_CONFLICT => Refusal::PresetConflict,
            refusal::COMPUTE_UNSUPPORTED => Refusal::ComputeUnsupported,
            refusal::NAME_TAKEN => Refusal::NameTaken,
            refusal::BUSY => Refusal::Busy,
            refusal::REJECTED => Refusal::Rejected,
            refusal::NODE_UNKNOWN => Refusal::NodeUnknown,
            _ => Refusal::TokenRefused,
        }
    }

    /// The refusal's name as section 3.3 spells it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Refusal::TokenRefused => "token_refused",
            Refusal::KeyInUse => "key_in_use",
            Refusal::TokenRevoked => "token_revoked",
            Refusal::TokenExpired { .. } => "token_expired",
            Refusal::TokenUsedUp => "token_used_up",
            Refusal::PresetConflict => "preset_conflict",
            Refusal::ComputeUnsupported => "compute_unsupported",
            Refusal::NameTaken => "name_taken",
            Refusal::Busy => "busy",
            Refusal::Rejected => "rejected",
            Refusal::NodeUnknown => "node_unknown",
        }
    }
}

/// Why `join` did not make the machine a node.
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    /// The local join state is `joined`, or `joining` with another token
    /// (section 3.3 step 1): nothing was changed.
    #[error(
        "this machine is already a node or joins with another token (its local join state is {0}), so join changed nothing"
    )]
    Conflict(JoinState),
    /// The token or an option is not valid: nothing was written or sent.
    #[error("{0}")]
    Usage(String),
    /// The cluster refused the joiner's release (section 3.3 step 5).
    #[error(
        "the cluster does not accept this release; it accepts {accepted}, and runs {highest_release}"
    )]
    Version {
        /// The accepted `MAJOR.MINOR` releases, comma separated.
        accepted: String,
        /// The highest release any node runs.
        highest_release: String,
    },
    /// No address of any anchor passed steps 3 to 5 within 10 seconds.
    #[error("no anchor of the token could be reached and checked within 10 seconds")]
    NoAnchor {
        /// Whether any address answered.
        answered: bool,
    },
    /// No answer to `join_request` arrived within 10 seconds.
    #[error("the anchor did not answer the join request within 10 seconds")]
    NoAnswer,
    /// The control plane refused the request.
    #[error("the cluster refused the join: {}", .0.name())]
    Refused(Refusal),
    /// The request waits for a cloud admin's approval. The local join
    /// state stays `joining`.
    #[error("the join request waits for a cloud admin's approval")]
    Pending,
    /// The reply did not verify: the record or view is not the cluster's.
    #[error("the anchor's reply does not verify: {0}")]
    Unverified(&'static str),
    /// A file of the data directory could not be read or written.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A key could not be generated.
    #[error(transparent)]
    Id(#[from] IdError),
    /// The mesh port could not be bound.
    #[error("the mesh port could not be bound: {0}")]
    Bind(NetError),
    /// The joiner's task did not start.
    #[error("the join could not start: {0}")]
    Spawn(#[from] alloy_rt::SpawnError),
}

impl JoinError {
    /// The exit status of `alloy join` for this error (section 3.3): 1 for
    /// `version` and for an address that answered but no address that
    /// passed, 2 for a usage error, `preset_conflict`, and
    /// `compute_unsupported`, 4 for the token refusals and `rejected`, 5
    /// for `key_in_use`, `name_taken`, and a conflict, 6 for no answer and
    /// `busy`, 0 for `pending`, and 1 for anything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            JoinError::Pending => 0,
            JoinError::Usage(_) => 2,
            JoinError::Conflict(_) => 5,
            JoinError::NoAnchor { answered: false } | JoinError::NoAnswer => 6,
            JoinError::Refused(refusal) => match refusal {
                Refusal::TokenRefused
                | Refusal::TokenRevoked
                | Refusal::TokenExpired { .. }
                | Refusal::TokenUsedUp
                | Refusal::Rejected
                | Refusal::NodeUnknown => 4,
                Refusal::KeyInUse | Refusal::NameTaken => 5,
                Refusal::PresetConflict | Refusal::ComputeUnsupported => 2,
                Refusal::Busy => 6,
            },
            JoinError::Version { .. }
            | JoinError::NoAnchor { answered: true }
            | JoinError::Unverified(_)
            | JoinError::Store(_)
            | JoinError::Id(_)
            | JoinError::Bind(_)
            | JoinError::Spawn(_) => 1,
        }
    }
}

/// The host file with join state `state` (ARFC 0017 section 4.2), before any
/// `alloyd` has started.
pub(crate) fn host_file(state: JoinState) -> String {
    format!("alloy-host 1\njoin {state}\nrelease none\n")
}

async fn read_state(paths: &NodePaths) -> Result<JoinState, JoinError> {
    match paths.data.read(HOST_FILE).await? {
        None => Ok(JoinState::Unjoined),
        Some(head) => parse_host_file(&head)
            .map_err(|e| JoinError::Usage(format!("the host file cannot be read: {e}"))),
    }
}

/// Runs `join` in the data directory at `paths`, on `runtime`, as the
/// module documentation says. The caller has run the checks of section
/// 3.8, among them that the data directory is an Alloy data directory or
/// was just created or adopted.
///
/// # Errors
///
/// [`JoinError::Usage`] for a token or option that is not valid and
/// [`JoinError::Conflict`], each before anything is written; then any
/// other [`JoinError`], after a failed attempt has left only what section
/// 6 keeps. [`JoinError::exit_code`] gives the CLI's exit status.
pub async fn join(
    runtime: &Runtime,
    paths: &NodePaths,
    options: &JoinOptions,
) -> Result<Joined, JoinError> {
    let token = JoinToken::decode(&options.token)
        .map_err(|e| JoinError::Usage(format!("the join token is not valid: {e}")))?;
    check_options(options, &token)?;
    match read_state(paths).await? {
        JoinState::Joined => return Err(JoinError::Conflict(JoinState::Joined)),
        JoinState::Joining => {
            let opened = paths.data.open(runtime.rng().as_ref()).await?;
            let same = opened
                .as_ref()
                .is_some_and(|o| o.secrets.join_token.as_str() == options.token.as_str());
            if !same {
                return Err(JoinError::Conflict(JoinState::Joining));
            }
        }
        JoinState::Unjoined => prepare(runtime, paths, &token, options).await?,
    }
    let sockets = bind(runtime, options.mesh_port)?;
    attempt(runtime, paths, sockets).await
}

/// Resumes the attempt that the data directory holds, in local join state
/// `joining`, at section 3.3 step 3 with the same keys and request
/// (section 6). `alloyd` calls it when it starts in that state.
///
/// # Errors
///
/// [`JoinError::Conflict`] when the local join state is not `joining`, and
/// as [`join`].
pub async fn resume(runtime: &Runtime, paths: &NodePaths) -> Result<Joined, JoinError> {
    let state = read_state(paths).await?;
    if state != JoinState::Joining {
        return Err(JoinError::Conflict(state));
    }
    let opened = paths
        .data
        .open(runtime.rng().as_ref())
        .await?
        .ok_or(StoreError::Damaged("a joining node holds no at-rest key"))?;
    let request = opened
        .secrets
        .join_request
        .as_ref()
        .ok_or(StoreError::Damaged("a joining node holds no join request"))?;
    let body = JoinRequestBody::decode(request.body.as_slice())
        .map_err(|_| StoreError::Damaged("the stored join request does not decode"))?;
    let port = u16::try_from(body.mesh_port)
        .map_err(|_| StoreError::Damaged("the stored join request names no mesh port"))?;
    let sockets = bind(runtime, port)?;
    attempt(runtime, paths, sockets).await
}

fn check_options(options: &JoinOptions, token: &JoinToken) -> Result<(), JoinError> {
    alloy_api::NodeName::new(&options.name).map_err(|rule| {
        JoinError::Usage(format!(
            "the node name `{}` {rule}",
            options.name.escape_debug()
        ))
    })?;
    if options.advertise.len() > alloy_id::record::MAX_ADVERTISE {
        return Err(JoinError::Usage(format!(
            "join takes at most {} advertise addresses, and {} were given",
            alloy_id::record::MAX_ADVERTISE,
            options.advertise.len()
        )));
    }
    if options
        .zone
        .as_ref()
        .is_some_and(|z| z.is_empty() || z.len() > alloy_id::record::MAX_NAME_LEN)
    {
        return Err(JoinError::Usage("a zone is 1 to 63 bytes".to_owned()));
    }
    if let Some(roles) = options.roles
        && !token.preset_roles.is_empty()
        && roles != token.preset_roles
    {
        return Err(JoinError::Usage(
            "--roles differs from the roles the join token presets".to_owned(),
        ));
    }
    if options
        .roles
        .is_some_and(|r| r.contains(alloy_id::Role::Compute))
        && !options.facts.virtualization
    {
        return Err(JoinError::Usage(
            "--roles names compute, and this machine's virtualization check is warn, so it \
             holds compute only once a cloud admin dedicates it to one organization"
                .to_owned(),
        ));
    }
    if options.facts.os == crate::nodes::WINDOWS_OS
        && (options.roles.is_some_and(|r| !r.is_empty()) || !token.preset_roles.is_empty())
    {
        return Err(JoinError::Usage(
            "on Windows every node is a client node, which holds no role".to_owned(),
        ));
    }
    Ok(())
}

fn bind(runtime: &Runtime, port: u16) -> Result<Vec<Box<dyn UdpSocket>>, JoinError> {
    crate::mesh::bind(runtime, port).map_err(JoinError::Bind)
}

/// Step 2: generates the node keys, or reuses those an earlier failed
/// attempt kept, and writes the request, the token included, with the
/// local join state `joining`.
async fn prepare(
    runtime: &Runtime,
    paths: &NodePaths,
    token: &JoinToken,
    options: &JoinOptions,
) -> Result<(), JoinError> {
    let rng = runtime.rng().as_ref();
    paths
        .data
        .write(HOST_FILE, host_file(JoinState::Unjoined).as_bytes())
        .await?;
    let opened = paths.data.open(rng).await?;
    let kept = opened.as_ref().and_then(|o| {
        let identity: [u8; 32] = o.secrets.identity_key.as_slice().try_into().ok()?;
        let mesh: [u8; 32] = o.secrets.mesh_key.as_slice().try_into().ok()?;
        Some((
            SigningKey::from_seed(&identity),
            StaticSecret::from_bytes(mesh),
        ))
    });
    let (identity, mesh) = match kept {
        Some(keys) => keys,
        None => (generate_ed25519(rng)?, generate_x25519(rng)?),
    };
    let body = JoinRequestBody {
        token_id: token.token_id.0.to_vec(),
        identity_pub: identity.verifying_key().as_bytes().to_vec(),
        name: options.name.clone(),
        name_is_default: options.name_is_default,
        roles: options
            .roles
            .map(|roles| roles.iter().map(|r| r as i32).collect())
            .unwrap_or_default(),
        zone: options.zone.clone().unwrap_or_default(),
        advertise: options
            .advertise
            .iter()
            .map(|ip| match ip {
                IpAddr::V4(v4) => v4.octets().to_vec(),
                IpAddr::V6(v6) => v6.octets().to_vec(),
            })
            .collect(),
        mesh_port: u32::from(options.mesh_port),
        api_port: u32::from(options.api_port),
        facts: Some(options.facts.clone()),
    }
    .encode_to_vec();
    let message = join_message(&token.cluster_id, &mesh.public_key(), &body);
    let request = alloy_id::proto::JoinRequest {
        token_signature: token.secret.token_key().sign(&message).to_bytes().to_vec(),
        identity_signature: identity.sign(&message).to_bytes().to_vec(),
        body,
    };
    if proto::decode(&proto::encode(&proto::NodeMessage {
        request_id: 1,
        body: Some(Body::JoinRequest(channel_request(&request))),
    }))
    .is_err()
    {
        return Err(JoinError::Usage(
            "the join request is outside the bounds of a node message".to_owned(),
        ));
    }
    let mut secrets = NodeSecrets::default();
    secrets.identity_key = identity.seed().to_vec();
    secrets.mesh_key = mesh.to_bytes().to_vec();
    secrets.join_token = options.token.as_str().to_owned();
    secrets.join_request = Some(request);
    match &opened {
        Some(o) => paths.data.write_secrets(rng, &o.key, &secrets).await?,
        None => {
            paths.data.create_secrets(rng, &secrets).await?;
        }
    }
    if paths.data.read_handshake_counter().await?.is_none() {
        paths.data.write_handshake_counter(0).await?;
    }
    paths
        .data
        .write_node_state(&StoredNode {
            cluster_id: token.cluster_id.as_bytes().to_vec(),
            ..StoredNode::default()
        })
        .await?;
    paths
        .data
        .write(HOST_FILE, host_file(JoinState::Joining).as_bytes())
        .await?;
    Ok(())
}

fn channel_request(request: &alloy_id::proto::JoinRequest) -> JoinRequest {
    JoinRequest {
        body: request.body.clone(),
        token_signature: request.token_signature.clone(),
        identity_signature: request.identity_signature.clone(),
    }
}

/// What step 7 stores.
struct Issued {
    signed: SignedNodeRecord,
    record: NodeRecord,
    view: BootstrapView,
    cluster_pub: VerifyingKey,
}

/// Steps 3 to 7 from the stored request, over `sockets`.
async fn attempt(
    runtime: &Runtime,
    paths: &NodePaths,
    sockets: Vec<Box<dyn UdpSocket>>,
) -> Result<Joined, JoinError> {
    let rng = runtime.rng().as_ref();
    let opened = paths
        .data
        .open(rng)
        .await?
        .ok_or(StoreError::Damaged("a joining node holds no at-rest key"))?;
    let token = JoinToken::decode(&opened.secrets.join_token)
        .map_err(|_| StoreError::Damaged("the stored join token does not decode"))?;
    let request = opened
        .secrets
        .join_request
        .as_ref()
        .map(channel_request)
        .ok_or(StoreError::Damaged("a joining node holds no join request"))?;
    let mesh_key: [u8; 32] = opened
        .secrets
        .mesh_key
        .as_slice()
        .try_into()
        .map_err(|_| StoreError::Damaged("the stored mesh key is not 32 bytes"))?;
    let mesh_key = StaticSecret::from_bytes(mesh_key);
    let own_mesh = mesh_key.public_key();
    let counter = reserve_counter(&paths.data).await?;
    let (mesh, inbound) = Mesh::new(
        runtime,
        mesh_key,
        counter,
        paths.data.clone(),
        token.cluster_id,
        false,
    );
    let (handle, commands) = MeshHandle::channel();
    // A short ASCII literal, which `TaskName::new` accepts.
    let name = TaskName::new("join-mesh").expect("a short literal task name is valid");
    let task = runtime.spawn(name, mesh.run(sockets, commands))?;
    let outcome = exchange(runtime, &handle, inbound, &token, request, own_mesh).await;
    task.cancel();
    // A cancelled task ends with an error; the outcome is not used.
    let _ = task.join().await;
    match outcome {
        Ok(issued) => store_joined(runtime, paths, &opened, &token.cluster_id, issued).await,
        Err(JoinError::Pending) => {
            let mut state = paths.data.read_node_state().await?.unwrap_or_default();
            state.pending = true;
            paths.data.write_node_state(&state).await?;
            Err(JoinError::Pending)
        }
        Err(error) => {
            paths.data.keep_only_node_keys(rng, &opened).await?;
            paths
                .data
                .write(HOST_FILE, host_file(JoinState::Unjoined).as_bytes())
                .await?;
            Err(error)
        }
    }
}

/// Waits until a session with `anchor` is made, at most `limit`.
pub(crate) async fn wait_session(
    runtime: &Runtime,
    inbound: &mut mpsc::Receiver<Inbound>,
    anchor: &PublicKey,
    limit: Duration,
) -> bool {
    let clock = runtime.clock();
    let deadline = clock.now().saturating_add(limit);
    loop {
        let left = deadline.saturating_duration_since(clock.now());
        if left.is_zero() {
            return false;
        }
        match runtime.timeout(left, inbound.recv()).await {
            Ok(Some(Inbound::SessionMade { peer })) if peer == *anchor => return true,
            Ok(Some(Inbound::Abandoned { peer })) if peer == *anchor => return false,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return false,
        }
    }
}

/// Section 3.3 step 5: the cluster key's fingerprint is `fp`, the token's
/// or that of the key a node holds, over the current key or a previous one
/// it cross signed, and the anchor's record is a record of `cluster_id`
/// signed by the current key, unexpired, and names the key the session
/// authenticated. The current key, when every check passes.
pub(crate) fn check_proof(
    proof: &ClusterProof,
    cluster_id: &ClusterId,
    fp: ClusterKeyFp,
    anchor: &PublicKey,
    now: alloy_rt::WallTime,
) -> Option<VerifyingKey> {
    let current = VerifyingKey::from_bytes(&proof.cluster_pub.as_slice().try_into().ok()?).ok()?;
    let fp_ok = if proof.previous_cluster_pub.is_empty() {
        fp == ClusterKeyFp::of(&current)
    } else {
        let previous =
            VerifyingKey::from_bytes(&proof.previous_cluster_pub.as_slice().try_into().ok()?)
                .ok()?;
        let signature = Signature::from_bytes(proof.cross_signature.as_slice().try_into().ok()?);
        let mut message = CROSS_CONTEXT.to_vec();
        message.extend_from_slice(cluster_id.as_bytes());
        message.extend_from_slice(current.as_bytes());
        let cross_ok = previous.verify(&message, &signature).is_ok();
        fp == ClusterKeyFp::of(&current) || (cross_ok && fp == ClusterKeyFp::of(&previous))
    };
    if !fp_ok {
        return None;
    }
    let signed = SignedNodeRecord::from_proto(proof.anchor_record.as_ref()?).ok()?;
    let record = signed.accept(cluster_id, &current, now).ok()?;
    record.mesh_pub.contains(anchor).then_some(current)
}

/// Steps 3 to 7 on the mesh.
async fn exchange(
    runtime: &Runtime,
    mesh: &MeshHandle,
    mut inbound: mpsc::Receiver<Inbound>,
    token: &JoinToken,
    request: JoinRequest,
    own_mesh: PublicKey,
) -> Result<Issued, JoinError> {
    let clock = Arc::clone(runtime.clock());
    let deadline = clock.now().saturating_add(STEP_LIMIT);
    let addresses: Vec<(PublicKey, SocketAddr)> = token
        .anchors
        .iter()
        .flat_map(|anchor| {
            anchor
                .addresses
                .iter()
                .map(|a| (anchor.mesh_pub, SocketAddr::new(a.ip, a.mesh_port)))
        })
        .collect();
    let mut answered = false;
    let mut chosen = None;
    for (i, (anchor, addr)) in addresses.iter().enumerate() {
        let left = deadline.saturating_duration_since(clock.now());
        if left.is_zero() {
            break;
        }
        let share = left / u32::try_from(addresses.len() - i).unwrap_or(u32::MAX);
        mesh.send(Command::Dial {
            peer: *anchor,
            addr: *addr,
        })
        .await;
        if !wait_session(runtime, &mut inbound, anchor, share).await {
            mesh.send(Command::Forget { peer: *anchor }).await;
            continue;
        }
        answered = true;
        let left = deadline.saturating_duration_since(clock.now());
        let hello = Body::JoinHello(JoinHello {
            release: crate::VERSION.to_owned(),
        });
        let ended = mesh.request(hello, left, Target::Peer(*anchor)).await;
        let proof = match ended.outcome {
            Outcome::Reply(Body::JoinHelloReply(JoinHelloReply {
                result: Some(HelloResult::Version(refused)),
            })) => {
                return Err(JoinError::Version {
                    accepted: refused.accepted_minors.join(", "),
                    highest_release: refused.highest_release,
                });
            }
            Outcome::Reply(Body::JoinHelloReply(JoinHelloReply {
                result: Some(HelloResult::Cluster(proof)),
            })) => proof,
            _ => {
                mesh.send(Command::Forget { peer: *anchor }).await;
                continue;
            }
        };
        match check_proof(
            &proof,
            &token.cluster_id,
            token.cluster_key_fp,
            anchor,
            clock.wall(),
        ) {
            Some(cluster_pub) => {
                chosen = Some((*anchor, cluster_pub));
                break;
            }
            None => {
                info!(%addr, "an anchor's reply failed the cluster key check");
                mesh.send(Command::Forget { peer: *anchor }).await;
            }
        }
    }
    let Some((anchor, cluster_pub)) = chosen else {
        return Err(JoinError::NoAnchor { answered });
    };
    let identity = JoinRequestBody::decode(request.body.as_slice())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b.identity_pub.as_slice()).ok())
        .ok_or(StoreError::Damaged(
            "the stored join request does not decode",
        ))?;
    let ended = mesh
        .request(Body::JoinRequest(request), STEP_LIMIT, Target::Peer(anchor))
        .await;
    let reply = match ended.outcome {
        Outcome::Reply(Body::JoinReply(JoinReply {
            result: Some(result),
        })) => result,
        _ => return Err(JoinError::NoAnswer),
    };
    let admitted = match reply {
        JoinResult::Admitted(admitted) => admitted,
        JoinResult::Pending(_) => return Err(JoinError::Pending),
        JoinResult::Refusal(refused) => return Err(JoinError::Refused(Refusal::of(&refused))),
    };
    let signed = admitted
        .record
        .as_ref()
        .and_then(|r| SignedNodeRecord::from_proto(r).ok())
        .ok_or(JoinError::Unverified("the record"))?;
    let record = signed
        .accept(&token.cluster_id, &cluster_pub, clock.wall())
        .map_err(|_| JoinError::Unverified("the record is not the cluster's or expired"))?;
    if record.mesh_pub.first() != Some(&own_mesh) || record.identity_pub.as_bytes() != &identity {
        return Err(JoinError::Unverified("the record names other keys"));
    }
    let view = admitted.view.ok_or(JoinError::Unverified("the view"))?;
    if view.cluster_pub.as_slice() != cluster_pub.as_bytes()
        || view.nodes.iter().any(|n| {
            SignedNodeRecord::from_proto(n)
                .and_then(|s| s.verify(&cluster_pub))
                .is_err()
        })
    {
        return Err(JoinError::Unverified("the view is not the cluster's"));
    }
    Ok(Issued {
        signed,
        record,
        view,
        cluster_pub,
    })
}

/// Step 7: stores the record and the view, clears the token and the
/// request, and makes the local join state `joined`.
async fn store_joined(
    runtime: &Runtime,
    paths: &NodePaths,
    opened: &Opened,
    cluster_id: &ClusterId,
    issued: Issued,
) -> Result<Joined, JoinError> {
    paths
        .data
        .write_node_state(&StoredNode {
            cluster_id: cluster_id.as_bytes().to_vec(),
            cluster_pub: vec![issued.cluster_pub.as_bytes().to_vec()],
            record: Some(issued.signed.to_proto()),
            view: Some(issued.view),
            ..StoredNode::default()
        })
        .await?;
    let mut kept = NodeSecrets::default();
    kept.identity_key.clone_from(&opened.secrets.identity_key);
    kept.mesh_key.clone_from(&opened.secrets.mesh_key);
    paths
        .data
        .write_secrets(runtime.rng().as_ref(), &opened.key, &kept)
        .await?;
    paths
        .data
        .write(HOST_FILE, host_file(JoinState::Joined).as_bytes())
        .await?;
    info!(node = %issued.record.name, "this machine joined the cluster");
    Ok(Joined {
        node: issued.record.name.clone(),
        roles: issued.record.roles,
        zone: issued.record.zone.clone(),
    })
}
