//! In-process client and server over TLS on loopback under the `os` runtime
//! (ADR 0042: TLS is tested under `os`, not the simulator). The clear path of
//! the `mem` `net` runs on the same loopback, since the `mem` `net` serves no
//! stream yet.

use std::collections::BTreeMap;
use std::future::poll_fn;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use alloy_proto::cluster::v1alpha1::{
    GetNodeRequest, ListNodesRequest, Node, NodeSpec, NodeState, NodeStatus, Role,
};
use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use alloy_proto::error::ERROR_TRAILER;
use alloy_rt::{
    BoxFuture, ListenOptions, OsHost, Runtime, RuntimeConfig, StreamAddr, StreamOptions,
    register_os,
};
use alloy_seam::{Binding, ConfigValue, ImplName, RegistryBuilder};
use prost::Message;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::server::ResolvesServerCert;
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::client::{Client, ConnectError, RetryPolicy, connect, retry};
use crate::io::RtIo;
use crate::tls::{ApiCaFingerprint, Protocol, client_config, server_config};
use crate::transport::H2_PREFACE;
use crate::{
    Access, Action, ApiPort, ApiServer, AuthnError, ConnLimits, MAX_MESSAGE_BYTES, NodeName,
    NodeStore, Principal, SessionToken, StreamConn, StreamUnderlay, Transport,
    UNAUTHENTICATED_MESSAGE,
};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";

/// The nodes of a test cluster, by name.
#[derive(Debug, Default)]
struct Nodes(BTreeMap<String, Node>);

impl NodeStore for Nodes {
    fn get<'a>(&'a self, name: &'a NodeName) -> BoxFuture<'a, Result<Option<Node>, Error>> {
        let node = self.0.get(name.as_str()).cloned();
        Box::pin(async move { Ok(node) })
    }

    fn list<'a>(
        &'a self,
        after: Option<&'a NodeName>,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<Node>, Error>> {
        let nodes = self
            .0
            .values()
            .filter(|n| after.is_none_or(|a| n.name.as_str() > a.as_str()))
            .take(limit)
            .cloned()
            .collect();
        Box::pin(async move { Ok(nodes) })
    }
}

/// Alice may do anything, Bob nothing; the local admin may do anything.
#[derive(Debug)]
struct Policy;

impl Access for Policy {
    fn authenticate<'a>(
        &'a self,
        token: &'a SessionToken,
    ) -> BoxFuture<'a, Result<Principal, AuthnError>> {
        let result = match token.expose() {
            ALICE => Ok(Principal::Authenticated("alice".to_owned())),
            BOB => Ok(Principal::Authenticated("bob".to_owned())),
            _ => Err(AuthnError::Refused),
        };
        Box::pin(async move { result })
    }

    fn authorize<'a>(
        &'a self,
        principal: &'a Principal,
        _action: Action,
        resource: &'a str,
    ) -> BoxFuture<'a, Result<(), Error>> {
        let result = match principal {
            Principal::Authenticated(name) if name == "bob" => Err(Error::new(
                ErrorCode::PermissionDenied,
                "You may not read nodes.",
            )
            .with_resource(resource)),
            _ => Ok(()),
        };
        Box::pin(async move { result })
    }
}

fn node(name: &str) -> Node {
    Node {
        name: name.to_owned(),
        uid: vec![7; 16],
        generation: 1,
        spec: Some(NodeSpec {
            roles: vec![Role::Control.into()],
            zone: "default".to_owned(),
            advertise: vec!["192.0.2.1".to_owned()],
        }),
        status: Some(NodeStatus {
            state: NodeState::Active.into(),
            ..NodeStatus::default()
        }),
        ..Node::default()
    }
}

fn runtime(host: &OsHost) -> Runtime {
    let mut builder = RegistryBuilder::new();
    register_os(&mut builder, host).expect("register");
    let os = ImplName::new("os").expect("name");
    let mut config = RuntimeConfig::all(os.clone());
    // `message OsDiskIoConfig { string root = 1; }`; the API uses no file.
    #[derive(Message)]
    struct OsDiskIoConfig {
        #[prost(string, tag = "1")]
        root: String,
    }
    let root = std::env::temp_dir().to_string_lossy().into_owned();
    config.disk_io = Binding {
        name: os,
        config: ConfigValue::new(1, OsDiskIoConfig { root }.encode_to_vec()).expect("config"),
    };
    Runtime::from_registry(&builder.build(), &config).expect("runtime")
}

/// A test API CA and a server certificate it issued for `ip`.
struct Pki {
    ca: CertificateDer<'static>,
    resolver: Arc<dyn ResolvesServerCert>,
}

fn pki(ip: IpAddr) -> Pki {
    let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("ca key");
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Alloy API CA test");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(ca_params, ca_key).expect("ca");

    let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("leaf key");
    let mut leaf_params = CertificateParams::new(Vec::<String>::new()).expect("params");
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "linux-1");
    leaf_params.subject_alt_names = vec![SanType::IpAddress(ip)];
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = leaf_params.signed_by(&leaf_key, &ca).expect("leaf");

    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    let signing = crate::tls::provider()
        .key_provider
        .load_private_key(key)
        .expect("signing key");
    let certified = CertifiedKey::new(vec![leaf.der().clone(), ca.der().clone()], signing);
    Pki {
        ca: ca.der().clone(),
        resolver: Arc::new(SingleCertAndKey::from(certified)),
    }
}

/// The `stream` underlay of the tests: echoes the first bytes it reads.
#[derive(Debug)]
struct Echo;

impl StreamUnderlay for Echo {
    fn serve(&self, mut conn: StreamConn) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let mut buf = [0; 64];
            let mut read = ReadBuf::new(&mut buf);
            let Ok(()) = poll_fn(|cx| Pin::new(&mut conn).poll_read(cx, &mut read)).await else {
                return;
            };
            let mut echo = read.filled().to_vec();
            echo.insert(0, u8::from(conn.is_tls()));
            let mut at = 0;
            while at < echo.len() {
                match poll_fn(|cx| Pin::new(&mut conn).poll_write(cx, &echo[at..])).await {
                    Ok(n) => at += n,
                    Err(_) => return,
                }
            }
            let _ = poll_fn(|cx| Pin::new(&mut conn).poll_shutdown(cx)).await;
        })
    }
}

/// How a test port is set up.
struct Setup {
    nodes: Vec<Node>,
    cert_ip: IpAddr,
    transport: Transport,
    limits: ConnLimits,
    stream: bool,
}

impl Setup {
    /// `count` nodes `node-00`, `node-01`, and so on, the API only, on TLS
    /// with a certificate for `cert_ip`.
    fn new(count: usize, cert_ip: IpAddr) -> Setup {
        Setup {
            nodes: (0..count).map(|i| node(&format!("node-{i:02}"))).collect(),
            cert_ip,
            transport: Transport::TLS,
            limits: ConnLimits::API_PORT,
            stream: false,
        }
    }
}

/// A running API port on loopback.
struct Fixture {
    host: OsHost,
    runtime: Runtime,
    addr: SocketAddr,
    pin: ApiCaFingerprint,
    server: ApiServer,
    transport: Transport,
}

impl Fixture {
    fn start(count: usize, cert_ip: IpAddr) -> Fixture {
        Fixture::with(Setup::new(count, cert_ip))
    }

    fn with(setup: Setup) -> Fixture {
        let host = OsHost::start(2).expect("host");
        let runtime = runtime(&host);
        let nodes = setup
            .nodes
            .into_iter()
            .map(|n| (n.name.clone(), n))
            .collect();
        let server = ApiServer::new(runtime.clone(), Arc::new(Nodes(nodes)), Arc::new(Policy));
        let pki = pki(setup.cert_ip);
        let tls = server_config(
            Arc::clone(runtime.clock()),
            Arc::clone(&pki.resolver),
            Protocol::Api,
        )
        .expect("server config");
        let listener = runtime
            .net()
            .listen(
                StreamAddr::Ip(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
                ListenOptions::default(),
            )
            .expect("listen");
        let StreamAddr::Ip(addr) = listener.local_addr() else {
            panic!("a TCP listener has an IP address");
        };
        let mut port = ApiPort::new(runtime.clone(), setup.transport)
            .with_limits(setup.limits)
            .with_api(server.clone(), tls);
        if setup.stream {
            let tls = server_config(Arc::clone(runtime.clock()), pki.resolver, Protocol::Stream)
                .expect("stream config");
            port = port.with_stream(Arc::new(Echo), tls);
        }
        runtime
            .spawn(crate::io::task_name("test-port"), async move {
                port.serve(listener).await;
            })
            .expect("spawn");
        let pin = ApiCaFingerprint::of_certificate(&pki.ca).expect("fingerprint");
        Fixture {
            host,
            runtime,
            addr,
            pin,
            server,
            transport: setup.transport,
        }
    }

    fn run<F: Future>(&self, future: F) -> F::Output {
        let output = self.host.block_on(future);
        self.host.shutdown(Duration::from_secs(1));
        output
    }

    async fn client(&self, token: Option<&str>) -> Client {
        self.try_client(token).await.expect("connect")
    }

    async fn try_client(&self, token: Option<&str>) -> Result<Client, ConnectError> {
        let token = token.map(|t| SessionToken::new(t).expect("token"));
        connect(
            &self.runtime,
            self.transport,
            self.addr,
            self.pin,
            token.as_ref(),
        )
        .await
    }

    /// Opens a raw stream to the port, writes `bytes`, and reads to its end.
    /// `None` when the port closes it with an error.
    async fn exchange(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        let stream = self
            .runtime
            .net()
            .connect(StreamAddr::Ip(self.addr), StreamOptions::default())
            .await
            .expect("connect");
        let mut at = 0;
        while at < bytes.len() {
            at += stream.write(&[&bytes[at..]]).await.expect("write");
        }
        let mut got = Vec::new();
        let mut buf = [0; 256];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => return Some(got),
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(_) => return None,
            }
        }
    }
}

fn localhost() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

#[test]
fn list_and_get_over_tls() {
    let f = Fixture::start(5, localhost());
    f.run(async {
        let client = f.client(Some(ALICE)).await;
        let mut names = Vec::new();
        let mut page_token = String::new();
        let mut pages = Vec::new();
        loop {
            let page = client
                .list_nodes(ListNodesRequest {
                    page_size: 2,
                    page_token,
                    ..ListNodesRequest::default()
                })
                .await
                .expect("list");
            pages.push(page.nodes.len());
            names.extend(page.nodes.into_iter().map(|n| n.name));
            if page.next_page_token.is_empty() {
                break;
            }
            page_token = page.next_page_token;
        }
        assert_eq!(pages, [2, 2, 1]);
        assert_eq!(
            names,
            ["node-00", "node-01", "node-02", "node-03", "node-04"]
        );

        let all = client.list_all_nodes().await.expect("list all");
        assert_eq!(all.len(), 5);
        assert_eq!(all[4], node("node-04"));
        assert_eq!(
            client.get_node("node-03").await.expect("get"),
            node("node-03")
        );
    });
}

#[test]
fn a_full_page_ends_with_an_empty_token() {
    let f = Fixture::start(2, localhost());
    f.run(async {
        let client = f.client(Some(ALICE)).await;
        let page = client
            .list_nodes(ListNodesRequest {
                page_size: 2,
                ..ListNodesRequest::default()
            })
            .await
            .expect("list");
        assert_eq!(page.nodes.len(), 2);
        assert_eq!(page.next_page_token, "");
    });
}

#[test]
fn errors_carry_their_code_and_trailer() {
    let f = Fixture::start(1, localhost());
    f.run(async {
        let client = f.client(Some(ALICE)).await;
        let missing = client.get_node("absent").await.expect_err("absent");
        assert_eq!(missing.code(), ErrorCode::NotFound);
        assert_eq!(missing.resource, "nodes/absent");
        assert!(missing.next_step.is_some());

        let invalid = client.get_node("Not-A-Name").await.expect_err("invalid");
        assert_eq!(invalid.code(), ErrorCode::InvalidArgument);
        assert_eq!(invalid.field_violations[0].field, "name");

        let status = client
            .cluster()
            .list_nodes(ListNodesRequest {
                page_size: -1,
                page_token: "00ff".to_owned(),
                filter: "zone = a".to_owned(),
                order_by: "zone".to_owned(),
            })
            .await
            .expect_err("invalid list");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.metadata().get_bin(ERROR_TRAILER).is_some());
        let error = Error::from_status(&status);
        assert_eq!(error.message, status.message());
        let fields: Vec<_> = error
            .field_violations
            .iter()
            .map(|v| v.field.as_str())
            .collect();
        assert_eq!(fields, ["page_size", "filter", "order_by", "page_token"]);

        let denied = f.client(Some(BOB)).await.get_node("node-00").await;
        assert_eq!(
            denied.map_err(|e| e.code()),
            Err(ErrorCode::PermissionDenied)
        );
    });
}

#[test]
fn an_unauthenticated_call_is_rejected() {
    let f = Fixture::start(1, localhost());
    f.run(async {
        let mut seen = Vec::new();
        for token in [None, Some("not-a-token")] {
            let status = f
                .client(token)
                .await
                .cluster()
                .list_nodes(ListNodesRequest::default())
                .await
                .expect_err("unauthenticated");
            assert_eq!(status.code(), tonic::Code::Unauthenticated);
            let error = Error::from_status(&status);
            assert_eq!(error.code(), ErrorCode::Unauthenticated);
            assert_eq!(error.message, UNAUTHENTICATED_MESSAGE);
            seen.push(error);
        }
        // The same error, byte for byte, whatever the cause.
        assert_eq!(seen[0].encode_to_vec(), seen[1].encode_to_vec());
    });
}

#[test]
fn a_path_that_is_no_method_is_unimplemented() {
    let f = Fixture::start(1, localhost());
    f.run(async {
        let client = f.client(None).await;
        let uri = format!("https://{}", f.addr);
        let path = http::uri::PathAndQuery::from_static(
            "/alloy.cluster.v1alpha1.ClusterService/DeleteNode",
        );
        let mut grpc =
            tonic::client::Grpc::with_origin(client.channel(), uri.parse().expect("uri"));
        grpc.ready().await.expect("ready");
        let codec = tonic_prost::ProstCodec::<ListNodesRequest, ListNodesRequest>::default();
        let status = grpc
            .unary(
                tonic::Request::new(ListNodesRequest::default()),
                path,
                codec,
            )
            .await
            .expect_err("no such method");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(status.metadata().get_bin(ERROR_TRAILER).is_none());
    });
}

#[test]
fn a_chain_that_fails_the_pin_or_the_address_is_refused() {
    let f = Fixture::start(1, localhost());
    f.run(async {
        let wrong = ApiCaFingerprint([0; 16]);
        let refused = connect(&f.runtime, f.transport, f.addr, wrong, None).await;
        assert!(
            matches!(refused, Err(ConnectError::Certificate(_))),
            "{refused:?}"
        );
    });
    let other_ip = Fixture::start(1, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 9)));
    other_ip.run(async {
        let refused = other_ip.try_client(None).await;
        assert!(
            matches!(refused, Err(ConnectError::Certificate(_))),
            "{refused:?}"
        );
    });
}

#[test]
fn a_client_hello_without_h2_gets_no_application_protocol() {
    let f = Fixture::start(1, localhost());
    f.run(async {
        for offered in [vec![b"http/1.1".to_vec()], vec![]] {
            let config = client_config(Arc::clone(f.runtime.clock()), f.pin).expect("config");
            let mut config = Arc::unwrap_or_clone(config);
            config.alpn_protocols = offered;
            let stream = f
                .runtime
                .net()
                .connect(StreamAddr::Ip(f.addr), StreamOptions::default())
                .await
                .expect("connect");
            let refused = tokio_rustls::TlsConnector::from(Arc::new(config))
                .connect(
                    ServerName::IpAddress(localhost().into()),
                    RtIo::new(Arc::from(stream)),
                )
                .await
                .expect_err("refused");
            let alert = refused
                .get_ref()
                .and_then(|e| e.downcast_ref::<rustls::Error>())
                .cloned();
            assert_eq!(
                alert,
                Some(rustls::Error::AlertReceived(
                    rustls::AlertDescription::NoApplicationProtocol
                ))
            );
        }
    });
}

#[test]
fn the_local_admin_socket_takes_no_credential() {
    let f = Fixture::start(3, localhost());
    f.run(async {
        // The socket is a Unix path in production; any stream listener
        // serves the same protocol.
        let listener = f
            .runtime
            .net()
            .listen(
                StreamAddr::Ip(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
                ListenOptions::default(),
            )
            .expect("listen");
        let StreamAddr::Ip(addr) = listener.local_addr() else {
            panic!("a TCP listener has an IP address");
        };
        let server = f.server.clone();
        f.runtime
            .spawn(crate::io::task_name("test-admin"), async move {
                server
                    .serve_local_admin(listener, ConnLimits::LOCAL_ADMIN)
                    .await;
            })
            .expect("spawn");
        let stream = f
            .runtime
            .net()
            .connect(StreamAddr::Ip(addr), StreamOptions::default())
            .await
            .expect("connect");
        let client = Client::over(
            &f.runtime,
            RtIo::new(Arc::from(stream)),
            "http://localhost",
            None,
        )
        .await
        .expect("h2");
        assert_eq!(client.list_all_nodes().await.expect("list").len(), 3);
    });
}

#[test]
fn connections_are_bounded_and_idle_ones_close() {
    let idle = Duration::from_millis(400);
    let f = Fixture::with(Setup {
        limits: ConnLimits {
            max_connections: 2,
            handshake: Duration::from_secs(10),
            idle,
        },
        ..Setup::new(1, localhost())
    });
    f.run(async {
        let alice = f.client(Some(ALICE)).await;
        assert!(alice.get_node("node-00").await.is_ok());
        // A connection with no credential that never calls holds a place.
        let _silent = f.client(None).await;
        // A third while two are open is closed at once, before TLS.
        let refused = f.try_client(Some(ALICE)).await;
        assert!(
            matches!(refused, Err(ConnectError::Tls(_) | ConnectError::Net(_))),
            "{refused:?}"
        );

        f.runtime.sleep(idle * 3).await;
        // Both idle connections were closed, and their places came back.
        assert!(alice.get_node("node-00").await.is_err());
        let again = f.client(Some(ALICE)).await;
        assert!(again.get_node("node-00").await.is_ok());
    });
}

#[test]
fn a_call_in_progress_keeps_its_connection() {
    let idle = Duration::from_millis(300);
    let f = Fixture::with(Setup {
        limits: ConnLimits {
            idle,
            ..ConnLimits::API_PORT
        },
        ..Setup::new(1, localhost())
    });
    f.run(async {
        let client = f.client(Some(ALICE)).await;
        // Calls closer together than `idle` keep the connection open well
        // past it.
        for _ in 0..6 {
            assert!(client.get_node("node-00").await.is_ok());
            f.runtime.sleep(idle / 3).await;
        }
    });
}

#[test]
fn a_response_over_4_mib_is_invalid_argument() {
    let mut big = node("node-00");
    if let Some(spec) = big.spec.as_mut() {
        spec.zone = "z".repeat(MAX_MESSAGE_BYTES);
    }
    let f = Fixture::with(Setup {
        nodes: vec![big],
        ..Setup::new(0, localhost())
    });
    f.run(async {
        let client = f.client(Some(ALICE)).await;
        let get = client
            .cluster()
            .get_node(GetNodeRequest {
                name: "node-00".to_owned(),
            })
            .await
            .expect_err("too large");
        let list = client
            .cluster()
            .list_nodes(ListNodesRequest::default())
            .await
            .expect_err("too large");
        for status in [get, list] {
            assert_eq!(status.code(), tonic::Code::InvalidArgument);
            assert!(status.metadata().get_bin(ERROR_TRAILER).is_some());
            assert_eq!(
                Error::from_status(&status).code(),
                ErrorCode::InvalidArgument
            );
        }
    });
}

#[test]
fn no_response_carries_a_date_header() {
    let f = Fixture::start(1, localhost());
    f.run(async {
        let client = f.client(Some(ALICE)).await;
        let found = client
            .cluster()
            .get_node(GetNodeRequest {
                name: "node-00".to_owned(),
            })
            .await
            .expect("get");
        assert!(found.metadata().get("date").is_none());
        let missing = client
            .cluster()
            .get_node(GetNodeRequest {
                name: "absent".to_owned(),
            })
            .await
            .expect_err("absent");
        assert!(missing.metadata().get("date").is_none());
    });
}

#[test]
fn in_the_clear_the_preface_tells_the_api_from_stream() {
    let f = Fixture::with(Setup {
        transport: Transport::CLEAR,
        stream: true,
        ..Setup::new(3, localhost())
    });
    f.run(async {
        // The client speaks HTTP/2 without TLS, and the token still counts.
        let client = f.client(Some(ALICE)).await;
        assert_eq!(client.list_all_nodes().await.expect("list").len(), 3);
        let status = f
            .client(None)
            .await
            .cluster()
            .list_nodes(ListNodesRequest::default())
            .await
            .expect_err("no token");
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        // Bytes that leave the preface go to `stream`, unchanged and in the
        // clear, the bytes read to decide included.
        let echoed = f.exchange(b"PRI not h2").await.expect("echo");
        assert_eq!(echoed, b"\0PRI not h2");
    });

    let api_only = Fixture::with(Setup {
        transport: Transport::CLEAR,
        ..Setup::new(1, localhost())
    });
    api_only.run(async {
        // A port without `stream` closes it having sent nothing.
        let closed = api_only.exchange(b"AL hello").await;
        assert!(closed.as_ref().is_none_or(Vec::is_empty), "{closed:?}");
        // A TLS client gets no TLS in the clear.
        let tls = connect(
            &api_only.runtime,
            Transport::TLS,
            api_only.addr,
            api_only.pin,
            None,
        )
        .await;
        assert!(tls.is_err(), "{tls:?}");
    });
}

#[test]
fn on_tls_the_preface_alone_gets_no_api() {
    let f = Fixture::with(Setup {
        stream: true,
        ..Setup::new(1, localhost())
    });
    f.run(async {
        let answer = f.exchange(H2_PREFACE).await;
        // Not a ClientHello: at most a TLS alert record (type 21) comes
        // back, and neither HTTP/2 nor `stream` sees the bytes.
        assert!(
            answer
                .as_ref()
                .is_none_or(|bytes| bytes.first().is_none_or(|&t| t == 21)),
            "{answer:?}"
        );
    });
}

#[test]
fn retry_stops_at_success_or_a_final_error() {
    let f = Fixture::start(0, localhost());
    f.run(async {
        let policy = RetryPolicy {
            attempts: 3,
            initial: Duration::from_millis(1),
            max: Duration::from_millis(2),
        };
        let mut calls = 0;
        let result = retry(&f.runtime, policy, || {
            calls += 1;
            let n = calls;
            async move {
                if n < 2 {
                    Err(Error::new(ErrorCode::Unavailable, "later"))
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(result, Ok(2));

        let mut calls = 0;
        let result: Result<(), Error> = retry(&f.runtime, policy, || {
            calls += 1;
            async { Err(Error::new(ErrorCode::RateLimited, "slow down")) }
        })
        .await;
        assert_eq!(result.map_err(|e| e.code()), Err(ErrorCode::RateLimited));
        assert_eq!(calls, 3);

        let mut calls = 0;
        let result: Result<(), Error> = retry(&f.runtime, policy, || {
            calls += 1;
            async { Err(Error::new(ErrorCode::NotFound, "gone")) }
        })
        .await;
        assert_eq!(result.map_err(|e| e.code()), Err(ErrorCode::NotFound));
        assert_eq!(calls, 1);
    });
}

#[test]
fn the_fingerprint_covers_the_whole_spki() {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("key");
    let cert = CertificateParams::new(Vec::<String>::new())
        .expect("params")
        .self_signed(&key)
        .expect("cert");
    let spki = rcgen::PublicKeyData::subject_public_key_info(&key);
    assert_eq!(spki.len(), 91);
    assert_eq!(
        ApiCaFingerprint::of_certificate(cert.der()),
        Some(ApiCaFingerprint::of_spki(&spki))
    );
}
