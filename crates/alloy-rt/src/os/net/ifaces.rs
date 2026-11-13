//! The host's interface list from the platform (ARFC 0005 clause 3.1.7):
//! `getifaddrs` on Unix, `GetAdaptersAddresses` on Windows.

use std::cmp::Ordering;
use std::io;

use crate::{Interface, InterfaceAddr};

/// The most interfaces a list holds, the lowest indexes.
const MAX_INTERFACES: usize = 1_024;
/// The most addresses an interface lists, the first in order.
const MAX_ADDRS: usize = 64;
/// The longest interface name, in bytes.
const MAX_NAME: usize = 15;

/// The host's interfaces in the order and within the bounds of ARFC 0005
/// section 2.3 and clause 3.1.7.
pub(super) fn read() -> io::Result<Vec<Interface>> {
    let mut interfaces = platform::list()?;
    for iface in &mut interfaces {
        iface.addrs.sort_by(order);
        iface.addrs.dedup();
        iface.addrs.truncate(MAX_ADDRS);
    }
    interfaces.sort_by_key(|iface| iface.index);
    interfaces.dedup_by_key(|iface| iface.index);
    interfaces.truncate(MAX_INTERFACES);
    Ok(interfaces)
}

/// IPv4 before IPv6, each ascending, then by prefix length.
fn order(a: &InterfaceAddr, b: &InterfaceAddr) -> Ordering {
    (a.ip.is_ipv6(), a.ip, a.prefix_len).cmp(&(b.ip.is_ipv6(), b.ip, b.prefix_len))
}

/// A name the list may hold: 1 to 15 bytes.
fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
}

#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "getifaddrs and if_nametoindex have no safe binding"
)]
mod platform {
    use std::collections::BTreeMap;
    use std::ffi::CStr;
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::valid_name;
    use crate::{Interface, InterfaceAddr};

    /// Frees the list `getifaddrs` returned.
    struct List(*mut libc::ifaddrs);

    impl Drop for List {
        fn drop(&mut self) {
            // SAFETY: the pointer came from a successful `getifaddrs` and is
            // freed once, here.
            unsafe { libc::freeifaddrs(self.0) }
        }
    }

    pub(super) fn list() -> io::Result<Vec<Interface>> {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: `head` is a valid place for the list pointer.
        if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let list = List(head);
        let mut by_index: BTreeMap<u32, Interface> = BTreeMap::new();
        let mut node = list.0;
        while !node.is_null() {
            // SAFETY: `node` is a node of the list, which lives until `list`
            // is dropped at the end of this function.
            let entry = unsafe { &*node };
            node = entry.ifa_next;
            if entry.ifa_name.is_null() {
                continue;
            }
            // SAFETY: `ifa_name` is a NUL-terminated string owned by the list.
            let raw_name = unsafe { CStr::from_ptr(entry.ifa_name) };
            // SAFETY: as above; `if_nametoindex` only reads the string.
            let index = unsafe { libc::if_nametoindex(entry.ifa_name) };
            // Linux lists IPv4 alias labels such as `eth0:1` under their
            // interface's index; the interface's own name has no colon.
            let Some(name) = raw_name
                .to_str()
                .ok()
                .and_then(|n| n.split(':').next())
                .filter(|n| valid_name(n))
            else {
                continue;
            };
            if index == 0 {
                continue;
            }
            let flags = entry.ifa_flags;
            let iface = by_index.entry(index).or_insert_with(|| Interface {
                index,
                name: name.to_owned(),
                mtu: mtu(name, entry),
                up: flags & (libc::IFF_UP as libc::c_uint) != 0
                    && flags & (libc::IFF_RUNNING as libc::c_uint) != 0,
                loopback: flags & (libc::IFF_LOOPBACK as libc::c_uint) != 0,
                addrs: Vec::new(),
            });
            if iface.mtu == 0 {
                iface.mtu = mtu(name, entry);
            }
            if let Some(addr) = address(entry) {
                iface.addrs.push(addr);
            }
        }
        Ok(by_index.into_values().collect())
    }

    /// The address of an entry with its prefix length, for IPv4 and IPv6.
    fn address(entry: &libc::ifaddrs) -> Option<InterfaceAddr> {
        if entry.ifa_addr.is_null() {
            return None;
        }
        // SAFETY: a non-null `ifa_addr` points to a socket address of the
        // family its header names, owned by the list.
        let family = libc::c_int::from(unsafe { (*entry.ifa_addr).sa_family });
        let prefix = |bits: &[u8]| {
            let ones: u32 = bits.iter().map(|b| b.count_ones()).sum();
            u8::try_from(ones).unwrap_or(u8::MAX)
        };
        match family {
            libc::AF_INET => {
                // SAFETY: the family is AF_INET, so this is a `sockaddr_in`.
                let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
                let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
                let prefix_len = if entry.ifa_netmask.is_null() {
                    32
                } else {
                    // SAFETY: the netmask of an AF_INET entry is a
                    // `sockaddr_in`.
                    let mask = unsafe { &*entry.ifa_netmask.cast::<libc::sockaddr_in>() };
                    prefix(&mask.sin_addr.s_addr.to_ne_bytes())
                };
                Some(InterfaceAddr {
                    ip: IpAddr::V4(ip),
                    prefix_len: prefix_len.min(32),
                })
            }
            libc::AF_INET6 => {
                // SAFETY: the family is AF_INET6, so this is a
                // `sockaddr_in6`.
                let addr = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in6>() };
                let ip = Ipv6Addr::from(addr.sin6_addr.s6_addr);
                let prefix_len = if entry.ifa_netmask.is_null() {
                    128
                } else {
                    // SAFETY: the netmask of an AF_INET6 entry is a
                    // `sockaddr_in6`.
                    let mask = unsafe { &*entry.ifa_netmask.cast::<libc::sockaddr_in6>() };
                    prefix(&mask.sin6_addr.s6_addr)
                };
                Some(InterfaceAddr {
                    ip: IpAddr::V6(ip),
                    prefix_len: prefix_len.min(128),
                })
            }
            _ => None,
        }
    }

    /// The MTU from sysfs, or 0 when it cannot be read.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn mtu(name: &str, _entry: &libc::ifaddrs) -> u32 {
        std::fs::read_to_string(format!("/sys/class/net/{name}/mtu"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    /// The MTU from the link-level entry's interface data, or 0 for the
    /// other entries, which carry none.
    #[cfg(target_vendor = "apple")]
    fn mtu(_name: &str, entry: &libc::ifaddrs) -> u32 {
        if entry.ifa_addr.is_null() || entry.ifa_data.is_null() {
            return 0;
        }
        // SAFETY: a non-null `ifa_addr` points to a socket address header.
        let family = libc::c_int::from(unsafe { (*entry.ifa_addr).sa_family });
        if family != libc::AF_LINK {
            return 0;
        }
        // SAFETY: the `ifa_data` of an AF_LINK entry points to its
        // `if_data`, owned by the list.
        unsafe { (*entry.ifa_data.cast::<libc::if_data>()).ifi_mtu }
    }

    /// This platform's MTU is not read.
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    fn mtu(_name: &str, _entry: &libc::ifaddrs) -> u32 {
        0
    }
}

#[cfg(windows)]
#[allow(unsafe_code, reason = "GetAdaptersAddresses has no safe binding")]
mod platform {
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceLuidToNameA, GAA_FLAG_INCLUDE_ALL_INTERFACES, GAA_FLAG_SKIP_ANYCAST,
        GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
        IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6,
    };

    use super::valid_name;
    use crate::{Interface, InterfaceAddr};

    /// The first buffer size Microsoft recommends, and the most attempts
    /// while the adapter list grows between calls.
    const FIRST_BUFFER: usize = 15_000;
    const ATTEMPTS: usize = 4;

    pub(super) fn list() -> io::Result<Vec<Interface>> {
        let flags = GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER
            | GAA_FLAG_INCLUDE_ALL_INTERFACES;
        let mut size = u32::try_from(FIRST_BUFFER).unwrap_or(u32::MAX);
        for _ in 0..ATTEMPTS {
            // `u64` elements give the 8-byte alignment the structures need.
            let words = (size as usize).div_ceil(8);
            let mut buffer = vec![0u64; words];
            let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            // SAFETY: `buffer` holds `size` writable bytes, aligned for the
            // structures, and lives across the call.
            let rc = unsafe {
                GetAdaptersAddresses(
                    u32::from(AF_UNSPEC),
                    flags,
                    std::ptr::null(),
                    first,
                    &raw mut size,
                )
            };
            match rc {
                NO_ERROR => return Ok(collect(first)),
                ERROR_NO_DATA => return Ok(Vec::new()),
                ERROR_BUFFER_OVERFLOW => {}
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
        Err(io::Error::other("the adapter list kept growing"))
    }

    /// Walks the adapter list at `first`, which must be a list that
    /// `GetAdaptersAddresses` filled and that outlives the call.
    fn collect(first: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<Interface> {
        let mut interfaces = Vec::new();
        let mut node = first;
        while !node.is_null() {
            // SAFETY: `node` is an adapter of the list, which outlives this
            // function.
            let adapter = unsafe { &*node };
            node = adapter.Next;
            // SAFETY: the union's `Anonymous` view is the one the API fills.
            let v4_index = unsafe { adapter.Anonymous1.Anonymous.IfIndex };
            let index = if v4_index == 0 {
                adapter.Ipv6IfIndex
            } else {
                v4_index
            };
            if index == 0 {
                continue;
            }
            let mut addrs = Vec::new();
            let mut unicast = adapter.FirstUnicastAddress;
            while !unicast.is_null() {
                // SAFETY: `unicast` is a node of the adapter's address list.
                let entry = unsafe { &*unicast };
                unicast = entry.Next;
                if let Some(ip) = ip_of(entry.Address.lpSockaddr) {
                    let max = if ip.is_ipv4() { 32 } else { 128 };
                    addrs.push(InterfaceAddr {
                        ip,
                        prefix_len: entry.OnLinkPrefixLength.min(max),
                    });
                }
            }
            interfaces.push(Interface {
                index,
                name: name_of(adapter).unwrap_or_else(|| format!("if{index}")),
                mtu: adapter.Mtu,
                up: adapter.OperStatus == IfOperStatusUp,
                loopback: adapter.IfType == IF_TYPE_SOFTWARE_LOOPBACK,
                addrs,
            });
        }
        interfaces
    }

    /// The interface's short name, such as `ethernet_32768`, when it has 1 to
    /// 15 bytes.
    fn name_of(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Option<String> {
        let mut name = [0u8; 257];
        // SAFETY: `name` is writable for its whole length, which is passed.
        let rc = unsafe {
            ConvertInterfaceLuidToNameA(&raw const adapter.Luid, name.as_mut_ptr(), name.len())
        };
        if rc != NO_ERROR {
            return None;
        }
        let end = name.iter().position(|b| *b == 0)?;
        let name = std::str::from_utf8(&name[..end]).ok()?;
        valid_name(name).then(|| name.to_owned())
    }

    fn ip_of(addr: *const SOCKADDR) -> Option<IpAddr> {
        if addr.is_null() {
            return None;
        }
        // SAFETY: a non-null address of the list starts with its family.
        let family = unsafe { (*addr).sa_family };
        if family == AF_INET {
            // SAFETY: the family is AF_INET, so this is a `SOCKADDR_IN`.
            let v4 = unsafe { &*addr.cast::<SOCKADDR_IN>() };
            // SAFETY: every view of the address union is four bytes.
            let raw = unsafe { v4.sin_addr.S_un.S_addr };
            Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(raw))))
        } else if family == AF_INET6 {
            // SAFETY: the family is AF_INET6, so this is a `SOCKADDR_IN6`.
            let v6 = unsafe { &*addr.cast::<SOCKADDR_IN6>() };
            // SAFETY: every view of the address union is sixteen bytes.
            let raw = unsafe { v6.sin6_addr.u.Byte };
            Some(IpAddr::V6(Ipv6Addr::from(raw)))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;

    #[test]
    fn the_list_is_sorted_and_bounded() {
        let list = read().unwrap();
        assert!(list.len() <= MAX_INTERFACES);
        assert!(list.windows(2).all(|w| w[0].index < w[1].index));
        for iface in &list {
            assert!(valid_name(&iface.name), "{iface:?}");
            assert!(iface.addrs.len() <= MAX_ADDRS);
            assert!(
                iface
                    .addrs
                    .windows(2)
                    .all(|w| order(&w[0], &w[1]) == Ordering::Less)
            );
        }
        let loopback = list
            .iter()
            .find(|iface| iface.addrs.iter().any(|a| a.ip.is_loopback()))
            .expect("the host has a loopback address");
        assert!(loopback.loopback, "{loopback:?}");
        assert!(loopback.up, "{loopback:?}");
        assert!(loopback.mtu > 0, "{loopback:?}");
        let v4 = loopback
            .addrs
            .iter()
            .find(|a| a.ip == IpAddr::from([127, 0, 0, 1]));
        assert_eq!(v4.map(|a| a.prefix_len), Some(8));
    }

    #[test]
    fn addresses_order_ipv4_first_then_ascending() {
        let addr = |ip: &str, prefix_len| InterfaceAddr {
            ip: ip.parse().unwrap(),
            prefix_len,
        };
        let mut addrs = vec![
            addr("::1", 128),
            addr("10.0.0.2", 8),
            addr("fe80::1", 64),
            addr("10.0.0.1", 24),
        ];
        addrs.sort_by(order);
        assert_eq!(
            addrs,
            [
                addr("10.0.0.1", 24),
                addr("10.0.0.2", 8),
                addr("::1", 128),
                addr("fe80::1", 64)
            ]
        );
    }
}
