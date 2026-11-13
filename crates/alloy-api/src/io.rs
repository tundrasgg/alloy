//! Adapters between the `alloy-rt` seams and the I/O traits of `tokio`,
//! `tokio-rustls`, and `hyper`.
//!
//! Every byte the API sends or receives goes through an `alloy-rt`
//! [`Stream`], and every task it starts goes through the `alloy-rt` spawn
//! seam: `hyper` and `tokio-rustls` see only these adapters.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use alloy_rt::{BoxFuture, NetError, Runtime, Stream, TaskName};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The most bytes one read of the underlying stream asks for, and the most
/// bytes one write takes from the caller.
const CHUNK: usize = 16 * 1024;

/// A pending read: the buffer it fills and its result.
type Reading = BoxFuture<'static, (Vec<u8>, Result<usize, NetError>)>;

/// An `alloy-rt` [`Stream`] as a `tokio` [`AsyncRead`] and [`AsyncWrite`].
///
/// A write is accepted into an owned buffer of at most 16 KiB and completes
/// in the background; its error, if any, is returned by the next write,
/// flush, or shutdown. Reads take at most 16 KiB from the stream at a time
/// and keep what the caller's buffer could not hold for the next read.
pub(crate) struct RtIo {
    stream: Arc<dyn Stream>,
    reading: Option<Reading>,
    /// Bytes read from the stream and not yet given to the caller.
    unread: Vec<u8>,
    writing: Option<BoxFuture<'static, Result<(), NetError>>>,
    shutting_down: Option<BoxFuture<'static, Result<(), NetError>>>,
}

impl std::fmt::Debug for RtIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtIo")
            .field("stream", &self.stream)
            .finish_non_exhaustive()
    }
}

impl RtIo {
    pub(crate) fn new(stream: Arc<dyn Stream>) -> RtIo {
        RtIo::with_unread(stream, Vec::new())
    }

    /// `stream`, whose first reads give `unread`, bytes already read from it,
    /// before any byte still in the stream.
    pub(crate) fn with_unread(stream: Arc<dyn Stream>, unread: Vec<u8>) -> RtIo {
        RtIo {
            stream,
            reading: None,
            unread,
            writing: None,
            shutting_down: None,
        }
    }

    /// Drives the pending write to completion.
    fn poll_written(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(writing) = self.writing.as_mut() {
            let result = ready!(writing.as_mut().poll(cx));
            self.writing = None;
            result.map_err(to_io)?;
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for RtIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.unread.is_empty() {
            if buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            let reading = this.reading.get_or_insert_with(|| {
                let stream = Arc::clone(&this.stream);
                Box::pin(async move {
                    let mut chunk = vec![0; CHUNK];
                    let result = stream.read(&mut chunk).await;
                    (chunk, result)
                })
            });
            let (mut chunk, result) = ready!(reading.as_mut().poll(cx));
            this.reading = None;
            let n = result.map_err(to_io)?;
            chunk.truncate(n);
            this.unread = chunk;
        }
        let n = this.unread.len().min(buf.remaining());
        buf.put_slice(&this.unread[..n]);
        this.unread.drain(..n);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for RtIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        ready!(this.poll_written(cx))?;
        if data.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let owned = data[..data.len().min(CHUNK)].to_vec();
        let n = owned.len();
        let stream = Arc::clone(&this.stream);
        let mut writing: BoxFuture<'static, Result<(), NetError>> = Box::pin(async move {
            let mut at = 0;
            while at < owned.len() {
                at += stream.write(&[&owned[at..]]).await?;
            }
            Ok(())
        });
        // Start the write now; its outcome is collected by the next call.
        match writing.as_mut().poll(cx) {
            Poll::Ready(result) => result.map_err(to_io)?,
            Poll::Pending => this.writing = Some(writing),
        }
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().poll_written(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_written(cx))?;
        let stream = Arc::clone(&this.stream);
        let shutting_down = this
            .shutting_down
            .get_or_insert_with(|| Box::pin(async move { stream.shutdown_write().await }));
        let result = ready!(shutting_down.as_mut().poll(cx));
        this.shutting_down = None;
        Poll::Ready(result.map_err(to_io))
    }
}

/// A `tokio` reader and writer as a `hyper` one.
#[derive(Debug)]
pub(crate) struct HyperIo<T>(pub(crate) T);

impl<T: AsyncRead + Unpin> hyper::rt::Read for HyperIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let mut chunk = [0; CHUNK];
        let len = buf.remaining().min(CHUNK);
        let mut read = ReadBuf::new(&mut chunk[..len]);
        ready!(Pin::new(&mut self.get_mut().0).poll_read(cx, &mut read))?;
        buf.put_slice(read.filled());
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncWrite + Unpin> hyper::rt::Write for HyperIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, data)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

/// Runs the tasks `hyper` starts, one per HTTP/2 connection and one per
/// stream, on the `alloy-rt` spawn seam, detached. A task the seam refuses,
/// at its task limit or while shutting down, is dropped, which closes the
/// connection or resets the stream it serves.
#[derive(Clone, Debug)]
pub(crate) struct RtExecutor {
    runtime: Runtime,
    name: TaskName,
}

impl RtExecutor {
    pub(crate) fn new(runtime: Runtime, name: TaskName) -> RtExecutor {
        RtExecutor { runtime, name }
    }
}

impl<F> hyper::rt::Executor<F> for RtExecutor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, future: F) {
        if let Err(e) = self.runtime.spawn(self.name.clone(), future) {
            tracing::debug!(reason = %e, "an HTTP/2 task was not started");
        }
    }
}

/// A task name the API uses: a fixed string within the limits of
/// [`TaskName::new`].
pub(crate) fn task_name(name: &'static str) -> TaskName {
    // Every caller passes a short ASCII literal, which `TaskName::new` accepts.
    TaskName::new(name).expect("a short literal task name is valid")
}

fn to_io(error: NetError) -> io::Error {
    let kind = match error {
        NetError::ConnectionReset => io::ErrorKind::ConnectionReset,
        NetError::Closed => io::ErrorKind::BrokenPipe,
        NetError::TimedOut => io::ErrorKind::TimedOut,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}
