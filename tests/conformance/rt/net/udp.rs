//! UDP sockets, clauses 2.3 and 3.1.8.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use alloy_rt::{Datagram, Net, NetError, NetSeam, RecvSlot, SendOutcome, UdpOptions, UdpSocket};

use super::{interfaces, loopback, loopback_v4};
use crate::support::{
    Ctx, PATIENCE, SETTLE, eventually, fail, fresh, name, poll_once, skip, within,
};
use crate::{Applies, Check, Env, Failure, Implementation, Outcome};

pub(super) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_bind_to_port_zero_picks_a_free_port
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::Only(Implementation::Mem),
            arfc0005_s3_1_8_mem_picks_ports_from_49152_to_65535
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_bind_to_a_bound_address_is_addr_in_use
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_bind_to_a_foreign_address_is_addr_not_available
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_buffer_sizes_outside_the_range_are_invalid_input
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_an_instance_holds_at_most_4096_udp_sockets
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_send_and_recv_over_loopback
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_send_of_more_than_64_is_batch_too_large
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_send_stops_at_the_first_failure
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_src_ip_must_be_the_bound_or_a_host_address
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_datagram_above_the_path_mtu_is_message_too_long
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_dropped_send_has_sent_a_prefix
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_recv_with_no_slots_is_invalid_input
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_recv_fills_at_most_64_slots_in_arrival_order
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_each_datagram_goes_to_exactly_one_recv
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_dropped_recv_has_taken_no_datagram
        ),
        check!(
            "arfc-0005#2.3",
            Applies::All,
            arfc0005_s2_3_recv_truncates_a_datagram_longer_than_its_slot
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::Only(Implementation::Mem),
            arfc0005_s3_1_8_mem_recv_buffer_drops_what_does_not_fit
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_ipv6_unspecified_bind_receives_ipv6_only
        ),
        check!(
            "arfc-0005#3.1.8",
            Applies::All,
            arfc0005_s3_1_8_icmp_errors_are_not_reported
        ),
    ]
}

/// UDP sockets per `net` instance (clause 3.1.8).
const UDP_SOCKETS: usize = 4_096;

/// A documentation address (RFC 5737) that no test host holds.
const FOREIGN_V4: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

/// A documentation address (RFC 3849) that no test host holds.
const FOREIGN_V6: IpAddr = IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1));

fn bind(net: &dyn Net, ip: IpAddr) -> Result<Box<dyn UdpSocket>, Failure> {
    net.bind_udp(SocketAddr::new(ip, 0), UdpOptions::default())
        .ctx("bind_udp")
}

fn slots(count: usize, capacity: usize) -> Result<Vec<RecvSlot>, Failure> {
    (0..count)
        .map(|_| RecvSlot::new(capacity).ctx("RecvSlot::new"))
        .collect()
}

/// A payload that starts with `id`, `len` bytes long (at least 4).
fn payload(id: u32, len: usize) -> Vec<u8> {
    let mut bytes = vec![0xA5u8; len.max(4)];
    bytes[..4].copy_from_slice(&id.to_be_bytes());
    bytes
}

fn id_of(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?))
}

/// One received datagram.
#[derive(Debug)]
struct Received {
    bytes: Vec<u8>,
    truncated: bool,
    src: SocketAddr,
    dst_ip: IpAddr,
}

/// Receives datagrams until none arrives for [`SETTLE`].
async fn drain(env: &Env, socket: &dyn UdpSocket) -> Result<Vec<Received>, Failure> {
    let mut received = Vec::new();
    loop {
        let mut buffers = slots(64, RecvSlot::MAX_CAPACITY)?;
        let Some(result) = within(env, SETTLE, socket.recv(&mut buffers)).await else {
            return Ok(received);
        };
        let count = result.ctx("recv")?;
        ensure!(
            (1..=64).contains(&count),
            "recv returned {count} for 64 slots"
        );
        for slot in &buffers[..count] {
            received.push(Received {
                bytes: slot.buf[..slot.len].to_vec(),
                truncated: slot.truncated,
                src: slot.src,
                dst_ip: slot.dst_ip,
            });
        }
    }
}

fn ids(received: &[Received]) -> Vec<u32> {
    received
        .iter()
        .filter_map(|datagram| id_of(&datagram.bytes))
        .collect()
}

/// Sends `batch` and returns its outcome, failing the check if it takes
/// longer than [`PATIENCE`].
async fn send(
    env: &Env,
    socket: &dyn UdpSocket,
    batch: &[Datagram<'_>],
) -> Result<SendOutcome, Failure> {
    within(env, PATIENCE, socket.send(batch)).await.ctx("send")
}

fn complete(count: usize) -> SendOutcome {
    SendOutcome {
        sent: count,
        error: None,
    }
}

/// `bind_udp` with port 0 picks a free port.
async fn arfc0005_s3_1_8_bind_to_port_zero_picks_a_free_port(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let first = bind(net.as_ref(), ip)?;
    let second = bind(net.as_ref(), ip)?;
    for socket in [&first, &second] {
        let local = socket.local_addr();
        ensure_eq!(local.ip(), ip, "the bound address");
        ensure!(local.port() != 0, "bind to port 0 left port 0");
    }
    ensure!(
        first.local_addr().port() != second.local_addr().port(),
        "two sockets got the same port {}",
        first.local_addr().port()
    );
    Ok(())
}

/// `mem` picks a port for port 0 from 49,152 to 65,535.
async fn arfc0005_s3_1_8_mem_picks_ports_from_49152_to_65535(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let mut sockets = Vec::new();
    for _ in 0..256 {
        let socket = bind(net.as_ref(), ip)?;
        let port = socket.local_addr().port();
        ensure!(port >= 49_152, "mem picked port {port}");
        sockets.push(socket);
    }
    Ok(())
}

/// `bind_udp` to an address and port that are bound fails with `AddrInUse`.
async fn arfc0005_s3_1_8_bind_to_a_bound_address_is_addr_in_use(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let first = bind(net.as_ref(), ip)?;
    let again = net.bind_udp(first.local_addr(), UdpOptions::default());
    ensure_eq!(
        again.err(),
        Some(NetError::AddrInUse),
        "bind to {}, which is bound",
        first.local_addr()
    );
    Ok(())
}

/// `bind_udp` to an address on no interface of the host fails with
/// `AddrNotAvailable`.
async fn arfc0005_s3_1_8_bind_to_a_foreign_address_is_addr_not_available(env: Env) -> Outcome {
    let list = interfaces(&env).await?;
    let held: BTreeSet<IpAddr> = list
        .interfaces
        .iter()
        .flat_map(|interface| interface.addrs.iter().map(|addr| addr.ip))
        .collect();
    let net = env.runtime.net().clone();
    let mut tested = 0;
    for foreign in [FOREIGN_V4, FOREIGN_V6] {
        if held.contains(&foreign) {
            continue;
        }
        tested += 1;
        let result = net.bind_udp(SocketAddr::new(foreign, 0), UdpOptions::default());
        ensure_eq!(
            result.err(),
            Some(NetError::AddrNotAvailable),
            "bind to {foreign}, which no interface holds"
        );
    }
    if tested == 0 {
        return skip("the host holds both documentation addresses");
    }
    Ok(())
}

/// A receive or send buffer outside 65,536 to 67,108,864 bytes fails with
/// `InvalidInput`; both bounds are accepted.
async fn arfc0005_s3_1_8_buffer_sizes_outside_the_range_are_invalid_input(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let normal = UdpOptions::default().recv_buffer;
    for (recv_buffer, send_buffer) in [
        (65_535, normal),
        (67_108_865, normal),
        (0, normal),
        (u32::MAX, normal),
        (normal, 65_535),
        (normal, 67_108_865),
        (normal, 0),
        (normal, u32::MAX),
    ] {
        let options = UdpOptions {
            recv_buffer,
            send_buffer,
        };
        let result = net.bind_udp(SocketAddr::new(ip, 0), options);
        ensure!(
            matches!(result, Err(NetError::InvalidInput(_))),
            "bind with {options:?}: {:?}",
            result.err()
        );
    }
    for size in [65_536, 67_108_864] {
        let options = UdpOptions {
            recv_buffer: size,
            send_buffer: size,
        };
        net.bind_udp(SocketAddr::new(ip, 0), options)
            .ctx("bind with buffers at a bound")?;
    }
    Ok(())
}

/// `bind_udp` fails with `TooManyOpen` when the instance has 4,096 open UDP
/// sockets. `os` may meet a lower platform limit first, and must then still
/// report `TooManyOpen`.
async fn arfc0005_s3_1_8_an_instance_holds_at_most_4096_udp_sockets(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net: Arc<dyn Net> = fresh::<NetSeam>(&env, &env.config.net)?;
    let mut sockets = Vec::with_capacity(UDP_SOCKETS);
    let refused = loop {
        if sockets.len() > UDP_SOCKETS {
            return Err(fail("an instance held more than 4096 UDP sockets"));
        }
        match net.bind_udp(SocketAddr::new(ip, 0), UdpOptions::default()) {
            Ok(socket) => sockets.push(socket),
            Err(error) => break error,
        }
    };
    ensure_eq!(
        refused,
        NetError::TooManyOpen,
        "bind beyond the socket limit"
    );
    if env.implementation == Implementation::Mem {
        ensure_eq!(
            sockets.len(),
            UDP_SOCKETS,
            "UDP sockets a fresh mem instance held"
        );
    }
    sockets.pop();
    net.bind_udp(SocketAddr::new(ip, 0), UdpOptions::default())
        .ctx("bind after closing one socket")?;
    Ok(())
}

/// Datagrams sent over loopback arrive in order, each in one slot with its
/// source, its destination address, and its length.
async fn arfc0005_s3_1_8_send_and_recv_over_loopback(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let sender = bind(net.as_ref(), ip)?;
    let receiver = bind(net.as_ref(), ip)?;
    let payloads = [payload(0, 4), payload(1, 100), payload(2, 1_200)];
    let batch: Vec<Datagram<'_>> = payloads
        .iter()
        .map(|bytes| Datagram {
            dst: receiver.local_addr(),
            src_ip: None,
            payload: bytes,
        })
        .collect();
    let outcome = send(&env, sender.as_ref(), &batch).await?;
    ensure_eq!(outcome, complete(3), "send of three datagrams");
    let received = drain(&env, receiver.as_ref()).await?;
    ensure_eq!(received.len(), 3, "datagrams received");
    for (datagram, expected) in received.iter().zip(&payloads) {
        ensure_eq!(&datagram.bytes, expected, "payload");
        ensure!(
            !datagram.truncated,
            "a datagram that fit was marked truncated"
        );
        ensure_eq!(datagram.src, sender.local_addr(), "source");
        ensure_eq!(datagram.dst_ip, ip, "destination address");
    }
    Ok(())
}

/// `send` with more than 64 datagrams returns `sent: 0` and `BatchTooLarge`,
/// and sends nothing.
async fn arfc0005_s3_1_8_send_of_more_than_64_is_batch_too_large(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let sender = bind(net.as_ref(), ip)?;
    let receiver = bind(net.as_ref(), ip)?;
    let payloads: Vec<Vec<u8>> = (0..65).map(|id| payload(id, 8)).collect();
    let batch: Vec<Datagram<'_>> = payloads
        .iter()
        .map(|bytes| Datagram {
            dst: receiver.local_addr(),
            src_ip: None,
            payload: bytes,
        })
        .collect();
    let outcome = send(&env, sender.as_ref(), &batch).await?;
    ensure_eq!(
        outcome,
        SendOutcome {
            sent: 0,
            error: Some(NetError::BatchTooLarge)
        },
        "send of 65 datagrams"
    );
    let received = drain(&env, receiver.as_ref()).await?;
    ensure_eq!(
        received.len(),
        0,
        "datagrams of a refused batch that arrived"
    );
    let outcome = send(&env, sender.as_ref(), &batch[..64]).await?;
    ensure_eq!(outcome, complete(64), "send of 64 datagrams");
    Ok(())
}

/// `send` sends in order and stops at the first datagram that fails;
/// `error` is that datagram's failure.
async fn arfc0005_s3_1_8_send_stops_at_the_first_failure(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let sender = bind(net.as_ref(), ip)?;
    let receiver = bind(net.as_ref(), ip)?;
    let payloads: Vec<Vec<u8>> = (0..3).map(|id| payload(id, 16)).collect();
    let batch = [
        Datagram {
            dst: receiver.local_addr(),
            src_ip: None,
            payload: &payloads[0],
        },
        Datagram {
            dst: receiver.local_addr(),
            src_ip: Some(FOREIGN_V4),
            payload: &payloads[1],
        },
        Datagram {
            dst: receiver.local_addr(),
            src_ip: None,
            payload: &payloads[2],
        },
    ];
    let outcome = send(&env, sender.as_ref(), &batch).await?;
    ensure_eq!(outcome.sent, 1, "datagrams sent before the failing one");
    ensure!(
        matches!(outcome.error, Some(NetError::InvalidInput(_))),
        "error of a datagram with a foreign src_ip: {:?}",
        outcome.error
    );
    let received = drain(&env, receiver.as_ref()).await?;
    ensure_eq!(ids(&received), vec![0], "ids received");
    Ok(())
}

/// `src_ip` must be `None`, the bound address, or, for a socket bound to an
/// unspecified address, an address of the host in the same family; any
/// other is `InvalidInput`.
async fn arfc0005_s3_1_8_src_ip_must_be_the_bound_or_a_host_address(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let receiver = bind(net.as_ref(), ip)?;
    let bound = bind(net.as_ref(), ip)?;
    let unspecified = net
        .bind_udp(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            UdpOptions::default(),
        )
        .ctx("bind to 0.0.0.0")?;
    let bytes = payload(0, 8);
    let datagram = |src_ip| Datagram {
        dst: receiver.local_addr(),
        src_ip,
        payload: &bytes,
    };
    for (socket, src_ip, valid) in [
        (&bound, None, true),
        (&bound, Some(ip), true),
        (&bound, Some(FOREIGN_V4), false),
        (&unspecified, None, true),
        (&unspecified, Some(ip), true),
        (&unspecified, Some(FOREIGN_V4), false),
        (&unspecified, Some(IpAddr::V6(Ipv6Addr::LOCALHOST)), false),
    ] {
        let outcome = send(&env, socket.as_ref(), &[datagram(src_ip)]).await?;
        if valid {
            ensure_eq!(outcome, complete(1), "send from {src_ip:?}");
        } else {
            ensure_eq!(outcome.sent, 0, "sent with src_ip {src_ip:?}");
            ensure!(
                matches!(outcome.error, Some(NetError::InvalidInput(_))),
                "send with src_ip {src_ip:?}: {:?}",
                outcome.error
            );
        }
    }
    let received = drain(&env, receiver.as_ref()).await?;
    ensure_eq!(
        received.len(),
        4,
        "datagrams with valid sources that arrived"
    );
    Ok(())
}

/// A datagram whose IP packet exceeds the path MTU fails with
/// `MessageTooLong`, with `max_payload` the MTU minus the IP header minus 8;
/// one of exactly `max_payload` bytes is sent.
async fn arfc0005_s3_1_8_datagram_above_the_path_mtu_is_message_too_long(env: Env) -> Outcome {
    let net = env.runtime.net().clone();
    let mut tested = 0;
    for (v6, header, largest_payload) in [(false, 20u32, 65_507u32), (true, 40, 65_527)] {
        let Some((interface, ip)) = loopback(&env, v6).await? else {
            continue;
        };
        let Some(max_payload) = interface.mtu.checked_sub(header + 8) else {
            continue;
        };
        if max_payload > largest_payload {
            // The MTU is larger than any IP packet of this family can be,
            // so no payload reaches the bound (see the report's spec issue).
            continue;
        }
        tested += 1;
        let receiver = net
            .bind_udp(
                SocketAddr::new(ip, 0),
                UdpOptions {
                    recv_buffer: 67_108_864,
                    send_buffer: 67_108_864,
                },
            )
            .ctx("bind")?;
        let sender = bind(net.as_ref(), ip)?;
        let fits = payload(1, max_payload as usize);
        let too_long = payload(2, max_payload as usize + 1);
        let outcome = send(
            &env,
            sender.as_ref(),
            &[Datagram {
                dst: receiver.local_addr(),
                src_ip: None,
                payload: &fits,
            }],
        )
        .await?;
        ensure_eq!(outcome, complete(1), "send of {max_payload} bytes to {ip}");
        let outcome = send(
            &env,
            sender.as_ref(),
            &[Datagram {
                dst: receiver.local_addr(),
                src_ip: None,
                payload: &too_long,
            }],
        )
        .await?;
        ensure_eq!(
            outcome,
            SendOutcome {
                sent: 0,
                error: Some(NetError::MessageTooLong { max_payload })
            },
            "send of {} bytes to {ip} with MTU {}",
            max_payload + 1,
            interface.mtu
        );
    }
    if tested == 0 {
        return skip("no loopback interface has an MTU whose bound a payload can reach");
    }
    Ok(())
}

/// A dropped `send` future has sent a prefix of its batch, possibly empty,
/// and nothing else of it.
async fn arfc0005_s3_1_8_dropped_send_has_sent_a_prefix(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let sender = bind(net.as_ref(), ip)?;
    let receiver = bind(net.as_ref(), ip)?;
    let payloads: Vec<Vec<u8>> = (0..10).map(|id| payload(id, 32)).collect();
    let batch: Vec<Datagram<'_>> = payloads
        .iter()
        .map(|bytes| Datagram {
            dst: receiver.local_addr(),
            src_ip: None,
            payload: bytes,
        })
        .collect();

    drop(sender.send(&batch));
    ensure_eq!(
        ids(&drain(&env, receiver.as_ref()).await?),
        Vec::<u32>::new(),
        "datagrams of a send dropped before its first poll"
    );

    let mut sending = sender.send(&batch);
    let _ = poll_once(&mut sending).await;
    drop(sending);
    let received = ids(&drain(&env, receiver.as_ref()).await?);
    let prefix: Vec<u32> = (0..received.len() as u32).collect();
    ensure_eq!(
        received,
        prefix,
        "datagrams of a send dropped after one poll"
    );
    Ok(())
}

/// `recv` with an empty `slots` returns `InvalidInput`.
async fn arfc0005_s3_1_8_recv_with_no_slots_is_invalid_input(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let socket = bind(env.runtime.net().as_ref(), ip)?;
    let mut none: [RecvSlot; 0] = [];
    let result = within(&env, PATIENCE, socket.recv(&mut none))
        .await
        .ctx("recv with no slots")?;
    ensure!(
        matches!(result, Err(NetError::InvalidInput(_))),
        "recv with no slots: {result:?}"
    );
    Ok(())
}

/// `recv` fills up to `min(slots.len(), 64)` slots, from the first, with
/// queued datagrams in arrival order, and returns the count.
async fn arfc0005_s3_1_8_recv_fills_at_most_64_slots_in_arrival_order(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let sender = bind(net.as_ref(), ip)?;
    let receiver = bind(net.as_ref(), ip)?;
    let payloads: Vec<Vec<u8>> = (0..100).map(|id| payload(id, 64)).collect();
    let batch: Vec<Datagram<'_>> = payloads
        .iter()
        .map(|bytes| Datagram {
            dst: receiver.local_addr(),
            src_ip: None,
            payload: bytes,
        })
        .collect();
    for chunk in batch.chunks(50) {
        let outcome = send(&env, sender.as_ref(), chunk).await?;
        ensure_eq!(
            outcome,
            complete(chunk.len()),
            "send of {} datagrams",
            chunk.len()
        );
    }
    let mut order = Vec::new();
    while order.len() < payloads.len() {
        let mut buffers = slots(100, 256)?;
        let count = within(&env, PATIENCE, receiver.recv(&mut buffers))
            .await
            .ctx("recv")?
            .ctx("recv")?;
        ensure!(
            (1..=64).contains(&count),
            "recv into 100 slots returned {count}"
        );
        for (index, slot) in buffers.iter().enumerate() {
            if index < count {
                order.push(id_of(&slot.buf[..slot.len]).ctx("an id in the payload")?);
            } else {
                ensure!(
                    slot.len == 0 && slot.buf.iter().all(|&byte| byte == 0),
                    "recv returned {count} but wrote slot {index}"
                );
            }
        }
    }
    let expected: Vec<u32> = (0..100).collect();
    ensure_eq!(order, expected, "arrival order across recv calls");
    Ok(())
}

/// Each datagram goes to exactly one `recv` call, also when several are
/// pending on the socket.
async fn arfc0005_s3_1_8_each_datagram_goes_to_exactly_one_recv(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let sender = bind(net.as_ref(), ip)?;
    let receiver: Arc<dyn UdpSocket> = Arc::from(bind(net.as_ref(), ip)?);
    let got: Arc<Mutex<Vec<u32>>> = Arc::default();
    let mut tasks = Vec::new();
    for task in 0..3 {
        let receiver = receiver.clone();
        let got = got.clone();
        let body = async move {
            loop {
                let Ok(mut buffers) = slots(1, 64) else {
                    return;
                };
                if let Ok(1) = receiver.recv(&mut buffers).await
                    && let Some(id) = id_of(&buffers[0].buf[..buffers[0].len])
                {
                    got.lock().unwrap_or_else(PoisonError::into_inner).push(id);
                }
            }
        };
        tasks.push(
            env.runtime
                .spawn(name(&format!("receiver-{task}"))?, body)
                .ctx("spawn")?,
        );
    }
    let count = 30u32;
    for id in 0..count {
        let bytes = payload(id, 16);
        let outcome = send(
            &env,
            sender.as_ref(),
            &[Datagram {
                dst: receiver.local_addr(),
                src_ip: None,
                payload: &bytes,
            }],
        )
        .await?;
        ensure_eq!(outcome, complete(1), "send of datagram {id}");
        env.runtime.spawner().yield_now().await;
    }
    let length = || got.lock().unwrap_or_else(PoisonError::into_inner).len();
    eventually(&env, "every datagram is received", || {
        length() >= count as usize
    })
    .await?;
    crate::support::sleep(&env, SETTLE).await;
    for task in &tasks {
        task.cancel();
    }
    for task in tasks {
        let _ = task.join().await;
    }
    let mut all = got.lock().unwrap_or_else(PoisonError::into_inner).clone();
    all.sort_unstable();
    let expected: Vec<u32> = (0..count).collect();
    ensure_eq!(all, expected, "ids received by three concurrent recv loops");
    Ok(())
}

/// A dropped `recv` future has taken no datagram.
async fn arfc0005_s3_1_8_dropped_recv_has_taken_no_datagram(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let sender = bind(net.as_ref(), ip)?;
    let receiver = bind(net.as_ref(), ip)?;

    let mut first = slots(4, 64)?;
    drop(receiver.recv(&mut first));
    let mut second = slots(4, 64)?;
    let mut waiting = receiver.recv(&mut second);
    ensure!(
        poll_once(&mut waiting).await.is_pending(),
        "recv on an empty socket was ready"
    );
    drop(waiting);

    let bytes = payload(7, 16);
    let outcome = send(
        &env,
        sender.as_ref(),
        &[Datagram {
            dst: receiver.local_addr(),
            src_ip: None,
            payload: &bytes,
        }],
    )
    .await?;
    ensure_eq!(outcome, complete(1), "send");
    let received = drain(&env, receiver.as_ref()).await?;
    ensure_eq!(ids(&received), vec![7], "ids received after dropped recvs");
    Ok(())
}

/// A datagram longer than its slot fills the slot, and the slot is marked
/// truncated.
async fn arfc0005_s2_3_recv_truncates_a_datagram_longer_than_its_slot(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let sender = bind(net.as_ref(), ip)?;
    let receiver = bind(net.as_ref(), ip)?;
    let bytes: Vec<u8> = (0..100u8).collect();
    let outcome = send(
        &env,
        sender.as_ref(),
        &[Datagram {
            dst: receiver.local_addr(),
            src_ip: None,
            payload: &bytes,
        }],
    )
    .await?;
    ensure_eq!(outcome, complete(1), "send");
    let mut buffers = slots(1, 10)?;
    let count = within(&env, PATIENCE, receiver.recv(&mut buffers))
        .await
        .ctx("recv")?
        .ctx("recv")?;
    ensure_eq!(count, 1, "slots filled");
    ensure_eq!(buffers[0].len, 10, "len of a truncated slot");
    ensure!(
        buffers[0].truncated,
        "a 100-byte datagram in a 10-byte slot was not truncated"
    );
    ensure_eq!(
        &buffers[0].buf[..],
        &bytes[..10],
        "bytes of a truncated slot"
    );
    Ok(())
}

/// `mem`: the socket queues received payload up to `recv_buffer` bytes and
/// drops a datagram that arrives when it does not fit.
async fn arfc0005_s3_1_8_mem_recv_buffer_drops_what_does_not_fit(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let sender = bind(net.as_ref(), ip)?;
    let receiver = net
        .bind_udp(
            SocketAddr::new(ip, 0),
            UdpOptions {
                recv_buffer: 65_536,
                send_buffer: 4_194_304,
            },
        )
        .ctx("bind")?;
    let payloads: Vec<Vec<u8>> = (0..64).map(|id| payload(id, 1_400)).collect();
    let batch: Vec<Datagram<'_>> = payloads
        .iter()
        .map(|bytes| Datagram {
            dst: receiver.local_addr(),
            src_ip: None,
            payload: bytes,
        })
        .collect();
    let outcome = send(&env, sender.as_ref(), &batch).await?;
    ensure_eq!(outcome, complete(64), "send of 64 datagrams of 1400 bytes");
    let received = drain(&env, receiver.as_ref()).await?;
    // 46 * 1,400 = 64,400 fits in 65,536; a 47th would make 65,800.
    let expected: Vec<u32> = (0..46).collect();
    ensure_eq!(
        ids(&received),
        expected,
        "datagrams queued under a 65536-byte receive buffer"
    );
    Ok(())
}

/// A socket bound to the IPv6 unspecified address receives IPv6 only.
async fn arfc0005_s3_1_8_ipv6_unspecified_bind_receives_ipv6_only(env: Env) -> Outcome {
    let Some((_, v6)) = loopback(&env, true).await? else {
        return skip("the host has no IPv6 loopback address");
    };
    let v4 = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let receiver = net
        .bind_udp(
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
            UdpOptions::default(),
        )
        .ctx("bind to [::]")?;
    let port = receiver.local_addr().port();
    let from_v4 = bind(net.as_ref(), v4)?;
    let from_v6 = bind(net.as_ref(), v6)?;
    let (bytes4, bytes6) = (payload(4, 8), payload(6, 8));
    let outcome = send(
        &env,
        from_v4.as_ref(),
        &[Datagram {
            dst: SocketAddr::new(v4, port),
            src_ip: None,
            payload: &bytes4,
        }],
    )
    .await?;
    ensure_eq!(outcome.sent, 1, "send to the IPv4 loopback address");
    let outcome = send(
        &env,
        from_v6.as_ref(),
        &[Datagram {
            dst: SocketAddr::new(v6, port),
            src_ip: None,
            payload: &bytes6,
        }],
    )
    .await?;
    ensure_eq!(outcome, complete(1), "send to the IPv6 loopback address");
    let received = drain(&env, receiver.as_ref()).await?;
    ensure_eq!(ids(&received), vec![6], "ids received on [::]");
    Ok(())
}

/// ICMP errors other than those of the path MTU rule are never reported: a
/// socket that sent to a closed port keeps sending and receives no error.
async fn arfc0005_s3_1_8_icmp_errors_are_not_reported(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let closed = bind(net.as_ref(), ip)?.local_addr();
    let sender = bind(net.as_ref(), ip)?;
    let bytes = payload(1, 8);
    let datagram = [Datagram {
        dst: closed,
        src_ip: None,
        payload: &bytes,
    }];
    ensure_eq!(
        send(&env, sender.as_ref(), &datagram).await?,
        complete(1),
        "first send to a closed port"
    );
    crate::support::sleep(&env, SETTLE).await;
    ensure_eq!(
        send(&env, sender.as_ref(), &datagram).await?,
        complete(1),
        "second send to a closed port"
    );
    let mut buffers = slots(1, 64)?;
    let result = within(&env, SETTLE, sender.recv(&mut buffers)).await;
    ensure!(
        result.is_none(),
        "recv on a socket that sent to a closed port returned {result:?}"
    );
    Ok(())
}
