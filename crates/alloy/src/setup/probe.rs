//! What the checks of ARFC 0002 section 3.8 read from the machine beyond
//! the `host` seam: the routing tables and the user and group databases as
//! files through `disk-io`, the interfaces through `net`, and whether a port
//! is free by binding it. The parsers are pure, so every platform tests them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use alloy_rt::{
    Access, Create, DiskIo, EntryKind, Interface, IoBuf, ListenOptions, NetError, OpenOptions,
    ReadOp, RtPath, StreamAddr, UdpOptions,
};

use super::Setup;

/// The most bytes read of one file: `/proc` and `/etc` files are far
/// smaller.
const MAX_FILE: usize = 4 * 1_048_576;

/// The guest id range (ARFC 0017 clause 3.17.7).
pub(crate) const GUEST_IDS: std::ops::RangeInclusive<u64> = 1_900_000_000..=1_900_065_535;

/// `RTF_UP`: the route is usable (Linux `include/uapi/linux/route.h`).
const RTF_UP: u32 = 0x0001;
/// `RTF_REJECT`: the route rejects what it matches, as the `unreachable`,
/// `prohibit`, and `blackhole` routes do (Linux
/// `include/uapi/linux/route.h`).
const RTF_REJECT: u32 = 0x0200;

/// One route of a routing table: its prefix, its interface, and whether
/// it carries traffic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    /// The destination's network address.
    pub(crate) dest: IpAddr,
    /// Its prefix length.
    pub(crate) len: u8,
    /// The interface it goes through.
    pub(crate) iface: String,
    /// Whether it is up and not a reject route: only such a route can be
    /// the default route that names the advertise address.
    pub(crate) usable: bool,
}

/// Whether the route flags `flags` are those of a route that carries
/// traffic: up, and not a reject route.
fn usable(flags: u32) -> bool {
    flags & RTF_UP != 0 && flags & RTF_REJECT == 0
}

/// The file at the absolute path `path`, as text, or `None` when it cannot
/// be read. Files under `/proc` report no length, so it reads until the
/// end, at most 4 MiB.
pub(crate) async fn read_text(disk: &dyn DiskIo, path: &str) -> Option<String> {
    let path = RtPath::new(path).ok()?;
    let options = OpenOptions {
        access: Access::Read,
        create: Create::Never,
        direct: false,
        exclusive: false,
    };
    let file = disk.open(&path, options).await.ok()?;
    let mut bytes = Vec::new();
    while bytes.len() < MAX_FILE {
        let buf = IoBuf::zeroed(65_536).ok()?;
        let offset = bytes.len() as u64;
        let done = file.read_at(vec![ReadOp { offset, buf }]).await.pop()?;
        let count = done.result.ok()?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(done.buf.get(..count)?);
    }
    String::from_utf8(bytes).ok()
}

/// The names in the directory `path`, or none when it cannot be listed.
pub(crate) async fn list_names(disk: &dyn DiskIo, path: &str) -> Vec<(String, EntryKind)> {
    let Ok(path) = RtPath::new(path) else {
        return Vec::new();
    };
    disk.list(&path)
        .await
        .map(|entries| entries.into_iter().map(|e| (e.name, e.kind)).collect())
        .unwrap_or_default()
}

/// The routes of `/proc/net/route`, the IPv4 main table: hexadecimal
/// little-endian destination and mask, and hexadecimal flags.
pub(crate) fn ipv4_routes(text: &str) -> Vec<Route> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let iface = (*fields.first()?).to_owned();
            let dest = u32::from_str_radix(fields.get(1)?, 16).ok()?;
            let flags = u32::from_str_radix(fields.get(3)?, 16).ok()?;
            let mask = u32::from_str_radix(fields.get(7)?, 16).ok()?;
            Some(Route {
                dest: IpAddr::V4(Ipv4Addr::from(dest.to_le_bytes())),
                len: u8::try_from(mask.count_ones()).ok()?,
                iface,
                usable: usable(flags),
            })
        })
        .collect()
}

/// The routes of `/proc/net/ipv6_route`: a destination of 32 hexadecimal
/// digits, its prefix length in hexadecimal, then the source, its length,
/// the next hop, the metric, the reference and use counts, the flags in
/// hexadecimal as the ninth field, and the interface last.
pub(crate) fn ipv6_routes(text: &str) -> Vec<Route> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let dest = u128::from_str_radix(fields.first()?, 16).ok()?;
            let len = u8::from_str_radix(fields.get(1)?, 16).ok()?;
            let flags = u32::from_str_radix(fields.get(8)?, 16).ok()?;
            let iface = (*fields.last()?).to_owned();
            (fields.len() >= 10 && len <= 128).then(|| Route {
                dest: IpAddr::V6(Ipv6Addr::from(dest)),
                len,
                iface,
                usable: usable(flags),
            })
        })
        .collect()
}

/// Every route of the main tables, IPv4 then IPv6, or none on a machine
/// without `/proc/net`.
pub(crate) async fn routes(setup: &Setup<'_>) -> Vec<Route> {
    let disk = setup.disk().as_ref();
    let mut routes = Vec::new();
    if let Some(text) = read_text(disk, "/proc/net/route").await {
        routes.extend(ipv4_routes(&text));
    }
    if let Some(text) = read_text(disk, "/proc/net/ipv6_route").await {
        routes.extend(ipv6_routes(&text));
    }
    routes
}

/// The machine's interfaces, or none when they cannot be listed.
pub(crate) async fn interfaces(setup: &Setup<'_>) -> Vec<Interface> {
    setup
        .ctx
        .runtime
        .net()
        .interfaces(None)
        .await
        .map(|list| list.interfaces)
        .unwrap_or_default()
}

/// The advertise addresses of ARFC 0002 section 3.1 step 2 when none is
/// given: the first address of the interface of the default IPv4 route,
/// else of the default IPv6 route, with that interface's name. The kernel
/// takes its source address from that interface for a route with no
/// preferred source.
///
/// A default route that is down or rejects, as the `::/0` reject route on
/// `lo` that Linux can list, is no default route. A loopback interface or
/// address, and a link-local IPv6 address, is never picked: other machines
/// cannot reach it.
pub(crate) fn default_route_address(
    routes: &[Route],
    interfaces: &[Interface],
) -> Option<(IpAddr, String)> {
    for v4 in [true, false] {
        let defaults = routes.iter().filter(|r| {
            r.usable && r.len == 0 && r.dest.is_unspecified() && r.dest.is_ipv4() == v4
        });
        for route in defaults {
            let iface = interfaces
                .iter()
                .find(|i| i.name == route.iface && !i.loopback);
            let addr = iface.and_then(|i| {
                i.addrs.iter().map(|a| a.ip).find(|ip| {
                    ip.is_ipv4() == v4
                        && !ip.is_loopback()
                        && match ip {
                            IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
                            IpAddr::V4(_) => true,
                        }
                })
            });
            if let Some(ip) = addr {
                return Some((ip, route.iface.clone()));
            }
        }
    }
    None
}

/// Whether the prefixes `a/a_len` and `b/b_len` overlap: one holds the
/// other. Prefixes of different families never overlap.
pub(crate) fn overlaps(a: IpAddr, a_len: u8, b: IpAddr, b_len: u8) -> bool {
    let bits = |ip: IpAddr| match ip {
        IpAddr::V4(v4) => (u128::from(u32::from(v4)) << 96, 32_u8),
        IpAddr::V6(v6) => (u128::from(v6), 128),
    };
    let ((a, a_max), (b, b_max)) = (bits(a), bits(b));
    if a_max != b_max {
        return false;
    }
    let len = u32::from(a_len.min(b_len).min(a_max));
    if len == 0 {
        return true;
    }
    let mask = u128::MAX << (128 - len);
    a & mask == b & mask
}

/// Whether `name` is reserved for Alloy's interfaces: `al` or `alv` then a
/// digit (ARFC 0017 clause 3.17).
pub(crate) fn reserved_interface(name: &str) -> bool {
    let rest = name
        .strip_prefix("alv")
        .or_else(|| name.strip_prefix("al"))
        .unwrap_or("");
    rest.starts_with(|c: char| c.is_ascii_digit())
}

/// The ids a `passwd` or `group` file names in the guest id range, with the
/// entry's name: fields 3 and 4 of `passwd`, field 3 of `group`.
pub(crate) fn database_ids(text: &str, fields: &[usize]) -> Vec<(String, u64)> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .flat_map(|line| {
            let parts: Vec<&str> = line.split(':').collect();
            let name = parts.first().copied().unwrap_or_default().to_owned();
            fields
                .iter()
                .filter_map(|&at| parts.get(at)?.trim().parse::<u64>().ok())
                .filter(|id| GUEST_IDS.contains(id))
                .map(|id| (name.clone(), id))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The lines of a `subuid` or `subgid` file whose range holds an id of the
/// guest id range: `name:start:count`.
pub(crate) fn subordinate_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| {
            let parts: Vec<&str> = line.split(':').collect();
            let (Some(start), Some(count)) = (
                parts.get(1).and_then(|s| s.trim().parse::<u64>().ok()),
                parts.get(2).and_then(|s| s.trim().parse::<u64>().ok()),
            ) else {
                return false;
            };
            count > 0
                && start <= *GUEST_IDS.end()
                && start.saturating_add(count - 1) >= *GUEST_IDS.start()
        })
        .map(str::to_owned)
        .collect()
}

/// Whether a `/proc/<pid>/status` file names an id of the guest id range
/// as a real, effective, or saved user or group id.
pub(crate) fn status_holds_guest_id(text: &str) -> bool {
    text.lines()
        .filter_map(|line| {
            line.strip_prefix("Uid:")
                .or_else(|| line.strip_prefix("Gid:"))
        })
        .any(|ids| {
            ids.split_whitespace()
                .take(3)
                .filter_map(|id| id.parse::<u64>().ok())
                .any(|id| GUEST_IDS.contains(&id))
        })
}

/// Why the port is not free: the UDP mesh port on both families, then the
/// TCP API port when `api` is `Some`.
pub(crate) fn busy_ports(setup: &Setup<'_>, mesh: u16, api: Option<u16>) -> Vec<String> {
    let net = setup.ctx.runtime.net();
    let unspecified = [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    ];
    let mut busy = Vec::new();
    let mut note = |what: String, error: NetError| {
        let reason = match error {
            NetError::AddrInUse => "in use".to_owned(),
            other => format!("not available: {other}"),
        };
        busy.push(format!("{what} is {reason}"));
    };
    for ip in unspecified {
        match net.bind_udp(SocketAddr::new(ip, mesh), UdpOptions::default()) {
            Ok(socket) => drop(socket),
            // A machine without IPv6 still has the port free on IPv4.
            Err(NetError::AddrNotAvailable) if ip.is_ipv6() => {}
            Err(e) => note(format!("UDP {mesh}"), e),
        }
    }
    if let Some(api) = api {
        for ip in unspecified {
            let address = StreamAddr::Ip(SocketAddr::new(ip, api));
            match net.listen(address, ListenOptions::default()) {
                Ok(listener) => drop(listener),
                Err(NetError::AddrNotAvailable) if ip.is_ipv6() => {}
                Err(e) => note(format!("TCP {api}"), e),
            }
        }
    }
    busy.dedup();
    busy
}

#[cfg(test)]
mod tests {
    use alloy_rt::InterfaceAddr;

    use super::*;

    const ROUTE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
        eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";

    const ROUTE6: &str = "fd12345678900000000000000000000000 30 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001     eth0\n\
        00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003     eth0\n";

    #[test]
    fn proc_routes_parse() {
        let v4 = ipv4_routes(ROUTE);
        assert_eq!(v4.len(), 2);
        assert_eq!(v4[0].dest, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(v4[0].len, 0);
        assert_eq!(v4[1].dest, "192.168.1.0".parse::<IpAddr>().unwrap());
        assert_eq!(v4[1].len, 24);
        let v6 = ipv6_routes(ROUTE6);
        assert_eq!(v6.len(), 1, "{v6:?}");
        assert_eq!(v6[0].len, 0);
        assert_eq!(v6[0].iface, "eth0");
        let line = "fd123456789000000000000000000000 30 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001     eth1\n";
        let v6 = ipv6_routes(line);
        assert_eq!(v6[0].dest, "fd12:3456:7890::".parse::<IpAddr>().unwrap());
        assert_eq!(v6[0].len, 48);
    }

    fn iface(name: &str, addrs: &[&str]) -> Interface {
        Interface {
            index: 2,
            name: name.to_owned(),
            mtu: 1_500,
            up: true,
            loopback: false,
            addrs: addrs
                .iter()
                .map(|a| InterfaceAddr {
                    ip: a.parse().unwrap(),
                    prefix_len: 24,
                })
                .collect(),
        }
    }

    #[test]
    fn the_default_route_names_the_address() {
        let routes = ipv4_routes(ROUTE);
        let interfaces = [
            iface("lo", &["127.0.0.1"]),
            iface("eth0", &["192.168.1.10", "fe80::1", "2001:db8::5"]),
        ];
        assert_eq!(
            default_route_address(&routes, &interfaces),
            Some(("192.168.1.10".parse().unwrap(), "eth0".to_owned()))
        );
        let v6_only = ipv6_routes(ROUTE6);
        assert_eq!(
            default_route_address(&v6_only, &interfaces),
            Some(("2001:db8::5".parse().unwrap(), "eth0".to_owned()))
        );
        assert_eq!(default_route_address(&[], &interfaces), None);
    }

    /// The `::/0` reject route on `lo` that Linux lists in
    /// `/proc/net/ipv6_route`, flags `RTF_REJECT | RTF_NONEXTHOP`.
    const REJECT6: &str = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200       lo\n";

    fn loopback() -> Interface {
        Interface {
            loopback: true,
            ..iface("lo", &["127.0.0.1", "::1"])
        }
    }

    #[test]
    fn a_reject_route_is_no_default_route() {
        let reject = ipv6_routes(REJECT6);
        assert_eq!(reject.len(), 1, "{reject:?}");
        assert!(!reject[0].usable);
        let interfaces = [loopback(), iface("eth0", &["2001:db8::5"])];
        assert_eq!(default_route_address(&reject, &interfaces), None);
        // Listed before a real IPv6 default, the reject route does not win.
        let both = ipv6_routes(&format!("{REJECT6}{ROUTE6}"));
        assert_eq!(
            default_route_address(&both, &interfaces),
            Some(("2001:db8::5".parse().unwrap(), "eth0".to_owned()))
        );
        // An IPv4 default route that is down or rejects is none either.
        for flags in ["0000", "0201"] {
            let table = format!(
                "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
                 eth0\t00000000\t0101A8C0\t{flags}\t0\t0\t100\t00000000\t0\t0\t0\n"
            );
            let routes = ipv4_routes(&table);
            assert!(!routes[0].usable, "{flags}");
            let v4 = [iface("eth0", &["192.168.1.10"])];
            assert_eq!(default_route_address(&routes, &v4), None, "{flags}");
        }
    }

    #[test]
    fn loopback_is_never_the_advertise_address() {
        let on_lo = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 00000400 00000001 00000000 00000001       lo\n";
        let routes = ipv6_routes(on_lo);
        assert!(routes[0].usable);
        assert_eq!(default_route_address(&routes, &[loopback()]), None);
        // A loopback address on another interface is not picked either.
        let routes = ipv6_routes(ROUTE6);
        let odd = [iface("eth0", &["::1", "2001:db8::7"])];
        assert_eq!(
            default_route_address(&routes, &odd),
            Some(("2001:db8::7".parse().unwrap(), "eth0".to_owned()))
        );
    }

    #[test]
    fn prefixes_overlap_when_one_holds_the_other() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(overlaps(
            ip("fd12:3456:7890::"),
            48,
            ip("fd12:3456:7890:1::"),
            64
        ));
        assert!(overlaps(ip("fd00::"), 8, ip("fd12:3456:7890::"), 48));
        assert!(!overlaps(
            ip("fd12:3456:7891::"),
            48,
            ip("fd12:3456:7890::"),
            48
        ));
        assert!(!overlaps(ip("10.0.0.0"), 8, ip("fd00::"), 8));
        assert!(overlaps(ip("10.1.0.0"), 16, ip("10.0.0.0"), 8));
        assert!(!overlaps(ip("10.1.0.0"), 16, ip("10.2.0.0"), 16));
    }

    #[test]
    fn reserved_interface_names_follow_clause_3_17() {
        for name in ["al0", "al12", "alv0", "alv3x"] {
            assert!(reserved_interface(name), "{name}");
        }
        for name in ["al", "alloy0", "alv", "eth0", "wlan0", "xal0"] {
            assert!(!reserved_interface(name), "{name}");
        }
    }

    #[test]
    fn guest_ids_are_found_in_the_databases_and_processes() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\nvmm:x:1900000005:100::/:/bin/false\n";
        assert_eq!(
            database_ids(passwd, &[2, 3]),
            vec![("vmm".to_owned(), 1_900_000_005)]
        );
        let group = "root:x:0:\nodd:x:1900065535:\n";
        assert_eq!(database_ids(group, &[2]).len(), 1);
        let subuid = "alice:100000:65536\nbig:1899999000:2000\nafter:1900065536:10\n";
        assert_eq!(
            subordinate_lines(subuid),
            vec!["big:1899999000:2000".to_owned()]
        );
        assert!(status_holds_guest_id(
            "Name:\tx\nUid:\t0\t1900000001\t0\t0\nGid:\t0\t0\t0\t0\n"
        ));
        assert!(!status_holds_guest_id(
            "Uid:\t0\t0\t0\t1900000001\nGid:\t5\t5\t5\t5\n"
        ));
    }
}
