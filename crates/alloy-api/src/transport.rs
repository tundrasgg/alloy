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

/// The bounds on the connections of one listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnLimits {
    /// The most connections open at once, counted from accept to close
    /// whatever protocol they speak. A connection accepted while this many
    /// are open is closed at once, with no byte sent.
    pub max_connections: usize,
    /// The time from accept to the end of the TLS handshake or, in the
    /// clear, to the bytes that tell the API from `stream`. A connection
    /// that takes longer is closed. Unused on the local admin socket, which
    /// has no handshake of its own.
    pub handshake: Duration,
    /// The longest an API connection stays open with no call in progress,
    /// counted from the end of the handshake or of its last call. It is then
    /// closed. A call is in progress from the arrival of its headers until
    /// its whole response is sent. A `stream` connection is the underlay's
    /// to close.
    pub idle: Duration,
}

impl ConnLimits {
    /// The API port: 1,024 connections, 10 s to hand shake, 60 s idle.
    pub const API_PORT: ConnLimits = ConnLimits {
        max_connections: 1_024,
        handshake: Duration::from_secs(10),
        idle: Duration::from_secs(60),
    };

    /// The local admin socket: 64 connections, 60 s idle.
    pub const LOCAL_ADMIN: ConnLimits = ConnLimits {
        max_connections: 64,
        handshake: Duration::from_secs(10),
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
