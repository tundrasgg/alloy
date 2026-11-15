//! The parts of `port_holders` under `os` that read no operating system
//! state (ARFC 0005 clause 3.1.18.5): the lines of `/proc/net`, the rows of
//! the Windows extended tables, process names made into text, and the order
//! and bound of the result. Every platform tests them.

#![allow(
    dead_code,
    reason = "Linux and Windows each use their own part; the tests run everywhere"
)]

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use super::parse::cut;
use crate::PortHolder;

/// The most entries a `port_holders` result holds (ARFC 0005 section 7).
pub(super) const MAX_HOLDERS: usize = 256;
/// The most bytes of `PortHolder::process` (ARFC 0005 section 7).
pub(super) const MAX_PROCESS: usize = 255;
/// The state of a listening TCP socket in `/proc/net/tcp`: `TCP_LISTEN`.
pub(super) const PROC_LISTEN: u8 = 0x0A;
/// `MIB_TCP_STATE_LISTEN`.
const MIB_LISTEN: u32 = 2;

/// `holders` sorted by the derived order, with no two equal entries, and
/// at most the first 256.
pub(super) fn finish(mut holders: Vec<PortHolder>) -> Vec<PortHolder> {
    holders.sort();
    holders.dedup();
    holders.truncate(MAX_HOLDERS);
    holders
}

/// The second and third steps of clause 3.1.18.5: each control character
/// becomes U+FFFD, then the text is cut to 255 bytes at a character
/// boundary. `None` when that leaves nothing.
pub(super) fn process_text(text: &str) -> Option<String> {
    let replaced: String = text
        .chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect();
    let kept = cut(&replaced, MAX_PROCESS);
    (!kept.is_empty()).then(|| kept.to_owned())
}

/// The name of a Linux process from the bytes of `/proc/<pid>/comm`.
pub(super) fn linux_name(comm: &[u8]) -> Option<String> {
    let comm = comm.strip_suffix(b"\n").unwrap_or(comm);
    process_text(&String::from_utf8_lossy(comm))
}

/// The name of a Windows process from the path that
/// `QueryFullProcessImageNameW` gives: the part after its last `\`.
pub(super) fn windows_name(path: &[u16]) -> Option<String> {
    let path = String::from_utf16_lossy(path);
    let name = path.rsplit('\\').next().unwrap_or(&path);
    process_text(name)
}

/// `text` as a number in `radix`, when it is 1 or more digits of that radix
/// and nothing else. `from_str_radix` alone also takes a leading sign.
fn digits(text: &str, radix: u32) -> Option<u64> {
    let all = !text.is_empty() && text.chars().all(|c| c.is_digit(radix));
    if all {
        u64::from_str_radix(text, radix).ok()
    } else {
        None
    }
}

/// One 32-bit value of a `/proc/net` address, 8 hexadecimal digits, as the
/// bytes it holds in the machine's native order.
fn native_word(hex: &str) -> Option<[u8; 4]> {
    if hex.len() != 8 {
        return None;
    }
    let value = u32::try_from(digits(hex, 16)?).ok()?;
    Some(value.to_ne_bytes())
}

/// What `port_holders` uses of one line of a `/proc/net` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProcSocket {
    /// The local address and port.
    pub(super) local: SocketAddr,
    /// The state, `0A` for a listening TCP socket.
    pub(super) state: u8,
    /// The socket's inode, 0 when no process holds it.
    pub(super) inode: u64,
}

/// One line after the first of `/proc/net/tcp`, `tcp6`, `udp`, or `udp6`,
/// or `None` for a line with fewer than ten fields or whose second, fourth,
/// or tenth field does not parse.
pub(super) fn proc_net_line(line: &str) -> Option<ProcSocket> {
    let fields: Vec<&str> = line.split(' ').filter(|f| !f.is_empty()).collect();
    if fields.len() < 10 {
        return None;
    }
    let (addr, port) = fields[1].split_once(':')?;
    if port.len() != 4 {
        return None;
    }
    let port = u16::try_from(digits(port, 16)?).ok()?;
    let local = match addr.len() {
        8 => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(native_word(addr)?), port)),
        32 => {
            let mut octets = [0_u8; 16];
            for (i, chunk) in octets.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *chunk = native_word(addr.get(i * 8..i * 8 + 8)?)?;
            }
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(octets), port, 0, 0))
        }
        _ => return None,
    };
    let state = u8::try_from(digits(fields[3], 16)?).ok()?;
    let inode = digits(fields[9], 10)?;
    Some(ProcSocket {
        local,
        state,
        inode,
    })
}

/// The inode of a socket from the target of a link in `/proc/<pid>/fd`,
/// `socket:[<inode>]`, or `None` for any other target.
pub(super) fn socket_inode(target: &[u8]) -> Option<u64> {
    let inner = target.strip_prefix(b"socket:[")?.strip_suffix(b"]")?;
    digits(std::str::from_utf8(inner).ok()?, 10)
}

/// The kind of row in a Windows extended table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowKind {
    /// `MIB_TCPROW_OWNER_PID`.
    Tcp4,
    /// `MIB_TCP6ROW_OWNER_PID`.
    Tcp6,
    /// `MIB_UDPROW_OWNER_PID`.
    Udp4,
    /// `MIB_UDP6ROW_OWNER_PID`.
    Udp6,
}

/// Byte offsets of the fields of one row kind.
struct Layout {
    size: usize,
    addr: usize,
    scope: Option<usize>,
    port: usize,
    state: Option<usize>,
    pid: usize,
}

impl RowKind {
    /// The layouts of the `windows-sys` structs, all of 4-byte fields and
    /// 16-byte address arrays.
    fn layout(self) -> Layout {
        match self {
            RowKind::Tcp4 => Layout {
                size: 24,
                addr: 4,
                scope: None,
                port: 8,
                state: Some(0),
                pid: 20,
            },
            RowKind::Tcp6 => Layout {
                size: 56,
                addr: 0,
                scope: Some(16),
                port: 20,
                state: Some(48),
                pid: 52,
            },
            RowKind::Udp4 => Layout {
                size: 12,
                addr: 0,
                scope: None,
                port: 4,
                state: None,
                pid: 8,
            },
            RowKind::Udp6 => Layout {
                size: 28,
                addr: 0,
                scope: Some(16),
                port: 20,
                state: None,
                pid: 24,
            },
        }
    }
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at + 4)?;
    let mut word = [0_u8; 4];
    word.copy_from_slice(field);
    Some(u32::from_ne_bytes(word))
}

/// The local address and owning pid of each row of a Windows extended
/// table: its `dwNumEntries`, then the rows, from offset 4. TCP rows count
/// only when they listen. Rows past the end of `table` are left out.
pub(super) fn windows_rows(table: &[u8], kind: RowKind) -> Vec<(SocketAddr, u32)> {
    let layout = kind.layout();
    let Some(count) = word(table, 0) else {
        return Vec::new();
    };
    let rows = table.get(4..).unwrap_or_default();
    rows.chunks_exact(layout.size)
        .take(usize::try_from(count).unwrap_or(usize::MAX))
        .filter_map(|row| {
            if let Some(at) = layout.state
                && word(row, at)? != MIB_LISTEN
            {
                return None;
            }
            // The low 16 bits hold the port in network byte order; the mask
            // keeps the value within `u16`.
            let port = u16::from_be((word(row, layout.port)? & 0xFFFF) as u16);
            let local = match layout.scope {
                None => {
                    let addr = Ipv4Addr::from(word(row, layout.addr)?.to_ne_bytes());
                    SocketAddr::V4(SocketAddrV4::new(addr, port))
                }
                Some(scope) => {
                    let mut octets = [0_u8; 16];
                    octets.copy_from_slice(row.get(layout.addr..layout.addr + 16)?);
                    let scope = word(row, scope)?;
                    SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(octets), port, 0, scope))
                }
            };
            Some((local, word(row, layout.pid)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hexadecimal text `/proc/net` gives for an IPv4 address.
    fn proc_v4(addr: Ipv4Addr) -> String {
        format!("{:08X}", u32::from_ne_bytes(addr.octets()))
    }

    /// The hexadecimal text `/proc/net` gives for an IPv6 address.
    fn proc_v6(addr: Ipv6Addr) -> String {
        addr.octets()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| format!("{:08X}", u32::from_ne_bytes(*c)))
            .collect()
    }

    fn tcp_line(local: &str, state: &str, inode: &str) -> String {
        format!(
            "   0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0"
        )
    }

    #[test]
    fn proc_net_lines_give_address_state_and_inode() {
        let v4 = format!("{}:1D34", proc_v4(Ipv4Addr::LOCALHOST));
        assert_eq!(
            proc_net_line(&tcp_line(&v4, "0A", "41234")),
            Some(ProcSocket {
                local: "127.0.0.1:7476".parse().unwrap(),
                state: PROC_LISTEN,
                inode: 41_234,
            })
        );
        let v6 = format!("{}:0050", proc_v6(Ipv6Addr::LOCALHOST));
        assert_eq!(
            proc_net_line(&tcp_line(&v6, "01", "0")),
            Some(ProcSocket {
                local: "[::1]:80".parse().unwrap(),
                state: 1,
                inode: 0,
            })
        );
        let any = format!("{}:1D34", proc_v6(Ipv6Addr::UNSPECIFIED));
        let udp = format!(
            "  377: {any} 00000000000000000000000000000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 9 2 0000000000000000 0"
        );
        let parsed = proc_net_line(&udp).unwrap();
        assert_eq!(parsed.local, "[::]:7476".parse().unwrap());
        assert_eq!((parsed.state, parsed.inode), (7, 9));
    }

    #[test]
    fn proc_net_lines_that_do_not_parse_are_skipped() {
        let v4 = format!("{}:1D34", proc_v4(Ipv4Addr::LOCALHOST));
        let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";
        let bad = [
            header.to_owned(),
            String::new(),
            format!("0: {v4} 00000000:0000 0A 0 0 0 0 0"),
            tcp_line("0100007F1D34", "0A", "1"),
            tcp_line("0100007F:1D3", "0A", "1"),
            tcp_line("0100007F:+D34", "0A", "1"),
            tcp_line("0100007:01D34", "0A", "1"),
            tcp_line("G100007F:1D34", "0A", "1"),
            tcp_line(&v4, "0X", "1"),
            tcp_line(&v4, "100", "1"),
            tcp_line(&v4, "0A", "-1"),
            tcp_line(&v4, "0A", "1a"),
        ];
        for line in &bad {
            assert_eq!(proc_net_line(line), None, "{line}");
        }
    }

    #[test]
    fn socket_links_give_their_inode() {
        assert_eq!(socket_inode(b"socket:[41234]"), Some(41_234));
        for target in [
            &b"socket:[]"[..],
            b"socket:[12",
            b"pipe:[41234]",
            b"/dev/null",
            b"socket:[+1]",
            b"anon_inode:[eventfd]",
        ] {
            assert_eq!(socket_inode(target), None);
        }
    }

    #[test]
    fn names_lose_control_characters_and_are_cut() {
        assert_eq!(
            process_text("a\u{1b}[31mb\r\nc\u{9b}d\u{7f}"),
            Some("a\u{FFFD}[31mb\u{FFFD}\u{FFFD}c\u{FFFD}d\u{FFFD}".to_owned())
        );
        assert_eq!(process_text(""), None);
        let long = "é".repeat(200);
        let cut = process_text(&long).unwrap();
        assert_eq!(cut.len(), 254);
        assert_eq!(process_text(&"x".repeat(300)).unwrap().len(), 255);
        assert_eq!(process_text("\0").as_deref(), Some("\u{FFFD}"));
    }

    #[test]
    fn linux_names_come_from_comm() {
        assert_eq!(linux_name(b"alloyd\n").as_deref(), Some("alloyd"));
        assert_eq!(linux_name(b"a\xffb\n").as_deref(), Some("a\u{FFFD}b"));
        assert_eq!(linux_name(b"two\n\n").as_deref(), Some("two\u{FFFD}"));
        assert_eq!(linux_name(b"\n"), None);
    }

    #[test]
    fn windows_names_are_the_last_component() {
        let wide = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(
            windows_name(&wide(r"C:\Program Files\Alloy\alloy.exe")).as_deref(),
            Some("alloy.exe")
        );
        assert_eq!(windows_name(&wide("plain")).as_deref(), Some("plain"));
        assert_eq!(windows_name(&wide(r"C:\dir\")), None);
        let mut lone = wide(r"C:\x");
        lone.push(0xD800);
        assert_eq!(windows_name(&lone).as_deref(), Some("x\u{FFFD}"));
    }

    fn table(count: u32, rows: &[Vec<u32>]) -> Vec<u8> {
        let mut bytes = count.to_ne_bytes().to_vec();
        for row in rows {
            for value in row {
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
        bytes
    }

    /// `dwLocalPort` for `port`: the low 16 bits in network byte order.
    fn win_port(port: u16) -> u32 {
        u32::from(u16::from_ne_bytes(port.to_be_bytes()))
    }

    fn win_v4(addr: Ipv4Addr) -> u32 {
        u32::from_ne_bytes(addr.octets())
    }

    fn win_v6(addr: Ipv6Addr) -> Vec<u32> {
        addr.octets()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_ne_bytes(*c))
            .collect()
    }

    #[test]
    fn windows_tcp_rows_count_only_listeners() {
        let lo = win_v4(Ipv4Addr::LOCALHOST);
        let rows = [
            vec![2, lo, win_port(7476), 0, 0, 1_234],
            vec![5, lo, win_port(7476), lo, win_port(50_000), 99],
            vec![2, 0, win_port(443) | 0xABCD_0000, 0, 0, 4],
        ];
        assert_eq!(
            windows_rows(&table(3, &rows), RowKind::Tcp4),
            vec![
                ("127.0.0.1:7476".parse().unwrap(), 1_234),
                ("0.0.0.0:443".parse().unwrap(), 4),
            ]
        );
        let mut row6 = win_v6(Ipv6Addr::LOCALHOST);
        row6.extend([3, win_port(8080)]);
        row6.extend(win_v6(Ipv6Addr::UNSPECIFIED));
        row6.extend([0, 0, 2, 77]);
        let mut closed = row6.clone();
        closed[12] = 8;
        let got = windows_rows(&table(2, &[row6, closed]), RowKind::Tcp6);
        let want = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 8080, 0, 3));
        assert_eq!(got, vec![(want, 77)]);
    }

    #[test]
    fn windows_udp_rows_all_count() {
        let rows = [
            vec![win_v4(Ipv4Addr::LOCALHOST), win_port(53), 10],
            vec![0, win_port(7476), 11],
        ];
        assert_eq!(
            windows_rows(&table(2, &rows), RowKind::Udp4),
            vec![
                ("127.0.0.1:53".parse().unwrap(), 10),
                ("0.0.0.0:7476".parse().unwrap(), 11),
            ]
        );
        let mut row6 = win_v6(Ipv6Addr::UNSPECIFIED);
        row6.extend([0, win_port(7476), 12]);
        assert_eq!(
            windows_rows(&table(1, &[row6]), RowKind::Udp6),
            vec![("[::]:7476".parse().unwrap(), 12)]
        );
    }

    #[test]
    fn windows_tables_stop_at_their_count_and_their_end() {
        let row = vec![0, win_port(1), 1];
        let full = table(2, &[row.clone(), row.clone(), row.clone()]);
        assert_eq!(windows_rows(&full, RowKind::Udp4).len(), 2);
        let mut short = table(3, &[row.clone(), row]);
        short.truncate(short.len() - 1);
        assert_eq!(windows_rows(&short, RowKind::Udp4).len(), 1);
        assert!(windows_rows(&[], RowKind::Tcp4).is_empty());
        assert!(windows_rows(&[1, 0], RowKind::Tcp6).is_empty());
    }

    #[test]
    fn results_are_sorted_unique_and_bounded() {
        let at = |port: u16| SocketAddr::from(([127, 0, 0, 1], port));
        let holder = |port, pid: Option<u32>, process: Option<&str>| PortHolder {
            local: at(port),
            pid,
            process: process.map(str::to_owned),
        };
        let got = finish(vec![
            holder(2, Some(5), Some("b")),
            holder(2, Some(5), None),
            holder(2, None, None),
            holder(1, Some(9), Some("a")),
            holder(2, Some(5), Some("b")),
        ]);
        assert_eq!(
            got,
            vec![
                holder(1, Some(9), Some("a")),
                holder(2, None, None),
                holder(2, Some(5), None),
                holder(2, Some(5), Some("b")),
            ]
        );
        let many: Vec<PortHolder> = (0..300).rev().map(|p| holder(p, None, None)).collect();
        let kept = finish(many);
        assert_eq!(kept.len(), 256);
        assert_eq!(kept[0].local.port(), 0);
        assert_eq!(kept[255].local.port(), 255);
    }
}
