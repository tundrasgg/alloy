//! Whether the API runs on TLS or in the clear (ARFC 0008 clause 3.8.1), and
//! the bounds on the connections of one listener.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use alloy_rt::{RuntimeConfig, Stream};

use crate::tls::Protocol;

/// The name of the `net` implementation under which no byte leaves the
/// process (ARFC 0005 section 2.3).
const MEM: &str = "mem";

/// The HTTP/2 connection preface (RFC 9113 section 3.4), which an API client
/// sends first and the `stream` underlay never does.
pub(crate) const H2_PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// How the API port and its clients carry bytes: on TLS 1.3, or the same
/// bytes in the clear. The `net` implementation of the runtime decides it
/// (ARFC 0008 clause 3.8.1, `agent/eng/crypto.md` rule 4): in the clear
/// under `mem`, where no byte leaves the process, so that a simulated run
/// draws no randomness outside `alloy-rt`, and on TLS under every other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transport {
    tls: bool,
}

impl Transport {
    /// The transport of a runtime built from `config`: in the clear when its
    /// `net` binding names `mem`, on TLS otherwise. `config` must be the
    /// configuration the runtime handed to the API was built from.
    #[must_use]
    pub fn of(config: &RuntimeConfig) -> Transport {
        Transport {
            tls: config.net.name.as_str() != MEM,
        }
    }

    /// Whether bytes go on TLS.
    #[must_use]
    pub fn is_tls(self) -> bool {
        self.tls
    }

    /// TLS, for tests under `os`.
    #[cfg(test)]
    pub(crate) const TLS: Transport = Transport { tls: true };

    /// The clear, for tests of the `mem` path on `os` loopback, since the
    /// `mem` `net` serves no stream yet.
    #[cfg(test)]
    pub(crate) const CLEAR: Transport = Transport { tls: false };
}

/// The bounds on the connections of the API port (ARFC 0008 clause 3.8,
/// I-21), and what happens at each.
///
/// A connection is *unproven* from accept until a call on it authenticates,
/// which *proves* it as an API connection, or until the `stream` underlay
/// authenticates its peer ([`StreamConn::prove`](crate::StreamConn::prove)),
/// which proves it as a `stream` connection. Unproven, proven API, and
/// proven `stream` connections are counted apart, so connections with no
/// credential never hold the places of authenticated API clients or of
/// `stream` peers. The port holds at most
/// [`ConnLimits::max_connections`] connections in all.
///
/// A *source* is an IPv4 address or an IPv6 /64, since a host is commonly
/// given a whole /64, and its *network* is its IPv4 /24 or IPv6 /48. When
/// every unproven place is taken, a new connection takes the place of the
/// oldest unproven connection of the source that holds the most unproven
/// connections, within the network that holds the most, the new connection
/// counted and ties going to the oldest connection. The new connection is
/// the newest, so it is always placed, unless its source holds its share.
/// An unproven connection is closed for a newer one only while its network
/// holds as many unproven connections as any other network, and its source
/// as many as any other in that network. So a client with no credential
/// that holds more unproven connections than it has networks closes only
/// its own connections, and keeps no other client out: to close a
/// connection whose network holds no other, it must hold at most one
/// unproven connection in each of its networks, with every unproven place
/// taken, which takes up to [`unproven`](ConnLimits::unproven) networks.
///
/// A call is *rejected* when it fails authentication, names no method, or
/// is refused because its connection cannot be proven. A rejected call
/// never counts as activity: it neither proves a connection nor keeps one
/// open.
///
/// A connection *closed* at one of these bounds takes no new call: one it
/// receives from then on fails with `Unavailable` without being
/// authenticated or served, including a call the client sent before it
/// read the GOAWAY. The calls already in progress are answered, and the
/// connection is dropped once they are or [`CLOSE_GRACE`](crate::CLOSE_GRACE)
/// has passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnLimits {
    /// The most unproven connections at once. A connection accepted while
    /// this many are open takes the place of one of them, or is closed at
    /// once with no byte sent, as the type's documentation says.
    pub unproven: usize,
    /// The most unproven connections at once from one source. A connection
    /// accepted while its source has this many is closed at once, with no
    /// byte sent.
    pub unproven_per_source: usize,
    /// The most proven API connections at once. A call that would prove a
    /// connection while this many are open fails with `Unavailable` and a
    /// retry delay, and is rejected.
    pub api: usize,
    /// The most proven API connections at once whose first authenticated
    /// call was by one principal. A call that would prove a connection while
    /// its principal holds this many fails as at [`api`](ConnLimits::api).
    pub api_per_principal: usize,
    /// The most proven `stream` connections at once. A connection the
    /// underlay proves while this many are open is closed.
    pub stream: usize,
    /// The time from accept to the end of the TLS handshake or, in the
    /// clear, to the bytes that tell the API from `stream`. A connection that
    /// takes longer is closed.
    pub handshake: Duration,
    /// The time from accept within which an authenticated call or the
    /// underlay must prove a connection. An unproven connection still open
    /// then is closed.
    pub prove: Duration,
    /// The most rejected calls on an unproven connection. It is closed once
    /// this many have been rejected.
    pub rejected: u32,
    /// The longest a call counts as in progress: from its authentication
    /// until its whole response is sent, or this long, whichever is first.
    /// A call that runs longer, such as one whose request never ends or
    /// whose response the client never reads, no longer keeps its
    /// connection open.
    pub call: Duration,
    /// The longest a proven API connection stays open with no call in
    /// progress, counted from the moment it was proven or its last call
    /// stopped counting as in progress. It is then closed. A proven
    /// `stream` connection is the underlay's to close.
    pub idle: Duration,
}

impl ConnLimits {
    /// The API port: 1,024 connections, of which 256 unproven, at most 8 of
    /// them from one source, 512 proven API connections, at most 32 of them
    /// for one principal, and 256 proven `stream` connections; 10 s to hand
    /// shake, 20 s to prove, 4 rejected calls, 60 s for a call, and 60 s
    /// idle.
    pub const API_PORT: ConnLimits = ConnLimits {
        unproven: 256,
        unproven_per_source: 8,
        api: 512,
        api_per_principal: 32,
        stream: 256,
        handshake: Duration::from_secs(10),
        prove: Duration::from_secs(20),
        rejected: 4,
        call: Duration::from_secs(60),
        idle: Duration::from_secs(60),
    };

    /// The most connections the port holds at once: the unproven, API, and
    /// `stream` bounds added.
    #[must_use]
    pub fn max_connections(&self) -> usize {
        self.unproven
            .saturating_add(self.api)
            .saturating_add(self.stream)
    }
}

/// The bounds on the connections of the local admin socket, whose callers
/// are all local admins (ARFC 0008 clause 3.7.2), so every connection is
/// proven at accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdminLimits {
    /// The most connections open at once. A connection accepted while this
    /// many are open is closed at once, with no byte sent.
    pub max_connections: usize,
    /// The longest a call counts as in progress, as
    /// [`ConnLimits::call`] says.
    pub call: Duration,
    /// The longest a connection stays open with no call in progress, counted
    /// from accept or from when its last call stopped counting as in
    /// progress. It is then closed, as [`ConnLimits`] says.
    pub idle: Duration,
}

impl AdminLimits {
    /// The local admin socket: 64 connections, 60 s for a call, 60 s idle.
    pub const LOCAL_ADMIN: AdminLimits = AdminLimits {
        max_connections: 64,
        call: Duration::from_secs(60),
        idle: Duration::from_secs(60),
    };
}

/// The count of a listener's open connections.
#[derive(Debug)]
pub(crate) struct Slots {
    open: AtomicUsize,
    max: usize,
}

impl Slots {
    pub(crate) fn new(max: usize) -> Arc<Slots> {
        Arc::new(Slots {
            open: AtomicUsize::new(0),
            max,
        })
    }

    /// A slot for one more connection, or `None` when `max` are open.
    pub(crate) fn take(self: &Arc<Slots>) -> Option<Slot> {
        self.open
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.max).then_some(n + 1)
            })
            .ok()
            .map(|_| Slot(Arc::clone(self)))
    }
}

/// One open connection's place in its listener's count, given back on drop.
#[derive(Debug)]
pub(crate) struct Slot(Arc<Slots>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Reads the first bytes of a connection in the clear until they tell the
/// API from `stream` (ARFC 0008 clause 3.8.1): the API once they are the
/// whole HTTP/2 preface, `stream` as soon as one differs from it. Returns the
/// protocol and the bytes read, which belong to it, or `None` when the stream
/// ends or fails first.
pub(crate) async fn sniff(stream: &Arc<dyn Stream>) -> Option<(Protocol, Vec<u8>)> {
    let mut seen = Vec::with_capacity(H2_PREFACE.len());
    let mut buf = [0; H2_PREFACE.len()];
    loop {
        let want = H2_PREFACE.len() - seen.len();
        let n = stream.read(&mut buf[..want]).await.ok()?;
        if n == 0 {
            return None;
        }
        seen.extend_from_slice(&buf[..n]);
        if !H2_PREFACE.starts_with(&seen) {
            return Some((Protocol::Stream, seen));
        }
        if seen.len() == H2_PREFACE.len() {
            return Some((Protocol::Api, seen));
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_seam::ImplName;

    use super::*;

    #[test]
    fn only_mem_runs_in_the_clear() {
        let of =
            |name: &str| Transport::of(&RuntimeConfig::all(ImplName::new(name).expect("name")));
        assert!(!of("mem").is_tls());
        assert!(of("os").is_tls());
        assert!(of("natlab").is_tls());
        let mut config = RuntimeConfig::all(ImplName::new("mem").expect("name"));
        config.net.name = ImplName::new("os").expect("name");
        assert!(Transport::of(&config).is_tls());
    }

    #[test]
    fn slots_stop_at_max_and_come_back_on_drop() {
        let slots = Slots::new(2);
        let a = slots.take().expect("first");
        let b = slots.take().expect("second");
        assert!(slots.take().is_none());
        drop(a);
        let c = slots.take().expect("freed");
        assert!(slots.take().is_none());
        drop((b, c));
        assert_eq!(slots.open.load(Ordering::Acquire), 0);
    }
}
