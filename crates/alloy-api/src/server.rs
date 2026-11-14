//! The API server: the API port with TLS and ALPN, and the local admin socket
//! (ARFC 0008 clauses 3.7.2, 3.8, 3.8.1, and 3.12).

use std::convert::Infallible;
use std::fmt;
use std::future::{Future, poll_fn};
use std::net::IpAddr;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use alloy_crypto::tls::{
    ClientHello, Hello, HelloReader, NO_APPLICATION_PROTOCOL_ALERT, ServerStream, ServerTls,
};
use alloy_proto::cluster::v1alpha1::cluster_service_server::ClusterServiceServer;
use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use alloy_proto::iam::v1alpha1::iam_service_server::IamServiceServer;
use alloy_rt::{BoxFuture, Instant, Listener, Runtime, Stream, StreamAddr};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Frame, Incoming, SizeHint};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tonic::Status;
use tonic::codegen::Service as _;

use crate::access::{self, Access, AuthnError, Principal};
use crate::conn::{Gate, InProgress, NotStarted, Place, Pools, Source};
use crate::iam::{ExchangeLimit, LoginCodes};
use crate::io::{HyperIo, RtExecutor, RtIo, task_name};
use crate::service::{ClusterApi, IamApi, Origin};
use crate::sigv4;
use crate::store::NodeStore;
use crate::tls::{Protocol, select_protocol};
use crate::transport::{AdminLimits, ConnLimits, Slots, Transport, sniff};

/// The largest request or response message, 4 MiB (ARFC 0008 section 7).
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// The gRPC length prefix before each message (ARFC 0008 clause 3.12).
const PREFIX_BYTES: usize = 5;

/// The pause after a failed accept, so that a listener at its socket limit
/// does not spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The most concurrent calls on one HTTP/2 connection.
const MAX_STREAMS: u32 = 100;

/// How long a connection closed at one of its limits has to answer the
/// calls it already received (`ConnLimits`, `AdminLimits`). It takes no new
/// call meanwhile, and is dropped when they are answered or this has passed.
pub const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// The methods M11 rung 2 serves (ARFC 0002 section 3.15). Every other path
/// fails with `UNIMPLEMENTED` and no Alloy code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    ListNodes,
    IssueLoginCode,
    ExchangeLoginCode,
}

impl Method {
    /// The method whose gRPC path is `path` (ARFC 0008 clause 3.12).
    fn of(path: &str) -> Option<Method> {
        match path {
            "/alloy.cluster.v1alpha1.ClusterService/ListNodes" => Some(Method::ListNodes),
            "/alloy.iam.v1alpha1.IamService/IssueLoginCode" => Some(Method::IssueLoginCode),
            "/alloy.iam.v1alpha1.IamService/ExchangeLoginCode" => Some(Method::ExchangeLoginCode),
            _ => None,
        }
    }
}

/// How a connection's calls are authenticated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Caller {
    /// Over the API port: each call carries a session token or a SigV4
    /// signature, except `ExchangeLoginCode`, which the login code alone
    /// authenticates.
    Token,
    /// Through the local admin socket: each call is a local admin call.
    LocalAdmin,
}

/// The user id whose connections the local admin socket serves (ARFC 0007
/// clause 3.10.2).
const ROOT_UID: u32 = 0;

/// The gRPC methods of the API over a node store, the login codes, and an
/// access subsystem. Clones share them.
#[derive(Clone)]
pub struct ApiServer {
    runtime: Runtime,
    cluster: ClusterServiceServer<ClusterApi>,
    iam: IamServiceServer<IamApi>,
    access: Arc<dyn Access>,
    exchanges: Arc<ExchangeLimit>,
}

impl fmt::Debug for ApiServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiServer")
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl ApiServer {
    /// A server of the `cluster` service over `store` and the `iam` service
    /// over `codes`, authenticating and authorizing with `access`, starting
    /// its tasks on `runtime`.
    #[must_use]
    pub fn new(
        runtime: Runtime,
        store: Arc<dyn NodeStore>,
        codes: Arc<dyn LoginCodes>,
        access: Arc<dyn Access>,
    ) -> ApiServer {
        let api = ClusterApi {
            store,
            access: Arc::clone(&access),
        };
        // Each service refuses a response over 4 MiB with `InvalidArgument`
        // before encoding it; this limit only backs that check.
        let cluster = ClusterServiceServer::new(api)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let iam = IamServiceServer::new(IamApi {
            codes,
            access: Arc::clone(&access),
        })
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let exchanges = Arc::new(ExchangeLimit::new(runtime.clock().now()));
        ApiServer {
            runtime,
            cluster,
            iam,
            access,
            exchanges,
        }
    }

    /// Serves the local admin socket (ARFC 0008 clause 3.7.2, ARFC 0007
    /// clause 3.10.2): a connection whose peer the kernel does not report
    /// as user id 0, or whose peer's user id cannot be read, is closed
    /// before a byte of it is read, and the refusal logged. Every other
    /// connection `listener` accepts is HTTP/2 by prior knowledge, without
    /// TLS, and each call on it is a local admin call whatever headers it
    /// carries. `limits` bound the connections, normally
    /// [`AdminLimits::LOCAL_ADMIN`]: one accepted while
    /// [`max_connections`](AdminLimits::max_connections) are open is closed
    /// at once, and one with no call in progress for
    /// [`idle`](AdminLimits::idle) is closed, a call counting as in progress
    /// for at most [`call`](AdminLimits::call). Runs until dropped.
    pub async fn serve_local_admin(&self, listener: Box<dyn Listener>, limits: AdminLimits) {
        let slots = Slots::new(limits.max_connections);
        loop {
            let stream = accept(&self.runtime, listener.as_ref()).await;
            match stream.peer_uid() {
                Ok(ROOT_UID) => {}
                Ok(uid) => {
                    tracing::warn!(
                        uid,
                        "a local admin connection was refused: its peer is not user id 0"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        reason = %e,
                        "a local admin connection was refused: its peer's user id cannot be read"
                    );
                    continue;
                }
            }
            let Some(slot) = slots.take() else {
                tracing::debug!(
                    max = limits.max_connections,
                    "a local admin connection was closed: the socket is at its connection limit"
                );
                continue;
            };
            let server = self.clone();
            let clock = Arc::clone(self.runtime.clock());
            let gate = Arc::new(Gate::proven(clock, limits.idle, limits.call));
            spawn_connection(&self.runtime, async move {
                let io = RtIo::new(Arc::from(stream));
                server.serve_h2(io, Caller::LocalAdmin, gate, None).await;
                drop(slot);
            });
        }
    }

    /// Serves HTTP/2 on one connection from `source` until it closes, or
    /// until `gate` says it must close.
    async fn serve_h2<I>(&self, io: I, caller: Caller, gate: Arc<Gate>, source: Option<IpAddr>)
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let router = Router {
            server: self.clone(),
            caller,
            gate: Arc::clone(&gate),
            source,
        };
        let executor = RtExecutor::new(self.runtime.clone(), task_name("api-h2"));
        // No `date` header: hyper would read it from the system clock, outside
        // `alloy-rt` (`agent/eng/rust.md` rule 2).
        let connection = hyper::server::conn::http2::Builder::new(executor)
            .max_concurrent_streams(MAX_STREAMS)
            .auto_date_header(false)
            .serve_connection(HyperIo(io), router);
        let mut connection = pin!(connection);
        match first(connection.as_mut(), gate.closed()).await {
            Some(Err(e)) => tracing::debug!(reason = %e, "an API connection ended with an error"),
            Some(Ok(())) => {}
            None if gate.was_evicted() => {
                // An unproven connection whose place was taken runs no
                // authenticated call, so it is dropped at once, not given
                // the grace, and its place is free for the newer one
                // (T-0118 re-review F1).
                tracing::debug!("an unproven API connection was closed: its place was taken");
            }
            None => {
                tracing::debug!("an API connection was closed at one of its limits");
                // GOAWAY, and the calls in progress are answered for at most
                // `CLOSE_GRACE`. A call the client sent before it read the
                // GOAWAY is refused by the gate, which is closing. Dropping
                // the connection then closes it.
                connection.as_mut().graceful_shutdown();
                let grace = self.runtime.sleep(CLOSE_GRACE);
                let _ = first(connection, grace).await;
            }
        }
    }

    /// Answers one call from `source`. A call on a connection that must
    /// close is refused unread. A call that names no method or fails
    /// authentication is rejected ([`ConnLimits`]); every other
    /// authenticated call is in progress until its response is sent.
    /// `ExchangeLoginCode` on the API port is authenticated by its login code
    /// alone: it neither proves its connection nor counts as in progress, and
    /// it is rejected when it fails.
    async fn call(
        self,
        request: http::Request<Incoming>,
        caller: Caller,
        gate: &Arc<Gate>,
        source: Option<IpAddr>,
    ) -> Response {
        if !gate.admit() {
            return respond(closing().into_status().into_http(), None);
        }
        let Some(method) = Method::of(request.uri().path()) else {
            gate.reject();
            return respond(Status::unimplemented("").into_http(), None);
        };
        let (mut parts, body) = request.into_parts();
        if method == Method::ExchangeLoginCode && caller == Caller::Token {
            return self.exchange(parts, body, gate, source).await;
        }
        let (principal, read) = match caller {
            Caller::LocalAdmin => (Principal::LocalAdmin, Read::Pending(body)),
            Caller::Token => match self.authenticate(&parts, body).await {
                Ok(done) => done,
                Err(error) => {
                    gate.reject();
                    return respond(error.into_status().into_http(), None);
                }
            },
        };
        let call = match gate.begin(&principal) {
            Ok(call) => call,
            Err(NotStarted::Full) => {
                gate.reject();
                return respond(port_full().into_status().into_http(), None);
            }
            Err(NotStarted::Closing) => {
                return respond(closing().into_status().into_http(), None);
            }
        };
        let body = match read {
            Read::Done(bytes) => bytes,
            Read::Pending(body) => match collect(body).await {
                Ok(bytes) => bytes,
                Err(status) => return respond(status.into_http(), Some(call)),
            },
        };
        parts.extensions.insert(Origin {
            principal: Some(principal),
            source,
        });
        let request = http::Request::from_parts(parts, Full::<Bytes>::new(body));
        let response = match method {
            Method::ListNodes => self.cluster.clone().call(request).await,
            Method::IssueLoginCode | Method::ExchangeLoginCode => {
                self.iam.clone().call(request).await
            }
        };
        match response {
            Ok(response) => respond(response, Some(call)),
            Err(never) => match never {},
        }
    }

    /// Serves `ExchangeLoginCode` on the API port: the rate limit first,
    /// then the call with no principal. A refused or failed exchange is
    /// rejected.
    async fn exchange(
        self,
        mut parts: http::request::Parts,
        body: Incoming,
        gate: &Arc<Gate>,
        source: Option<IpAddr>,
    ) -> Response {
        let now = self.runtime.clock().now();
        if let Err(error) = self.exchanges.take(source, now) {
            gate.reject();
            return respond(error.into_status().into_http(), None);
        }
        let body = match collect(body).await {
            Ok(bytes) => bytes,
            Err(status) => {
                gate.reject();
                return respond(status.into_http(), None);
            }
        };
        parts.extensions.insert(Origin {
            principal: None,
            source,
        });
        let request = http::Request::from_parts(parts, Full::<Bytes>::new(body));
        let response = match self.iam.clone().call(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
        // An error is sent trailers-only, its status in the headers; a
        // success carries its status in the trailers.
        let failed = response
            .headers()
            .get("grpc-status")
            .is_some_and(|status| status.as_bytes() != b"0");
        if failed {
            gate.reject();
        }
        respond(response, None)
    }

    /// Authenticates a call on the API port by its `authorization` header:
    /// a session token, checked by [`Access::authenticate`], or a SigV4
    /// signature, verified against the secret of the key it names, which
    /// needs the whole body. Returns the principal and the body, read or
    /// not.
    async fn authenticate(
        &self,
        parts: &http::request::Parts,
        body: Incoming,
    ) -> Result<(Principal, Read), Error> {
        let header = parts.headers.get(http::header::AUTHORIZATION);
        let sigv4 = header
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.starts_with(sigv4::ALGORITHM));
        let Some(value) = sigv4 else {
            let token = access::bearer(header)?;
            let principal = match self.access.authenticate(&token).await {
                Ok(principal) => principal,
                Err(AuthnError::Refused) => return Err(access::unauthenticated()),
                Err(AuthnError::Failed(error)) if error.code() == ErrorCode::Unauthenticated => {
                    return Err(access::unauthenticated());
                }
                Err(AuthnError::Failed(error)) => return Err(error),
            };
            return Ok((principal, Read::Pending(body)));
        };
        let authorization =
            sigv4::parse_authorization(value).ok_or_else(access::unauthenticated)?;
        let holder = match self.access.access_key(&authorization.key_id).await {
            Ok(holder) => holder,
            Err(AuthnError::Refused) => return Err(access::unauthenticated()),
            Err(AuthnError::Failed(error)) if error.code() == ErrorCode::Unauthenticated => {
                return Err(access::unauthenticated());
            }
            Err(AuthnError::Failed(error)) => return Err(error),
        };
        // A body that does not arrive or is over 4 MiB cannot hash to the
        // signed value, so it fails as any call that does not verify.
        let bytes = collect(body).await.map_err(|_| access::unauthenticated())?;
        let header_text = |name: &str| {
            parts
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
        };
        let host = parts
            .uri
            .authority()
            .map(http::uri::Authority::as_str)
            .unwrap_or_default();
        let signed = sigv4::Signed {
            path: parts.uri.path(),
            content_type: header_text(http::header::CONTENT_TYPE.as_str()),
            host,
            amz_date: header_text(sigv4::DATE_HEADER),
            content_sha256: header_text(sigv4::CONTENT_HEADER),
        };
        let now = self.runtime.clock().wall();
        if sigv4::verify(&holder.secret, &authorization, &signed, &bytes, now) {
            Ok((holder.principal, Read::Done(bytes)))
        } else {
            Err(access::unauthenticated())
        }
    }
}

/// A call's body, read in full or still to read.
enum Read {
    Done(Bytes),
    Pending(Incoming),
}

/// The whole body of a call, which no method streams from the client, or
/// the status that ends the call: `InvalidArgument` over 4 MiB, `CANCELLED`
/// when the body does not arrive.
async fn collect(body: Incoming) -> Result<Bytes, Status> {
    match Limited::new(body, MAX_MESSAGE_BYTES + PREFIX_BYTES)
        .collect()
        .await
    {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e) if e.is::<http_body_util::LengthLimitError>() => Err(too_large().into_status()),
        Err(_) => Err(Status::cancelled("the request body was not received")),
    }
}

type GrpcResponse = http::Response<tonic::body::Body>;

/// A response whose body keeps its call in progress until it is sent.
type Response = http::Response<CallBody>;

fn too_large() -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        "The request is larger than 4 MiB, so it was refused. Send a smaller request.",
    )
}

/// The pause a client is told to wait when the API count is full.
const PORT_FULL_RETRY: Duration = Duration::from_secs(1);

fn port_full() -> Error {
    Error::new(
        ErrorCode::Unavailable,
        "The API port is serving as many connections as it can, in all or for this caller, so the \
         call was refused. Try again shortly, or send calls on a connection already open.",
    )
    .with_retry_delay(PORT_FULL_RETRY)
}

fn closing() -> Error {
    Error::new(
        ErrorCode::Unavailable,
        "The connection is closing, so the call was not served. Send it again on a new connection.",
    )
}

/// `response`, whose body holds `call` in progress until it is sent.
fn respond(response: GrpcResponse, call: Option<InProgress>) -> Response {
    response.map(|body| CallBody { body, _call: call })
}

/// The HTTP/2 service of one connection.
#[derive(Clone)]
struct Router {
    server: ApiServer,
    caller: Caller,
    gate: Arc<Gate>,
    source: Option<IpAddr>,
}

impl hyper::service::Service<http::Request<Incoming>> for Router {
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Response, Infallible>>;

    fn call(&self, request: http::Request<Incoming>) -> Self::Future {
        let server = self.server.clone();
        let caller = self.caller;
        let gate = Arc::clone(&self.gate);
        let source = self.source;
        Box::pin(async move { Ok(server.call(request, caller, &gate, source).await) })
    }
}

/// A response body that holds its call in progress until hyper has sent it
/// or dropped it.
struct CallBody {
    body: tonic::body::Body,
    _call: Option<InProgress>,
}

impl hyper::body::Body for CallBody {
    type Data = Bytes;
    type Error = Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Status>>> {
        Pin::new(&mut self.get_mut().body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

/// The output of `a`, or `None` when `b` completes first. Polls `a` first.
pub(crate) async fn first<A: Future, B: Future>(a: A, b: B) -> Option<A::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if let Poll::Ready(output) = a.as_mut().poll(cx) {
            return Poll::Ready(Some(output));
        }
        b.as_mut().poll(cx).map(|_| None)
    })
    .await
}

/// The `stream` underlay as the API port hands it connections (ARFC 0004
/// section 3.6).
pub trait StreamUnderlay: Send + Sync + fmt::Debug + 'static {
    /// Serves one connection that chose `stream`, until it closes. The
    /// returned future must own the connection: the port closes it by
    /// dropping the future. The connection is unproven until the underlay
    /// calls [`StreamConn::prove`], and the port closes it if that has not
    /// happened [`prove`](ConnLimits::prove) after accept, or when its
    /// unproven place is taken for a newer connection ([`ConnLimits`]).
    fn serve(&self, conn: StreamConn) -> BoxFuture<'static, ()>;
}

/// A connection on the API port that chose `stream`: by ALPN `alloy-al/1` on
/// TLS, or by its first bytes in the clear (ARFC 0008 clause 3.8.1).
#[derive(Debug)]
pub struct StreamConn {
    conn: Conn,
    gate: Arc<Gate>,
}

#[derive(Debug)]
enum Conn {
    Tls(ServerStream<RtIo>),
    Clear(RtIo),
}

impl StreamConn {
    /// Whether the connection runs on TLS.
    #[must_use]
    pub fn is_tls(&self) -> bool {
        matches!(self.conn, Conn::Tls(_))
    }

    /// Proves the connection: the underlay calls it once the peer has
    /// authenticated, by completing its AL handshake (ARFC 0003). Its place
    /// moves from the port's unproven count to its
    /// [`stream`](ConnLimits::stream) count, and the port no longer closes
    /// it. `false` when the `stream` count is full or the connection was
    /// already closed at a bound: the port then closes it, and the underlay
    /// should stop serving it. `true` again on a proven connection.
    #[must_use]
    pub fn prove(&self) -> bool {
        self.gate.prove_stream()
    }
}

impl AsyncRead for StreamConn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut self.get_mut().conn {
            Conn::Tls(tls) => Pin::new(tls).poll_read(cx, buf),
            Conn::Clear(io) => Pin::new(io).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for StreamConn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut self.get_mut().conn {
            Conn::Tls(tls) => Pin::new(tls).poll_write(cx, data),
            Conn::Clear(io) => Pin::new(io).poll_write(cx, data),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.get_mut().conn {
            Conn::Tls(tls) => Pin::new(tls).poll_flush(cx),
            Conn::Clear(io) => Pin::new(io).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.get_mut().conn {
            Conn::Tls(tls) => Pin::new(tls).poll_shutdown(cx),
            Conn::Clear(io) => Pin::new(io).poll_shutdown(cx),
        }
    }
}

/// The API port (ARFC 0008 clause 3.8): one TCP listener that serves the API
/// on a node that holds `control` and the `stream` underlay on a node that
/// holds `anchor`.
///
/// On TLS it tells them apart by the ClientHello before choosing a
/// configuration: `h2` for the API, `alloy-al/1` for `stream`, and the alert
/// `no_application_protocol` for every other ClientHello. In the clear, under
/// the `mem` `net` only, it tells them apart by the HTTP/2 preface and closes
/// a connection whose protocol it does not serve (clause 3.8.1).
///
/// Its connections are bounded as [`ConnLimits`] says.
#[derive(Clone, Debug)]
pub struct ApiPort {
    runtime: Runtime,
    transport: Transport,
    limits: ConnLimits,
    api: Option<(ApiServer, ServerTls)>,
    stream: Option<(Arc<dyn StreamUnderlay>, ServerTls)>,
}

impl ApiPort {
    /// A port on `transport`, [`Transport::of`] the configuration `runtime`
    /// was built from, with [`ConnLimits::API_PORT`], that serves nothing
    /// yet: every connection is refused.
    #[must_use]
    pub fn new(runtime: Runtime, transport: Transport) -> ApiPort {
        ApiPort {
            runtime,
            transport,
            limits: ConnLimits::API_PORT,
            api: None,
            stream: None,
        }
    }

    /// Bounds the port's connections by `limits` instead.
    #[must_use]
    pub fn with_limits(mut self, limits: ConnLimits) -> ApiPort {
        self.limits = limits;
        self
    }

    /// Serves the API, with `tls` from
    /// [`server_config`](crate::tls::server_config) for [`Protocol::Api`].
    /// In the clear `tls` is not used.
    #[must_use]
    pub fn with_api(mut self, server: ApiServer, tls: ServerTls) -> ApiPort {
        self.api = Some((server, tls));
        self
    }

    /// Serves the `stream` underlay, with `tls` from
    /// [`server_config`](crate::tls::server_config) for
    /// [`Protocol::Stream`]. In the clear `tls` is not used.
    #[must_use]
    pub fn with_stream(mut self, underlay: Arc<dyn StreamUnderlay>, tls: ServerTls) -> ApiPort {
        self.stream = Some((underlay, tls));
        self
    }

    /// Serves every connection `listener` accepts, each in its own task,
    /// within the bounds of [`ConnLimits`]. Runs until dropped.
    pub async fn serve(&self, listener: Box<dyn Listener>) {
        let pools = Pools::new(self.limits);
        loop {
            let stream = accept(&self.runtime, listener.as_ref()).await;
            let accepted = self.runtime.clock().now();
            let Some(place) = pools.admit(Source::of(stream.peer_addr())) else {
                tracing::debug!(
                    unproven = self.limits.unproven,
                    per_source = self.limits.unproven_per_source,
                    "an API port connection was closed: the unproven connections are at their bound"
                );
                continue;
            };
            let port = self.clone();
            spawn_connection(&self.runtime, async move {
                let stream = Arc::from(stream);
                if port.transport.is_tls() {
                    port.tls_connection(stream, place, accepted).await;
                } else {
                    port.clear_connection(stream, place, accepted).await;
                }
            });
        }
    }

    /// Serves one connection on TLS. The ClientHello and the handshake share
    /// one deadline, [`handshake`](ConnLimits::handshake) after accept, and
    /// end early if the connection's place is taken.
    async fn tls_connection(self, stream: Arc<dyn Stream>, place: Place, accepted: Instant) {
        let deadline = accepted.saturating_add(self.limits.handshake);
        let timer = self.runtime.clock().sleep_until(deadline);
        let evicted = place.evicted();
        let end = first(timer, evicted.wait());
        let from = peer_ip(stream.as_ref());
        let Some(Some((protocol, tls))) = first(self.tls_handshake(stream), end).await else {
            return;
        };
        self.hand_off(protocol, Conn::Tls(tls), place, accepted, from)
            .await;
    }

    /// Reads the ClientHello, chooses the protocol by its ALPN, and completes
    /// the handshake with that protocol's configuration. `None` when the
    /// connection is refused or fails first.
    async fn tls_handshake(
        &self,
        stream: Arc<dyn Stream>,
    ) -> Option<(Protocol, ServerStream<RtIo>)> {
        let hello = client_hello(&stream).await?;
        let chosen = select_protocol(
            hello.alpn().as_deref(),
            self.api.is_some(),
            self.stream.is_some(),
        );
        let config = match chosen {
            Some(Protocol::Api) => self.api.as_ref().map(|(_, tls)| tls),
            Some(Protocol::Stream) => self.stream.as_ref().map(|(_, tls)| tls),
            None => None,
        };
        let (Some(protocol), Some(config)) = (chosen, config) else {
            // A failed alert write changes nothing: the connection closes.
            let _ = write_all(&stream, &NO_APPLICATION_PROTOCOL_ALERT).await;
            return None;
        };
        let tls = hello.handshake(RtIo::new(stream), config).await.ok()?;
        Some((protocol, tls))
    }

    /// Serves one connection in the clear (ARFC 0008 clause 3.8.1). Each API
    /// call still carries a session token.
    async fn clear_connection(self, stream: Arc<dyn Stream>, place: Place, accepted: Instant) {
        let deadline = accepted.saturating_add(self.limits.handshake);
        let timer = self.runtime.clock().sleep_until(deadline);
        let evicted = place.evicted();
        let end = first(timer, evicted.wait());
        let Some(Some((chosen, seen))) = first(sniff(&stream), end).await else {
            return;
        };
        let from = peer_ip(stream.as_ref());
        let io = RtIo::with_unread(stream, seen);
        self.hand_off(chosen, Conn::Clear(io), place, accepted, from)
            .await;
    }

    /// Serves a connection from `from` whose protocol is chosen, unproven
    /// until a call or the underlay proves it.
    async fn hand_off(
        self,
        protocol: Protocol,
        conn: Conn,
        place: Place,
        accepted: Instant,
        from: Option<IpAddr>,
    ) {
        let clock = Arc::clone(self.runtime.clock());
        let gate = Arc::new(Gate::unproven(clock, place, accepted, &self.limits));
        match (protocol, self.api, self.stream) {
            (Protocol::Api, Some((server, _)), _) => match conn {
                Conn::Tls(tls) => server.serve_h2(tls, Caller::Token, gate, from).await,
                Conn::Clear(io) => server.serve_h2(io, Caller::Token, gate, from).await,
            },
            (Protocol::Stream, _, Some((underlay, _))) => {
                let conn = StreamConn {
                    conn,
                    gate: Arc::clone(&gate),
                };
                if first(underlay.serve(conn), gate.closed()).await.is_none() {
                    tracing::debug!(
                        max = self.limits.stream,
                        "a stream connection was closed: it was not proven in time, its place was \
                         taken, or the port is at its stream bound"
                    );
                }
            }
            _ => {}
        }
    }
}

/// The IP address of `stream`'s peer, if it has one.
fn peer_ip(stream: &dyn Stream) -> Option<IpAddr> {
    match stream.peer_addr() {
        Some(StreamAddr::Ip(addr)) => Some(addr.ip()),
        _ => None,
    }
}

/// The next connection `listener` accepts. A failed accept is retried after
/// a pause, so that a listener at its socket limit does not spin.
async fn accept(runtime: &Runtime, listener: &dyn Listener) -> Box<dyn Stream> {
    loop {
        match listener.accept().await {
            Ok(stream) => return stream,
            Err(e) => {
                tracing::debug!(reason = %e, "accepting an API connection failed");
                runtime.sleep(ACCEPT_BACKOFF).await;
            }
        }
    }
}

/// Serves one connection in its own task. A task the spawn seam refuses is
/// dropped, which closes the connection.
fn spawn_connection<F>(runtime: &Runtime, task: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    if let Err(e) = runtime.spawn(task_name("api-conn"), task) {
        tracing::debug!(reason = %e, "an API connection was not served");
    }
}

/// Reads until a whole ClientHello has arrived. `None` when the stream ends or
/// fails first or the bytes are no ClientHello; those are answered with the
/// alert `rustls` chose, if any.
async fn client_hello(stream: &Arc<dyn Stream>) -> Option<ClientHello> {
    let mut reader = HelloReader::new();
    let mut buf = vec![0; 16 * 1024];
    loop {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        match reader.feed(&buf[..n]) {
            Hello::Incomplete => {}
            Hello::Complete(hello) => return Some(hello),
            Hello::Refused { alert } => {
                if !alert.is_empty() {
                    // A failed alert write changes nothing: the connection closes.
                    let _ = write_all(stream, &alert).await;
                }
                return None;
            }
        }
    }
}

async fn write_all(stream: &Arc<dyn Stream>, bytes: &[u8]) -> Result<(), alloy_rt::NetError> {
    let mut at = 0;
    while at < bytes.len() {
        at += stream.write(&[&bytes[at..]]).await?;
    }
    stream.shutdown_write().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_carry_their_codes() {
        assert_eq!(too_large().code(), ErrorCode::InvalidArgument);
        assert_eq!(port_full().code(), ErrorCode::Unavailable);
        assert_eq!(port_full().retry_delay(), Some(PORT_FULL_RETRY));
    }
}
