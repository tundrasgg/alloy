//! Suite `net` (ARFC 0005 section 5): the `Net` seam, UDP sockets, streams,
//! and listeners, clauses 2.1, 2.3, 3.1.3, and 3.1.7 to 3.1.9.

use std::net::IpAddr;
use std::time::Duration;

use alloy_rt::{Interface, InterfaceList};

use crate::support::{Ctx, PATIENCE, fail, poll_once, within};
use crate::{Applies, Check, Env, Failure, Outcome};

mod stream;
mod udp;

pub(crate) fn checks() -> Vec<Check> {
    let mut all = vec![
        check!(
            "arfc-0005#3.1.7",
            Applies::All,
            arfc0005_s3_1_7_interfaces_lists_the_host_in_order_and_within_bounds
        ),
        check!(
            "arfc-0005#3.1.7",
            Applies::All,
            arfc0005_s3_1_7_generation_is_stable_without_a_change
        ),
        check!(
            "arfc-0005#3.1.7",
            Applies::All,
            arfc0005_s3_1_7_newer_than_an_older_generation_completes_at_once
        ),
        check!(
            "arfc-0005#3.1.7",
            Applies::All,
            arfc0005_s3_1_7_newer_than_the_current_generation_waits_for_a_change
        ),
    ];
    all.extend(udp::checks());
    all.extend(stream::checks());
    all
}

/// The host's interface list.
pub(crate) async fn interfaces(env: &Env) -> Result<InterfaceList, Failure> {
    within(env, PATIENCE, env.runtime.net().interfaces(None))
        .await
        .ctx("interfaces(None)")?
        .ctx("interfaces(None)")
}

/// The up loopback interface that holds a loopback address of the family,
/// with that address.
pub(crate) async fn loopback(env: &Env, v6: bool) -> Result<Option<(Interface, IpAddr)>, Failure> {
    let list = interfaces(env).await?;
    Ok(list
        .interfaces
        .into_iter()
        .filter(|interface| interface.up && interface.loopback)
        .find_map(|interface| {
            let ip = interface
                .addrs
                .iter()
                .map(|addr| addr.ip)
                .find(|ip| ip.is_ipv6() == v6 && ip.is_loopback())?;
            Some((interface, ip))
        }))
}

/// The IPv4 loopback address, which every check that talks to itself needs.
pub(crate) async fn loopback_v4(env: &Env) -> Result<IpAddr, Failure> {
    loopback(env, false)
        .await?
        .map(|(_, ip)| ip)
        .ok_or_else(|| {
            fail("the host lists no up loopback interface with an IPv4 loopback address")
        })
}

/// Interfaces are sorted by index, names are 1 to 15 bytes, addresses are
/// IPv4 before IPv6 and each ascending, and the bounds of 1,024 interfaces
/// and 64 addresses hold.
async fn arfc0005_s3_1_7_interfaces_lists_the_host_in_order_and_within_bounds(env: Env) -> Outcome {
    let list = interfaces(&env).await?;
    ensure!(
        list.interfaces.len() <= 1_024,
        "{} interfaces listed",
        list.interfaces.len()
    );
    for pair in list.interfaces.windows(2) {
        ensure!(
            pair[0].index < pair[1].index,
            "interface {} is listed before {}",
            pair[0].index,
            pair[1].index
        );
    }
    for interface in &list.interfaces {
        ensure!(
            (1..=15).contains(&interface.name.len()),
            "interface {} has a name of {} bytes",
            interface.index,
            interface.name.len()
        );
        ensure!(
            interface.addrs.len() <= 64,
            "interface {} lists {} addresses",
            interface.index,
            interface.addrs.len()
        );
        for pair in interface.addrs.windows(2) {
            ensure!(
                pair[0].ip < pair[1].ip,
                "interface {} lists {} before {}",
                interface.index,
                pair[0].ip,
                pair[1].ip
            );
        }
        for addr in &interface.addrs {
            let bits = if addr.ip.is_ipv4() { 32 } else { 128 };
            ensure!(
                addr.prefix_len <= bits,
                "{}/{} on interface {}",
                addr.ip,
                addr.prefix_len,
                interface.index
            );
        }
    }
    Ok(())
}

/// The generation changes only when the list changes: two reads with no
/// change between them return the same generation and list.
async fn arfc0005_s3_1_7_generation_is_stable_without_a_change(env: Env) -> Outcome {
    let first = interfaces(&env).await?;
    let second = interfaces(&env).await?;
    ensure_eq!(
        second,
        first,
        "a second interface list with no change between"
    );
    Ok(())
}

/// `interfaces(Some(g))` with `g` below the current generation completes with
/// the current list at once.
async fn arfc0005_s3_1_7_newer_than_an_older_generation_completes_at_once(env: Env) -> Outcome {
    let current = interfaces(&env).await?;
    let Some(older) = current.generation.checked_sub(1) else {
        return crate::support::skip("the interface list is at generation 0");
    };
    let newer = within(&env, PATIENCE, env.runtime.net().interfaces(Some(older)))
        .await
        .ctx("interfaces(Some(older))")?
        .ctx("interfaces(Some(older))")?;
    ensure_eq!(newer, current, "interfaces(Some(generation - 1))");
    Ok(())
}

/// `interfaces(Some(g))` with `g` the current generation waits for the next
/// change, and its future can be dropped while it waits.
async fn arfc0005_s3_1_7_newer_than_the_current_generation_waits_for_a_change(env: Env) -> Outcome {
    let current = interfaces(&env).await?;
    let net = env.runtime.net().clone();
    let mut waiting = net.interfaces(Some(current.generation));
    let first = poll_once(&mut waiting).await;
    ensure!(
        first.is_pending(),
        "interfaces(Some(current generation)) completed with no change: {first:?}"
    );
    ensure!(
        within(&env, Duration::from_millis(100), waiting)
            .await
            .is_none(),
        "interfaces(Some(current generation)) completed with no change"
    );
    let mut huge = net.interfaces(Some(u64::MAX));
    let _ = poll_once(&mut huge).await;
    drop(huge);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use alloy_rt::{
        Datagram, ListenOptions, MAX_NET_BATCH, NetError, RecvSlot, SendOutcome, StreamOptions,
        UdpOptions, UnixPath,
    };
    use alloy_seam::Detail;

    use crate::support::Gen;

    #[test]
    fn arfc0005_s2_3_constants_and_defaults() {
        assert_eq!(MAX_NET_BATCH, 64);
        assert_eq!(RecvSlot::MAX_CAPACITY, 65_535);
        assert_eq!(
            UdpOptions::default(),
            UdpOptions {
                recv_buffer: 4_194_304,
                send_buffer: 4_194_304
            }
        );
        assert_eq!(StreamOptions::default(), StreamOptions { nodelay: true });
        assert_eq!(
            ListenOptions::default(),
            ListenOptions {
                backlog: 1_024,
                unix_mode: 0o600
            }
        );
    }

    #[test]
    fn arfc0005_s2_3_recv_slot_new_is_zeroed_and_bounded() {
        for capacity in [0, 1, 1_500, 65_534, RecvSlot::MAX_CAPACITY] {
            let slot = RecvSlot::new(capacity).expect("a capacity within the bound");
            assert_eq!(slot.buf.len(), capacity);
            assert!(slot.buf.iter().all(|&byte| byte == 0));
            assert_eq!(slot.len, 0);
            assert!(!slot.truncated);
            assert_eq!(
                slot.src,
                SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
            );
            assert_eq!(slot.dst_ip, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        }
        for capacity in [RecvSlot::MAX_CAPACITY + 1, 1 << 20, usize::MAX] {
            assert!(matches!(
                RecvSlot::new(capacity),
                Err(NetError::InvalidInput(_))
            ));
        }
    }

    #[test]
    fn arfc0005_s2_1_datagram_debug_prints_the_length_only() {
        let payload = [0x5Au8; 64];
        let datagram = Datagram {
            dst: "192.0.2.7:4242".parse().expect("an address"),
            src_ip: Some("192.0.2.8".parse().expect("an address")),
            payload: &payload,
        };
        assert_eq!(format!("{datagram:?}"), "Datagram { len: 64 }");
        assert_eq!(format!("{datagram:#?}"), "Datagram {\n    len: 64,\n}");
    }

    #[test]
    fn arfc0005_s2_1_recv_slot_debug_prints_capacity_and_length_only() {
        let mut slot = RecvSlot::new(1_500).expect("a valid capacity");
        slot.buf[..3].copy_from_slice(b"key");
        slot.len = 3;
        slot.truncated = true;
        slot.src = "192.0.2.9:53".parse().expect("an address");
        assert_eq!(format!("{slot:?}"), "RecvSlot { capacity: 1500, len: 3 }");
        assert_eq!(
            format!("{slot:#?}"),
            "RecvSlot {\n    capacity: 1500,\n    len: 3,\n}"
        );
    }

    #[test]
    fn arfc0005_s2_3_send_outcome_and_net_error_text() {
        let outcome = SendOutcome {
            sent: 2,
            error: Some(NetError::BatchTooLarge),
        };
        assert_eq!(outcome.clone(), outcome);
        let cases = [
            (NetError::AddrInUse, "address in use"),
            (
                NetError::AddrNotAvailable,
                "address not available on this host",
            ),
            (NetError::PermissionDenied, "permission denied"),
            (NetError::ConnectionRefused, "connection refused"),
            (NetError::ConnectionReset, "connection reset"),
            (NetError::TimedOut, "timed out"),
            (NetError::HostUnreachable, "host unreachable"),
            (NetError::NetworkUnreachable, "network unreachable"),
            (
                NetError::MessageTooLong { max_payload: 1_472 },
                "datagram too long, at most 1472 bytes of payload fit",
            ),
            (NetError::BatchTooLarge, "batch longer than 64"),
            (
                NetError::Busy,
                "another read or write is pending on this stream",
            ),
            (
                NetError::InvalidInput(Detail::new("bad port")),
                "invalid input: bad port",
            ),
            (NetError::TooManyOpen, "too many open sockets"),
            (NetError::Unsupported, "not supported on this platform"),
            (NetError::Closed, "closed"),
            (NetError::Os { code: 98 }, "operating system error 98"),
        ];
        for (error, text) in cases {
            assert_eq!(error.to_string(), text);
        }
    }

    fn valid_unix(path: &str) -> bool {
        path.starts_with('/') && path.len() <= 107 && !path.contains('\0')
    }

    #[test]
    fn arfc0005_s2_3_unix_path_accepts_exactly_its_rules() {
        let at_limit = format!("/{}", "a".repeat(106));
        let over_limit = format!("/{}", "a".repeat(107));
        for path in [
            "",
            "/",
            "a",
            "relative/x.sock",
            "/run/alloy.sock",
            "/a\0b",
            "/é",
            &at_limit,
            &over_limit,
        ] {
            check_unix(path);
        }
        let alphabet = ['/', 'a', '.', '\0', 'é', ' ', '\\'];
        for seed in 0..16 {
            let mut generator = Gen::new(seed);
            for _ in 0..500 {
                let len = generator.below(120) as usize;
                let path: String = (0..len)
                    .map(|_| alphabet[generator.below(alphabet.len() as u64) as usize])
                    .collect();
                check_unix(&path);
            }
        }
    }

    fn check_unix(path: &str) {
        match UnixPath::new(path) {
            Ok(unix) => {
                assert!(valid_unix(path), "{path:?} was accepted");
                assert_eq!(unix.as_str(), path);
            }
            Err(error) => {
                assert!(!valid_unix(path), "{path:?} was refused");
                assert!(
                    matches!(error, NetError::InvalidInput(_)),
                    "{path:?}: {error:?}"
                );
            }
        }
    }
}
