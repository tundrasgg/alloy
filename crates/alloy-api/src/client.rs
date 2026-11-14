//! The Rust client of the API (ARFC 0008 clause 3.10).
//!
//! [`connect`] opens TLS 1.3 to one API endpoint through the `alloy-rt` net
//! seam and verifies the server chain to the pinned API CA (ARFC 0008 clause
//! 3.8, ARFC 0002 section 3.7), or, under the `mem` `net`, connects in the
//! clear (clause 3.8.1); [`connect_local_admin`] opens the local admin
//! socket (clause 3.7.2). Either gives a [`Client`] whose generated
//! [`ClusterClient`] and [`IamClient`] send each call with its
//! [`Credential`]: none, a session token, or an access key's SigV4
//! signature (clause 3.7.1). Its helpers follow `product/principles.md`
//! A18: iterate a list across
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
use alloy_proto::cluster::v1alpha1::{ListNodesRequest, ListNodesResponse, Node};
use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use alloy_proto::iam::v1alpha1::iam_service_client::IamServiceClient;
use alloy_proto::iam::v1alpha1::{
    ExchangeLoginCodeRequest, ExchangeLoginCodeResponse, IssueLoginCodeRequest,
    IssueLoginCodeResponse,
};
use alloy_rt::{
    BoxFuture, Clock, NetError, Runtime, SpawnError, StreamAddr, StreamOptions, UnixPath, WallTime,
};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::http2::SendRequest;
use tokio::io::{AsyncRead, AsyncWrite};
use tonic::Status;

use crate::access::SessionToken;
use crate::io::{HyperIo, RtExecutor, RtIo, task_name};
use crate::page::MAX_PAGE_SIZE;
use crate::server::MAX_MESSAGE_BYTES;
use crate::sigv4::{self, AccessKeyCredential};
use crate::tls::{ALPN_H2, ApiCaFingerprint, TlsError, client_config};
use crate::transport::Transport;

/// The generated client of the `cluster` service over one connection.
pub type ClusterClient = ClusterServiceClient<Channel>;

/// The generated client of the `iam` service over one connection.
pub type IamClient = IamServiceClient<Channel>;

/// The most pages [`Client::list_all_nodes`] follows: 1,000,000 nodes at
/// 1,000 a page.
pub const MAX_PAGES: usize = 1_000;

/// How a client authenticates its calls (ARFC 0008 clause 3.7.1).
#[derive(Clone, Debug)]
pub enum Credential {
    /// No credential: `ExchangeLoginCode`, and every call through the local
    /// admin socket.
    None,
    /// A session token, sent as `authorization: Bearer <token>`.
    Token(SessionToken),
    /// An access key, which signs each call with SigV4.
    AccessKey(AccessKeyCredential),
}

/// Why a call could not be sent.
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    /// The HTTP/2 connection failed.
    #[error(transparent)]
    Http(#[from] hyper::Error),
    /// The request body could not be read to be signed.
    #[error("the request body could not be read to be signed: {0}")]
    Body(Status),
    /// The signed request could not be built.
    #[error("the signed request could not be built")]
    Header,
}

/// One HTTP/2 connection to the API, adding the credential to every call
/// (ARFC 0008 clause 3.7.1): a session token as `authorization: Bearer
/// <token>`, or an access key's SigV4 signature over the call, its body, and
/// the time on the runtime's wall clock. Clones share the connection.
#[derive(Clone)]
pub struct Channel {
    send: SendRequest<tonic::body::Body>,
    credential: Arc<Credential>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Channel")
            .field("credential", &self.credential)
            .finish_non_exhaustive()
    }
}

impl tower_service::Service<http::Request<tonic::body::Body>> for Channel {
    type Response = http::Response<Incoming>;
    type Error = ChannelError;
    type Future = BoxFuture<'static, Result<http::Response<Incoming>, ChannelError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), ChannelError>> {
        self.send.poll_ready(cx).map_err(ChannelError::Http)
    }

    fn call(&mut self, mut request: http::Request<tonic::body::Body>) -> Self::Future {
        let mut send = self.send.clone();
        let credential = Arc::clone(&self.credential);
        let clock = Arc::clone(&self.clock);
        Box::pin(async move {
            match &*credential {
                Credential::None => {}
                Credential::Token(token) => {
                    // A session token holds only visible ASCII, which a
                    // header value accepts.
                    let mut value =
                        http::HeaderValue::try_from(format!("Bearer {}", token.expose()))
                            .map_err(|_| ChannelError::Header)?;
                    value.set_sensitive(true);
                    request
                        .headers_mut()
                        .insert(http::header::AUTHORIZATION, value);
                }
                Credential::AccessKey(key) => {
                    request = sign(request, key, clock.wall()).await?;
                }
            }
            send.send_request(request).await.map_err(ChannelError::Http)
        })
    }
}

/// `request` signed with `key` at `now` (ARFC 0008 clause 3.7.1): the whole
/// body is read and hashed, and `x-amz-date`, `x-amz-content-sha256`, and
/// `authorization` are set.
async fn sign(
    request: http::Request<tonic::body::Body>,
    key: &AccessKeyCredential,
    now: WallTime,
) -> Result<http::Request<tonic::body::Body>, ChannelError> {
    let (mut parts, body) = request.into_parts();
    let bytes = body.collect().await.map_err(ChannelError::Body)?.to_bytes();
    let amz_date = sigv4::amz_date(now);
    let content_sha256 = sigv4::content_sha256(&bytes);
    let content_type = parts
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let host = parts
        .uri
        .authority()
        .map(|a| a.as_str().to_owned())
        .unwrap_or_default();
    let signed = sigv4::Signed {
        path: parts.uri.path(),
        content_type: &content_type,
        host: &host,
        amz_date: &amz_date,
        content_sha256: &content_sha256,
    };
    let authorization = sigv4::authorization(key, &signed);
    let value = |text: &str| http::HeaderValue::try_from(text).map_err(|_| ChannelError::Header);
    let mut authorization = value(&authorization)?;
    authorization.set_sensitive(true);
    parts.headers.insert(sigv4::DATE_HEADER, value(&amz_date)?);
    parts
        .headers
        .insert(sigv4::CONTENT_HEADER, value(&content_sha256)?);
    parts
        .headers
        .insert(http::header::AUTHORIZATION, authorization);
    Ok(http::Request::from_parts(
        parts,
        tonic::body::Body::new(Full::new(bytes)),
    ))
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
/// then carries `credential`.
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
    credential: &Credential,
) -> Result<Client, ConnectError> {
    let stream = runtime
        .net()
        .connect(StreamAddr::Ip(endpoint), StreamOptions::default())
        .await?;
    let io = RtIo::new(Arc::from(stream));
    if !transport.is_tls() {
        return Client::over(runtime, io, &format!("http://{endpoint}"), credential).await;
    }
    let config = client_config(Arc::clone(runtime.clock()), api_ca_fp)?;
    let tls = config.connect(endpoint.ip(), io).await?;
    if tls.alpn() != Some(ALPN_H2) {
        return Err(ConnectError::NotH2);
    }
    Client::over(runtime, tls, &format!("https://{endpoint}"), credential).await
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
    local_admin_over(runtime, stream).await
}

/// A connection to the API with the helpers of `product/principles.md` A18.
/// Clones share the connection.
#[derive(Clone, Debug)]
pub struct Client {
    channel: Channel,
    cluster: ClusterClient,
    iam: IamClient,
}

/// Speaks the local admin protocol (ARFC 0008 clause 3.7.2) on `stream`, a
/// connection already open to a local admin listener: HTTP/2 by prior
/// knowledge, without TLS. [`connect_local_admin`] opens the socket and
/// calls this.
///
/// # Errors
///
/// [`ConnectError`].
pub async fn local_admin_over(
    runtime: &Runtime,
    stream: Box<dyn alloy_rt::Stream>,
) -> Result<Client, ConnectError> {
    Client::over(
        runtime,
        RtIo::new(Arc::from(stream)),
        "http://localhost",
        &Credential::None,
    )
    .await
}

impl Client {
    pub(crate) async fn over<I>(
        runtime: &Runtime,
        io: I,
        origin: &str,
        credential: &Credential,
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
        let origin = http::Uri::try_from(origin).map_err(|_| ConnectError::Origin)?;
        let channel = Channel {
            send,
            credential: Arc::new(credential.clone()),
            clock: Arc::clone(runtime.clock()),
        };
        let cluster = ClusterServiceClient::with_origin(channel.clone(), origin.clone())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let iam = IamServiceClient::with_origin(channel.clone(), origin)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        Ok(Client {
            channel,
            cluster,
            iam,
        })
    }

    /// The generated client of the `iam` service on this connection.
    #[must_use]
    pub fn iam(&self) -> IamClient {
        self.iam.clone()
    }

    /// Calls `IssueLoginCode`.
    ///
    /// # Errors
    ///
    /// The call's [`Error`].
    pub async fn issue_login_code(
        &self,
        request: IssueLoginCodeRequest,
    ) -> Result<IssueLoginCodeResponse, Error> {
        self.iam()
            .issue_login_code(request)
            .await
            .map(tonic::Response::into_inner)
            .map_err(|s| Error::from_status(&s))
    }

    /// Calls `ExchangeLoginCode`.
    ///
    /// # Errors
    ///
    /// The call's [`Error`].
    pub async fn exchange_login_code(
        &self,
        request: ExchangeLoginCodeRequest,
    ) -> Result<ExchangeLoginCodeResponse, Error> {
        self.iam()
            .exchange_login_code(request)
            .await
            .map(tonic::Response::into_inner)
            .map_err(|s| Error::from_status(&s))
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
