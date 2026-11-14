//! `init` and a serving control node under the `os` runtime, on loopback
//! and in a data directory under the system's temporary directory (ADR
//! 0042: TLS is tested under `os`).
//!
//! The data directory of a test is not an Alloy data directory, since a test
//! does not run as root, so these tests call [`init`] and
//! [`ControlNode::start`] themselves, past the checks `alloy init` and
//! `alloyd` make first (ARFC 0002 sections 3.8 and 3.12). The local admin
//! listener is a loopback TCP listener whose connections report the peer
//! user id 0, as the kernel reports for root on the Unix socket.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use std::time::Duration;

use alloy_api::Transport;
use alloy_api::client::{Client, Credential, connect, local_admin_over};
use alloy_api::sigv4::AccessKeyCredential;
use alloy_api::tls::ApiCaFingerprint;
use alloy_crypto::Zeroizing;
use alloy_id::{DataDir, JoinToken, LoginCode};
use alloy_proto::cluster::v1alpha1::{Node, NodeState, Role};
use alloy_proto::common::v1alpha1::ErrorCode;
use alloy_proto::iam::v1alpha1::{ExchangeLoginCodeRequest, IssueLoginCodeRequest};
use alloy_rt::{
    BoxFuture, DirEntry, DiskError, DiskFile, DiskIo, ListenOptions, Listener, Metadata, NetError,
    OpenOptions, OsHost, Protection, RtPath, Runtime, Security, Stream, StreamAddr, StreamOptions,
};

use crate::data_dir::{JoinState, Location};
use crate::init::{InitError, InitOptions, init};
use crate::join::{JoinError, JoinOptions, Refusal, join};
use crate::{ControlNode, Listeners, MemberNode, NodePaths, SHUTDOWN_GRACE, os_runtime};

/// A data directory path no other test uses.
fn temp_data_dir(name: &str, rng: &dyn alloy_rt::Rng) -> PathBuf {
    let mut draw = [0u8; 8];
    rng.fill(&mut draw).expect("random");
    let n = u64::from_be_bytes(draw);
    std::env::temp_dir().join(format!("alloyd-test-{name}-{n:016x}"))
}

/// A listener whose connections report the peer user id 0.
#[derive(Debug)]
struct AsRoot(Box<dyn Listener>);

impl Listener for AsRoot {
    fn local_addr(&self) -> StreamAddr {
        self.0.local_addr()
    }

    fn accept(&self) -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>> {
        Box::pin(async move {
            let inner = self.0.accept().await?;
            Ok(Box::new(Root(inner)) as Box<dyn Stream>)
        })
    }
}

#[derive(Debug)]
struct Root(Box<dyn Stream>);

impl Stream for Root {
    fn local_addr(&self) -> Option<StreamAddr> {
        self.0.local_addr()
    }

    fn peer_addr(&self) -> Option<StreamAddr> {
        self.0.peer_addr()
    }

    fn read<'a>(&'a self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize, NetError>> {
        self.0.read(buf)
    }

    fn write<'a>(&'a self, bufs: &'a [&'a [u8]]) -> BoxFuture<'a, Result<usize, NetError>> {
        self.0.write(bufs)
    }

    fn shutdown_write(&self) -> BoxFuture<'_, Result<(), NetError>> {
        self.0.shutdown_write()
    }

    fn peer_uid(&self) -> Result<u32, NetError> {
        Ok(0)
    }
}

/// The `os` `disk-io` with every protection given as [`Protection::Default`]:
/// on Windows, a test that does not run as an administrator may not name
/// `Administrators` as an owner. What these tests check does not depend on
/// it; `alloy-rt` tests the protections themselves.
#[derive(Debug)]
struct Unprotected(Arc<dyn DiskIo>);

impl DiskIo for Unprotected {
    fn open<'a>(
        &'a self,
        path: &'a RtPath,
        options: OpenOptions,
    ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
        self.0.open(path, options)
    }
    fn metadata<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<Metadata, DiskError>> {
        self.0.metadata(path)
    }
    fn list<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<Vec<DirEntry>, DiskError>> {
        self.0.list(dir)
    }
    fn create_dir<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.create_dir(path)
    }
    fn remove<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.remove(path)
    }
    fn rename<'a>(
        &'a self,
        from: &'a RtPath,
        to: &'a RtPath,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.rename(from, to)
    }
    fn replace<'a>(
        &'a self,
        from: &'a RtPath,
        to: &'a RtPath,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.replace(from, to)
    }
    fn sync_dir<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.sync_dir(dir)
    }
    fn create_file<'a>(
        &'a self,
        path: &'a RtPath,
        _protection: Protection,
    ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
        self.0.create_file(path, Protection::Default)
    }
    fn create_dir_protected<'a>(
        &'a self,
        path: &'a RtPath,
        _protection: Protection,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.create_dir_protected(path, Protection::Default)
    }
    fn security<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<Security, DiskError>> {
        self.0.security(path)
    }
    fn set_mode<'a>(&'a self, path: &'a RtPath, mode: u32) -> BoxFuture<'a, Result<(), DiskError>> {
        self.0.set_mode(path, mode)
    }
    fn rt_path(&self, native: &str) -> Result<RtPath, DiskError> {
        self.0.rt_path(native)
    }
}

/// A loopback TCP listener on `port`, 0 for a free one, and its address.
fn listen(runtime: &Runtime, port: u16) -> (Box<dyn Listener>, SocketAddr) {
    let listener = runtime
        .net()
        .listen(
            StreamAddr::Ip(SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
            ListenOptions::default(),
        )
        .expect("listen");
    let StreamAddr::Ip(addr) = listener.local_addr() else {
        panic!("a TCP listener has an IP address");
    };
    (listener, addr)
}

/// One test's data directory and runtime.
struct Machine {
    host: OsHost,
    runtime: Runtime,
    transport: Transport,
    location: Location,
    native: PathBuf,
}

impl Machine {
    fn new(name: &str) -> Machine {
        // Every data directory under the temporary directory has the same
        // root, so the runtime is built before the name is drawn.
        let temp = std::env::temp_dir();
        let host = OsHost::start(2).expect("host");
        let root = Location::of(&temp).expect("location");
        let (os, transport) = os_runtime(&host, &root).expect("runtime");
        let native = temp_data_dir(name, os.rng().as_ref());
        let location = Location::of(&native).expect("location");
        let runtime = Runtime::new(
            Arc::clone(os.clock()),
            Arc::clone(os.net()),
            Arc::new(Unprotected(Arc::clone(os.disk_io()))),
            Arc::clone(os.rng()),
            Arc::clone(os.spawner()),
        );
        Machine {
            host,
            runtime,
            transport,
            location,
            native,
        }
    }

    fn paths(&self) -> NodePaths {
        NodePaths {
            data: DataDir::new(
                Arc::clone(self.runtime.disk_io()),
                self.location.dir().clone(),
            ),
            dir: self.location.dir().clone(),
            native: self.native.clone(),
        }
    }

    /// Runs `body` with a fresh, empty data directory, and removes it after.
    fn run<T>(self, body: impl AsyncFnOnce(&Machine) -> T) -> T {
        let disk = Arc::clone(self.runtime.disk_io());
        let dir = self.location.dir().clone();
        self.host
            .block_on(disk.create_dir(&dir))
            .expect("data directory");
        let output = self.host.block_on(body(&self));
        self.host
            .block_on(crate::init::remove_tree(disk.as_ref(), &dir))
            .expect("cleanup");
        self.host.shutdown(SHUTDOWN_GRACE);
        output
    }

    /// Starts the control node, its API on `api` and its local admin
    /// socket on a new listener whose address it returns.
    async fn start(&self, api: Box<dyn Listener>) -> (ControlNode, SocketAddr) {
        let (admin, admin_addr) = listen(&self.runtime, 0);
        let (mesh, _) = mesh_socket(&self.runtime);
        let listeners = Listeners {
            api,
            admin: Some(Box::new(AsRoot(admin))),
            mesh: vec![mesh],
        };
        let node = ControlNode::start(
            &self.runtime,
            &self.paths(),
            move |_| Ok(listeners),
            self.transport,
        )
        .await
        .expect("start");
        (node, admin_addr)
    }

    async fn client(&self, code: &LoginCode, credential: &Credential) -> Client {
        connect(
            &self.runtime,
            self.transport,
            code.endpoints[0],
            ApiCaFingerprint(code.api_ca_fp),
            credential,
        )
        .await
        .expect("connect")
    }

    async fn admin(&self, addr: SocketAddr) -> Client {
        let stream = self
            .runtime
            .net()
            .connect(StreamAddr::Ip(addr), StreamOptions::default())
            .await
            .expect("connect");
        local_admin_over(&self.runtime, stream)
            .await
            .expect("admin")
    }

    /// Every node, once all of them are `active`, within 10 seconds.
    async fn all_active(&self, client: &Client) -> Vec<Node> {
        for _ in 0..100 {
            let nodes = client.list_all_nodes().await.expect("list");
            let active = |n: &Node| {
                n.status
                    .as_ref()
                    .is_some_and(|s| s.state == i32::from(NodeState::Active))
            };
            if !nodes.is_empty() && nodes.iter().all(active) {
                return nodes;
            }
            self.runtime.sleep(Duration::from_millis(100)).await;
        }
        panic!("the nodes did not become active");
    }
}

fn options(api_port: u16) -> InitOptions {
    InitOptions {
        name: "linux-1".to_owned(),
        advertise: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        mesh_port: 7476,
        api_port,
        compute: true,
    }
}

fn exchange(code: &LoginCode, name: &str) -> ExchangeLoginCodeRequest {
    ExchangeLoginCodeRequest {
        code_id: code.code_id.0.to_vec(),
        secret: code.secret.as_bytes().to_vec(),
        name: name.to_owned(),
        replaces: String::new(),
    }
}

#[test]
fn init_serves_both_listeners_restarts_and_refuses_a_second_init() {
    Machine::new("init").run(async |m| {
        let (api, addr) = listen(&m.runtime, 0);
        let done = init(&m.runtime, &m.paths(), &options(addr.port()))
            .await
            .expect("init");
        assert_eq!(done.node, "linux-1");
        let token = JoinToken::decode(&done.join_token).expect("token");
        assert_eq!(token.anchors.len(), 1);
        assert_eq!(token.anchors[0].addresses[0].stream_port, addr.port());
        let code = LoginCode::decode(&done.login_code).expect("code");
        assert_eq!(code.endpoints, [addr]);

        let (node, admin_addr) = m.start(api).await;
        // The code of `init` is exchanged on the API port with no credential.
        let anonymous = m.client(&code, &Credential::None).await;
        let key = anonymous
            .exchange_login_code(exchange(&code, "laptop"))
            .await
            .expect("exchange");
        assert_eq!(
            (key.user.as_str(), key.org.as_str(), key.project.as_str()),
            ("admin", "default", "")
        );
        assert_eq!(key.name, "laptop");
        let again = anonymous
            .exchange_login_code(exchange(&code, "laptop"))
            .await
            .expect_err("used");
        // The secret matched, so the reason is given (ARFC 0002 section
        // 3.7).
        assert_eq!(again.code(), ErrorCode::PermissionDenied);
        assert!(again.message.starts_with("login_code_used"), "{again:?}");
        let mut wrong = exchange(&code, "laptop");
        wrong.secret[0] ^= 1;
        let wrong = anonymous
            .exchange_login_code(wrong)
            .await
            .expect_err("wrong");
        assert_eq!(wrong.code(), ErrorCode::Unauthenticated);

        // `ListNodes` with the new key's SigV4 signature shows the one node
        // healthy, with every role.
        let credential = Credential::AccessKey(AccessKeyCredential::new(
            &key.access_key_id,
            Zeroizing::new(key.secret.clone()),
        ));
        let admin_user = m.client(&code, &credential).await;
        let nodes = m.all_active(&admin_user).await;
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "linux-1");
        let roles = nodes[0].spec.as_ref().map(|s| s.roles.clone());
        let all: Vec<i32> = [Role::Anchor, Role::Control, Role::Compute, Role::Storage]
            .into_iter()
            .map(i32::from)
            .collect();
        assert_eq!(roles, Some(all));
        // `IssueLoginCode` is refused on the API port.
        let refused = admin_user
            .issue_login_code(IssueLoginCodeRequest {
                secret_hash: vec![0; 32],
                ..IssueLoginCodeRequest::default()
            })
            .await
            .expect_err("not on the API port");
        assert_eq!(refused.code(), ErrorCode::PermissionDenied);

        // The local admin socket lists the node and issues a code for
        // `admin`, which the API port exchanges: the new key takes the next
        // free name.
        let admin = m.admin(admin_addr).await;
        assert_eq!(m.all_active(&admin).await.len(), 1);
        let secret = [5u8; 16];
        let issued = admin
            .issue_login_code(IssueLoginCodeRequest {
                secret_hash: alloy_crypto::hash(&secret).to_vec(),
                ..IssueLoginCodeRequest::default()
            })
            .await
            .expect("issue");
        assert_eq!(issued.endpoints.len(), 1);
        assert_eq!(issued.api_ca_fp, code.api_ca_fp);
        let second = anonymous
            .exchange_login_code(ExchangeLoginCodeRequest {
                code_id: issued.code_id.clone(),
                secret: secret.to_vec(),
                name: "laptop".to_owned(),
                replaces: String::new(),
            })
            .await
            .expect("exchange the issued code");
        assert_eq!(second.name, "laptop-2");
        let on_admin = admin
            .exchange_login_code(exchange(&code, "laptop"))
            .await
            .expect_err("not on the socket");
        assert_eq!(on_admin.code(), ErrorCode::PermissionDenied);
        drop((anonymous, admin_user, admin));
        node.stop().await.expect("stop");

        // A restart serves from the stored state: the node is healthy again
        // and the first key still signs.
        let (api, _) = listen(&m.runtime, addr.port());
        let (node, _) = m.start(api).await;
        let admin_user = m.client(&code, &credential).await;
        let nodes = m.all_active(&admin_user).await;
        assert_eq!(nodes.len(), 1);
        drop(admin_user);
        node.stop().await.expect("stop");

        // A second `init` changes nothing.
        let refused = init(&m.runtime, &m.paths(), &options(addr.port())).await;
        assert!(
            matches!(refused, Err(InitError::Conflict(JoinState::Joined))),
            "{refused:?}"
        );
    });
}

#[test]
fn init_refuses_options_out_of_range_and_writes_nothing() {
    Machine::new("options").run(async |m| {
        let mut none = options(443);
        none.advertise.clear();
        let refused = init(&m.runtime, &m.paths(), &none).await;
        assert!(matches!(refused, Err(InitError::Usage(_))), "{refused:?}");
        let mut bad_name = options(443);
        bad_name.name = "Linux_1".to_owned();
        let refused = init(&m.runtime, &m.paths(), &bad_name).await;
        assert!(matches!(refused, Err(InitError::Usage(_))), "{refused:?}");
        let listed = m
            .runtime
            .disk_io()
            .list(m.location.dir())
            .await
            .expect("list");
        assert!(listed.is_empty(), "{listed:?}");
    });
}

/// A data directory beside the machine's own, for another node on the same
/// host, created empty and removed by [`Other::remove`].
struct Other {
    location: Location,
    native: PathBuf,
}

impl Other {
    async fn new(m: &Machine, name: &str) -> Other {
        let native = temp_data_dir(name, m.runtime.rng().as_ref());
        let location = Location::of(&native).expect("location");
        m.runtime
            .disk_io()
            .create_dir(location.dir())
            .await
            .expect("data directory");
        Other { location, native }
    }

    fn paths(&self, m: &Machine) -> NodePaths {
        NodePaths {
            data: DataDir::new(Arc::clone(m.runtime.disk_io()), self.location.dir().clone()),
            dir: self.location.dir().clone(),
            native: self.native.clone(),
        }
    }

    async fn remove(self, m: &Machine) {
        crate::init::remove_tree(m.runtime.disk_io().as_ref(), self.location.dir())
            .await
            .expect("cleanup");
    }
}

/// A UDP socket on a free loopback port, and the port.
fn mesh_socket(runtime: &Runtime) -> (Box<dyn alloy_rt::UdpSocket>, u16) {
    let socket = runtime
        .net()
        .bind_udp(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            alloy_rt::UdpOptions::default(),
        )
        .expect("bind");
    let port = socket.local_addr().port();
    (socket, port)
}

/// A free UDP port: bound once on loopback, then released.
fn free_udp_port(runtime: &Runtime) -> u16 {
    mesh_socket(runtime).1
}

fn facts(os: &str, virtualization: bool) -> alloy_id::proto::Facts {
    alloy_id::proto::Facts {
        os: os.to_owned(),
        arch: "x86_64".to_owned(),
        cores: 4,
        memory_bytes: 8 << 30,
        virtualization,
        disks: Vec::new(),
    }
}

fn join_options(token: &str, name: &str, os: &str, mesh_port: u16) -> JoinOptions {
    JoinOptions {
        token: Zeroizing::new(token.to_owned()),
        name: name.to_owned(),
        name_is_default: false,
        roles: None,
        zone: None,
        advertise: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        mesh_port,
        api_port: 443,
        facts: facts(os, true),
    }
}

/// Starts the control node of `m` with its API on `api` and its mesh on
/// `mesh`.
async fn start_control(
    m: &Machine,
    api: Box<dyn Listener>,
    mesh: Box<dyn alloy_rt::UdpSocket>,
) -> ControlNode {
    let listeners = Listeners {
        api,
        admin: None,
        mesh: vec![mesh],
    };
    ControlNode::start(&m.runtime, &m.paths(), move |_| Ok(listeners), m.transport)
        .await
        .expect("start")
}

async fn start_member(m: &Machine, other: &Other) -> MemberNode {
    let runtime = m.runtime.clone();
    MemberNode::start(&m.runtime, &other.paths(m), move |record| {
        crate::mesh::bind(&runtime, record.mesh_port).map_err(crate::NodeError::Listen)
    })
    .await
    .expect("member")
}

/// Every node, once `count` of them are `active` and each has a heartbeat
/// accepted at or after `since`, within 40 seconds.
async fn all_heard(m: &Machine, client: &Client, count: usize, since: i64) -> Vec<Node> {
    for _ in 0..400 {
        if let Ok(nodes) = client.list_all_nodes().await {
            let heard = |n: &Node| {
                n.status.as_ref().is_some_and(|s| {
                    s.state == i32::from(NodeState::Active)
                        && s.last_heartbeat_time
                            .as_ref()
                            .is_some_and(|t| t.seconds >= since)
                })
            };
            if nodes.len() == count && nodes.iter().all(heard) {
                return nodes;
            }
        }
        m.runtime.sleep(Duration::from_millis(100)).await;
    }
    let nodes = client.list_all_nodes().await;
    panic!("the nodes were not all active and heard from: {nodes:?}");
}

fn roles_of(node: &Node) -> Vec<i32> {
    node.spec
        .as_ref()
        .map(|s| s.roles.clone())
        .unwrap_or_default()
}

#[test]
fn three_nodes_on_loopback_are_healthy_and_heal_after_a_control_node_restart() {
    Machine::new("cluster").run(async |m| {
        let (api, addr) = listen(&m.runtime, 0);
        let (mesh, mesh_port) = mesh_socket(&m.runtime);
        let mut init_options = options(addr.port());
        init_options.mesh_port = mesh_port;
        let done = init(&m.runtime, &m.paths(), &init_options)
            .await
            .expect("init");
        let control = start_control(m, api, mesh).await;

        // One Linux node and one client node join with the token `init`
        // printed.
        let linux = Other::new(m, "linux-2").await;
        let options = join_options(
            &done.join_token,
            "linux-2",
            "linux",
            free_udp_port(&m.runtime),
        );
        let joined = join(&m.runtime, &linux.paths(m), &options)
            .await
            .expect("the Linux node joins");
        assert_eq!(joined.node, "linux-2");
        assert_eq!(
            joined.roles,
            alloy_id::Roles::of(&[alloy_id::Role::Compute, alloy_id::Role::Storage])
        );
        assert_eq!(joined.zone, "default");
        let laptop = Other::new(m, "laptop").await;
        let options = join_options(
            &done.join_token,
            "laptop",
            "windows",
            free_udp_port(&m.runtime),
        );
        let joined = join(&m.runtime, &laptop.paths(m), &options)
            .await
            .expect("the client node joins");
        assert!(joined.roles.is_empty(), "{joined:?}");
        // A second join on a joined machine changes nothing.
        let again = join(&m.runtime, &laptop.paths(m), &options).await;
        assert!(
            matches!(again, Err(JoinError::Conflict(JoinState::Joined))),
            "{again:?}"
        );

        let linux_node = start_member(m, &linux).await;
        let laptop_node = start_member(m, &laptop).await;

        let code = LoginCode::decode(&done.login_code).expect("code");
        let anonymous = m.client(&code, &Credential::None).await;
        let key = anonymous
            .exchange_login_code(exchange(&code, "laptop"))
            .await
            .expect("exchange");
        let credential = Credential::AccessKey(AccessKeyCredential::new(
            &key.access_key_id,
            Zeroizing::new(key.secret.clone()),
        ));
        let admin = m.client(&code, &credential).await;
        let nodes = all_heard(m, &admin, 3, 0).await;
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["laptop", "linux-1", "linux-2"]);
        assert!(roles_of(&nodes[0]).is_empty());
        assert_eq!(
            roles_of(&nodes[2]),
            [i32::from(Role::Compute), i32::from(Role::Storage)]
        );
        drop((anonymous, admin));

        // The control node restarts from its stored state; the other two
        // make new sessions and are heard from again.
        control.stop().await.expect("stop");
        let restarted = m.runtime.clock().wall().0.div_euclid(1_000_000_000);
        let (api, _) = listen(&m.runtime, addr.port());
        let mesh = m
            .runtime
            .net()
            .bind_udp(
                SocketAddr::from((Ipv4Addr::LOCALHOST, mesh_port)),
                alloy_rt::UdpOptions::default(),
            )
            .expect("the mesh port again");
        let control = start_control(m, api, mesh).await;
        let admin = m.client(&code, &credential).await;
        let nodes = all_heard(m, &admin, 3, restarted).await;
        assert_eq!(nodes.len(), 3);

        // A joiner whose token has another secret is refused, and is left
        // unjoined with its keys.
        let mut wrong = JoinToken::decode(&done.join_token).expect("token");
        wrong.secret = alloy_id::token::TokenSecret::from_bytes([9; 16]);
        let wrong = wrong.encode().expect("encode");
        let stranger = Other::new(m, "stranger").await;
        let options = join_options(&wrong, "stranger", "linux", free_udp_port(&m.runtime));
        let refused = join(&m.runtime, &stranger.paths(m), &options)
            .await
            .expect_err("refused");
        assert!(
            matches!(refused, JoinError::Refused(Refusal::TokenRefused)),
            "{refused:?}"
        );
        assert_eq!(refused.exit_code(), 4);
        let data = stranger.paths(m).data;
        let head = data
            .read(alloy_id::store::HOST_FILE)
            .await
            .expect("read")
            .expect("host file");
        assert_eq!(
            crate::data_dir::parse_host_file(&head).expect("parse"),
            JoinState::Unjoined
        );
        let kept = data
            .open(m.runtime.rng().as_ref())
            .await
            .expect("open")
            .expect("keys kept");
        assert_eq!(kept.secrets.identity_key.len(), 32);
        assert!(kept.secrets.join_token.is_empty());
        assert!(data.read_node_state().await.expect("read").is_none());
        let nodes = admin.list_all_nodes().await.expect("list");
        assert_eq!(nodes.len(), 3);
        drop(admin);

        linux_node.stop().await;
        laptop_node.stop().await;
        control.stop().await.expect("stop");
        for other in [linux, laptop, stranger] {
            other.remove(m).await;
        }
    });
}

#[test]
fn joins_are_refused_by_name_and_compute_and_a_refused_joiner_retries_with_its_keys() {
    Machine::new("refusals").run(async |m| {
        let (api, addr) = listen(&m.runtime, 0);
        let (mesh, mesh_port) = mesh_socket(&m.runtime);
        let mut init_options = options(addr.port());
        init_options.mesh_port = mesh_port;
        let done = init(&m.runtime, &m.paths(), &init_options)
            .await
            .expect("init");
        let control = start_control(m, api, mesh).await;
        let token = done.join_token.as_str();

        let first = Other::new(m, "first").await;
        let options = join_options(token, "linux-2", "linux", free_udp_port(&m.runtime));
        join(&m.runtime, &first.paths(m), &options)
            .await
            .expect("joins");

        // A name given with --name that is in use is refused; the default
        // name takes the lowest free suffix.
        let second = Other::new(m, "second").await;
        let port = free_udp_port(&m.runtime);
        let taken = join(
            &m.runtime,
            &second.paths(m),
            &join_options(token, "linux-2", "linux", port),
        )
        .await
        .expect_err("name in use");
        assert!(
            matches!(taken, JoinError::Refused(Refusal::NameTaken)),
            "{taken:?}"
        );
        assert_eq!(taken.exit_code(), 5);
        let kept = second
            .paths(m)
            .data
            .open(m.runtime.rng().as_ref())
            .await
            .expect("open")
            .expect("keys kept");
        let mut by_default = join_options(token, "linux-2", "linux", port);
        by_default.name_is_default = true;
        let joined = join(&m.runtime, &second.paths(m), &by_default)
            .await
            .expect("the retry joins");
        assert_eq!(joined.node, "linux-2-2");
        let reopened = second
            .paths(m)
            .data
            .open(m.runtime.rng().as_ref())
            .await
            .expect("open")
            .expect("keys");
        // The retry used the keys the refused attempt kept.
        assert_eq!(reopened.secrets.identity_key, kept.secrets.identity_key);
        assert_eq!(reopened.secrets.mesh_key, kept.secrets.mesh_key);
        assert!(reopened.secrets.join_request.is_none());

        // Roles naming compute on a machine without virtualization are
        // refused.
        let third = Other::new(m, "third").await;
        let mut compute = join_options(token, "linux-4", "linux", free_udp_port(&m.runtime));
        compute.roles = Some(alloy_id::Roles::of(&[alloy_id::Role::Compute]));
        compute.facts.virtualization = false;
        let refused = join(&m.runtime, &third.paths(m), &compute)
            .await
            .expect_err("no virtualization");
        assert!(
            matches!(refused, JoinError::Refused(Refusal::ComputeUnsupported)),
            "{refused:?}"
        );
        assert_eq!(refused.exit_code(), 2);
        // A token that does not decode is a usage error, before anything is
        // written.
        let usage = join(
            &m.runtime,
            &third.paths(m),
            &join_options("alyt_nope", "linux-4", "linux", 0),
        )
        .await
        .expect_err("usage");
        assert!(matches!(usage, JoinError::Usage(_)), "{usage:?}");

        control.stop().await.expect("stop");
        for other in [first, second, third] {
            other.remove(m).await;
        }
    });
}
