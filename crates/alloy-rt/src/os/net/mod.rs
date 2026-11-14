//! The `os` net (ARFC 0005 clauses 3.1.7 to 3.1.9): `tokio` sockets, UDP
//! through `quinn-udp`, and the platform's interface list.

use std::fmt;
use std::future::ready;
use std::io::{self, IoSlice};
use std::net::{IpAddr, Shutdown, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use alloy_seam::Detail;
use socket2::{Domain, Protocol, SockRef, Socket, Type};

use super::errors;
use super::{Host, Permit, Stopped};
use crate::{
    BoxFuture, Interface, InterfaceList, ListenOptions, Listener, Net, NetError, Stream,
    StreamAddr, StreamOptions, UdpOptions, UdpSocket, UnixPath,
};

mod ifaces;
mod udp;

/// The most open UDP sockets of one instance (ARFC 0005 section 7).
const MAX_UDP: usize = 4_096;
/// The most open streams and listeners of one instance (ARFC 0005 section 7).
const MAX_STREAMS: usize = 65_536;
/// How often a wait for an interface change reads the list again. ARFC 0005
/// clause 3.1.7 asks for a change to be reported within 1 s.
const INTERFACE_POLL: Duration = Duration::from_millis(250);
/// The most buffers one vectored write hands the platform; Linux refuses more
/// than 1,024 (`IOV_MAX`).
const MAX_WRITE_SLICES: usize = 1_024;

/// Seam `net`, implementation `os`.
pub(super) struct OsNet {
    host: Host,
    interfaces: Arc<Interfaces>,
    udp: Arc<AtomicUsize>,
    streams: Arc<AtomicUsize>,
}

/// Writes the counts of open sockets.
impl fmt::Debug for OsNet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsNet")
            .field("udp", &self.udp)
            .field("streams", &self.streams)
            .finish_non_exhaustive()
    }
}

impl OsNet {
    pub(super) fn new(host: Host) -> OsNet {
        OsNet {
            host,
            interfaces: Arc::new(Interfaces::default()),
            udp: Arc::new(AtomicUsize::new(0)),
            streams: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn listen_ip(&self, addr: SocketAddr, backlog: i32, permit: Permit) -> io::Result<OsListener> {
        let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
        if addr.is_ipv6() {
            socket.set_only_v6(true)?;
        }
        // Unix keeps a closed listener's port in TIME_WAIT unless this is set;
        // Windows allows the new bind already, and there the option would let
        // another socket take a port that is in use.
        if cfg!(unix) {
            socket.set_reuse_address(true)?;
        }
        socket.bind(&addr.into())?;
        socket.listen(backlog)?;
        socket.set_nonblocking(true)?;
        let listener = {
            let _context = self.host.handle().enter();
            tokio::net::TcpListener::from_std(socket.into())?
        };
        let local = StreamAddr::Ip(listener.local_addr()?);
        Ok(OsListener {
            host: self.host.clone(),
            io: ListenerIo::Tcp(listener),
            local,
            streams: Arc::clone(&self.streams),
            _permit: permit,
            _file: None,
        })
    }

    #[cfg(unix)]
    fn listen_unix(
        &self,
        path: &UnixPath,
        options: ListenOptions,
        backlog: i32,
        permit: Permit,
    ) -> io::Result<OsListener> {
        use std::os::unix::fs::PermissionsExt;

        let native = std::path::PathBuf::from(path.as_str());
        if std::fs::symlink_metadata(&native).is_ok() {
            return Err(io::ErrorKind::AddrInUse.into());
        }
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.bind(&socket2::SockAddr::unix(&native)?)?;
        // The file exists from here and is this listener's: remove it on any
        // later failure.
        let built = (|| {
            let mode = std::fs::Permissions::from_mode(options.unix_mode & 0o777);
            std::fs::set_permissions(&native, mode)?;
            socket.listen(backlog)?;
            socket.set_nonblocking(true)?;
            let std_listener: std::os::unix::net::UnixListener = socket.into();
            let _context = self.host.handle().enter();
            tokio::net::UnixListener::from_std(std_listener)
        })();
        match built {
            Ok(listener) => Ok(OsListener {
                host: self.host.clone(),
                io: ListenerIo::Unix(listener),
                local: StreamAddr::Unix(path.clone()),
                streams: Arc::clone(&self.streams),
                _permit: permit,
                _file: Some(UnixFile(native)),
            }),
            Err(e) => {
                // Best effort: the error being returned is the one that matters.
                let _ = std::fs::remove_file(&native);
                Err(e)
            }
        }
    }
}

impl Net for OsNet {
    fn interfaces(
        &self,
        newer_than: Option<u64>,
    ) -> BoxFuture<'_, Result<InterfaceList, NetError>> {
        let host = self.host.clone();
        let cache = Arc::clone(&self.interfaces);
        Box::pin(async move {
            let mut first = None;
            loop {
                let fresh = host
                    .blocking(ifaces::read)
                    .await
                    .map_err(|Stopped| NetError::Closed)?
                    .map_err(|e| errors::net(&e))?;
                let list = cache.update(fresh);
                let first = *first.get_or_insert(list.generation);
                if answers(newer_than, first, list.generation) {
                    return Ok(list);
                }
                let now = std::time::Instant::now();
                let next = now.checked_add(INTERFACE_POLL).unwrap_or(now);
                host.sleep_until(next).await;
            }
        })
    }

    fn bind_udp(
        &self,
        local: SocketAddr,
        options: UdpOptions,
    ) -> Result<Box<dyn UdpSocket>, NetError> {
        const BUFFER: std::ops::RangeInclusive<u32> = 65_536..=67_108_864;
        if !BUFFER.contains(&options.recv_buffer) || !BUFFER.contains(&options.send_buffer) {
            return Err(NetError::InvalidInput(Detail::new(
                "UDP buffer sizes must be 65536 to 67108864 bytes",
            )));
        }
        let permit = Permit::take(&self.udp, MAX_UDP).ok_or(NetError::TooManyOpen)?;
        let socket = udp::OsUdpSocket::bind(
            self.host.clone(),
            Arc::clone(&self.interfaces),
            local,
            options,
            permit,
        )?;
        Ok(Box::new(socket))
    }

    fn connect(
        &self,
        remote: StreamAddr,
        options: StreamOptions,
    ) -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>> {
        let Some(permit) = Permit::take(&self.streams, MAX_STREAMS) else {
            return Box::pin(ready(Err(NetError::TooManyOpen)));
        };
        let host = self.host.clone();
        match remote {
            StreamAddr::Ip(addr) => self.host.run(async move {
                let stream = tokio::net::TcpStream::connect(addr)
                    .await
                    .map_err(|e| errors::net(&e))?;
                stream
                    .set_nodelay(options.nodelay)
                    .map_err(|e| errors::net(&e))?;
                let stream: Box<dyn Stream> = Box::new(OsStream::tcp(host, stream, permit));
                Ok(stream)
            }),
            StreamAddr::Unix(path) => connect_unix(host, path, permit),
        }
    }

    fn listen(
        &self,
        local: StreamAddr,
        options: ListenOptions,
    ) -> Result<Box<dyn Listener>, NetError> {
        if !(1..=65_535).contains(&options.backlog) {
            return Err(NetError::InvalidInput(Detail::new(
                "listen backlog must be 1 to 65535",
            )));
        }
        // At most 65,535, so it fits.
        let backlog = i32::try_from(options.backlog).unwrap_or(i32::MAX);
        let permit = Permit::take(&self.streams, MAX_STREAMS).ok_or(NetError::TooManyOpen)?;
        let listener = match local {
            StreamAddr::Ip(addr) => self.listen_ip(addr, backlog, permit),
            #[cfg(unix)]
            StreamAddr::Unix(path) => self.listen_unix(&path, options, backlog, permit),
            #[cfg(not(unix))]
            StreamAddr::Unix(_) => return Err(NetError::Unsupported),
        };
        match listener {
            Ok(listener) => Ok(Box::new(listener)),
            Err(e) => Err(errors::net(&e)),
        }
    }
}

#[cfg(unix)]
fn connect_unix(
    host: Host,
    path: UnixPath,
    permit: Permit,
) -> BoxFuture<'static, Result<Box<dyn Stream>, NetError>> {
    let runner = host.clone();
    runner.run(async move {
        let stream = tokio::net::UnixStream::connect(path.as_str())
            .await
            .map_err(|e| errors::net(&e))?;
        let stream: Box<dyn Stream> = Box::new(OsStream::unix(host, stream, permit));
        Ok(stream)
    })
}

#[cfg(not(unix))]
fn connect_unix(
    _host: Host,
    _path: UnixPath,
    _permit: Permit,
) -> BoxFuture<'static, Result<Box<dyn Stream>, NetError>> {
    Box::pin(ready(Err(NetError::Unsupported)))
}

/// Whether `interfaces(newer_than)` completes with a list of `generation`,
/// when its first read of the list had generation `first` (ARFC 0005 clause
/// 3.1.7): at once when `first` is newer than `g`, otherwise at the first
/// change after the call, also when `g` is ahead of every generation this
/// instance has had.
fn answers(newer_than: Option<u64>, first: u64, generation: u64) -> bool {
    newer_than.is_none_or(|g| generation > g.min(first))
}

/// The host's interface list as last read, and its generation (ARFC 0005
/// clause 3.1.7).
#[derive(Debug, Default)]
struct Interfaces {
    latest: Mutex<Option<(u64, Vec<Interface>)>>,
}

impl Interfaces {
    /// Records `fresh` as the current list, raising the generation when it
    /// differs from the last one read. The first list read has generation 1.
    fn update(&self, fresh: Vec<Interface>) -> InterfaceList {
        let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        let generation = match &*latest {
            Some((generation, list)) if *list == fresh => *generation,
            Some((generation, _)) => generation + 1,
            None => 1,
        };
        *latest = Some((generation, fresh.clone()));
        InterfaceList {
            generation,
            interfaces: fresh,
        }
    }

    /// The interface that holds `ip`, reading the list again when the last
    /// one read does not hold it.
    fn holding(&self, ip: IpAddr) -> Option<Interface> {
        let find = |list: &[Interface]| {
            list.iter()
                .find(|iface| iface.addrs.iter().any(|addr| addr.ip == ip))
                .cloned()
        };
        let known = {
            let latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            latest.as_ref().and_then(|(_, list)| find(list))
        };
        known.or_else(|| {
            let fresh = ifaces::read().ok()?;
            let found = find(&fresh);
            self.update(fresh);
            found
        })
    }
}

/// Clears its flag when dropped: one pending `read` or `write` of a stream.
struct Pending<'a>(&'a AtomicBool);

impl<'a> Pending<'a> {
    fn take(flag: &'a AtomicBool) -> Option<Pending<'a>> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Pending(flag))
    }
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The socket of a stream.
#[derive(Debug)]
enum StreamIo {
    Tcp(tokio::net::TcpStream),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

impl StreamIo {
    async fn readable(&self) -> io::Result<()> {
        match self {
            StreamIo::Tcp(s) => s.readable().await,
            #[cfg(unix)]
            StreamIo::Unix(s) => s.readable().await,
        }
    }

    fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            StreamIo::Tcp(s) => s.try_read(buf),
            #[cfg(unix)]
            StreamIo::Unix(s) => s.try_read(buf),
        }
    }

    async fn writable(&self) -> io::Result<()> {
        match self {
            StreamIo::Tcp(s) => s.writable().await,
            #[cfg(unix)]
            StreamIo::Unix(s) => s.writable().await,
        }
    }

    fn try_write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match self {
            StreamIo::Tcp(s) => s.try_write_vectored(bufs),
            #[cfg(unix)]
            StreamIo::Unix(s) => s.try_write_vectored(bufs),
        }
    }

    fn shutdown_write(&self) -> io::Result<()> {
        match self {
            StreamIo::Tcp(s) => SockRef::from(s).shutdown(Shutdown::Write),
            #[cfg(unix)]
            StreamIo::Unix(s) => SockRef::from(s).shutdown(Shutdown::Write),
        }
    }

    /// ARFC 0005 clause 3.1.9.2.
    fn peer_uid(&self) -> Result<u32, NetError> {
        match self {
            StreamIo::Tcp(_) => Err(NetError::InvalidInput(Detail::new(
                "peer_uid applies to Unix streams only",
            ))),
            // `UnixStream::peer_cred` on Linux is `getsockopt` with
            // `SOL_SOCKET` and `SO_PEERCRED`, returning `ucred.uid`, and
            // `last_os_error` when the call fails (tokio 1.54.1,
            // src/net/unix/ucred.rs, `impl_linux::get_peer_cred`).
            #[cfg(target_os = "linux")]
            StreamIo::Unix(s) => s.peer_cred().map(|cred| cred.uid()).map_err(|e| {
                e.raw_os_error()
                    .map_or_else(|| errors::net(&e), |code| NetError::Os { code })
            }),
            #[cfg(all(unix, not(target_os = "linux")))]
            StreamIo::Unix(_) => Err(NetError::Unsupported),
        }
    }
}

/// A connected stream, implementation `os` (ARFC 0005 clause 3.1.9).
struct OsStream {
    host: Host,
    io: StreamIo,
    local: Option<StreamAddr>,
    peer: Option<StreamAddr>,
    reading: AtomicBool,
    writing: AtomicBool,
    /// `shutdown_write` completed.
    shut: AtomicBool,
    /// An operation saw a reset; every later one fails with it.
    reset: AtomicBool,
    _permit: Permit,
}

/// Writes the addresses, never data.
impl fmt::Debug for OsStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsStream")
            .field("local", &self.local)
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

#[cfg(unix)]
fn unix_addr(addr: io::Result<tokio::net::unix::SocketAddr>) -> Option<StreamAddr> {
    let addr = addr.ok()?;
    let path = addr.as_pathname()?.to_str()?;
    UnixPath::new(path).ok().map(StreamAddr::Unix)
}

impl OsStream {
    fn new(
        host: Host,
        io: StreamIo,
        local: Option<StreamAddr>,
        peer: Option<StreamAddr>,
        permit: Permit,
    ) -> OsStream {
        OsStream {
            host,
            io,
            local,
            peer,
            reading: AtomicBool::new(false),
            writing: AtomicBool::new(false),
            shut: AtomicBool::new(false),
            reset: AtomicBool::new(false),
            _permit: permit,
        }
    }

    fn tcp(host: Host, stream: tokio::net::TcpStream, permit: Permit) -> OsStream {
        let local = stream.local_addr().ok().map(StreamAddr::Ip);
        let peer = stream.peer_addr().ok().map(StreamAddr::Ip);
        OsStream::new(host, StreamIo::Tcp(stream), local, peer, permit)
    }

    #[cfg(unix)]
    fn unix(host: Host, stream: tokio::net::UnixStream, permit: Permit) -> OsStream {
        let local = unix_addr(stream.local_addr());
        let peer = unix_addr(stream.peer_addr());
        OsStream::new(host, StreamIo::Unix(stream), local, peer, permit)
    }

    /// The error of a failed operation, remembering a reset.
    fn failed(&self, error: &io::Error) -> NetError {
        let error = errors::net(error);
        if error == NetError::ConnectionReset {
            self.reset.store(true, Ordering::SeqCst);
        }
        error
    }

    fn is_reset(&self) -> bool {
        self.reset.load(Ordering::SeqCst)
    }
}

impl Stream for OsStream {
    fn local_addr(&self) -> Option<StreamAddr> {
        self.local.clone()
    }

    fn peer_addr(&self) -> Option<StreamAddr> {
        self.peer.clone()
    }

    fn read<'a>(&'a self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize, NetError>> {
        if buf.is_empty() {
            return Box::pin(ready(Ok(0)));
        }
        if self.is_reset() {
            return Box::pin(ready(Err(NetError::ConnectionReset)));
        }
        let Some(pending) = Pending::take(&self.reading) else {
            return Box::pin(ready(Err(NetError::Busy)));
        };
        self.host.run(async move {
            let _pending = pending;
            loop {
                self.io.readable().await.map_err(|e| self.failed(&e))?;
                match self.io.try_read(buf) {
                    Ok(n) => return Ok(n),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(self.failed(&e)),
                }
            }
        })
    }

    fn write<'a>(&'a self, bufs: &'a [&'a [u8]]) -> BoxFuture<'a, Result<usize, NetError>> {
        if self.is_reset() {
            return Box::pin(ready(Err(NetError::ConnectionReset)));
        }
        if self.shut.load(Ordering::SeqCst) {
            return Box::pin(ready(Err(NetError::Closed)));
        }
        if bufs.iter().all(|b| b.is_empty()) {
            return Box::pin(ready(Ok(0)));
        }
        let Some(pending) = Pending::take(&self.writing) else {
            return Box::pin(ready(Err(NetError::Busy)));
        };
        self.host.run(async move {
            let _pending = pending;
            // Windows takes at most u32::MAX bytes per slice; a write may
            // write fewer bytes than offered.
            let slices: Vec<IoSlice<'_>> = bufs
                .iter()
                .filter(|b| !b.is_empty())
                .take(MAX_WRITE_SLICES)
                .map(|b| IoSlice::new(&b[..b.len().min(u32::MAX as usize)]))
                .collect();
            loop {
                self.io.writable().await.map_err(|e| self.failed(&e))?;
                match self.io.try_write_vectored(&slices) {
                    Ok(n) => return Ok(n),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(self.failed(&e)),
                }
            }
        })
    }

    fn shutdown_write(&self) -> BoxFuture<'_, Result<(), NetError>> {
        let result = if self.is_reset() {
            Err(NetError::ConnectionReset)
        } else if self.shut.load(Ordering::SeqCst) {
            Ok(())
        } else {
            match self.io.shutdown_write() {
                Ok(()) => {
                    self.shut.store(true, Ordering::SeqCst);
                    Ok(())
                }
                // A socket that the peer reset is no longer connected.
                Err(e) if e.kind() == io::ErrorKind::NotConnected => {
                    self.reset.store(true, Ordering::SeqCst);
                    Err(NetError::ConnectionReset)
                }
                Err(e) => Err(self.failed(&e)),
            }
        };
        Box::pin(ready(result))
    }

    fn peer_uid(&self) -> Result<u32, NetError> {
        self.io.peer_uid()
    }
}

/// The socket of a listener.
#[derive(Debug)]
enum ListenerIo {
    Tcp(tokio::net::TcpListener),
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
}

/// A Unix socket file, removed when dropped.
#[derive(Debug)]
struct UnixFile(std::path::PathBuf);

impl Drop for UnixFile {
    fn drop(&mut self) {
        // Nothing can report a failure from a drop; the file is left behind.
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A listener, implementation `os`.
struct OsListener {
    host: Host,
    io: ListenerIo,
    local: StreamAddr,
    streams: Arc<AtomicUsize>,
    _permit: Permit,
    /// The socket file of a Unix listener, dropped after the socket.
    _file: Option<UnixFile>,
}

/// Writes the address listened on.
impl fmt::Debug for OsListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsListener")
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

impl Listener for OsListener {
    fn local_addr(&self) -> StreamAddr {
        self.local.clone()
    }

    fn accept(&self) -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>> {
        let Some(permit) = Permit::take(&self.streams, MAX_STREAMS) else {
            return Box::pin(ready(Err(NetError::TooManyOpen)));
        };
        let host = self.host.clone();
        self.host.run(async move {
            let stream: Box<dyn Stream> = match &self.io {
                ListenerIo::Tcp(listener) => {
                    let (stream, _) = listener.accept().await.map_err(|e| errors::net(&e))?;
                    // Accepted streams take the `StreamOptions` default.
                    stream.set_nodelay(true).map_err(|e| errors::net(&e))?;
                    Box::new(OsStream::tcp(host, stream, permit))
                }
                #[cfg(unix)]
                ListenerIo::Unix(listener) => {
                    let (stream, _) = listener.accept().await.map_err(|e| errors::net(&e))?;
                    Box::new(OsStream::unix(host, stream, permit))
                }
            };
            Ok(stream)
        })
    }
}

#[cfg(test)]
mod tests;
