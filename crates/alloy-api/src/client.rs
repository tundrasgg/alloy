//! The Rust client of the API (ARFC 0008 clause 3.10).
//!
//! [`connect`] opens TLS 1.3 to one API endpoint through the `alloy-rt` net
//! seam and verifies the server chain to the pinned API CA (ARFC 0008 clause
//! 3.8, ARFC 0002 section 3.7), or, under the `mem` `net`, connects in the
//! clear (clause 3.8.1); [`connect_local_admin`] opens the local admin
//! socket (clause 3.7.2). Either gives a [`Client`] whose generated
//! [`ClusterClient`] sends each call with the session token, if any, and
//! whose helpers follow `product/principles.md` A18: iterate a list across
//! its pages ([`Client::list_all_nodes`]) and retry `Unavailable` and
//! `RateLimited` with backoff ([`retry`]). Rung 2 has no operations, so the
//! helper that waits for one comes with the first method that returns one.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use alloy_crypto::tls::HandshakeError;
use alloy_proto::cluster::v1alpha1::cluster_service_client::ClusterServiceClient;
use alloy_proto::cluster::v1alpha1::{GetNodeRequest, ListNodesRequest, ListNodesResponse, Node};
use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use alloy_rt::{BoxFuture, NetError, Runtime, SpawnError, StreamAddr, StreamOptions, UnixPath};
use hyper::body::Incoming;
use hyper::client::conn::http2::SendRequest;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::access::SessionToken;
use crate::io::{HyperIo, RtExecutor, RtIo, task_name};
use crate::page::MAX_PAGE_SIZE;
use crate::server::MAX_MESSAGE_BYTES;
use crate::tls::{ALPN_H2, ApiCaFingerprint, TlsError, client_config};
use crate::transport::Transport;

/// The generated client of the `cluster` service over one connection.
pub type ClusterClient = ClusterServiceClient<Channel>;

/// The most pages [`Client::list_all_nodes`] follows: 1,000,000 nodes at
/// 1,000 a page.
pub const MAX_PAGES: usize = 1_000;

/// One HTTP/2 connection to the API, adding the session token, if any, to
/// every call as `authorization: Bearer <token>` (ARFC 0008 clause 3.7.1).
/// Clones share the connection.
#[derive(Clone, Debug)]
pub struct Channel {
    send: SendRequest<tonic::body::Body>,
    authorization: Option<http::HeaderValue>,
}

impl tower_service::Service<http::Request<tonic::body::Body>> for Channel {
    type Response = http::Response<Incoming>;
    type Error = hyper::Error;
    type Future = BoxFuture<'static, Result<http::Response<Incoming>, hyper::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), hyper::Error>> {
        self.send.poll_ready(cx)
    }

    fn call(&mut self, mut request: http::Request<tonic::body::Body>) -> Self::Future {
        if let Some(authorization) = &self.authorization {
            request
                .headers_mut()
                .insert(http::header::AUTHORIZATION, authorization.clone());
        }
        Box::pin(self.send.send_request(request))
    }
}

/// Why a connection to the API could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The stream could not be opened.
    #[error("the API endpoint could not be reached: {0}")]
    Net(#[from] NetError),
    /// The server's certificate chain does not verify to the pinned API CA,
    /// or does not name the address connected to. No request was sent.
    #[error("the API server's certificate does not match the pinned API CA: {0}")]
    Certificate(TlsError),
    /// The TLS handshake failed for another reason: the server's alert, or
    /// an I/O or protocol error.
    #[error(transparent)]
    Tls(HandshakeError),
    /// The server chose no ALPN protocol, or one other than `h2`.
    #[error("the server did not choose HTTP/2")]
    NotH2,
    /// The HTTP/2 handshake failed.
    #[error("the HTTP/2 handshake failed: {0}")]
    Http(#[from] hyper::Error),
    /// The client configuration was refused.
    #[error(transparent)]
    Config(#[from] TlsError),
    /// The task that drives the connection could not be started.
    #[error("the connection task did not start: {0}")]
    Spawn(#[from] SpawnError),
    /// The endpoint gives no valid URI origin.
    #[error("the endpoint is not a valid origin")]
    Origin,
}

impl From<HandshakeError> for ConnectError {
    fn from(error: HandshakeError) -> ConnectError {
        match error {
            HandshakeError::Certificate(e) => ConnectError::Certificate(e),
            other => ConnectError::Tls(other),
        }
    }
}

/// Opens an HTTP/2 connection to the API at `endpoint`, on `transport`,
/// [`Transport::of`] the configuration `runtime` was built from. Each call
/// then carries `token` when one is given.
///
/// On TLS 1.3 the server's chain must verify to a CA certificate whose
/// SubjectPublicKeyInfo has the fingerprint `api_ca_fp` and name
/// `endpoint`'s address among the leaf's `iPAddress` names; otherwise the
/// connection closes having sent no request. In the clear, under the `mem`
/// `net` only, no certificate is presented and `api_ca_fp` is not used.
///
/// # Errors
///
/// [`ConnectError`], [`ConnectError::Certificate`] for a chain that fails the
/// pin or the address.
pub async fn connect(
    runtime: &Runtime,
    transport: Transport,
    endpoint: SocketAddr,
    api_ca_fp: ApiCaFingerprint,
    token: Option<&SessionToken>,
) -> Result<Client, ConnectError> {
    let stream = runtime
        .net()
        .connect(StreamAddr::Ip(endpoint), StreamOptions::default())
        .await?;
    let io = RtIo::new(Arc::from(stream));
    if !transport.is_tls() {
        return Client::over(runtime, io, &format!("http://{endpoint}"), token).await;
    }
    let config = client_config(Arc::clone(runtime.clock()), api_ca_fp)?;
    let tls = config.connect(endpoint.ip(), io).await?;
    if tls.alpn() != Some(ALPN_H2) {
        return Err(ConnectError::NotH2);
    }
    Client::over(runtime, tls, &format!("https://{endpoint}"), token).await
}

/// Opens the local admin socket at `path` (ARFC 0008 clause 3.7.2): HTTP/2 by
/// prior knowledge, without TLS. Each call on it is a local admin call.
///
/// # Errors
///
/// [`ConnectError`].
pub async fn connect_local_admin(
    runtime: &Runtime,
    path: UnixPath,
) -> Result<Client, ConnectError> {
    let stream = runtime
        .net()
        .connect(StreamAddr::Unix(path), StreamOptions::default())
        .await?;
    Client::over(
        runtime,
        RtIo::new(Arc::from(stream)),
        "http://localhost",
        None,
    )
    .await
}

/// A connection to the API with the helpers of `product/principles.md` A18.
/// Clones share the connection.
#[derive(Clone, Debug)]
pub struct Client {
    channel: Channel,
    cluster: ClusterClient,
}

impl Client {
    pub(crate) async fn over<I>(
        runtime: &Runtime,
        io: I,
        origin: &str,
        token: Option<&SessionToken>,
    ) -> Result<Client, ConnectError>
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let executor = RtExecutor::new(runtime.clone(), task_name("api-client-h2"));
        let (send, connection) =
            hyper::client::conn::http2::handshake(executor, HyperIo(io)).await?;
        runtime.spawn(task_name("api-client-conn"), async move {
            if let Err(e) = connection.await {
                tracing::debug!(reason = %e, "an API client connection ended with an error");
            }
        })?;
        // A session token holds only visible ASCII, which a header value
        // accepts, so `try_from` fails for none.
        let authorization = token
            .and_then(|t| http::HeaderValue::try_from(format!("Bearer {}", t.expose())).ok())
            .map(|mut value| {
                value.set_sensitive(true);
                value
            });
        let origin = http::Uri::try_from(origin).map_err(|_| ConnectError::Origin)?;
        let channel = Channel {
            send,
            authorization,
        };
        let cluster = ClusterServiceClient::with_origin(channel.clone(), origin)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        Ok(Client { channel, cluster })
    }

    /// The connection itself, for a call by path through
    /// [`tonic::client::Grpc`]. Request URIs must carry the scheme and
    /// authority of the endpoint.
    #[must_use]
    pub fn channel(&self) -> Channel {
        self.channel.clone()
    }

    /// The generated client of the `cluster` service on this connection.
    #[must_use]
    pub fn cluster(&self) -> ClusterClient {
        self.cluster.clone()
    }

    /// Calls `GetNode`.
    ///
    /// # Errors
    ///
    /// The call's [`Error`].
    pub async fn get_node(&self, name: &str) -> Result<Node, Error> {
        let request = GetNodeRequest {
            name: name.to_owned(),
        };
        self.cluster()
            .get_node(request)
            .await
            .map(tonic::Response::into_inner)
            .map_err(|s| Error::from_status(&s))
    }

    /// Calls `ListNodes` once.
    ///
    /// # Errors
    ///
    /// The call's [`Error`].
    pub async fn list_nodes(&self, request: ListNodesRequest) -> Result<ListNodesResponse, Error> {
        self.cluster()
            .list_nodes(request)
            .await
            .map(tonic::Response::into_inner)
            .map_err(|s| Error::from_status(&s))
    }

    /// Every node, following every page of `ListNodes` in order, 1,000 a
    /// page (ARFC 0019 clause 3.1.15).
    ///
    /// # Errors
    ///
    /// The first failing call's [`Error`], and `Internal` when the server
    /// returns more than [`MAX_PAGES`] pages.
    pub async fn list_all_nodes(&self) -> Result<Vec<Node>, Error> {
        let mut nodes = Vec::new();
        let mut page_token = String::new();
        for _ in 0..MAX_PAGES {
            let page = self
                .list_nodes(ListNodesRequest {
                    page_size: i32::try_from(MAX_PAGE_SIZE).unwrap_or(i32::MAX),
                    page_token,
                    ..ListNodesRequest::default()
                })
                .await?;
            nodes.extend(page.nodes);
            if page.next_page_token.is_empty() {
                return Ok(nodes);
            }
            page_token = page.next_page_token;
        }
        Err(Error::new(
            ErrorCode::Internal,
            "The server returned more than 1,000 pages of nodes, so the list was abandoned.",
        ))
    }
}

/// How [`retry`] retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The most calls, the first included. At least 1.
    pub attempts: u32,
    /// The pause before the first retry. Each later pause doubles it.
    pub initial: Duration,
    /// The longest pause the doubling reaches.
    pub max: Duration,
}

impl Default for RetryPolicy {
    /// 5 attempts, pauses from 200 ms doubling to at most 5 s.
    fn default() -> RetryPolicy {
        RetryPolicy {
            attempts: 5,
            initial: Duration::from_millis(200),
            max: Duration::from_secs(5),
        }
    }
}

/// Runs `call` until it succeeds, fails with a code other than `Unavailable`
/// and `RateLimited`, or has run `policy.attempts` times. Before each retry it
/// waits for the longer of the policy's pause and the error's
/// `retry_delay_duration` on `runtime`'s clock. A mutating call must reuse its
/// idempotency key on every attempt (`product/principles.md` A14).
///
/// # Errors
///
/// The last call's [`Error`].
pub async fn retry<T, F, Fut>(
    runtime: &Runtime,
    policy: RetryPolicy,
    mut call: F,
) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Error>>,
{
    let mut pause = policy.initial;
    let mut attempt = 1;
    loop {
        let error = match call().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let retriable = matches!(
            error.code(),
            ErrorCode::Unavailable | ErrorCode::RateLimited
        );
        if !retriable || attempt >= policy.attempts {
            return Err(error);
        }
        let wait = error.retry_delay().map_or(pause, |d| d.max(pause));
        runtime.sleep(wait).await;
        pause = pause.saturating_mul(2).min(policy.max);
        attempt += 1;
    }
}
