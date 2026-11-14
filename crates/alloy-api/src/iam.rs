//! The login codes as the API server reads and writes them (ARFC 0002
//! section 3.7), and the rate limit of `ExchangeLoginCode`.

use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use alloy_crypto::Zeroizing;
use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use alloy_proto::iam::v1alpha1::{ExchangeLoginCodeResponse, IssueLoginCodeResponse};
use alloy_rt::{BoxFuture, Instant};

use crate::access::Principal;

/// A login code to issue: for `user` of `org`, asked for by `principal`,
/// with `secret_hash`, the BLAKE3 hash of the secret the client holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// Who asks for it. In M11 rung 2 always the local admin.
    pub principal: Principal,
    /// The user the code is for.
    pub user: String,
    /// The user's org.
    pub org: String,
    /// The hash of the code's secret.
    pub secret_hash: [u8; 32],
}

/// A login code to exchange (ARFC 0002 section 3.7). Its `Debug` output
/// never shows the secret (I-22).
pub struct Exchange {
    /// The code's public lookup id, as sent: 8 bytes in a valid call.
    pub code_id: Vec<u8>,
    /// The code's secret, as sent: 16 bytes in a valid call.
    pub secret: Zeroizing<Vec<u8>>,
    /// The name the new key asks for.
    pub name: String,
    /// The id of a key the new one replaces, or empty.
    pub replaces: String,
    /// The source address of the call, for its audit record.
    pub source: Option<IpAddr>,
}

impl fmt::Debug for Exchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exchange")
            .field("name", &self.name)
            .field("replaces", &self.replaces)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// The login codes of the cluster as the API server uses them.
///
/// Methods take `&self`, may be called concurrently, and never panic.
pub trait LoginCodes: Send + Sync + fmt::Debug + 'static {
    /// Issues a login code as [`Issue`] says, once the server has
    /// authorized it.
    ///
    /// # Errors
    ///
    /// An error of ARFC 0008 clause 3.6: `NotFound` for no such user,
    /// `PermissionDenied` when it or its org is disabled, `Unavailable`.
    fn issue(&self, issue: Issue) -> BoxFuture<'_, Result<IssueLoginCodeResponse, Error>>;

    /// Exchanges a login code for a new access key of its user. The server
    /// has applied the rate limit first.
    ///
    /// # Errors
    ///
    /// The errors of ARFC 0002 section 3.7, `Unauthenticated` alike for an
    /// unknown code and a wrong secret.
    fn exchange(
        &self,
        exchange: Exchange,
    ) -> BoxFuture<'_, Result<ExchangeLoginCodeResponse, Error>>;
}

/// The calls a second that `ExchangeLoginCode` takes from one source.
pub const EXCHANGES_PER_SOURCE: u32 = 10;

/// The calls a second that `ExchangeLoginCode` takes in all.
pub const EXCHANGES_IN_ALL: u32 = 100;

/// The most sources the limiter tracks at once. Past it, a new source's
/// call is refused until a tracked source's window has passed.
const MAX_SOURCES: usize = 4_096;

/// The window the limits count calls in.
const WINDOW: Duration = Duration::from_secs(1);

/// Where calls come from for the per-source limit: an IPv4 address, an IPv6
/// /64, or one shared source for a connection with no IP peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    V4([u8; 4]),
    V6([u8; 8]),
    Other,
}

impl Source {
    fn of(ip: Option<IpAddr>) -> Source {
        match ip.map(|ip| ip.to_canonical()) {
            Some(IpAddr::V4(v4)) => Source::V4(v4.octets()),
            Some(IpAddr::V6(v6)) => {
                let mut prefix = [0; 8];
                prefix.copy_from_slice(&v6.octets()[..8]);
                Source::V6(prefix)
            }
            None => Source::Other,
        }
    }
}

/// The calls of one window.
#[derive(Clone, Copy, Debug)]
struct Window {
    start: Instant,
    calls: u32,
}

impl Window {
    /// Counts one call at `now` within `limit`: `true` when it fits.
    fn take(&mut self, now: Instant, limit: u32) -> bool {
        if now.saturating_duration_since(self.start) >= WINDOW {
            self.start = now;
            self.calls = 0;
        }
        if self.calls >= limit {
            return false;
        }
        self.calls += 1;
        true
    }

    fn ended(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= WINDOW
    }
}

#[derive(Debug)]
struct Counts {
    all: Window,
    by_source: BTreeMap<Source, Window>,
}

/// The rate limit of `ExchangeLoginCode` (ARFC 0002 section 3.7): at most
/// [`EXCHANGES_PER_SOURCE`] calls in a second from one source, an IPv6
/// source counted by its /64, and [`EXCHANGES_IN_ALL`] in all. M11 rung 2
/// has one control node, so the sum over control nodes is this node's.
#[derive(Debug)]
pub(crate) struct ExchangeLimit {
    counts: Mutex<Counts>,
}

impl ExchangeLimit {
    pub(crate) fn new(now: Instant) -> ExchangeLimit {
        let fresh = Window {
            start: now,
            calls: 0,
        };
        ExchangeLimit {
            counts: Mutex::new(Counts {
                all: fresh,
                by_source: BTreeMap::new(),
            }),
        }
    }

    /// Counts a call from `ip` at `now`: `Ok` when it is within both
    /// limits, else the `RateLimited` error with its retry delay, and the
    /// call counts toward neither.
    pub(crate) fn take(&self, ip: Option<IpAddr>, now: Instant) -> Result<(), Error> {
        let source = Source::of(ip);
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !counts.by_source.contains_key(&source) && counts.by_source.len() >= MAX_SOURCES {
            counts.by_source.retain(|_, w| !w.ended(now));
            if counts.by_source.len() >= MAX_SOURCES {
                return Err(limited());
            }
        }
        let mut window = counts.by_source.get(&source).copied().unwrap_or(Window {
            start: now,
            calls: 0,
        });
        let mut all = counts.all;
        if !window.take(now, EXCHANGES_PER_SOURCE) || !all.take(now, EXCHANGES_IN_ALL) {
            return Err(limited());
        }
        counts.all = all;
        counts.by_source.insert(source, window);
        Ok(())
    }
}

fn limited() -> Error {
    Error::new(
        ErrorCode::RateLimited,
        "Too many login codes were presented in the last second, so this one was not looked up \
         and is unchanged. Try again in a second.",
    )
    .with_retry_delay(WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn at(ms: u64) -> Instant {
        Instant::from_nanos(ms * 1_000_000)
    }

    #[test]
    fn ten_a_second_per_source_and_a_hundred_in_all() {
        let limit = ExchangeLimit::new(at(0));
        let a = Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)));
        for _ in 0..EXCHANGES_PER_SOURCE {
            assert!(limit.take(a, at(10)).is_ok());
        }
        let refused = limit.take(a, at(10)).unwrap_err();
        assert_eq!(refused.code(), ErrorCode::RateLimited);
        assert_eq!(refused.retry_delay(), Some(WINDOW));
        assert!(limit.take(a, at(1_010)).is_ok());

        let limit = ExchangeLimit::new(at(0));
        let mut taken = 0;
        for n in 0..20u8 {
            for _ in 0..10 {
                if limit
                    .take(Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))), at(5))
                    .is_ok()
                {
                    taken += 1;
                }
            }
        }
        assert_eq!(taken, EXCHANGES_IN_ALL);
    }

    #[test]
    fn an_ipv6_source_counts_by_its_64() {
        let limit = ExchangeLimit::new(at(0));
        for n in 0..EXCHANGES_PER_SOURCE {
            let ip = Ipv6Addr::new(0x2001, 0xdb8, 0, 1, 0, 0, 0, n as u16);
            assert!(limit.take(Some(IpAddr::V6(ip)), at(1)).is_ok());
        }
        let other_host = Ipv6Addr::new(0x2001, 0xdb8, 0, 1, 9, 9, 9, 9);
        assert!(limit.take(Some(IpAddr::V6(other_host)), at(1)).is_err());
        let other_net = Ipv6Addr::new(0x2001, 0xdb8, 0, 2, 0, 0, 0, 1);
        assert!(limit.take(Some(IpAddr::V6(other_net)), at(1)).is_ok());
    }
}
