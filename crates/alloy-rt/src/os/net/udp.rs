//! `os` UDP sockets (ARFC 0005 clause 3.1.8): batched sends with segmentation
//! offload where the kernel offers it, batched receives, packet information
//! for source and destination addresses, and no IP fragmentation (I-10).

use std::collections::VecDeque;
use std::fmt;
use std::future::ready;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use alloy_seam::Detail;
use quinn_udp::{RecvMeta, Transmit, UdpSockRef, UdpSocketState};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::Interest;

use super::Interfaces;
use crate::os::errors;
use crate::os::{Host, Permit};
use crate::{
    BoxFuture, Datagram, MAX_NET_BATCH, NetError, RecvSlot, SendOutcome, UdpOptions, UdpSocket,
};

/// The most payload bytes one segmentation offload send carries, below the
/// 65,507 bytes of a UDP datagram over IPv4.
const MAX_GSO_BYTES: usize = 65_000;
/// The receive buffers a socket keeps, each large enough for any datagram, so
/// that one call receives several datagrams and none is cut short.
const RX_BUFFERS: usize = 8;
/// The size of each receive buffer: the largest UDP length field, which holds
/// every datagram (ARFC 0005 section 2.3), and with receive offload a run of
/// datagrams.
const RX_BUFFER_LEN: usize = RecvSlot::MAX_CAPACITY;
/// The largest UDP payload over IPv4 and over IPv6.
const MAX_PAYLOAD_V4: u32 = 65_507;
const MAX_PAYLOAD_V6: u32 = 65_527;
/// The smallest MTU each family guarantees, used when the path MTU cannot be
/// learned.
const MIN_MTU_V4: u32 = 576;
const MIN_MTU_V6: u32 = 1_280;

/// A bound UDP socket, implementation `os`.
pub(super) struct OsUdpSocket {
    host: Host,
    io: tokio::net::UdpSocket,
    state: UdpSocketState,
    local: SocketAddr,
    interfaces: Arc<Interfaces>,
    rx: Mutex<Rx>,
    _permit: Permit,
}

/// Writes the local address, never data.
impl fmt::Debug for OsUdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsUdpSocket")
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

/// Receive state, locked for the whole of one receive so that each datagram
/// goes to exactly one `recv`.
#[derive(Default)]
struct Rx {
    /// At most [`RX_BUFFERS`], allocated on the first receives that need them.
    buffers: Vec<Box<[u8]>>,
    /// Datagrams that receive offload delivered beyond the slots of the call
    /// that received them, in arrival order.
    queued: VecDeque<Queued>,
}

/// One datagram held for the next `recv`.
struct Queued {
    payload: Vec<u8>,
    src: SocketAddr,
    dst_ip: IpAddr,
}

impl OsUdpSocket {
    pub(super) fn bind(
        host: Host,
        interfaces: Arc<Interfaces>,
        local: SocketAddr,
        options: UdpOptions,
        permit: Permit,
    ) -> Result<OsUdpSocket, NetError> {
        let net = |e: io::Error| errors::net(&e);
        let socket = Socket::new(Domain::for_address(local), Type::DGRAM, Some(Protocol::UDP))
            .map_err(net)?;
        if local.is_ipv6() {
            socket.set_only_v6(true).map_err(net)?;
        }
        // The platform may cap or round the sizes; it accounts for them.
        socket
            .set_recv_buffer_size(options.recv_buffer as usize)
            .map_err(net)?;
        socket
            .set_send_buffer_size(options.send_buffer as usize)
            .map_err(net)?;
        socket.bind(&local.into()).map_err(net)?;
        socket.set_nonblocking(true).map_err(net)?;
        let io = {
            let _context = host.handle().enter();
            tokio::net::UdpSocket::from_std(socket.into()).map_err(net)?
        };
        let state = UdpSocketState::new(UdpSockRef::from(&io)).map_err(net)?;
        platform::forbid_fragmentation(&io, &state, local.is_ipv6())?;
        let local = io.local_addr().map_err(net)?;
        Ok(OsUdpSocket {
            host,
            io,
            state,
            local,
            interfaces,
            rx: Mutex::new(Rx::default()),
            _permit: permit,
        })
    }

    /// Why `datagram` cannot be sent from this socket, or `None`.
    fn reject(&self, datagram: &Datagram<'_>) -> Option<NetError> {
        let invalid = |detail: &str| Some(NetError::InvalidInput(Detail::new(detail)));
        if datagram.dst.is_ipv4() != self.local.is_ipv4() {
            return invalid("the destination is not in the socket's address family");
        }
        let src = datagram.src_ip?;
        if src.is_ipv4() != self.local.is_ipv4() {
            return invalid("src_ip is not in the socket's address family");
        }
        if self.local.ip().is_unspecified() {
            if src.is_unspecified() || self.interfaces.holding(src).is_none() {
                return invalid("src_ip is not an address of this host");
            }
        } else if src != self.local.ip() {
            return invalid("src_ip is not the socket's bound address");
        }
        None
    }

    /// How many datagrams from `first` one segmentation offload send can
    /// carry: the same destination and source, equal lengths but for a
    /// shorter last one.
    fn run_len(&self, batch: &[Datagram<'_>], first: usize) -> usize {
        let max = self.state.max_gso_segments();
        let head = &batch[first];
        let segment = head.payload.len();
        if max <= 1 || segment == 0 {
            return 1;
        }
        let mut count = 1;
        let mut total = segment;
        for next in &batch[first + 1..] {
            let len = next.payload.len();
            if count == max
                || next.dst != head.dst
                || next.src_ip != head.src_ip
                || len == 0
                || len > segment
                || total + len > MAX_GSO_BYTES
            {
                break;
            }
            count += 1;
            total += len;
            if len < segment {
                break;
            }
        }
        count
    }

    async fn transmit(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        self.io
            .async_io(Interest::WRITABLE, || {
                self.state.try_send(UdpSockRef::from(&self.io), transmit)
            })
            .await
    }

    /// Sends a run of datagrams as one offloaded send. Nothing of the run is
    /// sent when it fails.
    async fn send_run(&self, run: &[Datagram<'_>]) -> io::Result<()> {
        let head = &run[0];
        let mut contents = Vec::with_capacity(run.iter().map(|d| d.payload.len()).sum());
        for datagram in run {
            contents.extend_from_slice(datagram.payload);
        }
        self.transmit(&Transmit {
            destination: head.dst,
            ecn: None,
            contents: &contents,
            segment_size: Some(head.payload.len()),
            src_ip: head.src_ip,
        })
        .await
    }

    async fn send_one(&self, datagram: &Datagram<'_>) -> Result<(), NetError> {
        let sent = self
            .transmit(&Transmit {
                destination: datagram.dst,
                ecn: None,
                contents: datagram.payload,
                segment_size: None,
                src_ip: datagram.src_ip,
            })
            .await;
        match sent {
            Ok(()) => Ok(()),
            Err(e) if errors::message_too_long(&e) => Err(NetError::MessageTooLong {
                max_payload: self.max_payload(datagram),
            }),
            // An ICMP error about an earlier datagram. Only path MTU errors
            // reach the caller (ARFC 0005 clause 3.1.8); this datagram was
            // handed to the network.
            Err(e) if is_icmp_report(&e) => Ok(()),
            Err(e) => Err(errors::net(&e)),
        }
    }

    /// The largest payload that fits the path MTU to the datagram's
    /// destination (ARFC 0005 clause 3.1.8), capped at the largest UDP
    /// payload of the family.
    fn max_payload(&self, datagram: &Datagram<'_>) -> u32 {
        let v6 = datagram.dst.is_ipv6();
        let (header, cap, floor) = if v6 {
            (48, MAX_PAYLOAD_V6, MIN_MTU_V6)
        } else {
            (28, MAX_PAYLOAD_V4, MIN_MTU_V4)
        };
        let mtu = self.path_mtu(datagram).unwrap_or(floor);
        mtu.saturating_sub(header).min(cap)
    }

    /// The path MTU to the datagram's destination: what the kernel learned
    /// for the route where it reports it, otherwise the MTU of the interface
    /// that holds the source address the route picks.
    fn path_mtu(&self, datagram: &Datagram<'_>) -> Option<u32> {
        let dst = datagram.dst;
        let probe = Socket::new(Domain::for_address(dst), Type::DGRAM, Some(Protocol::UDP)).ok()?;
        if dst.is_ipv6() {
            probe.set_only_v6(true).ok()?;
        }
        let src = datagram.src_ip.unwrap_or_else(|| self.local.ip());
        probe.bind(&SocketAddr::new(src, 0).into()).ok()?;
        probe.connect(&dst.into()).ok()?;
        if let Some(mtu) = platform::route_mtu(&probe, dst.is_ipv6()) {
            return Some(mtu);
        }
        let source = probe.local_addr().ok()?.as_socket()?.ip();
        self.interfaces.holding(source).map(|iface| iface.mtu)
    }
}

/// True for the errors an earlier datagram's ICMP report leaves on a socket.
fn is_icmp_report(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
    )
}

impl Rx {
    /// Fills `slots`, from the first, with queued datagrams. Returns the
    /// count.
    fn drain(&mut self, slots: &mut [RecvSlot]) -> usize {
        let mut filled = 0;
        while filled < slots.len() {
            let Some(datagram) = self.queued.pop_front() else {
                break;
            };
            fill(
                &mut slots[filled],
                &datagram.payload,
                datagram.src,
                datagram.dst_ip,
            );
            filled += 1;
        }
        filled
    }

    /// Receives once into the socket's buffers and fills `slots`, from the
    /// first, queueing what does not fit. `WouldBlock` when nothing is queued
    /// at the socket.
    fn receive(&mut self, socket: &OsUdpSocket, slots: &mut [RecvSlot]) -> io::Result<usize> {
        let wanted = slots.len().clamp(1, RX_BUFFERS);
        while self.buffers.len() < wanted {
            self.buffers.push(vec![0; RX_BUFFER_LEN].into_boxed_slice());
        }
        let mut meta = [RecvMeta::default(); RX_BUFFERS];
        let count = {
            let mut bufs: Vec<IoSliceMut<'_>> = self.buffers[..wanted]
                .iter_mut()
                .map(|b| IoSliceMut::new(b))
                .collect();
            socket
                .state
                .recv(UdpSockRef::from(&socket.io), &mut bufs, &mut meta[..wanted])?
        };
        let mut filled = 0;
        for (meta, buffer) in meta.iter().zip(&self.buffers).take(count) {
            let dst_ip = meta.dst_ip.unwrap_or_else(|| socket.local.ip());
            let received = &buffer[..meta.len.min(buffer.len())];
            // With receive offload one buffer holds a run of datagrams,
            // `stride` bytes each but the last. A zero-length datagram has
            // stride 0.
            let stride = if meta.stride == 0 {
                received.len().max(1)
            } else {
                meta.stride
            };
            let mut datagrams: Vec<&[u8]> = received.chunks(stride).collect();
            if datagrams.is_empty() {
                datagrams.push(&[]);
            }
            for payload in datagrams {
                if filled < slots.len() {
                    fill(&mut slots[filled], payload, meta.addr, dst_ip);
                    filled += 1;
                } else {
                    self.queued.push_back(Queued {
                        payload: payload.to_vec(),
                        src: meta.addr,
                        dst_ip,
                    });
                }
            }
        }
        Ok(filled)
    }
}

fn fill(slot: &mut RecvSlot, payload: &[u8], src: SocketAddr, dst_ip: IpAddr) {
    let len = payload.len().min(slot.buf.len());
    slot.buf[..len].copy_from_slice(&payload[..len]);
    slot.len = len;
    slot.truncated = payload.len() > slot.buf.len();
    slot.src = src;
    slot.dst_ip = dst_ip;
}

impl UdpSocket for OsUdpSocket {
    fn local_addr(&self) -> SocketAddr {
        self.local
    }

    fn send<'a>(&'a self, batch: &'a [Datagram<'a>]) -> BoxFuture<'a, SendOutcome> {
        if batch.len() > MAX_NET_BATCH {
            return Box::pin(ready(SendOutcome {
                sent: 0,
                error: Some(NetError::BatchTooLarge),
            }));
        }
        self.host.run(async move {
            let mut sent = 0;
            while sent < batch.len() {
                if let Some(error) = self.reject(&batch[sent]) {
                    return SendOutcome {
                        sent,
                        error: Some(error),
                    };
                }
                let run = self.run_len(batch, sent);
                if run > 1 && self.send_run(&batch[sent..sent + run]).await.is_ok() {
                    sent += run;
                    continue;
                }
                // One at a time: a run the kernel refused, whatever the
                // reason, is sent again datagram by datagram, so that the
                // outcome names the datagram that fails.
                for datagram in &batch[sent..sent + run] {
                    if let Err(error) = self.send_one(datagram).await {
                        return SendOutcome {
                            sent,
                            error: Some(error),
                        };
                    }
                    sent += 1;
                }
            }
            SendOutcome { sent, error: None }
        })
    }

    fn recv<'a>(&'a self, slots: &'a mut [RecvSlot]) -> BoxFuture<'a, Result<usize, NetError>> {
        if slots.is_empty() {
            return Box::pin(ready(Err(NetError::InvalidInput(Detail::new(
                "recv needs at least one slot",
            )))));
        }
        self.host.run(async move {
            let wanted = slots.len().min(MAX_NET_BATCH);
            let slots = &mut slots[..wanted];
            loop {
                let received = self
                    .io
                    .async_io(Interest::READABLE, || {
                        let mut rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
                        if !rx.queued.is_empty() {
                            return Ok(rx.drain(slots));
                        }
                        rx.receive(self, slots)
                    })
                    .await;
                match received {
                    Ok(0) => {}
                    Ok(filled) => return Ok(filled),
                    Err(e) if is_icmp_report(&e) => {}
                    Err(e) => return Err(errors::net(&e)),
                }
                // Datagrams queued by another call wait for no readiness.
                let mut rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
                if !rx.queued.is_empty() {
                    return Ok(rx.drain(slots));
                }
            }
        })
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code, reason = "two socket options with no safe binding")]
mod platform {
    use std::io;
    use std::os::fd::{AsFd, AsRawFd};

    use quinn_udp::UdpSocketState;

    use crate::NetError;
    use crate::os::errors;

    /// Makes the kernel refuse a datagram above the path MTU it knows, the one
    /// it learned from ICMP included, instead of sending it to be dropped:
    /// `IP_PMTUDISC_DO`. `quinn-udp` sets `IP_PMTUDISC_PROBE`, which ignores
    /// learned values. A kernel that refuses the option fails the bind.
    pub(super) fn forbid_fragmentation(
        socket: &impl AsFd,
        _state: &UdpSocketState,
        v6: bool,
    ) -> Result<(), NetError> {
        let (level, name, value) = if v6 {
            (
                libc::IPPROTO_IPV6,
                libc::IPV6_MTU_DISCOVER,
                libc::IPV6_PMTUDISC_DO,
            )
        } else {
            (
                libc::IPPROTO_IP,
                libc::IP_MTU_DISCOVER,
                libc::IP_PMTUDISC_DO,
            )
        };
        let len = libc::socklen_t::try_from(size_of::<libc::c_int>()).unwrap_or(4);
        // SAFETY: the descriptor is open for the call, and the option value is
        // a live `c_int` whose size is passed with it.
        let rc = unsafe {
            libc::setsockopt(
                socket.as_fd().as_raw_fd(),
                level,
                name,
                (&raw const value).cast(),
                len,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(errors::net(&io::Error::last_os_error()))
        }
    }

    /// The path MTU of a connected socket's route, as the kernel knows it.
    pub(super) fn route_mtu(socket: &impl AsFd, v6: bool) -> Option<u32> {
        let (level, name) = if v6 {
            (libc::IPPROTO_IPV6, libc::IPV6_MTU)
        } else {
            (libc::IPPROTO_IP, libc::IP_MTU)
        };
        let mut value: libc::c_int = 0;
        let mut len = libc::socklen_t::try_from(size_of::<libc::c_int>()).ok()?;
        // SAFETY: the descriptor is open for the call, and `value` and `len`
        // are live and describe a buffer of `len` bytes.
        let rc = unsafe {
            libc::getsockopt(
                socket.as_fd().as_raw_fd(),
                level,
                name,
                (&raw mut value).cast(),
                &raw mut len,
            )
        };
        if rc == 0 {
            u32::try_from(value).ok().filter(|mtu| *mtu > 0)
        } else {
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use quinn_udp::UdpSocketState;

    use crate::NetError;

    /// Whether `quinn-udp` sets a don't fragment option for this family on
    /// this platform: on Windows, Apple platforms, FreeBSD, and Android for
    /// both, elsewhere for IPv6 only.
    const SETS_DONT_FRAGMENT_V4: bool = cfg!(any(
        windows,
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "android"
    ));

    /// Checks that the socket cannot emit a fragmented datagram. `quinn-udp`
    /// sets the platform's don't fragment options when it builds the socket
    /// state, failing on Windows when one is refused, and elsewhere recording
    /// a refusal in `may_fragment`.
    ///
    /// # Errors
    ///
    /// [`NetError::Unsupported`] when the platform does not forbid
    /// fragmentation on this socket (I-10).
    pub(super) fn forbid_fragmentation<S>(
        _socket: &S,
        state: &UdpSocketState,
        v6: bool,
    ) -> Result<(), NetError> {
        if state.may_fragment() || (!v6 && !SETS_DONT_FRAGMENT_V4) {
            Err(NetError::Unsupported)
        } else {
            Ok(())
        }
    }

    /// This platform does not report a route's MTU.
    pub(super) fn route_mtu<S>(_socket: &S, _v6: bool) -> Option<u32> {
        None
    }
}
