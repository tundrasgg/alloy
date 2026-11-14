//! `init`, the steps that make a machine the first node of a new cluster
//! (ARFC 0002 section 3.1), as M11 rung 2 keeps them (section 3.15).
//!
//! [`init`] is the interface `alloy init` calls, in its own process, as
//! root, once its checks of section 3.8 pass and it has picked the
//! advertise addresses (steps 1 and 2): it writes the host file, generates
//! and stores every key and certificate of step 3, creates the global
//! group with this node as its one voter (ARFC 0006 clause 3.2), creates the
//! node, the org `default`, the user `admin`, the join token, and the login
//! code in it, and returns the token and the code for step 5 to print. The
//! CLI then enables and starts `alloyd` (step 4), which serves from the
//! stored data ([`ControlNode`](crate::ControlNode)).
//!
//! When a step fails, `init` removes everything it wrote but the host file,
//! whose join line stays `unjoined`, so a rerun starts clean.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};

use alloy_crypto::Zeroizing;
use alloy_crypto::tls::ApiCaFingerprint;
use alloy_id::keys::{generate_ed25519, generate_x25519};
use alloy_id::login::{CodeId as LoginCodeId, CodeSecret};
use alloy_id::proto::{BootstrapView, NodeSecrets, NodeState as StoredNode};
use alloy_id::record::RECORD_LIFETIME_SECS;
use alloy_id::store::HOST_FILE;
use alloy_id::token::{TokenAddress, TokenAnchor, TokenId as TokenIdBytes, TokenSecret};
use alloy_id::{
    ApiCa, ClusterId, ClusterKeyFp, CodeError, IdError, JoinToken, LoginCode, NodeId, NodeRecord,
    Role, Roles, SignedNodeRecord, StoreError, TokenError,
};
use alloy_rt::{DiskError, DiskIo, EntryKind, RtPath, Runtime, WallTime};
use alloy_state::{
    ClusterRole, Group, GroupError, IdentityError, IssuedBy, JoinTokenSpec, NodeId as MemberId,
    NodeSpec, NodeStatus, RecordTerms, Role as StateRole, TokenId,
};

use crate::control::{NodeError, NodePaths, create_state_dir, open_group};
use crate::data_dir::{JoinState, parse_host_file};
use crate::join::host_file;
use crate::records::{self, Enrolled};

/// The zone `init` puts the node in (ARFC 0002 section 3.1).
pub const DEFAULT_ZONE: &str = "default";

/// The org `init` creates (ARFC 0002 section 3.15).
pub const DEFAULT_ORG: &str = "default";

/// The user `init` creates (ARFC 0002 section 3.15).
pub const ADMIN_USER: &str = "admin";

/// How long the join token `init` creates lasts: 24 hours (section 3.1).
pub const TOKEN_LIFETIME_NANOS: i64 = 24 * 3_600 * 1_000_000_000;

/// The most advertise addresses a node record holds.
pub const MAX_ADVERTISE: usize = alloy_id::record::MAX_ADVERTISE;

/// What `init` is told: what the CLI's flags and step 2 chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitOptions {
    /// The node's name, by the rule of section 3.3 or as `--name` gave it.
    pub name: String,
    /// The advertise addresses step 2 picked, 1 to 16, the first one the
    /// login code's endpoint.
    pub advertise: Vec<IpAddr>,
    /// The UDP port of the node's AL socket.
    pub mesh_port: u16,
    /// The TCP port of the API and the `stream` underlay.
    pub api_port: u16,
    /// Whether the node takes `compute`: false when the virtualization check
    /// of section 3.8 is `warn`.
    pub compute: bool,
    /// The `cluster_id` the CLI drew for the addresses check of section
    /// 3.8, whose prefix overlaps no route and no address. `None` draws one
    /// here.
    pub cluster_id: Option<ClusterId>,
}

/// What `init` made that step 5 prints, once each. Neither is written to
/// any log or file.
pub struct Initialized {
    /// The node's name.
    pub node: String,
    /// The join token, an `alyt_` string.
    pub join_token: Zeroizing<String>,
    /// The login code for `admin`, an `alyc_` string.
    pub login_code: Zeroizing<String>,
}

impl std::fmt::Debug for Initialized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Initialized")
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}

/// Why `init` failed. Every case but [`InitError::Conflict`] and
/// [`InitError::Usage`] has removed what it wrote.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// The local join state is not `unjoined` (section 3.1 step 1): nothing
    /// was written.
    #[error(
        "this machine is already a node (its local join state is {0}), so init changed nothing"
    )]
    Conflict(JoinState),
    /// An option is out of range: nothing was written.
    #[error("{0}")]
    Usage(String),
    /// A file of the data directory could not be read or written.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A key or certificate could not be generated.
    #[error(transparent)]
    Id(#[from] IdError),
    /// The node record could not be built or signed.
    #[error("the node record: {0}")]
    Record(String),
    /// The global group could not be created or written.
    #[error(transparent)]
    Node(#[from] NodeError),
    /// An identity object could not be created.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// The join token could not be encoded.
    #[error(transparent)]
    Token(#[from] TokenError),
    /// The login code could not be encoded.
    #[error(transparent)]
    Code(#[from] CodeError),
}

impl From<GroupError> for InitError {
    fn from(error: GroupError) -> InitError {
        InitError::Node(NodeError::Group(error))
    }
}

/// Runs the steps of `init` that write, in the data directory at `paths`, on
/// `runtime`, as the module documentation says. The caller has run the
/// checks of section 3.8, among them that the data directory is an Alloy
/// data directory or was just created or adopted.
///
/// # Errors
///
/// [`InitError::Conflict`] when the local join state is not `unjoined`,
/// [`InitError::Usage`] for options out of range, each before anything is
/// written; any other [`InitError`] after removing what was written.
pub async fn init(
    runtime: &Runtime,
    paths: &NodePaths,
    options: &InitOptions,
) -> Result<Initialized, InitError> {
    check_options(options)?;
    if let Some(head) = paths.data.read(HOST_FILE).await? {
        let state = parse_host_file(&head)
            .map_err(|e| InitError::Usage(format!("the host file cannot be read: {e}")))?;
        if state != JoinState::Unjoined {
            return Err(InitError::Conflict(state));
        }
    }
    paths
        .data
        .write(HOST_FILE, host_file(JoinState::Unjoined).as_bytes())
        .await?;
    match create(runtime, paths, options).await {
        Ok(done) => {
            paths
                .data
                .write(HOST_FILE, host_file(JoinState::Joined).as_bytes())
                .await?;
            Ok(done)
        }
        Err(error) => {
            if let Err(e) = remove_written(runtime.disk_io().as_ref(), &paths.dir).await {
                tracing::warn!(reason = %e, "init could not remove what it wrote");
            }
            Err(error)
        }
    }
}

fn check_options(options: &InitOptions) -> Result<(), InitError> {
    if options.advertise.is_empty() || options.advertise.len() > MAX_ADVERTISE {
        return Err(InitError::Usage(format!(
            "init takes 1 to {MAX_ADVERTISE} advertise addresses, and {} were given",
            options.advertise.len()
        )));
    }
    alloy_api::NodeName::new(&options.name).map_err(|rule| {
        InitError::Usage(format!(
            "the node name `{}` {rule}",
            options.name.escape_debug()
        ))
    })?;
    Ok(())
}

fn seconds(time: WallTime) -> i64 {
    time.0.div_euclid(1_000_000_000)
}

/// Step 3 and what it creates, then the strings of step 5.
async fn create(
    runtime: &Runtime,
    paths: &NodePaths,
    options: &InitOptions,
) -> Result<Initialized, InitError> {
    let rng = runtime.rng().as_ref();
    let now = runtime.clock().wall();
    let cluster_key = generate_ed25519(rng)?;
    let cluster_pub = cluster_key.verifying_key();
    let cluster_id = match options.cluster_id {
        Some(id) => id,
        None => ClusterId::generate(rng)?,
    };
    let api_ca = ApiCa::generate(rng, &cluster_id, now)?;
    let identity = generate_ed25519(rng)?;
    let mesh = generate_x25519(rng)?;
    let server = api_ca.issue_server(rng, &options.name, &options.advertise, now)?;
    let mut roles = vec![Role::Anchor, Role::Control];
    if options.compute {
        roles.push(Role::Compute);
    }
    roles.push(Role::Storage);
    let record = NodeRecord {
        cluster_id,
        node_id: NodeId::of(&identity.verifying_key()),
        name: options.name.clone(),
        identity_pub: identity.verifying_key(),
        mesh_pub: vec![mesh.public_key()],
        roles: Roles::of(&roles),
        zone: DEFAULT_ZONE.to_owned(),
        infra_addr: cluster_id.infra_addr(&mesh.public_key()),
        advertise: options.advertise.clone(),
        mesh_port: options.mesh_port,
        api_port: options.api_port,
        not_after: seconds(now).saturating_add(RECORD_LIFETIME_SECS),
        serial: 1,
    };
    let signed = SignedNodeRecord::sign(&record, &cluster_key)
        .map_err(|e| InitError::Record(e.to_string()))?;
    let mut secrets = NodeSecrets::default();
    secrets.identity_key = identity.seed().to_vec();
    secrets.mesh_key = mesh.to_bytes().to_vec();
    secrets.cluster_key = cluster_key.seed().to_vec();
    secrets.api_ca_key = api_ca.key_pkcs8()?.to_vec();
    secrets.api_server_key = server.key.to_pkcs8_der().map_err(IdError::from)?.to_vec();
    paths.data.create_secrets(rng, &secrets).await?;
    paths.data.write_handshake_counter(0).await?;
    let view = BootstrapView {
        cluster_pub: cluster_pub.as_bytes().to_vec(),
        nodes: vec![signed.to_proto()],
    };
    paths
        .data
        .write_node_state(&StoredNode {
            cluster_id: cluster_id.as_bytes().to_vec(),
            cluster_pub: vec![cluster_pub.as_bytes().to_vec()],
            record: Some(signed.to_proto()),
            view: Some(view),
            pending: false,
            api_ca_certificate: api_ca.certificate().to_vec(),
            api_server_certificate: server.certificate.clone(),
        })
        .await?;

    create_state_dir(runtime, paths).await?;
    let group = open_group(runtime, paths, MemberId(*record.node_id.as_bytes())).await?;
    let created = populate(
        runtime,
        &group,
        &signed,
        &record,
        &cluster_pub,
        &api_ca,
        now,
    )
    .await;
    let shut = group.shutdown().await;
    let (join_token, login_code) = created?;
    shut?;
    Ok(Initialized {
        node: options.name.clone(),
        join_token,
        login_code,
    })
}

fn state_role(role: Role) -> StateRole {
    match role {
        Role::Anchor => StateRole::Anchor,
        Role::Control => StateRole::Control,
        Role::Compute => StateRole::Compute,
        Role::Storage => StateRole::Storage,
    }
}

/// Makes the group a one-voter group and creates the objects of section
/// 3.15 in it. Returns the join token and login code strings.
async fn populate(
    runtime: &Runtime,
    group: &Group,
    signed: &SignedNodeRecord,
    record: &NodeRecord,
    cluster_pub: &alloy_crypto::ed25519::VerifyingKey,
    api_ca: &ApiCa,
    now: WallTime,
) -> Result<(Zeroizing<String>, Zeroizing<String>), InitError> {
    let rng = runtime.rng().as_ref();
    group.initialize().await?;
    group
        .create_node(
            &record.name,
            NodeSpec {
                roles: record.roles.iter().map(state_role).collect(),
                zone: record.zone.clone(),
                advertise: record.advertise.clone(),
            },
            NodeStatus {
                node_id: MemberId(*record.node_id.as_bytes()),
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
        .await?;
    let own = Enrolled::new(signed.clone(), record.clone(), None, None, None);
    records::store(group, cluster_pub, &own).await?;
    group.create_org(DEFAULT_ORG, true).await?;
    group
        .create_user(
            DEFAULT_ORG,
            ADMIN_USER,
            true,
            BTreeSet::from([ClusterRole::CloudAdmin]),
        )
        .await?;

    let secret = TokenSecret::generate(rng)?;
    let mut token_id = [0u8; 8];
    rng.fill(&mut token_id).map_err(IdError::from)?;
    let cluster_key_fp = ClusterKeyFp::of(cluster_pub);
    let expire_time = WallTime(now.0.saturating_add(TOKEN_LIFETIME_NANOS));
    group
        .create_join_token(JoinTokenSpec {
            token_id: TokenId(token_id),
            token_pub: *secret.token_key().verifying_key().as_bytes(),
            cluster_key_fp: *cluster_key_fp.as_bytes(),
            expire_time,
            use_limit: 0,
            preset_roles: BTreeSet::new(),
            preset_zone: String::new(),
            revoked: false,
        })
        .await?;
    let mesh_pub = record
        .mesh_pub
        .first()
        .copied()
        .ok_or_else(|| InitError::Record("no mesh key".to_owned()))?;
    let token = JoinToken {
        cluster_id: record.cluster_id,
        cluster_key_fp,
        token_id: TokenIdBytes(token_id),
        secret,
        preset_roles: Roles::NONE,
        anchors: vec![TokenAnchor {
            mesh_pub,
            addresses: record
                .advertise
                .iter()
                .take(alloy_id::token::MAX_ADDRESSES)
                .map(|ip| TokenAddress {
                    ip: *ip,
                    mesh_port: record.mesh_port,
                    stream_port: record.api_port,
                })
                .collect(),
        }],
    };
    let join_token = Zeroizing::new(token.encode()?);

    let code_secret = CodeSecret::generate(rng)?;
    let code = group
        .issue_login_code(DEFAULT_ORG, ADMIN_USER, code_secret.hash(), IssuedBy::Init)
        .await?;
    let api_ca_fp = ApiCaFingerprint::of_certificate(api_ca.certificate())
        .ok_or_else(|| InitError::Record("the API CA certificate does not parse".to_owned()))?;
    let first = record
        .advertise
        .first()
        .copied()
        .ok_or_else(|| InitError::Usage("no advertise address".to_owned()))?;
    let login = LoginCode {
        cluster_id: record.cluster_id,
        api_ca_fp: api_ca_fp.0,
        code_id: LoginCodeId(code.code_id.0),
        secret: code_secret,
        endpoints: vec![SocketAddr::new(first, record.api_port)],
    };
    let login_code = Zeroizing::new(login.encode()?);
    Ok((join_token, login_code))
}

/// Removes every entry of the data directory `dir` but the host file, and
/// everything below them.
async fn remove_written(disk: &dyn DiskIo, dir: &RtPath) -> Result<(), DiskError> {
    for entry in disk.list(dir).await? {
        if entry.name == HOST_FILE {
            continue;
        }
        let path = dir.join(&entry.name)?;
        if entry.kind == EntryKind::Dir {
            remove_tree(disk, &path).await?;
        } else {
            disk.remove(&path).await?;
        }
    }
    disk.sync_dir(dir).await
}

/// Removes the directory `dir` and everything below it, each directory
/// once it is empty.
pub(crate) async fn remove_tree(disk: &dyn DiskIo, dir: &RtPath) -> Result<(), DiskError> {
    let mut stack = vec![(dir.clone(), false)];
    while let Some((path, listed)) = stack.pop() {
        if listed {
            disk.remove(&path).await?;
            continue;
        }
        stack.push((path.clone(), true));
        for entry in disk.list(&path).await? {
            let child = path.join(&entry.name)?;
            if entry.kind == EntryKind::Dir {
                stack.push((child, false));
            } else {
                disk.remove(&child).await?;
            }
        }
    }
    Ok(())
}
