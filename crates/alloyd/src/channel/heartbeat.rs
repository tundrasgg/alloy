//! Heartbeats (ARFC 0006 clause 3.13), sans-I/O: what one holds and when
//! the next starts.

use std::net::IpAddr;
use std::time::Duration;

use alloy_rt::{Instant, InterfaceList};

use super::proto::{self, NodeStatus, NodeStatusReply};

/// The time limit of one heartbeat.
pub const HEARTBEAT_LIMIT: Duration = Duration::from_secs(4);

/// After a success, the next heartbeat starts this long after the previous
/// one started.
pub const SUCCESS_PERIOD: Duration = Duration::from_secs(5);

/// The first delay after a failure, counted from its end. It doubles with
/// each further failure in a row, up to [`MAX_FAILURE_DELAY`].
pub const FIRST_FAILURE_DELAY: Duration = Duration::from_secs(1);

/// The longest delay after a failure that is not a refusal.
pub const MAX_FAILURE_DELAY: Duration = Duration::from_secs(4);

/// The delay after `REFUSED` or `RELEASE_REFUSED`, counted from its end.
pub const REFUSED_DELAY: Duration = Duration::from_secs(60);

/// How a heartbeat ended, for its schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeartbeatResult {
    /// The reply's result was `OK`.
    Ok,
    /// The reply's result was `REFUSED` or `RELEASE_REFUSED`.
    Refused,
    /// Any other result, or no reply within the time limit.
    Failed,
}

impl HeartbeatResult {
    /// The schedule's reading of a reply.
    #[must_use]
    pub fn of(reply: &NodeStatusReply) -> HeartbeatResult {
        match reply.result {
            proto::status::OK => HeartbeatResult::Ok,
            proto::status::REFUSED | proto::status::RELEASE_REFUSED => HeartbeatResult::Refused,
            _ => HeartbeatResult::Failed,
        }
    }
}

/// When a node's heartbeats start. It holds the start of the next one, and
/// the failures in a row since the last success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    next: Instant,
    failures: u32,
}

impl Schedule {
    /// The schedule of a node that holds a valid record from `now`: the
    /// first heartbeat is due at once.
    #[must_use]
    pub fn new(now: Instant) -> Schedule {
        Schedule {
            next: now,
            failures: 0,
        }
    }

    /// When the next heartbeat is due. One that falls due while another
    /// request is outstanding starts when that request ends.
    #[must_use]
    pub fn due(&self) -> Instant {
        self.next
    }

    /// The heartbeat that started at `started` ended at `ended` with
    /// `result`: sets when the next one starts.
    pub fn ended(&mut self, result: HeartbeatResult, started: Instant, ended: Instant) {
        match result {
            HeartbeatResult::Ok => {
                self.failures = 0;
                self.next = started.saturating_add(SUCCESS_PERIOD);
            }
            HeartbeatResult::Refused => {
                self.failures = self.failures.saturating_add(1);
                self.next = ended.saturating_add(REFUSED_DELAY);
            }
            HeartbeatResult::Failed => {
                let doublings = self.failures.min(2);
                self.failures = self.failures.saturating_add(1);
                let delay = FIRST_FAILURE_DELAY.saturating_mul(1 << doublings);
                self.next = ended.saturating_add(delay.min(MAX_FAILURE_DELAY));
            }
        }
    }
}

/// The `addresses` of a heartbeat: the node's current addresses, leaving out
/// loopback interfaces, link-local addresses, and `al*` interfaces, at most
/// 64, IPv4 before IPv6, each family ascending, with no repeats (ARFC 0006
/// clause 3.13 and ARFC 0002 section 3.10).
#[must_use]
pub fn addresses(list: &InterfaceList) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = list
        .interfaces
        .iter()
        .filter(|i| !i.loopback && !i.name.starts_with("al"))
        .flat_map(|i| i.addrs.iter().map(|a| a.ip))
        .filter(|ip| !ip.is_loopback() && !is_link_local(ip))
        .collect();
    out.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    out.dedup();
    out.truncate(proto::MAX_STATUS_ADDRESSES);
    out
}

fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 == 0xfe80,
    }
}

/// The `NodeStatus` of a heartbeat with `addresses` and `release`, empty
/// when the node does not write its running release.
#[must_use]
pub fn status(addresses: &[IpAddr], release: &str) -> NodeStatus {
    NodeStatus {
        addresses: addresses
            .iter()
            .map(|ip| match ip {
                IpAddr::V4(v4) => v4.octets().to_vec(),
                IpAddr::V6(v6) => v6.octets().to_vec(),
            })
            .collect(),
        release: release.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use alloy_rt::{Interface, InterfaceAddr};

    use super::*;

    fn at(secs: u64) -> Instant {
        Instant::from_nanos(secs * 1_000_000_000)
    }

    #[test]
    fn three_failures_put_the_fourth_start_24_seconds_after_a_success() {
        // ARFC 0006 clause 3.13: accepted at 0, then three that fail in a
        // row, each after its 4 seconds.
        let mut s = Schedule::new(at(0));
        assert_eq!(s.due(), at(0));
        s.ended(HeartbeatResult::Ok, at(0), at(0));
        assert_eq!(s.due(), at(5));
        s.ended(HeartbeatResult::Failed, at(5), at(9));
        assert_eq!(s.due(), at(10));
        s.ended(HeartbeatResult::Failed, at(10), at(14));
        assert_eq!(s.due(), at(16));
        s.ended(HeartbeatResult::Failed, at(16), at(20));
        assert_eq!(s.due(), at(24));
        s.ended(HeartbeatResult::Failed, at(24), at(28));
        assert_eq!(s.due(), at(32), "the delay stays at 4 seconds");
        s.ended(HeartbeatResult::Ok, at(32), at(33));
        assert_eq!(s.due(), at(37));
        s.ended(HeartbeatResult::Failed, at(37), at(41));
        assert_eq!(s.due(), at(42), "a success restarts the doubling");
    }

    #[test]
    fn a_refusal_waits_60_seconds() {
        let mut s = Schedule::new(at(0));
        s.ended(HeartbeatResult::Refused, at(0), at(1));
        assert_eq!(s.due(), at(61));
        let reply = |result| NodeStatusReply {
            result,
            finalized_release: String::new(),
        };
        assert_eq!(
            HeartbeatResult::of(&reply(proto::status::OK)),
            HeartbeatResult::Ok
        );
        assert_eq!(
            HeartbeatResult::of(&reply(proto::status::REFUSED)),
            HeartbeatResult::Refused
        );
        assert_eq!(
            HeartbeatResult::of(&reply(proto::status::RELEASE_REFUSED)),
            HeartbeatResult::Refused
        );
        assert_eq!(
            HeartbeatResult::of(&reply(proto::status::UNAVAILABLE)),
            HeartbeatResult::Failed
        );
        assert_eq!(
            HeartbeatResult::of(&reply(proto::status::INVALID)),
            HeartbeatResult::Failed
        );
    }

    fn interface(name: &str, loopback: bool, ips: &[IpAddr]) -> Interface {
        Interface {
            index: 1,
            name: name.to_owned(),
            mtu: 1500,
            up: true,
            loopback,
            addrs: ips
                .iter()
                .map(|ip| InterfaceAddr {
                    ip: *ip,
                    prefix_len: 24,
                })
                .collect(),
        }
    }

    #[test]
    fn addresses_leave_out_loopback_link_local_and_al() {
        let v4 = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        let global6: IpAddr = "2001:db8::2".parse::<Ipv6Addr>().unwrap().into();
        let low6: IpAddr = "2001:db8::1".parse::<Ipv6Addr>().unwrap().into();
        let list = InterfaceList {
            generation: 1,
            interfaces: vec![
                interface(
                    "lo",
                    true,
                    &[v4(127, 0, 0, 1), IpAddr::V6(Ipv6Addr::LOCALHOST)],
                ),
                interface(
                    "eth0",
                    false,
                    &[
                        v4(192, 168, 1, 9),
                        v4(169, 254, 0, 1),
                        global6,
                        "fe80::1".parse::<Ipv6Addr>().unwrap().into(),
                    ],
                ),
                interface("eth1", false, &[v4(10, 0, 0, 1), low6, v4(10, 0, 0, 1)]),
                interface("al0", false, &[v4(100, 64, 0, 1)]),
            ],
        };
        assert_eq!(
            addresses(&list),
            [v4(10, 0, 0, 1), v4(192, 168, 1, 9), low6, global6]
        );
        let s = status(&addresses(&list), "");
        assert_eq!(s.addresses[0], [10, 0, 0, 1]);
        assert_eq!(s.addresses[3].len(), 16);
    }

    #[test]
    fn addresses_are_bounded() {
        let ips: Vec<IpAddr> = (0..100u8)
            .map(|n| IpAddr::V4(Ipv4Addr::new(10, 0, 1, n)))
            .collect();
        let list = InterfaceList {
            generation: 1,
            interfaces: vec![interface("eth0", false, &ips)],
        };
        assert_eq!(addresses(&list).len(), 64);
    }
}
