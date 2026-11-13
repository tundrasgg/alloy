//! The places of the API port's connections, and the state of one HTTP/2
//! connection that decides when it closes ([`ConnLimits`]).

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use alloy_rt::{Clock, Instant, StreamAddr};
use tokio::sync::Notify;

use crate::transport::ConnLimits;

/// Where an unproven connection comes from, for
/// [`ConnLimits::unproven_per_source`]: an IPv4 address, an IPv6 /64
/// prefix, or, for a stream with no IP peer, one shared source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Source {
    V4([u8; 4]),
    V6([u8; 8]),
    Other,
}

impl Source {
    pub(crate) fn of(peer: Option<StreamAddr>) -> Source {
        match peer {
            Some(StreamAddr::Ip(addr)) => match addr.ip().to_canonical() {
                IpAddr::V4(v4) => Source::V4(v4.octets()),
                IpAddr::V6(v6) => {
                    let mut prefix = [0; 8];
                    prefix.copy_from_slice(&v6.octets()[..8]);
                    Source::V6(prefix)
                }
            },
            _ => Source::Other,
        }
    }
}

/// The count a connection is held in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pool {
    Unproven(Source),
    Api,
    Stream,
}

#[derive(Debug, Default)]
struct Counts {
    unproven: usize,
    /// The unproven connections of each source that has any. It has at most
    /// [`ConnLimits::unproven`] entries, since an entry is removed when its
    /// count falls to 0.
    by_source: BTreeMap<Source, usize>,
    api: usize,
    stream: usize,
}

/// The counts of the API port's connections.
#[derive(Debug)]
pub(crate) struct Pools {
    limits: ConnLimits,
    counts: Mutex<Counts>,
}

impl Pools {
    pub(crate) fn new(limits: ConnLimits) -> Arc<Pools> {
        Arc::new(Pools {
            limits,
            counts: Mutex::new(Counts::default()),
        })
    }

    fn counts(&self) -> std::sync::MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An unproven place for a connection from `source`, or `None` when the
    /// unproven bound or the source's share is reached.
    pub(crate) fn admit(self: &Arc<Pools>, source: Source) -> Option<Place> {
        let mut counts = self.counts();
        let from_source = counts.by_source.get(&source).copied().unwrap_or(0);
        if counts.unproven >= self.limits.unproven || from_source >= self.limits.unproven_per_source
        {
            return None;
        }
        counts.unproven += 1;
        counts.by_source.insert(source, from_source + 1);
        Some(Place {
            pools: Arc::clone(self),
            pool: Pool::Unproven(source),
        })
    }

    fn take(counts: &mut Counts, limits: &ConnLimits, pool: Pool) -> bool {
        match pool {
            Pool::Api if counts.api < limits.api => counts.api += 1,
            Pool::Stream if counts.stream < limits.stream => counts.stream += 1,
            _ => return false,
        }
        true
    }

    fn release(counts: &mut Counts, pool: Pool) {
        match pool {
            Pool::Unproven(source) => {
                counts.unproven = counts.unproven.saturating_sub(1);
                if let Some(n) = counts.by_source.get_mut(&source) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        counts.by_source.remove(&source);
                    }
                }
            }
            Pool::Api => counts.api = counts.api.saturating_sub(1),
            Pool::Stream => counts.stream = counts.stream.saturating_sub(1),
        }
    }

    #[cfg(test)]
    pub(crate) fn open(&self) -> (usize, usize, usize, usize) {
        let counts = self.counts();
        (
            counts.unproven,
            counts.by_source.len(),
            counts.api,
            counts.stream,
        )
    }
}

/// One connection's place in the port's counts, given back on drop.
#[derive(Debug)]
pub(crate) struct Place {
    pools: Arc<Pools>,
    pool: Pool,
}

impl Place {
    /// Moves the place from the unproven count to `to`, [`Pool::Api`] or
    /// [`Pool::Stream`]. `false`, and the place stays where it is, when `to`
    /// is full or the place is not unproven.
    pub(crate) fn promote(&mut self, to: Pool) -> bool {
        if !matches!(self.pool, Pool::Unproven(_)) {
            return false;
        }
        let mut counts = self.pools.counts();
        if !Pools::take(&mut counts, &self.pools.limits, to) {
            return false;
        }
        Pools::release(&mut counts, self.pool);
        self.pool = to;
        true
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        Pools::release(&mut self.pools.counts(), self.pool);
    }
}

/// When one HTTP/2 connection closes.
#[derive(Debug)]
enum Rule {
    /// Unproven on the API port: closed at `until`, or once `rejected_max`
    /// calls are rejected; proven by its first authenticated call, which
    /// moves `place` to the API count, and then closed after `idle`.
    Unproven {
        place: Place,
        until: Instant,
        rejected_max: u32,
        idle: Duration,
    },
    /// Proven: closed after `idle` with no authenticated call in progress.
    /// `place` is `None` on the local admin socket, which counts its
    /// connections apart.
    Proven {
        _place: Option<Place>,
        idle: Duration,
    },
}

#[derive(Debug)]
struct GateState {
    rule: Rule,
    /// Authenticated calls in progress.
    calls: usize,
    /// When `calls` last fell to 0, or the connection was proven.
    since: Instant,
    /// Rejected calls while unproven.
    rejected: u32,
}

/// The state of one HTTP/2 connection that decides when it closes. Calls
/// report to it, and [`Gate::closed`] completes when the connection must
/// close.
#[derive(Debug)]
pub(crate) struct Gate {
    clock: Arc<dyn Clock>,
    state: Mutex<GateState>,
    changed: Notify,
}

impl Gate {
    /// An unproven API port connection accepted at `accepted`, holding
    /// `place`.
    pub(crate) fn unproven(
        clock: Arc<dyn Clock>,
        place: Place,
        accepted: Instant,
        limits: &ConnLimits,
    ) -> Gate {
        let now = clock.now();
        Gate {
            clock,
            state: Mutex::new(GateState {
                rule: Rule::Unproven {
                    place,
                    until: accepted.saturating_add(limits.prove),
                    rejected_max: limits.rejected,
                    idle: limits.idle,
                },
                calls: 0,
                since: now,
                rejected: 0,
            }),
            changed: Notify::new(),
        }
    }

    /// A connection proven at accept, as on the local admin socket.
    pub(crate) fn proven(clock: Arc<dyn Clock>, idle: Duration) -> Gate {
        let now = clock.now();
        Gate {
            clock,
            state: Mutex::new(GateState {
                rule: Rule::Proven { _place: None, idle },
                calls: 0,
                since: now,
                rejected: 0,
            }),
            changed: Notify::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a rejected call. On an unproven connection that reaches its
    /// rejected bound, the connection closes.
    pub(crate) fn reject(&self) {
        let mut state = self.state();
        if matches!(state.rule, Rule::Unproven { .. }) {
            state.rejected = state.rejected.saturating_add(1);
            drop(state);
            self.changed.notify_one();
        }
    }

    /// Starts an authenticated call, proving the connection if it was not
    /// yet. `None`, and the call is not started, when proving it finds the
    /// API count full.
    pub(crate) fn begin(self: &Arc<Gate>) -> Option<InProgress> {
        let mut state = self.state();
        if let Rule::Unproven { place, .. } = &mut state.rule {
            if !place.promote(Pool::Api) {
                return None;
            }
            let placeholder = Rule::Proven {
                _place: None,
                idle: Duration::ZERO,
            };
            state.rule = match std::mem::replace(&mut state.rule, placeholder) {
                Rule::Unproven { place, idle, .. } => Rule::Proven {
                    _place: Some(place),
                    idle,
                },
                proven @ Rule::Proven { .. } => proven,
            };
            self.changed.notify_one();
        }
        state.calls += 1;
        Some(InProgress(Arc::clone(self)))
    }

    /// Completes when the connection must close.
    pub(crate) async fn closed(&self) {
        loop {
            let notified = self.changed.notified();
            let deadline = {
                let state = self.state();
                let now = self.clock.now();
                match &state.rule {
                    Rule::Unproven {
                        until,
                        rejected_max,
                        ..
                    } => {
                        if state.rejected >= *rejected_max || now >= *until {
                            return;
                        }
                        Some(*until)
                    }
                    Rule::Proven { idle, .. } if state.calls == 0 => {
                        let at = state.since.saturating_add(*idle);
                        if now >= at {
                            return;
                        }
                        Some(at)
                    }
                    Rule::Proven { .. } => None,
                }
            };
            match deadline {
                Some(at) => {
                    let sleep = self.clock.sleep_until(at);
                    crate::server::first(notified, sleep).await;
                }
                None => notified.await,
            }
        }
    }
}

/// One authenticated call in progress on a connection.
#[derive(Debug)]
pub(crate) struct InProgress(Arc<Gate>);

impl Drop for InProgress {
    fn drop(&mut self) {
        let now = self.0.clock.now();
        let mut state = self.0.state();
        state.calls = state.calls.saturating_sub(1);
        if state.calls == 0 {
            state.since = now;
            drop(state);
            self.0.changed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    use super::*;

    fn limits() -> ConnLimits {
        ConnLimits {
            unproven: 3,
            unproven_per_source: 2,
            api: 1,
            stream: 1,
            ..ConnLimits::API_PORT
        }
    }

    fn ip(addr: impl Into<IpAddr>) -> Source {
        Source::of(Some(StreamAddr::Ip(SocketAddr::new(addr.into(), 443))))
    }

    #[test]
    fn a_source_is_an_ipv4_address_or_an_ipv6_64() {
        let a = ip(Ipv4Addr::new(192, 0, 2, 1));
        assert_ne!(a, ip(Ipv4Addr::new(192, 0, 2, 2)));
        assert_eq!(a, ip(Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped()));
        let v6 = |last: u16| ip(Ipv6Addr::new(0x2001, 0xdb8, 0, 1, 0, 0, 0, last));
        assert_eq!(v6(1), v6(2));
        assert_ne!(v6(1), ip(Ipv6Addr::new(0x2001, 0xdb8, 0, 2, 0, 0, 0, 1)));
        assert_eq!(Source::of(None), Source::Other);
    }

    #[test]
    fn unproven_places_are_bounded_in_all_and_per_source() {
        let pools = Pools::new(limits());
        let a = ip(Ipv4Addr::new(192, 0, 2, 1));
        let b = ip(Ipv4Addr::new(192, 0, 2, 2));
        let a1 = pools.admit(a).expect("a1");
        let a2 = pools.admit(a).expect("a2");
        assert!(pools.admit(a).is_none(), "the share of a is 2");
        let b1 = pools.admit(b).expect("b1");
        assert!(pools.admit(b).is_none(), "3 unproven in all");
        assert_eq!(pools.open(), (3, 2, 0, 0));
        drop(a1);
        assert_eq!(pools.open(), (2, 2, 0, 0));
        drop((a2, b1));
        assert_eq!(pools.open(), (0, 0, 0, 0), "no source entry is left");
    }

    #[test]
    fn promotion_moves_a_place_and_stops_at_each_bound() {
        let pools = Pools::new(limits());
        let a = ip(Ipv4Addr::new(192, 0, 2, 1));
        let mut api = pools.admit(a).expect("admit");
        assert!(api.promote(Pool::Api));
        assert!(!api.promote(Pool::Stream), "only an unproven place moves");
        let mut second = pools.admit(a).expect("admit");
        assert!(!second.promote(Pool::Api), "the API count is full");
        assert!(second.promote(Pool::Stream));
        assert_eq!(pools.open(), (0, 0, 1, 1));
        // With the source's places promoted, it may open more unproven ones.
        let _third = pools.admit(a).expect("admit");
        drop((api, second));
        assert_eq!(pools.open(), (1, 1, 0, 0));
    }
}
