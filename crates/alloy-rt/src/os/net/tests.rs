//! Unit tests of the `os` net on the host's loopback interface.

use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use super::*;
use crate::os::OsHost;
use crate::os::tests::host;
use crate::{Datagram, MAX_NET_BATCH, RecvSlot, SendOutcome};

const LO4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// An address no host holds (RFC 5737, TEST-NET-1).
const FOREIGN: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

fn setup() -> (OsHost, OsNet) {
    let host = host();
    let net = OsNet::new(host.host());
    (host, net)
}

fn finish(host: OsHost) {
    host.shutdown(Duration::from_secs(1));
}

fn any_port(ip: IpAddr) -> SocketAddr {
    SocketAddr::new(ip, 0)
}

fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

#[test]
fn bind_udp_checks_its_input_and_the_address() {
    let (host, net) = setup();
    for (recv_buffer, send_buffer) in [(65_535, 65_536), (65_536, 67_108_865)] {
        let options = UdpOptions {
            recv_buffer,
            send_buffer,
        };
        assert!(matches!(
            net.bind_udp(any_port(LO4), options).map(|_| ()),
            Err(NetError::InvalidInput(_))
        ));
    }
    let socket = net.bind_udp(any_port(LO4), UdpOptions::default()).unwrap();
    let bound = socket.local_addr();
    assert_eq!(bound.ip(), LO4);
    assert_ne!(bound.port(), 0);
    assert_eq!(
        net.bind_udp(bound, UdpOptions::default()).map(|_| ()),
        Err(NetError::AddrInUse)
    );
    assert_eq!(
        net.bind_udp(any_port(FOREIGN), UdpOptions::default())
            .map(|_| ()),
        Err(NetError::AddrNotAvailable)
    );
    net.udp.store(MAX_UDP, Ordering::SeqCst);
    assert_eq!(
        net.bind_udp(any_port(LO4), UdpOptions::default())
            .map(|_| ()),
        Err(NetError::TooManyOpen)
    );
    net.udp.store(1, Ordering::SeqCst);
    assert!(format!("{socket:?}").contains("127.0.0.1"));
    drop(socket);
    assert_eq!(net.udp.load(Ordering::SeqCst), 0);
    finish(host);
}

fn slots(count: usize, capacity: usize) -> Vec<RecvSlot> {
    (0..count)
        .map(|_| RecvSlot::new(capacity).unwrap())
        .collect()
}

/// Receives until `want` datagrams have arrived.
async fn recv_all(socket: &dyn UdpSocket, want: usize, capacity: usize) -> Vec<RecvSlot> {
    let mut got = Vec::new();
    while got.len() < want {
        let mut batch = slots(want - got.len(), capacity);
        let filled = socket.recv(&mut batch).await.unwrap();
        assert!(filled >= 1);
        got.extend(batch.into_iter().take(filled));
    }
    got
}

#[test]
fn send_and_recv_batches_over_loopback() {
    let (host, net) = setup();
    let a = net.bind_udp(any_port(LO4), UdpOptions::default()).unwrap();
    let b = net.bind_udp(any_port(LO4), UdpOptions::default()).unwrap();
    let to = b.local_addr();
    // Equal lengths with a shorter last one can go as one offloaded send;
    // the zero-length datagram and the change of length cannot.
    let payloads: Vec<Vec<u8>> = [100, 100, 100, 40, 0, 7, 1_200]
        .iter()
        .enumerate()
        .map(|(i, len)| vec![u8::try_from(i).unwrap(); *len])
        .collect();
    let batch: Vec<Datagram<'_>> = payloads
        .iter()
        .map(|p| Datagram {
            dst: to,
            src_ip: None,
            payload: p,
        })
        .collect();
    let outcome = host.block_on(a.send(&batch));
    assert_eq!(
        outcome,
        SendOutcome {
            sent: batch.len(),
            error: None
        }
    );
    let got = host.block_on(recv_all(&*b, batch.len(), 2_000));
    for (slot, payload) in got.iter().zip(&payloads) {
        assert_eq!(&slot.buf[..slot.len], &payload[..]);
        assert!(!slot.truncated);
        assert_eq!(slot.src, a.local_addr());
        assert_eq!(slot.dst_ip, LO4);
    }
    finish(host);
}

#[test]
fn recv_truncates_and_checks_its_slots() {
    let (host, net) = setup();
    let a = net.bind_udp(any_port(LO4), UdpOptions::default()).unwrap();
    let b = net.bind_udp(any_port(LO4), UdpOptions::default()).unwrap();
    let payload = [9u8; 100];
    let one = [Datagram {
        dst: b.local_addr(),
        src_ip: None,
        payload: &payload,
    }];
    assert_eq!(host.block_on(a.send(&one)).error, None);
    let mut small = slots(1, 10);
    assert_eq!(host.block_on(b.recv(&mut small)), Ok(1));
    assert_eq!((small[0].len, small[0].truncated), (10, true));
    assert_eq!(&small[0].buf[..], &[9; 10]);
    assert!(matches!(
        host.block_on(b.recv(&mut [])),
        Err(NetError::InvalidInput(_))
    ));
    let many = vec![one[0]; MAX_NET_BATCH + 1];
    assert_eq!(
        host.block_on(a.send(&many)),
        SendOutcome {
            sent: 0,
            error: Some(NetError::BatchTooLarge)
        }
    );
    // A dropped receive takes nothing: the datagram goes to the next one.
    let mut slot = slots(1, 200);
    {
        let mut pending = b.recv(&mut slot);
        assert!(poll_once(pending.as_mut()).is_pending());
    }
    assert_eq!(host.block_on(a.send(&one)).error, None);
    assert_eq!(host.block_on(b.recv(&mut slot)), Ok(1));
    assert_eq!(slot[0].len, 100);
    finish(host);
}

#[test]
fn src_ip_must_be_the_bound_or_a_host_address() {
    let (host, net) = setup();
    let bound = net.bind_udp(any_port(LO4), UdpOptions::default()).unwrap();
    let any = net
        .bind_udp(
            any_port(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            UdpOptions::default(),
        )
        .unwrap();
    let sink = net.bind_udp(any_port(LO4), UdpOptions::default()).unwrap();
    let dst = sink.local_addr();
    let datagram = |src_ip| Datagram {
        dst,
        src_ip,
        payload: b"x",
    };
    for (socket, src_ip, ok) in [
        (&bound, Some(LO4), true),
        (&bound, Some(FOREIGN), false),
        (&bound, Some(IpAddr::V6(Ipv6Addr::LOCALHOST)), false),
        (&any, Some(LO4), true),
        (&any, Some(FOREIGN), false),
        (&any, Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), false),
        (&any, None, true),
    ] {
        let outcome = host.block_on(socket.send(&[datagram(src_ip), datagram(None)]));
        if ok {
            assert_eq!(outcome.sent, 2, "{src_ip:?}: {outcome:?}");
        } else {
            assert_eq!(outcome.sent, 0, "{src_ip:?}");
            assert!(
                matches!(outcome.error, Some(NetError::InvalidInput(_))),
                "{src_ip:?}: {outcome:?}"
            );
        }
    }
    let v6 = Datagram {
        dst: SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9),
        src_ip: None,
        payload: b"x",
    };
    assert!(matches!(
        host.block_on(bound.send(&[v6])).error,
        Some(NetError::InvalidInput(_))
    ));
    finish(host);
}

#[test]
fn a_datagram_above_the_path_mtu_is_message_too_long() {
    let (host, net) = setup();
    let a = net.bind_udp(any_port(LO4), UdpOptions::default()).unwrap();
    let b = net.bind_udp(any_port(LO4), UdpOptions::default()).unwrap();
    let big = vec![1u8; 65_508];
    let small = [2u8; 10];
    let batch = [
        Datagram {
            dst: b.local_addr(),
            src_ip: None,
            payload: &small,
        },
        Datagram {
            dst: b.local_addr(),
            src_ip: None,
            payload: &big,
        },
    ];
    let outcome = host.block_on(a.send(&batch));
    assert_eq!(outcome.sent, 1);
    let Some(NetError::MessageTooLong { max_payload }) = outcome.error else {
        panic!("{outcome:?}");
    };
    assert!((548..=65_507).contains(&max_payload), "{max_payload}");
    let fits = vec![3u8; max_payload as usize];
    let fitting = [Datagram {
        dst: b.local_addr(),
        src_ip: None,
        payload: &fits,
    }];
    assert_eq!(host.block_on(a.send(&fitting)).error, None);
    finish(host);
}

#[test]
fn ipv6_unspecified_receives_ipv6_only() {
    let (host, net) = setup();
    let v6 = match net.bind_udp(
        any_port(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
        UdpOptions::default(),
    ) {
        Ok(socket) => socket,
        // A host without IPv6 has nothing to check.
        Err(NetError::AddrNotAvailable | NetError::Unsupported) => return finish(host),
        Err(e) => panic!("{e:?}"),
    };
    let port = v6.local_addr().port();
    // The IPv4 port is still free: the IPv6 socket does not take it.
    let v4 = net
        .bind_udp(SocketAddr::new(LO4, port), UdpOptions::default())
        .unwrap();
    drop(v4);
    finish(host);
}

#[test]
fn interfaces_report_a_generation() {
    let (host, net) = setup();
    let list = host.block_on(net.interfaces(None)).unwrap();
    assert!(list.generation >= 1);
    assert!(list.interfaces.iter().any(|i| i.loopback));
    let again = host
        .block_on(net.interfaces(Some(list.generation - 1)))
        .unwrap();
    assert!(again.generation >= list.generation);
    let mut waiting = net.interfaces(Some(again.generation));
    // The wait reads the list on the blocking pool; give it the first read.
    let first = host.block_on(async {
        let mut polls = 0;
        std::future::poll_fn(|cx| {
            polls += 1;
            match waiting.as_mut().poll(cx) {
                Poll::Ready(r) => Poll::Ready(Some(r)),
                Poll::Pending if polls > 1 => Poll::Ready(None),
                Poll::Pending => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        })
        .await
    });
    // Unless the host's interfaces changed meanwhile, the wait goes on.
    if let Some(changed) = first {
        assert!(changed.unwrap().generation > again.generation);
    }
    drop(waiting);
    finish(host);
}

#[test]
fn a_wait_ends_at_the_first_change_after_the_call() {
    // No generation given: the current list at once.
    assert!(answers(None, 1, 1));
    // A first read already newer than `g`: at once.
    assert!(answers(Some(2), 3, 3));
    // `g` current: at the next change, and not before.
    assert!(!answers(Some(3), 3, 3));
    assert!(answers(Some(3), 3, 4));
    // `g` ahead of this instance, kept from an earlier one: at the next change
    // all the same, not once the generation passes `g`.
    assert!(!answers(Some(10), 3, 3));
    assert!(answers(Some(10), 3, 4));
}

/// A connected pair over TCP on loopback: (client, server).
fn tcp_pair(host: &OsHost, net: &OsNet) -> (Box<dyn Stream>, Box<dyn Stream>, Box<dyn Listener>) {
    let listener = net
        .listen(StreamAddr::Ip(any_port(LO4)), ListenOptions::default())
        .unwrap();
    let addr = listener.local_addr();
    let client = host
        .block_on(net.connect(addr, StreamOptions::default()))
        .unwrap();
    let server = host.block_on(listener.accept()).unwrap();
    (client, server, listener)
}

#[test]
fn tcp_streams_carry_bytes_and_end_of_stream() {
    let (host, net) = setup();
    let (client, server, listener) = tcp_pair(&host, &net);
    assert_eq!(client.peer_addr(), Some(listener.local_addr()));
    assert_eq!(server.local_addr(), Some(listener.local_addr()));
    assert_eq!(host.block_on(client.write(&[b"hel", b"", b"lo"])), Ok(5));
    assert_eq!(host.block_on(client.write(&[])), Ok(0));
    let mut buf = [0u8; 16];
    let mut got = Vec::new();
    while got.len() < 5 {
        let n = host.block_on(server.read(&mut buf)).unwrap();
        assert!(n >= 1);
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, b"hello");
    assert_eq!(host.block_on(server.read(&mut [])), Ok(0));
    host.block_on(client.shutdown_write()).unwrap();
    assert_eq!(
        host.block_on(client.write(&[b"late"])),
        Err(NetError::Closed)
    );
    assert_eq!(host.block_on(server.read(&mut buf)), Ok(0));
    // The other direction still works.
    assert_eq!(host.block_on(server.write(&[b"back"])), Ok(4));
    let n = host.block_on(client.read(&mut buf)).unwrap();
    assert_eq!(&buf[..n], &b"back"[..n]);
    assert_eq!(net.streams.load(Ordering::SeqCst), 3);
    drop((client, server, listener));
    assert_eq!(net.streams.load(Ordering::SeqCst), 0);
    finish(host);
}

#[test]
fn a_second_pending_read_or_write_is_busy() {
    let (host, net) = setup();
    let (client, _server, _listener) = tcp_pair(&host, &net);
    let mut first_buf = [0u8; 4];
    let mut second_buf = [0u8; 4];
    let mut first = client.read(&mut first_buf);
    assert!(poll_once(first.as_mut()).is_pending());
    assert_eq!(
        host.block_on(client.read(&mut second_buf)),
        Err(NetError::Busy)
    );
    drop(first);
    let mut again = client.read(&mut second_buf);
    assert!(poll_once(again.as_mut()).is_pending());
    drop(again);
    finish(host);
}

#[test]
fn a_reset_fails_reads_and_writes() {
    let (host, net) = setup();
    let listener = net
        .listen(StreamAddr::Ip(any_port(LO4)), ListenOptions::default())
        .unwrap();
    let StreamAddr::Ip(addr) = listener.local_addr() else {
        panic!("a TCP listener has an IP address");
    };
    let raw = host
        .block_on(async { tokio::net::TcpStream::connect(addr).await })
        .unwrap();
    let server = host.block_on(listener.accept()).unwrap();
    // A zero linger time makes the close send a reset.
    raw.set_zero_linger().unwrap();
    drop(raw);
    let mut buf = [0u8; 8];
    assert_eq!(
        host.block_on(server.read(&mut buf)),
        Err(NetError::ConnectionReset)
    );
    assert_eq!(
        host.block_on(server.write(&[b"x"])),
        Err(NetError::ConnectionReset)
    );
    assert_eq!(
        host.block_on(server.shutdown_write()),
        Err(NetError::ConnectionReset)
    );
    finish(host);
}

#[test]
fn listen_and_connect_errors() {
    let (host, net) = setup();
    for backlog in [0, 65_536] {
        let options = ListenOptions {
            backlog,
            ..ListenOptions::default()
        };
        assert!(matches!(
            net.listen(StreamAddr::Ip(any_port(LO4)), options)
                .map(|_| ()),
            Err(NetError::InvalidInput(_))
        ));
    }
    let listener = net
        .listen(StreamAddr::Ip(any_port(LO4)), ListenOptions::default())
        .unwrap();
    let addr = listener.local_addr();
    assert_eq!(
        net.listen(addr.clone(), ListenOptions::default())
            .map(|_| ()),
        Err(NetError::AddrInUse)
    );
    drop(listener);
    // The port can be listened on again at once, then refuses once closed.
    let again = net.listen(addr.clone(), ListenOptions::default()).unwrap();
    drop(again);
    assert_eq!(
        host.block_on(net.connect(addr, StreamOptions::default()))
            .map(|_| ()),
        Err(NetError::ConnectionRefused)
    );
    net.streams.store(MAX_STREAMS, Ordering::SeqCst);
    assert_eq!(
        net.listen(StreamAddr::Ip(any_port(LO4)), ListenOptions::default())
            .map(|_| ()),
        Err(NetError::TooManyOpen)
    );
    net.streams.store(0, Ordering::SeqCst);
    finish(host);
}

#[cfg(unix)]
#[test]
fn unix_streams_round_trip_and_clean_up() {
    use std::os::unix::fs::PermissionsExt;

    let (host, net) = setup();
    let dir = std::env::temp_dir().join(format!("alloy-rt-unix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("s.sock");
    let unix = UnixPath::new(path.to_str().unwrap()).unwrap();
    let options = ListenOptions {
        unix_mode: 0o640,
        ..ListenOptions::default()
    };
    let listener = net.listen(StreamAddr::Unix(unix.clone()), options).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o640);
    assert_eq!(
        net.listen(StreamAddr::Unix(unix.clone()), options)
            .map(|_| ()),
        Err(NetError::AddrInUse)
    );
    let client = host
        .block_on(net.connect(StreamAddr::Unix(unix.clone()), StreamOptions::default()))
        .unwrap();
    let server = host.block_on(listener.accept()).unwrap();
    assert_eq!(client.peer_addr(), Some(StreamAddr::Unix(unix)));
    assert_eq!(client.local_addr(), None);
    assert_eq!(host.block_on(client.write(&[b"ping"])), Ok(4));
    let mut buf = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        got += host.block_on(server.read(&mut buf[got..])).unwrap();
    }
    assert_eq!(&buf, b"ping");
    drop(listener);
    assert!(std::fs::symlink_metadata(&path).is_err());
    drop((client, server));
    std::fs::remove_dir(&dir).unwrap();
    finish(host);
}

#[test]
fn peer_uid_of_a_tcp_stream_is_invalid_input() {
    let (host, net) = setup();
    let (client, server, listener) = tcp_pair(&host, &net);
    for stream in [&client, &server] {
        assert!(matches!(stream.peer_uid(), Err(NetError::InvalidInput(_))));
    }
    drop((client, server, listener));
    finish(host);
}

#[cfg(unix)]
#[allow(unsafe_code, reason = "geteuid has no safe binding")]
fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory of ours, and
    // always succeeds.
    unsafe { libc::geteuid() }
}

/// ARFC 0005 clause 3.1.9.2: on Linux both ends see the user id of this
/// process, and every other Unix platform returns `Unsupported`.
#[cfg(unix)]
#[test]
fn peer_uid_of_a_unix_stream_is_the_peer_process_user() {
    let (host, net) = setup();
    let dir = std::env::temp_dir().join(format!("alloy-rt-peer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.sock");
    let unix = StreamAddr::Unix(UnixPath::new(path.to_str().unwrap()).unwrap());
    let listener = net.listen(unix.clone(), ListenOptions::default()).unwrap();
    let client = host
        .block_on(net.connect(unix, StreamOptions::default()))
        .unwrap();
    let server = host.block_on(listener.accept()).unwrap();
    let want = if cfg!(target_os = "linux") {
        Ok(effective_uid())
    } else {
        Err(NetError::Unsupported)
    };
    assert_eq!(client.peer_uid(), want);
    assert_eq!(server.peer_uid(), want);
    // It reads and writes nothing: the stream still carries bytes after.
    assert_eq!(host.block_on(client.write(&[b"ok"])), Ok(2));
    let mut buf = [0u8; 2];
    let mut got = 0;
    while got < 2 {
        got += host.block_on(server.read(&mut buf[got..])).unwrap();
    }
    assert_eq!(&buf, b"ok");
    drop((client, server, listener));
    std::fs::remove_dir(&dir).unwrap();
    finish(host);
}

#[cfg(not(unix))]
#[test]
fn unix_addresses_are_unsupported() {
    let (host, net) = setup();
    let unix = UnixPath::new("/tmp/alloy-rt.sock").unwrap();
    assert_eq!(
        net.listen(StreamAddr::Unix(unix.clone()), ListenOptions::default())
            .map(|_| ()),
        Err(NetError::Unsupported)
    );
    assert_eq!(
        host.block_on(net.connect(StreamAddr::Unix(unix), StreamOptions::default()))
            .map(|_| ()),
        Err(NetError::Unsupported)
    );
    finish(host);
}
