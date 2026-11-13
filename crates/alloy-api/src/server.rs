//! The API server: the API port with TLS and ALPN, and the local admin socket
//! (ARFC 0008 clauses 3.7.2, 3.8, 3.8.1, and 3.12).

use std::convert::Infallible;
use std::fmt;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use alloy_proto::cluster::v1alpha1::cluster_service_server::ClusterServiceServer;
use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use alloy_rt::{BoxFuture, Clock, Instant, Listener, Runtime, Stream};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Frame, Incoming, SizeHint};
use rustls::ServerConfig;
use rustls::server::{Accepted, Acceptor};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_rustls::server::{StartHandshake, TlsStream};
use tonic::Status;
use tonic::codegen::Service as _;

use crate::access::{self, Access, AuthnError, Principal};
use crate::io::{HyperIo, RtExecutor, RtIo, task_name};
use crate::service::ClusterApi;
use crate::store::NodeStore;
use crate::tls::{NO_APPLICATION_PROTOCOL_ALERT, Protocol, select_protocol};
use crate::transport::{ConnLimits, Slots, Transport, sniff};

/// The largest request or response message, 4 MiB (ARFC 0008 section 7).
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// The gRPC length prefix before each message (ARFC 0008 clause 3.12).
const PREFIX_BYTES: usize = 5;

/// The pause after a failed accept, so that a listener at its socket limit
/// does not spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The most concurrent calls on one HTTP/2 connection.
const MAX_STREAMS: u32 = 100;

/// The paths of the methods this server serves (ARFC 0008 clause 3.12).
const METHODS: [&str; 2] = [
    "/alloy.cluster.v1alpha1.ClusterService/GetNode",
    "/alloy.cluster.v1alpha1.ClusterService/ListNodes",
];

/// How a connection's calls are authenticated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Caller {
    /// Over the API port: each call carries a session token.
    Token,
    /// Through the local admin socket: each call is a local admin call.
    LocalAdmin,
}

/// The gRPC methods of the API over a node store and an access subsystem.
/// Clones share them.
#[derive(Clone)]
pub struct ApiServer {
    runtime: Runtime,
    cluster: ClusterServiceServer<ClusterApi>,
    access: Arc<dyn Access>,
}

impl fmt::Debug for ApiServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiServer")
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl ApiServer {
    /// A server of the `cluster` service over `store`, authenticating and
    /// authorizing with `access`, starting its tasks on `runtime`.
    #[must_use]
    pub fn new(runtime: Runtime, store: Arc<dyn NodeStore>, access: Arc<dyn Access>) -> ApiServer {
        let api = ClusterApi {
            store,
            access: Arc::clone(&access),
        };
        // The service refuses a response over 4 MiB with `InvalidArgument`
        // before encoding it; this limit only backs that check.
        let cluster = ClusterServiceServer::new(api)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        ApiServer {
            runtime,
            cluster,
            access,
        }
    }

    /// Serves the local admin socket (ARFC 0008 clause 3.7.2): every
    /// connection `listener` accepts is HTTP/2 by prior knowledge, without
    /// TLS, and each call on it is a local admin call whatever headers it
    /// carries. `limits` bound the connections, normally
    /// [`ConnLimits::LOCAL_ADMIN`]. Runs until dropped.
    pub async fn serve_local_admin(&self, listener: Box<dyn Listener>, limits: ConnLimits) {
        accept_loop(&self.runtime, listener, limits, |stream| {
            let server = self.clone();
            async move {
                let io = RtIo::new(Arc::from(stream));
                server.serve_h2(io, Caller::LocalAdmin, limits.idle).await;
            }
        })
        .await;
    }

    /// Serves HTTP/2 on one connection until it closes, or until no call
    /// has been in progress on it for `idle`.
    async fn serve_h2<I>(&self, io: I, caller: Caller, idle: Duration)
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let activity = Arc::new(Activity::new(Arc::clone(self.runtime.clock())));
        let router = Router {
            server: self.clone(),
            caller,
            activity: Arc::clone(&activity),
        };
        let executor = RtExecutor::new(self.runtime.clone(), task_name("api-h2"));
        // No `date` header: hyper would read it from the system clock, outside
        // `alloy-rt` (`agent/eng/rust.md` rule 2).
        let connection = hyper::server::conn::http2::Builder::new(executor)
            .max_concurrent_streams(MAX_STREAMS)
            .auto_date_header(false)
            .serve_connection(HyperIo(io), router);
        // Dropping the connection at the idle limit closes it. No call is in
        // progress then, so no response is cut short.
        match first(connection, idle_for(&self.runtime, activity, idle)).await {
            Some(Err(e)) => tracing::debug!(reason = %e, "an API connection ended with an error"),
            Some(Ok(())) => {}
            None => tracing::debug!("an idle API connection was closed"),
        }
    }

    /// Answers one call.
    async fn call(self, request: http::Request<Incoming>, caller: Caller) -> GrpcResponse {
        if !METHODS.contains(&request.uri().path()) {
            return Status::unimplemented("").into_http();
        }
        let principal = match caller {
            Caller::LocalAdmin => Principal::LocalAdmin,
            Caller::Token => {
                match self
                    .authenticate(request.headers().get(http::header::AUTHORIZATION))
                    .await
                {
                    Ok(principal) => principal,
                    Err(error) => return error.into_status().into_http(),
                }
            }
        };
        let (mut parts, body) = request.into_parts();
        // No method streams from the client, so the body is one message.
        let body = match Limited::new(body, MAX_MESSAGE_BYTES + PREFIX_BYTES)
            .collect()
            .await
        {
            Ok(collected) => collected.to_bytes(),
            Err(e) if e.is::<http_body_util::LengthLimitError>() => {
                return too_large().into_status().into_http();
            }
            Err(_) => return Status::cancelled("the request body was not received").into_http(),
        };
        parts.extensions.insert(principal);
        let request = http::Request::from_parts(parts, Full::<Bytes>::new(body));
        match self.cluster.clone().call(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        }
    }

    async fn authenticate(&self, header: Option<&http::HeaderValue>) -> Result<Principal, Error> {
        let token = access::bearer(header)?;
        match self.access.authenticate(&token).await {
            Ok(principal) => Ok(principal),
            Err(AuthnError::Refused) => Err(access::unauthenticated()),
            Err(AuthnError::Failed(error)) if error.code() == ErrorCode::Unauthenticated => {
                Err(access::unauthenticated())
            }
            Err(AuthnError::Failed(error)) => Err(error),
        }
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

/// The HTTP/2 service of one connection.
#[derive(Clone)]
struct Router {
    server: ApiServer,
    caller: Caller,
    activity: Arc<Activity>,
}

impl hyper::service::Service<http::Request<Incoming>> for Router {
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Response, Infallible>>;

    fn call(&self, request: http::Request<Incoming>) -> Self::Future {
        let server = self.server.clone();
        let caller = self.caller;
        let call = Activity::begin(&self.activity);
        Box::pin(async move {
            let response = server.call(request, caller).await;
            Ok(response.map(|body| CallBody { body, _call: call }))
        })
    }
}

/// The calls in progress on one HTTP/2 connection, and since when none has
/// been.
#[derive(Debug)]
struct Activity {
    clock: Arc<dyn Clock>,
    /// The calls in progress, and the time the count last fell to 0 or the
    /// connection began.
    state: Mutex<(usize, Instant)>,
}

impl Activity {
    fn new(clock: Arc<dyn Clock>) -> Activity {
        let now = clock.now();
        Activity {
            clock,
            state: Mutex::new((0, now)),
        }
    }

    /// Counts one call as in progress until the returned guard drops.
    fn begin(this: &Arc<Activity>) -> InProgress {
        this.state.lock().unwrap_or_else(PoisonError::into_inner).0 += 1;
        InProgress(Arc::clone(this))
    }

    /// Since when no call has been in progress, or `None` while one is.
    fn idle_since(&self) -> Option<Instant> {
        let (calls, since) = *self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (calls == 0).then_some(since)
    }
}

/// One call in progress on a connection.
#[derive(Debug)]
struct InProgress(Arc<Activity>);

impl Drop for InProgress {
    fn drop(&mut self) {
        let now = self.0.clock.now();
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = state.0.saturating_sub(1);
        if state.0 == 0 {
            state.1 = now;
        }
    }
}

/// Completes once no call has been in progress on the connection of
/// `activity` for `idle`: at most `idle` after the last call ended, or `2 ×
/// idle` when the last call ended while it waited on one in progress.
async fn idle_for(runtime: &Runtime, activity: Arc<Activity>, idle: Duration) {
    let clock = Arc::clone(runtime.clock());
    loop {
        let deadline = match activity.idle_since() {
            Some(since) => since.saturating_add(idle),
            None => clock.now().saturating_add(idle),
        };
        clock.sleep_until(deadline).await;
        if let Some(since) = activity.idle_since()
            && clock.now().saturating_duration_since(since) >= idle
        {
            return;
        }
    }
}

/// A response body that holds its call in progress until hyper has sent it
/// or dropped it.
struct CallBody {
    body: tonic::body::Body,
    _call: InProgress,
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
async fn first<A: Future, B: Future<Output = ()>>(a: A, b: B) -> Option<A::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if let Poll::Ready(output) = a.as_mut().poll(cx) {
            return Poll::Ready(Some(output));
        }
        b.as_mut().poll(cx).map(|()| None)
    })
    .await
}

/// The `stream` underlay as the API port hands it connections (ARFC 0004
/// section 3.6).
pub trait StreamUnderlay: Send + Sync + fmt::Debug + 'static {
    /// Serves one connection that chose `stream`, until it closes. The
    /// connection holds its place in the port's
    /// [`max_connections`](ConnLimits::max_connections) until then, and the
    /// underlay bounds its idle time.
    fn serve(&self, conn: StreamConn) -> BoxFuture<'static, ()>;
}

/// A connection on the API port that chose `stream`: by ALPN `alloy-al/1` on
/// TLS, or by its first bytes in the clear (ARFC 0008 clause 3.8.1).
#[derive(Debug)]
pub struct StreamConn(Conn);

#[derive(Debug)]
enum Conn {
    Tls(Box<TlsStream<RtIo>>),
    Clear(RtIo),
}

impl StreamConn {
    /// Whether the connection runs on TLS.
    #[must_use]
    pub fn is_tls(&self) -> bool {
        matches!(self.0, Conn::Tls(_))
    }
}

impl AsyncRead for StreamConn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut self.get_mut().0 {
            Conn::Tls(tls) => Pin::new(tls.as_mut()).poll_read(cx, buf),
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
        match &mut self.get_mut().0 {
            Conn::Tls(tls) => Pin::new(tls.as_mut()).poll_write(cx, data),
            Conn::Clear(io) => Pin::new(io).poll_write(cx, data),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.get_mut().0 {
            Conn::Tls(tls) => Pin::new(tls.as_mut()).poll_flush(cx),
            Conn::Clear(io) => Pin::new(io).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.get_mut().0 {
            Conn::Tls(tls) => Pin::new(tls.as_mut()).poll_shutdown(cx),
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
#[derive(Clone, Debug)]
pub struct ApiPort {
    runtime: Runtime,
    transport: Transport,
    limits: ConnLimits,
    api: Option<(ApiServer, Arc<ServerConfig>)>,
    stream: Option<(Arc<dyn StreamUnderlay>, Arc<ServerConfig>)>,
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
    pub fn with_api(mut self, server: ApiServer, tls: Arc<ServerConfig>) -> ApiPort {
        self.api = Some((server, tls));
        self
    }

    /// Serves the `stream` underlay, with `tls` from
    /// [`server_config`](crate::tls::server_config) for
    /// [`Protocol::Stream`]. In the clear `tls` is not used.
    #[must_use]
    pub fn with_stream(
        mut self,
        underlay: Arc<dyn StreamUnderlay>,
        tls: Arc<ServerConfig>,
    ) -> ApiPort {
        self.stream = Some((underlay, tls));
        self
    }

    /// Serves every connection `listener` accepts, each in its own task, at
    /// most [`max_connections`](ConnLimits::max_connections) at once. Runs
    /// until dropped.
    pub async fn serve(&self, listener: Box<dyn Listener>) {
        accept_loop(&self.runtime, listener, self.limits, |stream| {
            let port = self.clone();
            async move {
                let stream = Arc::from(stream);
                if port.transport.is_tls() {
                    port.tls_connection(stream).await;
                } else {
                    port.clear_connection(stream).await;
                }
            }
        })
        .await;
    }

    async fn tls_connection(self, stream: Arc<dyn Stream>) {
        let hello = self
            .runtime
            .timeout(self.limits.handshake, client_hello(&stream))
            .await;
        let Ok(Some(accepted)) = hello else {
            return;
        };
        let alpn = accepted.client_hello().alpn();
        let chosen = select_protocol(alpn, self.api.is_some(), self.stream.is_some());
        let config = match chosen {
            Some(Protocol::Api) => self.api.as_ref().map(|(_, tls)| Arc::clone(tls)),
            Some(Protocol::Stream) => self.stream.as_ref().map(|(_, tls)| Arc::clone(tls)),
            None => None,
        };
        let Some(config) = config else {
            // A failed alert write changes nothing: the connection closes.
            let _ = write_all(&stream, &NO_APPLICATION_PROTOCOL_ALERT).await;
            return;
        };
        let handshake = StartHandshake::from_parts(accepted, RtIo::new(stream)).into_stream(config);
        let Ok(Ok(tls)) = self.runtime.timeout(self.limits.handshake, handshake).await else {
            return;
        };
        match (chosen, self.api, self.stream) {
            (Some(Protocol::Api), Some((server, _)), _) => {
                server.serve_h2(tls, Caller::Token, self.limits.idle).await;
            }
            (Some(Protocol::Stream), _, Some((underlay, _))) => {
                underlay.serve(StreamConn(Conn::Tls(Box::new(tls)))).await;
            }
            _ => {}
        }
    }

    /// Serves one connection in the clear (ARFC 0008 clause 3.8.1). Each API
    /// call still carries a session token.
    async fn clear_connection(self, stream: Arc<dyn Stream>) {
        let sniffed = self
            .runtime
            .timeout(self.limits.handshake, sniff(&stream))
            .await;
        let Ok(Some((chosen, seen))) = sniffed else {
            return;
        };
        let io = RtIo::with_unread(stream, seen);
        match (chosen, self.api, self.stream) {
            (Protocol::Api, Some((server, _)), _) => {
                server.serve_h2(io, Caller::Token, self.limits.idle).await;
            }
            (Protocol::Stream, _, Some((underlay, _))) => {
                underlay.serve(StreamConn(Conn::Clear(io))).await;
            }
            _ => {}
        }
    }
}

/// Accepts connections until dropped, starting `serve` for each in its own
/// task while fewer than `limits.max_connections` are open, and closing it at
/// once otherwise.
async fn accept_loop<F, Fut>(
    runtime: &Runtime,
    listener: Box<dyn Listener>,
    limits: ConnLimits,
    serve: F,
) where
    F: Fn(Box<dyn Stream>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let slots = Slots::new(limits.max_connections);
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let Some(slot) = slots.take() else {
                    tracing::debug!(
                        max = limits.max_connections,
                        "an API connection was closed: the listener is at its connection limit"
                    );
                    drop(stream);
                    continue;
                };
                let served = serve(stream);
                let task = async move {
                    served.await;
                    drop(slot);
                };
                if let Err(e) = runtime.spawn(task_name("api-conn"), task) {
                    tracing::debug!(reason = %e, "an API connection was not served");
                }
            }
            Err(e) => {
                tracing::debug!(reason = %e, "accepting an API connection failed");
                runtime.sleep(ACCEPT_BACKOFF).await;
            }
        }
    }
}

/// Reads until a whole ClientHello has arrived. `None` when the stream ends or
/// fails first or the ClientHello is malformed; a malformed one is answered
/// with the alert `rustls` chose.
async fn client_hello(stream: &Arc<dyn Stream>) -> Option<Accepted> {
    let mut acceptor = Acceptor::default();
    let mut buf = vec![0; 16 * 1024];
    loop {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        let mut chunk = &buf[..n];
        while !chunk.is_empty() {
            if acceptor.read_tls(&mut chunk).ok()? == 0 {
                return None;
            }
        }
        match acceptor.accept() {
            Ok(Some(accepted)) => return Some(accepted),
            Ok(None) => {}
            Err((_, mut alert)) => {
                let mut bytes = Vec::new();
                if alert.write_all(&mut bytes).is_ok() {
                    // A failed alert write changes nothing: the connection closes.
                    let _ = write_all(stream, &bytes).await;
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
    fn the_alert_is_no_application_protocol() {
        // RFC 8446 section 6: AlertDescription no_application_protocol(120),
        // AlertLevel fatal(2), in a record of ContentType alert(21).
        assert_eq!(
            NO_APPLICATION_PROTOCOL_ALERT,
            [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x78]
        );
        assert_eq!(too_large().code(), ErrorCode::InvalidArgument);
    }
}
