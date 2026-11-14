//! A control node's side of the node channel and the node lifecycle
//! controller (ARFC 0002 sections 3.3, 3.5, 3.13, and 3.15, ARFC 0006
//! clauses 3.13 to 3.16), as M11 rung 2 builds them.
//!
//! [`NodeService`] is the one task that answers the requests a control
//! node's mesh passes it: the join service (`join_hello`, `join_request`,
//! `join_state`, `renew`) and heartbeats. Every control node of rung 2 is
//! also an anchor, so it handles join service requests itself and forwards
//! none (ARFC 0002 section 3.13, This version). It keeps one entry per
//! origin, so a request is handled at most once while it runs.
//!
//! The same task runs the node lifecycle controller of clause 3.16 once
//! every [`LIFECYCLE_PERIOD`]: it deletes a `joining` node within 60
//! seconds after its record's `not_after`, completes or deletes a record
//! that an admission stopped between its writes left with no `Node`, sets
//! and clears `unreachable_since`, and renews the control node's own record
//! at half life, as it renews any node's. All the writes to nodes and their
//! records are made by this task, one at a time, so two joins never take
//! the same name.
//!
//! A node is a registered peer of the mesh only while its record is valid
//! by this node's wall clock (ARFC 0002 section 3.5, I-09). Once it has
//! expired, the node's sessions are dropped, and its node messages other
//! than the join service requests are dropped, so an expired node renews
//! as an unregistered initiator, through the join service. It is a
//! registered peer again once it is issued a valid record.
//!
//! Not built in rung 2 (ARFC 0002 section 3.15): approval, so no request is
//! ever `pending` and `join_state` is answered only for an admitted node;
//! `withdraw` and `leave`, which are dropped; and an audit log, so each
//! refusal and each admission is logged with its source address.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use alloy_crypto::al::PublicKey;
use alloy_crypto::ed25519::{Signature, SigningKey, VerifyingKey};
use alloy_id::proto::{BootstrapView, Facts};
use alloy_id::record::{RECORD_LIFETIME_SECS, verify_renew};
use alloy_id::token::join_message;
use alloy_id::{ClusterId, DataDir, NodeId, NodeRecord, Role, Roles, SignedNodeRecord};
use alloy_rt::{Runtime, WallTime};
use alloy_state::{
    Group, Heartbeat, NODE_KIND, NodeSpec, NodeState, NodeStatus, RecordTerms, Role as StateRole,
    Scope, StatusReply, TokenId,
};
use prost::Message;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::channel::entries::{Admit, Entries, Origin};
use crate::channel::proto::{
    self, Admitted, Body, ClusterProof, HelloResult, JoinHello, JoinHelloReply, JoinReply,
    JoinRequest, JoinRequestBody, JoinResult, Kind, NodeMessage, NodeStatusReply, Refusal, Renew,
    VersionRefusal, refusal, status,
};
use crate::mesh::{Command, Inbound, MeshHandle};
use crate::records::{self, Enrolled};

/// How often the node lifecycle controller runs: within the 5 seconds
/// clause 3.16 allows.
pub(crate) const LIFECYCLE_PERIOD: Duration = Duration::from_secs(1);

/// How long after its record's `not_after` the lifecycle controller
/// deletes a `joining` node. ARFC 0002 section 6 allows at most 60 seconds;
/// the 10 left cover the wait for the next tick of [`LIFECYCLE_PERIOD`] and
/// the deletion's writes.
pub(crate) const JOINING_DELETE_AFTER_SECS: i64 = 50;

/// The zone a node is put in when neither the request nor the token names
/// one (ARFC 0002 section 3.3).
pub(crate) const DEFAULT_ZONE: &str = "default";

/// The value of `Facts.os` on a Windows machine, where every node is a
/// client node (ARFC 0002 section 3.11).
pub(crate) const WINDOWS_OS: &str = "windows";

/// The most records a bootstrap view holds.
const MAX_VIEW: usize = 16;

fn seconds(time: WallTime) -> i64 {
    time.0.div_euclid(1_000_000_000)
}

/// A release as `MAJOR.MINOR.PATCH` with any pre-release suffix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    preview: bool,
}

fn version(release: &str) -> Option<Version> {
    alloy_state::Release::parse(release)?;
    let (core, pre) = match release.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (release, None),
    };
    let mut parts = core.split('.').map(str::parse::<u64>);
    let major = parts.next()?.ok()?;
    let minor = parts.next()?.ok()?;
    let _patch = parts.next()?.ok()?;
    // A preview is `0.0.PATCH-preview.N` (ARFC 0017 clause 3.13).
    let preview = major == 0
        && minor == 0
        && pre.is_some_and(|pre| {
            pre.strip_prefix("preview.").is_some_and(|n| {
                !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && !n.starts_with('0')
            })
        });
    Some(Version {
        major,
        minor,
        preview,
    })
}

/// Whether a joiner running `joiner` is accepted by a cluster whose lowest
/// release is `lowest` (ARFC 0002 section 3.3 step 5): its `MAJOR.MINOR`
/// is that of `lowest` or the next minor release, and, when either is a
/// preview, it is exactly `lowest`.
///
/// Rung 2 records no node's running release, so the control node answers
/// with its own release as the cluster's lowest and highest.
#[must_use]
pub(crate) fn release_accepted(lowest: &str, joiner: &str) -> bool {
    let (Some(l), Some(j)) = (version(lowest), version(joiner)) else {
        return false;
    };
    if l.preview || j.preview {
        return lowest == joiner;
    }
    j.major == l.major && (j.minor == l.minor || Some(j.minor) == l.minor.checked_add(1))
}

/// The `MAJOR.MINOR` releases a cluster whose lowest release is `lowest`
/// accepts, for a `version` refusal: none but `lowest` itself for a
/// preview, which `highest_release` names.
fn accepted_minors(lowest: &str) -> Vec<String> {
    match version(lowest) {
        Some(v) if !v.preview => {
            let mut minors = vec![format!("{}.{}", v.major, v.minor)];
            if let Some(next) = v.minor.checked_add(1) {
                minors.push(format!("{}.{next}", v.major));
            }
            minors
        }
        _ => Vec::new(),
    }
}

/// The roles a request that names none gets when the token presets none
/// (ARFC 0002 section 3.3): none on Windows, else `compute` and `storage`,
/// with `compute` left out when the facts show no hardware virtualization.
#[must_use]
pub(crate) fn default_roles(facts: Option<&Facts>) -> Roles {
    let Some(facts) = facts else {
        return Roles::of(&[Role::Storage]);
    };
    if facts.os == WINDOWS_OS {
        return Roles::NONE;
    }
    if facts.virtualization {
        Roles::of(&[Role::Compute, Role::Storage])
    } else {
        Roles::of(&[Role::Storage])
    }
}

/// `base` when it is free, else the lowest free `base-2`, `base-3`, and so
/// on, cutting `base` so the result stays within 63 bytes (ARFC 0002
/// section 3.3).
#[must_use]
pub(crate) fn free_name(base: &str, taken: &BTreeSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_owned();
    }
    let mut n: u64 = 2;
    loop {
        let suffix = format!("-{n}");
        let keep = alloy_id::record::MAX_NAME_LEN.saturating_sub(suffix.len());
        let mut cut = base.len().min(keep);
        while !base.is_char_boundary(cut) {
            cut -= 1;
        }
        let candidate = format!("{}{suffix}", &base[..cut]);
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn state_role(role: Role) -> StateRole {
    match role {
        Role::Anchor => StateRole::Anchor,
        Role::Control => StateRole::Control,
        Role::Compute => StateRole::Compute,
        Role::Storage => StateRole::Storage,
    }
}

fn id_role(role: StateRole) -> Role {
    match role {
        StateRole::Anchor => Role::Anchor,
        StateRole::Control => Role::Control,
        StateRole::Compute => Role::Compute,
        StateRole::Storage => Role::Storage,
    }
}

fn refused(reason: i32) -> JoinReply {
    JoinReply {
        result: Some(JoinResult::Refusal(Refusal {
            reason,
            expire_time: 0,
        })),
    }
}

fn parse_ip(bytes: &[u8]) -> Option<IpAddr> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes).ok().map(IpAddr::from),
        16 => <[u8; 16]>::try_from(bytes).ok().map(IpAddr::from),
        _ => None,
    }
}

/// The name of a refusal reason, for the log.
fn reason_name(reason: i32) -> &'static str {
    match reason {
        refusal::TOKEN_REFUSED => "token_refused",
        refusal::KEY_IN_USE => "key_in_use",
        refusal::TOKEN_REVOKED => "token_revoked",
        refusal::TOKEN_EXPIRED => "token_expired",
        refusal::TOKEN_USED_UP => "token_used_up",
        refusal::PRESET_CONFLICT => "preset_conflict",
        refusal::COMPUTE_UNSUPPORTED => "compute_unsupported",
        refusal::NAME_TAKEN => "name_taken",
        refusal::NODE_UNKNOWN => "node_unknown",
        _ => "refused",
    }
}

/// What a control node knows of itself for its node channel.
pub(crate) struct Own {
    /// Its name.
    pub(crate) name: String,
    /// Its current signed record.
    pub(crate) signed: SignedNodeRecord,
    /// That record, verified.
    pub(crate) record: NodeRecord,
}

/// The control node's node channel task. See the module documentation.
pub(crate) struct NodeService {
    runtime: Runtime,
    group: Arc<Group>,
    data: DataDir,
    cluster_id: ClusterId,
    cluster_key: SigningKey,
    cluster_pub: VerifyingKey,
    own: Own,
    mesh: MeshHandle,
    entries: Entries,
    /// The name of the node each recorded mesh key belongs to, and its
    /// record's `not_after`.
    by_mesh: BTreeMap<PublicKey, (String, i64)>,
    /// The mesh keys this task made registered peers of the mesh.
    admitted: BTreeSet<PublicKey>,
}

impl std::fmt::Debug for NodeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeService")
            .field("own", &self.own.name)
            .finish_non_exhaustive()
    }
}

/// What a reply to a request is built from.
type Reply = Option<Body>;

impl NodeService {
    /// The task for the control node `own` of cluster `cluster_id`, over
    /// `group`, writing its own node file in `data`, sending through `mesh`.
    #[must_use]
    pub(crate) fn new(
        runtime: &Runtime,
        group: Arc<Group>,
        data: DataDir,
        cluster_id: ClusterId,
        cluster_key: SigningKey,
        own: Own,
        mesh: MeshHandle,
    ) -> NodeService {
        let cluster_pub = cluster_key.verifying_key();
        NodeService {
            runtime: runtime.clone(),
            group,
            data,
            cluster_id,
            cluster_key,
            cluster_pub,
            own,
            mesh,
            entries: Entries::new(),
            by_mesh: BTreeMap::new(),
            admitted: BTreeSet::new(),
        }
    }

    /// Admits every node whose record the group holds as valid as a peer
    /// of the mesh, then answers what `inbound` brings and runs the
    /// lifecycle controller, until `inbound` closes or the task is
    /// cancelled.
    pub(crate) async fn serve(mut self, mut inbound: mpsc::Receiver<Inbound>) {
        match records::list(&self.group, &self.cluster_pub).await {
            Ok(nodes) => {
                for node in &nodes {
                    self.register(node).await;
                }
            }
            Err(e) => warn!(reason = %e, "the nodes' records could not be read"),
        }
        let clock = Arc::clone(self.runtime.clock());
        let mut next_tick = clock.now();
        loop {
            let wait = next_tick.saturating_duration_since(clock.now());
            match self.runtime.timeout(wait, inbound.recv()).await {
                Ok(None) => return,
                Ok(Some(Inbound::Request {
                    peer,
                    registered,
                    source,
                    message,
                })) => self.request(peer, registered, source, message).await,
                Ok(Some(Inbound::SessionMade { .. } | Inbound::Abandoned { .. })) => {}
                Err(_) => {
                    self.lifecycle().await;
                    next_tick = clock.now().saturating_add(LIFECYCLE_PERIOD);
                }
            }
        }
    }

    /// Makes `node`'s mesh keys registered peers while its record is valid
    /// by the wall clock, and drops them and their sessions once it has
    /// expired, but for this node's own keys.
    async fn register(&mut self, node: &Enrolled) {
        let valid = node.record.is_valid_at(self.runtime.clock().wall());
        for key in &node.record.mesh_pub {
            self.by_mesh
                .insert(*key, (node.record.name.clone(), node.record.not_after));
            if node.record.name == self.own.name {
                continue;
            }
            if valid && self.admitted.insert(*key) {
                self.mesh.send(Command::Admit { peer: *key }).await;
            } else if !valid && self.admitted.remove(key) {
                info!(
                    node = %node.record.name,
                    "a node's record expired, so it is no longer a registered peer"
                );
                self.mesh.send(Command::Forget { peer: *key }).await;
            }
        }
    }

    /// Whether `peer` is a mesh key of a node whose record is valid by the
    /// wall clock.
    fn holds_valid(&self, peer: &PublicKey) -> bool {
        let now = seconds(self.runtime.clock().wall());
        self.by_mesh
            .get(peer)
            .is_some_and(|(_, not_after)| now < *not_after)
    }

    /// Forgets every mesh key of node `name`, and drops its sessions.
    async fn forget(&mut self, name: &str) {
        let keys: Vec<PublicKey> = self
            .by_mesh
            .iter()
            .filter(|(_, (n, _))| n.as_str() == name)
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            self.by_mesh.remove(&key);
            self.admitted.remove(&key);
            self.mesh.send(Command::Forget { peer: key }).await;
        }
    }

    async fn request(
        &mut self,
        peer: PublicKey,
        registered: bool,
        source: Option<SocketAddr>,
        message: NodeMessage,
    ) {
        let request_id = message.request_id;
        let Some(body) = message.body else {
            return;
        };
        let Some(kind) = Kind::of(&body) else {
            return;
        };
        // A node whose record has expired is held as an unregistered
        // initiator, whatever its session (ARFC 0006 clause 3.14).
        let registered = registered && self.holds_valid(&peer);
        if !registered && !kind.is_join_service() {
            return;
        }
        let origin = Origin {
            session: peer,
            forwarded: None,
        };
        let now = self.runtime.clock().now();
        match self.entries.admit(origin, request_id, now) {
            Admit::Drop => return,
            Admit::Resend(bytes) => {
                self.mesh
                    .send(Command::Reply {
                        peer,
                        message: bytes,
                    })
                    .await;
                return;
            }
            Admit::Handle => {}
        }
        let reply: Reply = match body {
            Body::JoinHello(hello) => Some(Body::JoinHelloReply(self.hello(&hello, source))),
            Body::JoinRequest(request) => self
                .join_request(peer, source, &request)
                .await
                .map(Body::JoinReply),
            Body::JoinState(_) => self.join_state(&peer).await.map(Body::JoinReply),
            Body::Renew(renew) => self.renew(&peer, &renew).await.map(Body::JoinReply),
            Body::NodeStatus(beat) => Some(Body::NodeStatusReply(self.status(&peer, beat).await)),
            _ => None,
        };
        let Some(reply) = reply else {
            return;
        };
        let bytes = proto::encode(&NodeMessage {
            request_id,
            body: Some(reply),
        });
        self.entries.reply(origin, request_id, bytes.clone());
        self.mesh
            .send(Command::Reply {
                peer,
                message: bytes,
            })
            .await;
    }

    /// Section 3.3 step 5: the version check before any token is looked
    /// at, then the cluster key and this anchor's own record.
    fn hello(&self, hello: &JoinHello, source: Option<SocketAddr>) -> JoinHelloReply {
        let own = crate::VERSION;
        if !release_accepted(own, &hello.release) {
            warn!(
                source = ?source,
                release = %hello.release.escape_debug(),
                "a join was refused: version"
            );
            return JoinHelloReply {
                result: Some(HelloResult::Version(VersionRefusal {
                    accepted_minors: accepted_minors(own),
                    highest_release: own.to_owned(),
                })),
            };
        }
        JoinHelloReply {
            result: Some(HelloResult::Cluster(ClusterProof {
                cluster_pub: self.cluster_pub.as_bytes().to_vec(),
                anchor_record: Some(self.own.signed.to_proto()),
                previous_cluster_pub: Vec::new(),
                cross_signature: Vec::new(),
            })),
        }
    }

    /// The bootstrap view: the cluster key and the records of its anchors
    /// and control nodes, at most 16, those holding `control` first, then
    /// by name.
    fn view(&self, nodes: &[Enrolled]) -> BootstrapView {
        let mut homes: Vec<&Enrolled> = nodes
            .iter()
            .filter(|n| {
                n.record.roles.contains(Role::Anchor) || n.record.roles.contains(Role::Control)
            })
            .collect();
        homes.sort_by(|a, b| {
            (!a.record.roles.contains(Role::Control), &a.record.name)
                .cmp(&(!b.record.roles.contains(Role::Control), &b.record.name))
        });
        BootstrapView {
            cluster_pub: self.cluster_pub.as_bytes().to_vec(),
            nodes: homes
                .into_iter()
                .take(MAX_VIEW)
                .map(|n| n.signed.to_proto())
                .collect(),
        }
    }

    fn admitted(&self, node: &Enrolled, nodes: &[Enrolled]) -> JoinReply {
        JoinReply {
            result: Some(JoinResult::Admitted(Admitted {
                record: Some(node.signed.to_proto()),
                view: Some(self.view(nodes)),
            })),
        }
    }

    fn log_refusal(&self, reason: i32, source: Option<SocketAddr>) {
        warn!(
            source = ?source,
            reason = reason_name(reason),
            "a join request was refused"
        );
    }

    /// Counts one use of the token `token_id` (section 3.3). A token that
    /// changed in between is read again, once.
    async fn count_use(&self, token_id: [u8; 8]) -> bool {
        for _ in 0..2 {
            let Ok(Some(mut token)) = self.group.get_join_token(TokenId(token_id)).await else {
                return false;
            };
            token.uses = token.uses.saturating_add(1);
            match self.group.update_join_token(&token).await {
                Ok(_) => return true,
                Err(alloy_state::GroupError::Resource(alloy_state::ResourceError::Conflict {
                    ..
                })) => {}
                Err(e) => {
                    warn!(reason = %e, "a use of a join token could not be counted");
                    return false;
                }
            }
        }
        false
    }

    /// Section 3.3 step 6, checked in the order it states. `None` when the
    /// group cannot answer, which the joiner sees as no answer.
    async fn join_request(
        &mut self,
        peer: PublicKey,
        source: Option<SocketAddr>,
        request: &JoinRequest,
    ) -> Option<JoinReply> {
        let body = JoinRequestBody::decode(request.body.as_slice()).ok()?;
        let token_id: [u8; 8] = body.token_id.as_slice().try_into().ok()?;
        let token = match self.group.get_join_token(TokenId(token_id)).await {
            Ok(token) => token,
            Err(e) => {
                warn!(reason = %e, "a join request could not be checked");
                return None;
            }
        };
        let message = join_message(&self.cluster_id, &peer, &request.body);
        let signature = |bytes: &[u8]| <[u8; 64]>::try_from(bytes).ok().map(Signature::from_bytes);
        let identity = <[u8; 32]>::try_from(body.identity_pub.as_slice())
            .ok()
            .and_then(|b| VerifyingKey::from_bytes(&b).ok());
        let token_ok = token.as_ref().is_some_and(|t| {
            VerifyingKey::from_bytes(&t.spec.token_pub).is_ok_and(|key| {
                signature(&request.token_signature)
                    .is_some_and(|s| key.verify(&message, &s).is_ok())
            })
        });
        let identity_ok = identity.as_ref().is_some_and(|key| {
            signature(&request.identity_signature).is_some_and(|s| key.verify(&message, &s).is_ok())
        });
        let (Some(token), Some(identity), true, true) = (token, identity, token_ok, identity_ok)
        else {
            // A known token counts the use; an unknown one gets the same
            // bytes.
            self.count_use(token_id).await;
            self.log_refusal(refusal::TOKEN_REFUSED, source);
            return Some(refused(refusal::TOKEN_REFUSED));
        };

        let nodes = match records::list(&self.group, &self.cluster_pub).await {
            Ok(nodes) => nodes,
            Err(e) => {
                warn!(reason = %e, "a join request could not be checked");
                return None;
            }
        };
        if let Some(node) = nodes.iter().find(|n| {
            n.record.identity_pub == identity
                && n.holds_mesh_key(&peer)
                && n.token_id == Some(token_id)
        }) {
            // A retry: the node's current record, creating nothing.
            self.ensure_node(node).await?;
            self.register(&node.clone()).await;
            return Some(self.admitted(node, &nodes));
        }
        if nodes
            .iter()
            .any(|n| n.record.identity_pub == identity || n.holds_mesh_key(&peer))
        {
            self.log_refusal(refusal::KEY_IN_USE, source);
            return Some(refused(refusal::KEY_IN_USE));
        }

        let wall = self.runtime.clock().wall();
        if token.spec.revoked {
            self.log_refusal(refusal::TOKEN_REVOKED, source);
            return Some(refused(refusal::TOKEN_REVOKED));
        }
        if wall.0 >= token.spec.expire_time.0 {
            self.log_refusal(refusal::TOKEN_EXPIRED, source);
            return Some(JoinReply {
                result: Some(JoinResult::Refusal(Refusal {
                    reason: refusal::TOKEN_EXPIRED,
                    expire_time: seconds(token.spec.expire_time),
                })),
            });
        }
        if token.spec.use_limit != 0 && token.uses >= token.spec.use_limit {
            self.log_refusal(refusal::TOKEN_USED_UP, source);
            return Some(refused(refusal::TOKEN_USED_UP));
        }

        let preset = Roles::of(
            &token
                .spec
                .preset_roles
                .iter()
                .copied()
                .map(id_role)
                .collect::<Vec<_>>(),
        );
        let given = if body.roles.is_empty() {
            None
        } else {
            let parsed: Option<Vec<Role>> =
                body.roles.iter().map(|r| Role::from_proto(*r)).collect();
            match parsed {
                Some(roles) => Some(Roles::of(&roles)),
                None => {
                    self.log_refusal(refusal::PRESET_CONFLICT, source);
                    return Some(refused(refusal::PRESET_CONFLICT));
                }
            }
        };
        let zone_given = (!body.zone.is_empty()).then(|| body.zone.clone());
        let preset_zone = &token.spec.preset_zone;
        if given.is_some_and(|g| !preset.is_empty() && g != preset)
            || zone_given
                .as_ref()
                .is_some_and(|z| !preset_zone.is_empty() && z != preset_zone)
        {
            self.log_refusal(refusal::PRESET_CONFLICT, source);
            return Some(refused(refusal::PRESET_CONFLICT));
        }
        let roles = given
            .or((!preset.is_empty()).then_some(preset))
            .unwrap_or_else(|| default_roles(body.facts.as_ref()));
        let virtualization = body.facts.as_ref().is_some_and(|f| f.virtualization);
        if roles.contains(Role::Compute) && !virtualization {
            self.log_refusal(refusal::COMPUTE_UNSUPPORTED, source);
            return Some(refused(refusal::COMPUTE_UNSUPPORTED));
        }
        let zone = zone_given
            .or_else(|| (!preset_zone.is_empty()).then(|| preset_zone.clone()))
            .unwrap_or_else(|| DEFAULT_ZONE.to_owned());

        let mut taken: BTreeSet<String> = nodes.iter().map(|n| n.record.name.clone()).collect();
        match self.group.list_nodes().await {
            Ok(views) => taken.extend(views.into_iter().map(|v| v.node.name)),
            Err(e) => {
                warn!(reason = %e, "a join request could not be checked");
                return None;
            }
        }
        if alloy_api::NodeName::new(&body.name).is_err() {
            // No refusal names a malformed name; the CLI derives and checks
            // it before it sends anything.
            self.log_refusal(refusal::NAME_TAKEN, source);
            return Some(refused(refusal::NAME_TAKEN));
        }
        let name = if body.name_is_default {
            free_name(&body.name, &taken)
        } else if taken.contains(&body.name) {
            self.log_refusal(refusal::NAME_TAKEN, source);
            return Some(refused(refusal::NAME_TAKEN));
        } else {
            body.name.clone()
        };
        let advertise: Vec<IpAddr> = body.advertise.iter().filter_map(|a| parse_ip(a)).collect();
        let record = NodeRecord {
            cluster_id: self.cluster_id,
            node_id: NodeId::of(&identity),
            name: name.clone(),
            identity_pub: identity,
            mesh_pub: vec![peer],
            roles,
            zone,
            infra_addr: self.cluster_id.infra_addr(&peer),
            advertise,
            mesh_port: u16::try_from(body.mesh_port).ok()?,
            api_port: u16::try_from(body.api_port).ok()?,
            not_after: seconds(wall).saturating_add(RECORD_LIFETIME_SECS),
            serial: 1,
        };
        let signed = SignedNodeRecord::sign(&record, &self.cluster_key).ok()?;
        if !self.count_use(token_id).await {
            return None;
        }
        let node = Enrolled::new(signed, record, Some(token_id), body.facts.clone(), source);
        let node = match records::store(&self.group, &self.cluster_pub, &node).await {
            Ok(node) => node,
            Err(e) => {
                warn!(reason = %e, "an admitted node could not be stored");
                return None;
            }
        };
        self.ensure_node(&node).await?;
        self.register(&node).await;
        info!(node = %name, source = ?source, "a node was admitted");
        let mut all = nodes;
        all.push(node.clone());
        Some(self.admitted(&node, &all))
    }

    /// Creates the `Node` of `node` when the group holds none, as after a
    /// stop between the two writes of an admission. `None` when the group
    /// cannot answer.
    async fn ensure_node(&self, node: &Enrolled) -> Option<()> {
        let record = &node.record;
        match self.group.get_node(&record.name).await {
            Ok(Some(_)) => return Some(()),
            Ok(None) => {}
            Err(e) => {
                warn!(reason = %e, "a node could not be read");
                return None;
            }
        }
        let created = self
            .group
            .create_node(
                &record.name,
                NodeSpec {
                    roles: record.roles.iter().map(state_role).collect(),
                    zone: record.zone.clone(),
                    advertise: record.advertise.clone(),
                },
                NodeStatus {
                    node_id: alloy_state::NodeId(*record.node_id.as_bytes()),
                    record: Some(RecordTerms {
                        serial: record.serial,
                        not_after: record.not_after,
                    }),
                    reported: false,
                    unreachable_since: None,
                    running_release: None,
                },
                BTreeMap::new(),
            )
            .await;
        match created {
            Ok(_) => Some(()),
            Err(e) => {
                warn!(reason = %e, "a node could not be created");
                None
            }
        }
    }

    /// `join_state`: rung 2 has no approval, so only an admitted node's
    /// request has a state, its current record.
    async fn join_state(&self, peer: &PublicKey) -> Option<JoinReply> {
        let nodes = records::list(&self.group, &self.cluster_pub).await.ok()?;
        let node = nodes.iter().find(|n| n.holds_mesh_key(peer))?;
        Some(self.admitted(node, &nodes))
    }

    /// Section 3.5: a new record only for the node's current serial, signed
    /// by its identity key, on a session its mesh key authenticated.
    async fn renew(&mut self, peer: &PublicKey, renew: &Renew) -> Option<JoinReply> {
        let nodes = records::list(&self.group, &self.cluster_pub).await.ok()?;
        let unknown = Some(refused(refusal::NODE_UNKNOWN));
        let Ok(node_id) = <[u8; 16]>::try_from(renew.node_id.as_slice()) else {
            return unknown;
        };
        let node_id = NodeId::from_bytes(node_id);
        let Some(node) = nodes.iter().find(|n| n.node_id() == node_id) else {
            return unknown;
        };
        let Ok(signature) = <[u8; 64]>::try_from(renew.signature.as_slice()) else {
            return unknown;
        };
        let signature = Signature::from_bytes(signature);
        let signed_ok = verify_renew(
            &node.record.identity_pub,
            &self.cluster_id,
            &node_id,
            renew.serial,
            &signature,
        )
        .is_ok();
        if !signed_ok || !node.holds_mesh_key(peer) || renew.serial > node.record.serial {
            return unknown;
        }
        if renew.serial < node.record.serial {
            return Some(self.admitted(node, &nodes));
        }
        let renewed = self.reissue(node).await?;
        info!(node = %renewed.record.name, serial = renewed.record.serial, "a node record was renewed");
        // A node that renewed an expired record through the join service
        // is a registered peer again.
        self.register(&renewed).await;
        let all: Vec<Enrolled> = nodes
            .into_iter()
            .map(|n| {
                if n.node_id() == node_id {
                    renewed.clone()
                } else {
                    n
                }
            })
            .collect();
        Some(self.admitted(&renewed, &all))
    }

    /// Issues `node` its next record, valid for 24 hours from now, and
    /// stores it with the `Node`'s record terms.
    async fn reissue(&self, node: &Enrolled) -> Option<Enrolled> {
        let wall = self.runtime.clock().wall();
        let mut record = node.record.clone();
        record.serial = record.serial.checked_add(1)?;
        record.not_after = seconds(wall).saturating_add(RECORD_LIFETIME_SECS);
        let signed = SignedNodeRecord::sign(&record, &self.cluster_key).ok()?;
        let mut next = node.clone();
        next.signed = signed;
        next.record = record.clone();
        let stored = match records::store(&self.group, &self.cluster_pub, &next).await {
            Ok(stored) => stored,
            Err(e) => {
                warn!(reason = %e, "a renewed record could not be stored");
                return None;
            }
        };
        for _ in 0..3 {
            let Ok(Some(view)) = self.group.get_node(&record.name).await else {
                break;
            };
            let mut current = view.node;
            current.status.record = Some(RecordTerms {
                serial: record.serial,
                not_after: record.not_after,
            });
            if self.group.update_node(&current).await.is_ok() {
                break;
            }
        }
        Some(stored)
    }

    /// A heartbeat (ARFC 0006 clause 3.14), for the node whose record holds
    /// the mesh key the session authenticated.
    async fn status(&self, peer: &PublicKey, beat: proto::NodeStatus) -> NodeStatusReply {
        let reply = |result: i32| NodeStatusReply {
            result,
            finalized_release: String::new(),
        };
        let Some((name, _)) = self.by_mesh.get(peer) else {
            return reply(status::REFUSED);
        };
        let heartbeat = Heartbeat {
            addresses: beat.addresses,
            release: beat.release,
        };
        match self.group.accept_heartbeat(name, &heartbeat).await {
            StatusReply::Ok => reply(status::OK),
            StatusReply::Unavailable => reply(status::UNAVAILABLE),
            StatusReply::Invalid => reply(status::INVALID),
            StatusReply::Refused => reply(status::REFUSED),
            StatusReply::ReleaseRefused { finalized_release } => NodeStatusReply {
                result: status::RELEASE_REFUSED,
                finalized_release: finalized_release.as_str().to_owned(),
            },
        }
    }

    /// The node lifecycle controller (clause 3.16), and this node's own
    /// renewal.
    async fn lifecycle(&mut self) {
        let records = match records::list(&self.group, &self.cluster_pub).await {
            Ok(records) => records,
            Err(e) => {
                warn!(reason = %e, "the nodes' records could not be read");
                return;
            }
        };
        for node in &records {
            self.register(node).await;
        }
        let Ok(views) = self.group.list_nodes().await else {
            return;
        };
        let wall = self.runtime.clock().wall();
        let now = seconds(wall);
        let delete_due =
            |not_after: i64| now >= not_after.saturating_add(JOINING_DELETE_AFTER_SECS);
        let with_node: BTreeSet<&str> = views.iter().map(|v| v.node.name.as_str()).collect();
        let orphans: Vec<Enrolled> = records
            .iter()
            .filter(|n| !with_node.contains(n.record.name.as_str()))
            .cloned()
            .collect();
        for orphan in &orphans {
            // An admission that stopped after the record's write and before
            // the `Node`'s (I-14): it is completed as a `joining` node, or
            // removed once that node would have been deleted.
            if delete_due(orphan.record.not_after) {
                self.remove_record(&orphan.record.name).await;
            } else if self.ensure_node(orphan).await.is_some() {
                info!(node = %orphan.record.name, "a node admitted before a stop was completed");
            }
        }
        let not_after: BTreeMap<String, i64> = records
            .iter()
            .map(|n| (n.record.name.clone(), n.record.not_after))
            .collect();
        for view in views {
            let mut node = view.node;
            match view.state {
                NodeState::Joining => {
                    // The stored record is the one a renewal last issued,
                    // whatever the `Node`'s copy of its terms holds.
                    let expiry = not_after
                        .get(&node.name)
                        .copied()
                        .or_else(|| node.status.record.map(|r| r.not_after));
                    if expiry.is_some_and(delete_due) {
                        self.delete(&node.name, node.generation).await;
                    }
                }
                NodeState::Unreachable if node.status.unreachable_since.is_none() => {
                    node.status.unreachable_since = Some(wall);
                    if let Err(e) = self.group.update_node(&node).await {
                        warn!(node = %node.name, reason = %e, "unreachable_since could not be set");
                    }
                }
                NodeState::Active if node.status.unreachable_since.is_some() => {
                    node.status.unreachable_since = None;
                    if let Err(e) = self.group.update_node(&node).await {
                        warn!(node = %node.name, reason = %e, "unreachable_since could not be cleared");
                    }
                }
                _ => {}
            }
        }
        if self.own.record.renew_at() <= now {
            self.renew_own().await;
        }
    }

    /// Deletes the `joining` node `name` whose record expired (ARFC 0002
    /// section 6), with its record, and forgets its sessions.
    async fn delete(&mut self, name: &str, generation: u64) {
        let marked = match self
            .group
            .delete(NODE_KIND, &Scope::Cluster, name, generation)
            .await
        {
            Ok(marked) => marked,
            Err(e) => {
                warn!(node = %name, reason = %e, "an expired joining node could not be deleted");
                return;
            }
        };
        if let Err(e) = self
            .group
            .remove(NODE_KIND, &Scope::Cluster, name, marked.generation)
            .await
        {
            warn!(node = %name, reason = %e, "an expired joining node could not be removed");
            return;
        }
        // A record a stop leaves here has no `Node`; the next tick removes
        // it.
        self.remove_record(name).await;
        info!(node = %name, "a joining node whose record expired was deleted");
    }

    /// Removes the record of node `name`, which has no `Node`, and forgets
    /// its mesh keys and sessions, so its name and keys are free.
    async fn remove_record(&mut self, name: &str) {
        if let Err(e) = records::remove(&self.group, name).await {
            warn!(node = %name, reason = %e, "an expired joining node's record could not be removed");
            return;
        }
        self.forget(name).await;
    }

    /// Renews this control node's own record in its own process, as it
    /// handles any node's renewal, and writes it to its node file.
    async fn renew_own(&mut self) {
        let Ok(nodes) = records::list(&self.group, &self.cluster_pub).await else {
            return;
        };
        let Some(own) = nodes.iter().find(|n| n.record.name == self.own.name) else {
            return;
        };
        let Some(renewed) = self.reissue(own).await else {
            return;
        };
        match self.data.read_node_state().await {
            Ok(Some(mut state)) => {
                state.record = Some(renewed.signed.to_proto());
                if let Some(view) = state.view.as_mut() {
                    for entry in &mut view.nodes {
                        let same = SignedNodeRecord::from_proto(entry)
                            .ok()
                            .and_then(|s| s.verify(&self.cluster_pub).ok())
                            .is_some_and(|r| r.node_id == renewed.record.node_id);
                        if same {
                            *entry = renewed.signed.to_proto();
                        }
                    }
                }
                if let Err(e) = self.data.write_node_state(&state).await {
                    warn!(reason = %e, "the renewed record could not be written to the node file");
                }
            }
            Ok(None) => {}
            Err(e) => warn!(reason = %e, "the node file could not be read"),
        }
        info!(
            serial = renewed.record.serial,
            "this node's record was renewed"
        );
        self.own.signed = renewed.signed;
        self.own.record = renewed.record;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_joiner_of_the_same_or_next_minor_release_is_accepted() {
        assert!(release_accepted("1.4.2", "1.4.0"));
        assert!(release_accepted("1.4.2", "1.5.9"));
        assert!(!release_accepted("1.4.2", "1.6.0"));
        assert!(!release_accepted("1.4.2", "1.3.9"));
        assert!(!release_accepted("1.4.2", "2.4.2"));
        assert!(!release_accepted("1.4.2", "not a release"));
        assert_eq!(accepted_minors("1.4.2"), ["1.4", "1.5"]);
    }

    #[test]
    fn a_preview_accepts_only_its_own_release() {
        assert!(release_accepted("0.0.3-preview.2", "0.0.3-preview.2"));
        assert!(!release_accepted("0.0.3-preview.2", "0.0.3-preview.3"));
        assert!(!release_accepted("0.0.3", "0.0.3-preview.1"));
        assert!(release_accepted("0.0.0", "0.0.5"));
        assert!(accepted_minors("0.0.3-preview.2").is_empty());
    }

    #[test]
    fn a_default_name_in_use_takes_the_lowest_free_suffix() {
        let taken: BTreeSet<String> = ["node", "node-2", "node-4"].map(str::to_owned).into();
        assert_eq!(free_name("free", &taken), "free");
        assert_eq!(free_name("node", &taken), "node-3");
        let long = "a".repeat(63);
        let taken: BTreeSet<String> = [long.clone()].into();
        let name = free_name(&long, &taken);
        assert_eq!(name.len(), 63);
        assert!(name.ends_with("-2"), "{name}");
    }

    #[test]
    fn the_default_roles_follow_the_facts() {
        let mut facts = Facts {
            os: "linux".to_owned(),
            virtualization: true,
            ..Facts::default()
        };
        assert_eq!(
            default_roles(Some(&facts)),
            Roles::of(&[Role::Compute, Role::Storage])
        );
        facts.virtualization = false;
        assert_eq!(default_roles(Some(&facts)), Roles::of(&[Role::Storage]));
        facts.os = WINDOWS_OS.to_owned();
        assert_eq!(default_roles(Some(&facts)), Roles::NONE);
    }
}
