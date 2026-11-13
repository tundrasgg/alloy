//! The `net` seam (ARFC 0005 section 2.3 and clauses 3.1.7 to 3.1.9).

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use alloy_seam::Detail;

use crate::BoxFuture;

/// The most datagrams one [`UdpSocket::send`] or [`UdpSocket::recv`] handles.
pub const MAX_NET_BATCH: usize = 64;

/// Seam `net`: interfaces, UDP sockets, and streams (ARFC 0005 clauses 3.1.7 to
/// 3.1.9).
///
/// Methods take `&self`, may be called concurrently from any task, and never
/// panic: invalid input returns an error. Dropping a returned future cancels
/// it with the guarantee its method states. `mem` never returns
/// [`NetError::Os`].
pub trait Net: Send + Sync + fmt::Debug + 'static {
    /// The host's interfaces. With `None`, completes with the current list.
    /// With `Some(g)`, completes at once when the list's generation is greater
    /// than `g`, and otherwise when the list next changes.
    ///
    /// The list holds at most 1,024 interfaces, the lowest indexes, and at most
    /// 64 addresses per interface.
    ///
    /// # Errors
    ///
    /// A [`NetError`] when the platform cannot list interfaces.
    fn interfaces(&self, newer_than: Option<u64>)
    -> BoxFuture<'_, Result<InterfaceList, NetError>>;

    /// Binds a UDP socket to `local`. Port 0 picks a free port. A bind to the
    /// IPv6 unspecified address receives IPv6 only.
    ///
    /// # Errors
    ///
    /// [`NetError::AddrInUse`] when the address and port are bound,
    /// [`NetError::AddrNotAvailable`] when the address is on no interface of
    /// the host, [`NetError::PermissionDenied`] when the platform refuses the
    /// port, [`NetError::TooManyOpen`] at 4,096 open UDP sockets, and
    /// [`NetError::InvalidInput`] when a buffer size in `options` is outside
    /// 65,536 to 67,108,864.
    fn bind_udp(
        &self,
        local: SocketAddr,
        options: UdpOptions,
    ) -> Result<Box<dyn UdpSocket>, NetError>;

    /// Opens a stream to `remote`: TCP with `options.nodelay` applied, or a
    /// Unix stream socket. A dropped future leaves no connection open.
    ///
    /// # Errors
    ///
    /// [`NetError::ConnectionRefused`], [`NetError::TimedOut`] (`mem`: after
    /// 30 s of virtual time without an answer), [`NetError::HostUnreachable`],
    /// [`NetError::NetworkUnreachable`], [`NetError::TooManyOpen`] at 65,536
    /// open streams and listeners, and [`NetError::Unsupported`] for a Unix
    /// address on a platform without Unix sockets.
    fn connect(
        &self,
        remote: StreamAddr,
        options: StreamOptions,
    ) -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>>;

    /// Listens for streams on `local`. On a Unix path it creates the socket
    /// file with the permission bits `unix_mode & 0o777` and removes it when
    /// the listener is dropped. A TCP port whose earlier listener has closed
    /// can be listened on again at once.
    ///
    /// # Errors
    ///
    /// [`NetError::InvalidInput`] for a backlog outside 1 to 65,535,
    /// [`NetError::AddrInUse`] when the address is bound or the Unix path
    /// exists, [`NetError::TooManyOpen`] at 65,536 open streams and listeners,
    /// and [`NetError::Unsupported`] for a Unix address on a platform without
    /// Unix sockets.
    fn listen(
        &self,
        local: StreamAddr,
        options: ListenOptions,
    ) -> Result<Box<dyn Listener>, NetError>;
}

/// A snapshot of the host's interfaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceList {
    /// Increases each time any field of the list changes, and never otherwise.
    pub generation: u64,
    /// Sorted by `index`.
    pub interfaces: Vec<Interface>,
}

/// One network interface of the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    /// The platform's interface index.
    pub index: u32,
    /// 1 to 15 bytes.
    pub name: String,
    /// The interface MTU, in bytes.
    pub mtu: u32,
    /// The interface is administratively and operationally up.
    pub up: bool,
    /// The interface is a loopback interface.
    pub loopback: bool,
    /// IPv4 before IPv6, each ascending.
    pub addrs: Vec<InterfaceAddr>,
}

/// An address assigned to an interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceAddr {
    /// The address.
    pub ip: IpAddr,
    /// The network prefix length, in bits.
    pub prefix_len: u8,
}

/// Options of [`Net::bind_udp`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpOptions {
    /// Bytes of payload queued for `recv`. Default 4,194,304. 65,536 to
    /// 67,108,864.
    pub recv_buffer: u32,
    /// Bytes of payload queued for sending. Default 4,194,304. 65,536 to
    /// 67,108,864.
    pub send_buffer: u32,
}

impl Default for UdpOptions {
    /// Both buffers 4,194,304 bytes.
    fn default() -> UdpOptions {
        UdpOptions {
            recv_buffer: 4_194_304,
            send_buffer: 4_194_304,
        }
    }
}

/// A bound UDP socket (ARFC 0005 clause 3.1.8). Dropping it closes it.
///
/// No datagram is ever fragmented by IP (I-10).
pub trait UdpSocket: Send + Sync + fmt::Debug {
    /// The address the socket is bound to, with the port chosen at bind.
    fn local_addr(&self) -> SocketAddr;

    /// Sends the datagrams of `batch` in order and stops at the first that
    /// fails. May wait while the send buffer is full; never drops a datagram
    /// for that reason. Completion means handed to the network, not delivered.
    /// A dropped future has sent a prefix of the batch, possibly empty.
    ///
    /// A batch longer than [`MAX_NET_BATCH`] sends nothing and reports
    /// [`NetError::BatchTooLarge`]. A datagram whose IP packet would exceed the
    /// path MTU fails with [`NetError::MessageTooLong`], and one whose `src_ip`
    /// the socket cannot send from fails with [`NetError::InvalidInput`].
    fn send<'a>(&'a self, batch: &'a [Datagram<'a>]) -> BoxFuture<'a, SendOutcome>;

    /// Waits until a datagram is queued, then fills up to
    /// `min(slots.len(), 64)` slots, from the first, with queued datagrams in
    /// arrival order, and returns how many slots, from the first, it filled.
    /// Each datagram goes to exactly one call. A dropped future has taken no
    /// datagram.
    ///
    /// # Errors
    ///
    /// [`NetError::InvalidInput`] for an empty `slots`, and [`NetError::Closed`]
    /// or a platform error when the socket can no longer receive.
    fn recv<'a>(&'a self, slots: &'a mut [RecvSlot]) -> BoxFuture<'a, Result<usize, NetError>>;
}

/// One datagram to send.
#[derive(Clone, Copy)]
pub struct Datagram<'a> {
    /// The destination.
    pub dst: SocketAddr,
    /// The source address to send from. `None` lets the host choose. Otherwise
    /// the socket's bound address or, for a socket bound to an unspecified
    /// address, an address of the host in the same family.
    pub src_ip: Option<IpAddr>,
    /// The UDP payload.
    pub payload: &'a [u8],
}

/// Writes `Datagram { len: <payload length> }`: lengths only, never the
/// payload (ARFC 0005 section 2.1, I-22).
impl fmt::Debug for Datagram<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Datagram")
            .field("len", &self.payload.len())
            .finish()
    }
}

/// The result of [`UdpSocket::send`]: datagrams `0..sent` were sent. When
/// `sent` is less than the batch length, `error` is the failure of datagram
/// `sent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendOutcome {
    /// How many datagrams, from the first, were sent.
    pub sent: usize,
    /// The failure of datagram `sent`, when there is one.
    pub error: Option<NetError>,
}

/// A caller-owned buffer that [`UdpSocket::recv`] fills with one datagram.
pub struct RecvSlot {
    /// The buffer. Its length is the slot's capacity.
    pub buf: Box<[u8]>,
    /// Bytes of `buf` holding the datagram.
    pub len: usize,
    /// The datagram was longer than `buf` and its tail was discarded.
    pub truncated: bool,
    /// The sender.
    pub src: SocketAddr,
    /// The local address the datagram was sent to.
    pub dst_ip: IpAddr,
}

/// Writes `RecvSlot { capacity: <buffer length>, len: <datagram length> }`:
/// lengths only, never the bytes (ARFC 0005 section 2.1, I-22).
impl fmt::Debug for RecvSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvSlot")
            .field("capacity", &self.buf.len())
            .field("len", &self.len)
            .finish()
    }
}

impl RecvSlot {
    /// The largest UDP payload, in bytes.
    pub const MAX_CAPACITY: usize = 65_535;

    /// A slot with a zeroed buffer of `capacity` bytes, `len` 0, `truncated`
    /// false, `src` `0.0.0.0:0`, and `dst_ip` `0.0.0.0`.
    ///
    /// # Errors
    ///
    /// [`NetError::InvalidInput`] when `capacity` is above
    /// [`RecvSlot::MAX_CAPACITY`].
    pub fn new(capacity: usize) -> Result<RecvSlot, NetError> {
        if capacity > Self::MAX_CAPACITY {
            return Err(NetError::InvalidInput(Detail::new(&format!(
                "receive slot capacity {capacity} is above 65535"
            ))));
        }
        Ok(RecvSlot {
            buf: vec![0; capacity].into_boxed_slice(),
            len: 0,
            truncated: false,
            src: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            dst_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        })
    }
}

/// The address of a stream or a listener.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StreamAddr {
    /// TCP.
    Ip(SocketAddr),
    /// A Unix domain stream socket.
    Unix(UnixPath),
}

/// An absolute native path of a Unix domain socket: 1 to 107 bytes, starting
/// with `/`, with no NUL byte.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnixPath(String);

impl UnixPath {
    /// The longest path, in bytes, that fits `sun_path` with its terminator.
    const MAX_LEN: usize = 107;

    /// Accepts exactly the strings of 1 to 107 bytes that start with `/` and
    /// hold no NUL byte.
    ///
    /// # Errors
    ///
    /// [`NetError::InvalidInput`] for any other string.
    pub fn new(path: &str) -> Result<UnixPath, NetError> {
        let reason = if !path.starts_with('/') {
            Some("does not start with /")
        } else if path.len() > Self::MAX_LEN {
            Some("is longer than 107 bytes")
        } else if path.contains('\0') {
            Some("contains NUL")
        } else {
            None
        };
        match reason {
            Some(reason) => Err(NetError::InvalidInput(Detail::new(&format!(
                "Unix socket path {reason}"
            )))),
            None => Ok(UnixPath(path.to_owned())),
        }
    }

    /// The path as given to [`UnixPath::new`].
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Options of [`Net::connect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamOptions {
    /// Disables send coalescing on TCP. Default true. Ignored for Unix
    /// sockets.
    pub nodelay: bool,
}

/// Options of [`Net::listen`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenOptions {
    /// Pending connections. 1 to 65,535, default 1,024.
    pub backlog: u32,
    /// Permission bits of a Unix socket file. Default 0o600. Ignored for TCP.
    pub unix_mode: u32,
}

impl Default for StreamOptions {
    /// `nodelay` true.
    fn default() -> StreamOptions {
        StreamOptions { nodelay: true }
    }
}

impl Default for ListenOptions {
    /// `backlog` 1,024 and `unix_mode` 0o600.
    fn default() -> ListenOptions {
        ListenOptions {
            backlog: 1_024,
            unix_mode: 0o600,
        }
    }
}

/// A connected TCP or Unix stream (ARFC 0005 clause 3.1.9). Dropping it closes
/// both directions.
///
/// At most one `read` and one `write` may be pending at a time; another fails
/// at once with [`NetError::Busy`].
pub trait Stream: Send + Sync + fmt::Debug {
    /// The local address. `None` for an unnamed Unix socket.
    fn local_addr(&self) -> Option<StreamAddr>;

    /// The peer's address. `None` for an unnamed Unix socket.
    fn peer_addr(&self) -> Option<StreamAddr>;

    /// Waits for data and returns between 1 and `buf.len()` bytes. Returns 0
    /// only at end of stream or, at once, for an empty `buf`. A dropped future
    /// has read nothing.
    ///
    /// # Errors
    ///
    /// [`NetError::ConnectionReset`] after a reset, [`NetError::Busy`] while
    /// another read is pending.
    fn read<'a>(&'a self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize, NetError>>;

    /// Waits for buffer space and writes from `bufs`, in order, at least 1
    /// byte unless `bufs` hold none. Returns the bytes written. A dropped
    /// future has written nothing.
    ///
    /// # Errors
    ///
    /// [`NetError::ConnectionReset`] after a reset, [`NetError::Closed`] after
    /// [`Stream::shutdown_write`], [`NetError::Busy`] while another write is
    /// pending.
    fn write<'a>(&'a self, bufs: &'a [&'a [u8]]) -> BoxFuture<'a, Result<usize, NetError>>;

    /// Closes the write direction. The peer reads end of stream once it has
    /// read every byte written before.
    ///
    /// # Errors
    ///
    /// [`NetError::ConnectionReset`] after a reset.
    fn shutdown_write(&self) -> BoxFuture<'_, Result<(), NetError>>;
}

/// A listening TCP or Unix socket. Dropping it stops listening and, for a
/// Unix socket, removes its file.
pub trait Listener: Send + Sync + fmt::Debug {
    /// The address listened on, with the port chosen at listen.
    fn local_addr(&self) -> StreamAddr;

    /// Waits for and returns the next pending connection, in arrival order. A
    /// dropped future has taken no connection.
    ///
    /// # Errors
    ///
    /// [`NetError::TooManyOpen`] at 65,536 open streams and listeners.
    fn accept(&self) -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>>;
}

/// Why a network operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    /// The address and port are bound, or the Unix path exists.
    #[error("address in use")]
    AddrInUse,
    /// The address is on no interface of the host.
    #[error("address not available on this host")]
    AddrNotAvailable,
    /// The platform refused the operation.
    #[error("permission denied")]
    PermissionDenied,
    /// The peer refused the connection.
    #[error("connection refused")]
    ConnectionRefused,
    /// The peer reset the connection.
    #[error("connection reset")]
    ConnectionReset,
    /// The peer did not answer in time.
    #[error("timed out")]
    TimedOut,
    /// No route to the host.
    #[error("host unreachable")]
    HostUnreachable,
    /// No route to the network.
    #[error("network unreachable")]
    NetworkUnreachable,
    /// The datagram's IP packet would exceed the path MTU.
    #[error("datagram too long, at most {max_payload} bytes of payload fit")]
    MessageTooLong {
        /// The path MTU minus the IP header (20 bytes for IPv4, 40 for IPv6)
        /// minus the 8-byte UDP header.
        max_payload: u32,
    },
    /// A batch held more than [`MAX_NET_BATCH`] datagrams.
    #[error("batch longer than 64")]
    BatchTooLarge,
    /// Another read or write is pending on the stream.
    #[error("another read or write is pending on this stream")]
    Busy,
    /// An argument is outside its allowed range.
    #[error("invalid input: {0}")]
    InvalidInput(Detail),
    /// The instance's limit on open sockets is reached.
    #[error("too many open sockets")]
    TooManyOpen,
    /// The operation is not supported on this platform.
    #[error("not supported on this platform")]
    Unsupported,
    /// The socket or the direction is closed.
    #[error("closed")]
    Closed,
    /// An operating system error with no variant above. `os` only.
    #[error("operating system error {code}")]
    Os {
        /// The platform's error code.
        code: i32,
    },
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use super::*;

    #[test]
    fn option_defaults() {
        assert_eq!(
            UdpOptions::default(),
            UdpOptions {
                recv_buffer: 4_194_304,
                send_buffer: 4_194_304
            }
        );
        assert!(StreamOptions::default().nodelay);
        assert_eq!(
            ListenOptions::default(),
            ListenOptions {
                backlog: 1_024,
                unix_mode: 0o600
            }
        );
    }

    #[test]
    fn recv_slot_new_is_zeroed_and_bounded() {
        let slot = RecvSlot::new(1_500).unwrap();
        assert_eq!(slot.buf.len(), 1_500);
        assert!(slot.buf.iter().all(|b| *b == 0));
        assert_eq!((slot.len, slot.truncated), (0, false));
        assert_eq!(slot.src, "0.0.0.0:0".parse::<SocketAddr>().unwrap());
        assert_eq!(slot.dst_ip, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(RecvSlot::new(0).unwrap().buf.len(), 0);
        assert_eq!(RecvSlot::new(65_535).unwrap().buf.len(), 65_535);
        assert!(matches!(
            RecvSlot::new(65_536),
            Err(NetError::InvalidInput(_))
        ));
    }

    #[test]
    fn debug_never_shows_packet_bytes() {
        let datagram = Datagram {
            dst: SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9),
            src_ip: None,
            payload: b"tenant payload",
        };
        assert_eq!(format!("{datagram:?}"), "Datagram { len: 14 }");
        let mut slot = RecvSlot::new(4).unwrap();
        slot.buf.copy_from_slice(&[0xAB; 4]);
        slot.len = 3;
        slot.truncated = true;
        slot.src = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9);
        assert_eq!(format!("{slot:?}"), "RecvSlot { capacity: 4, len: 3 }");
    }

    #[test]
    fn unix_paths_follow_the_rules() {
        let longest = format!("/{}", "s".repeat(106));
        for ok in ["/", "/run/alloy.sock", longest.as_str()] {
            assert_eq!(
                UnixPath::new(ok).map(|p| p.as_str().to_owned()),
                Ok(ok.to_owned())
            );
        }
        let too_long = format!("/{}", "s".repeat(107));
        for bad in ["", "run/alloy.sock", "/a\0b", too_long.as_str()] {
            assert!(
                matches!(UnixPath::new(bad), Err(NetError::InvalidInput(_))),
                "{bad:?}"
            );
        }
    }
}
