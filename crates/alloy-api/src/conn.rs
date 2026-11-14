//! The places of the API port's connections, and the state of one connection
//! that decides when it closes ([`ConnLimits`]).

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use alloy_rt::{Clock, Instant, StreamAddr};
use tokio::sync::Notify;

use crate::access::Principal;
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

/// The network a source belongs to, which decides which unproven connection
/// gives up its place when they are all taken: an IPv4 /24, an IPv6 /48, or
/// the one source with no IP peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Network {
    V4([u8; 3]),
    V6([u8; 6]),
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

    pub(crate) fn network(self) -> Network {
        match self {
            Source::V4([a, b, c, _]) => Network::V4([a, b, c]),
            Source::V6([a, b, c, d, e, f, _, _]) => Network::V6([a, b, c, d, e, f]),
            Source::Other => Network::Other,
        }
    }
}

/// Fired when the port takes an unproven connection's place for a newer
/// one: the connection must then close.
#[derive(Debug, Default)]
pub(crate) struct Evicted {
    fired: AtomicBool,
    notify: Notify,
}

impl Evicted {
    fn fire(&self) {
        self.fired.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    pub(crate) fn fired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }

    /// Completes once the place is taken.
    pub(crate) async fn wait(&self) {
        loop {
            // Created before the check: a `notify_one` in between leaves its
            // permit for this future.
            let notified = self.notify.notified();
            if self.fired() {
                return;
            }
            notified.await;
        }
    }
}

/// One unproven connection.
#[derive(Debug)]
struct Unproven {
    source: Source,
    evicted: Arc<Evicted>,
}

#[derive(Debug, Default)]
struct Counts {
    /// The next connection id. Ids rise with the time of accept.
    next: u64,
    /// The unproven connections by id, so oldest first. At most
    /// [`ConnLimits::unproven`] entries, one more while a newcomer is placed.
    unproven: BTreeMap<u64, Unproven>,
    /// The unproven connections of each source that has any.
    by_source: BTreeMap<Source, usize>,
    /// The unproven connections of each network that has any.
    by_network: BTreeMap<Network, usize>,
    api: usize,
    /// The proven API connections of each principal that has any. At most
    /// [`ConnLimits::api`] entries.
    by_principal: BTreeMap<Principal, usize>,
    stream: usize,
}

fn add<K: Ord>(map: &mut BTreeMap<K, usize>, key: K) {
    *map.entry(key).or_insert(0) += 1;
}

fn remove<K: Ord>(map: &mut BTreeMap<K, usize>, key: &K) {
    if let Some(n) = map.get_mut(key) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            map.remove(key);
        }
    }
}

impl Counts {
    fn of_source(&self, source: Source) -> usize {
        self.by_source.get(&source).copied().unwrap_or(0)
    }

    fn of_network(&self, network: Network) -> usize {
        self.by_network.get(&network).copied().unwrap_or(0)
    }

    fn of_principal(&self, principal: &Principal) -> usize {
        self.by_principal.get(principal).copied().unwrap_or(0)
    }

    fn insert(&mut self, source: Source) -> (u64, Arc<Evicted>) {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        let evicted = Arc::new(Evicted::default());
        self.unproven.insert(
            id,
            Unproven {
                source,
                evicted: Arc::clone(&evicted),
            },
        );
        add(&mut self.by_source, source);
        add(&mut self.by_network, source.network());
        (id, evicted)
    }

    fn take_unproven(&mut self, id: u64) -> Option<Unproven> {
        let entry = self.unproven.remove(&id)?;
        remove(&mut self.by_source, &entry.source);
        remove(&mut self.by_network, &entry.source.network());
        Some(entry)
    }

    /// The unproven connection that gives up its place: the oldest one of
    /// the source holding the most places in the network holding the most.
    /// Ties go to the network, then the source, with the oldest connection.
    fn victim(&self) -> Option<u64> {
        let mut network = None;
        let mut most = 0;
        for entry in self.unproven.values() {
            let n = self.of_network(entry.source.network());
            if n > most {
                most = n;
                network = Some(entry.source.network());
            }
        }
        let network = network?;
        let mut source = None;
        let mut most = 0;
        for entry in self.unproven.values() {
            if entry.source.network() == network {
                let n = self.of_source(entry.source);
                if n > most {
                    most = n;
                    source = Some(entry.source);
                }
            }
        }
        let source = source?;
        self.unproven
            .iter()
            .find(|(_, entry)| entry.source == source)
            .map(|(&id, _)| id)
    }
}

/// The counts of the API port's connections.
#[derive(Debug)]
pub(crate) struct Pools {
    limits: ConnLimits,
    counts: Mutex<Counts>,
}

/// What an unproven place is moved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Proof {
    /// An API connection whose first authenticated call was by this
    /// principal.
    Api(Principal),
    /// A `stream` connection whose peer the underlay authenticated.
    Stream,
}

/// Why a place was not moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The count it would move to, or the principal's share of it, is full.
    Full,
    /// The place was taken for a newer connection, or is not unproven.
    Gone,
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
    /// source holds its share. When every unproven place is taken, the
    /// [`victim`](Counts::victim) among them and the newcomer gives up its
    /// place. The newcomer is the newest, so it is the victim only when the
    /// unproven bound is 0, and then `None`.
    pub(crate) fn admit(self: &Arc<Pools>, source: Source) -> Option<Place> {
        let mut counts = self.counts();
        if counts.of_source(source) >= self.limits.unproven_per_source {
            return None;
        }
        let (id, evicted) = counts.insert(source);
        if counts.unproven.len() > self.limits.unproven {
            let victim = counts.victim().and_then(|v| counts.take_unproven(v));
            match victim {
                Some(entry) if !Arc::ptr_eq(&entry.evicted, &evicted) => entry.evicted.fire(),
                _ => {
                    counts.take_unproven(id);
                    return None;
                }
            }
        }
        Some(Place {
            pools: Arc::clone(self),
            held: Held::Unproven(id),
            evicted,
        })
    }

    fn release(counts: &mut Counts, held: &Held) {
        match held {
            Held::Unproven(id) => {
                counts.take_unproven(*id);
            }
            Held::Api(principal) => {
                counts.api = counts.api.saturating_sub(1);
                remove(&mut counts.by_principal, principal);
            }
            Held::Stream => counts.stream = counts.stream.saturating_sub(1),
            Held::Gone => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn open(&self) -> Open {
        let counts = self.counts();
        Open {
            unproven: counts.unproven.len(),
            sources: counts.by_source.len(),
            networks: counts.by_network.len(),
            api: counts.api,
            principals: counts.by_principal.len(),
            stream: counts.stream,
        }
    }
}

/// The counts, for tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Open {
    pub(crate) unproven: usize,
    pub(crate) sources: usize,
    pub(crate) networks: usize,
    pub(crate) api: usize,
    pub(crate) principals: usize,
    pub(crate) stream: usize,
}

/// Which count a place is held in.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Held {
    Unproven(u64),
    Api(Principal),
    Stream,
    /// Taken for a newer connection: counted nowhere.
    Gone,
}

/// One connection's place in the port's counts, given back on drop.
#[derive(Debug)]
pub(crate) struct Place {
    pools: Arc<Pools>,
    held: Held,
    evicted: Arc<Evicted>,
}

impl Place {
    /// Fired when the place is taken for a newer connection.
    pub(crate) fn evicted(&self) -> Arc<Evicted> {
        Arc::clone(&self.evicted)
    }

    /// Moves an unproven place to the count `to` names. On
    /// [`Refused::Full`] it stays unproven.
    pub(crate) fn promote(&mut self, to: Proof) -> Result<(), Refused> {
        let Held::Unproven(id) = self.held else {
            return Err(Refused::Gone);
        };
        let limits = self.pools.limits;
        let mut counts = self.pools.counts();
        if !counts.unproven.contains_key(&id) {
            self.held = Held::Gone;
            return Err(Refused::Gone);
        }
        let held = match to {
            Proof::Api(principal) => {
                if counts.api >= limits.api
                    || counts.of_principal(&principal) >= limits.api_per_principal
                {
                    return Err(Refused::Full);
                }
                counts.api += 1;
                add(&mut counts.by_principal, principal.clone());
                Held::Api(principal)
            }
            Proof::Stream => {
                if counts.stream >= limits.stream {
                    return Err(Refused::Full);
                }
                counts.stream += 1;
                Held::Stream
            }
        };
        counts.take_unproven(id);
        self.held = held;
        Ok(())
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        Pools::release(&mut self.pools.counts(), &self.held);
    }
}

/// When one connection closes.
#[derive(Debug)]
enum Rule {
    /// Unproven on the API port: closed at `until`, once `rejected_max`
    /// calls are rejected, or once its place is taken for a newer
    /// connection.
    Unproven {
        place: Place,
        until: Instant,
        rejected_max: u32,
    },
    /// A proven API connection: closed after the gate's `idle` with no
    /// authenticated call in progress. `place` is `None` on the local admin
    /// socket, which counts its connections apart.
    Api { _place: Option<Place> },
    /// A proven `stream` connection: the underlay's to close.
    Stream { _place: Place },
}

#[derive(Debug)]
struct GateState {
    rule: Rule,
    /// Set once the connection must close; it then takes no new call.
    closing: bool,
    /// Rejected calls while unproven.
    rejected: u32,
    /// The authenticated calls in progress, by the instant each stops
    /// counting as in progress ([`ConnLimits::call`] after it began). At
    /// most as many calls as the connection runs at once.
    calls: BTreeMap<Instant, usize>,
    /// When the last authenticated call stopped counting as in progress, or
    /// the connection was proven.
    since: Instant,
}

/// Whether a connection must close now, and if not, until when it may stay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Due {
    Now,
    At(Instant),
    Never,
}

/// Why a call was not started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotStarted {
    /// Proving the connection found the API count, or the principal's share
    /// of it, full.
    Full,
    /// The connection must close.
    Closing,
}

/// The state of one connection that decides when it closes. Calls report to
/// it, and [`Gate::closed`] completes when the connection must close.
#[derive(Debug)]
pub(crate) struct Gate {
    clock: Arc<dyn Clock>,
    idle: Duration,
    call: Duration,
    /// Fired when the place of the connection, while unproven, is taken.
    evicted: Option<Arc<Evicted>>,
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
        let evicted = Some(place.evicted());
        Gate::new(
            clock,
            Rule::Unproven {
                place,
                until: accepted.saturating_add(limits.prove),
                rejected_max: limits.rejected,
            },
            now,
            limits.idle,
            limits.call,
            evicted,
        )
    }

    /// A connection proven at accept, as on the local admin socket.
    pub(crate) fn proven(clock: Arc<dyn Clock>, idle: Duration, call: Duration) -> Gate {
        let now = clock.now();
        Gate::new(clock, Rule::Api { _place: None }, now, idle, call, None)
    }

    fn new(
        clock: Arc<dyn Clock>,
        rule: Rule,
        now: Instant,
        idle: Duration,
        call: Duration,
        evicted: Option<Arc<Evicted>>,
    ) -> Gate {
        Gate {
            clock,
            idle,
            call,
            evicted,
            state: Mutex::new(GateState {
                rule,
                closing: false,
                rejected: 0,
                calls: BTreeMap::new(),
                since: now,
            }),
            changed: Notify::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn due(&self, state: &GateState, now: Instant) -> Due {
        if state.closing {
            return Due::Now;
        }
        match &state.rule {
            Rule::Unproven {
                until,
                rejected_max,
                ..
            } => {
                let evicted = self.evicted.as_ref().is_some_and(|e| e.fired());
                if evicted || state.rejected >= *rejected_max || now >= *until {
                    Due::Now
                } else {
                    Due::At(*until)
                }
            }
            Rule::Api { .. } => {
                // A call past its bound no longer counts as in progress.
                let last = state.calls.keys().next_back().copied();
                let from = last.map_or(state.since, |last| last.max(state.since));
                let at = from.saturating_add(self.idle);
                if now >= at { Due::Now } else { Due::At(at) }
            }
            Rule::Stream { .. } => Due::Never,
        }
    }

    /// Whether the connection must close now, and if so, marks it closing.
    fn check(&self, state: &mut GateState) -> Due {
        let due = self.due(state, self.clock.now());
        if due == Due::Now && !state.closing {
            state.closing = true;
            self.changed.notify_one();
        }
        due
    }

    /// Whether an authenticated call or the underlay has proven the
    /// connection, or it was proven at accept.
    pub(crate) fn is_proven(&self) -> bool {
        !matches!(self.state().rule, Rule::Unproven { .. })
    }

    /// Whether the connection still takes calls. `false` once it must
    /// close: the call is then not served.
    pub(crate) fn admit(&self) -> bool {
        self.check(&mut self.state()) != Due::Now
    }

    /// Records a rejected call. On an unproven connection that reaches its
    /// rejected bound, the connection closes and takes no further call.
    pub(crate) fn reject(&self) {
        let mut state = self.state();
        if matches!(state.rule, Rule::Unproven { .. }) {
            state.rejected = state.rejected.saturating_add(1);
            self.check(&mut state);
            self.changed.notify_one();
        }
    }

    /// Moves an unproven place by `proof`. On [`Refused::Gone`] the
    /// connection must close.
    fn prove(&self, state: &mut GateState, proof: Proof) -> Result<(), Refused> {
        let Rule::Unproven { place, .. } = &mut state.rule else {
            return Err(Refused::Gone);
        };
        let stream = proof == Proof::Stream;
        if let Err(refused) = place.promote(proof) {
            if refused == Refused::Gone {
                state.closing = true;
                self.changed.notify_one();
            }
            return Err(refused);
        }
        let placeholder = Rule::Api { _place: None };
        state.rule = match std::mem::replace(&mut state.rule, placeholder) {
            Rule::Unproven { place, .. } if stream => Rule::Stream { _place: place },
            Rule::Unproven { place, .. } => Rule::Api {
                _place: Some(place),
            },
            other => other,
        };
        state.since = self.clock.now();
        self.changed.notify_one();
        Ok(())
    }

    /// Starts a call authenticated as `principal`, proving the connection
    /// if it was not yet.
    pub(crate) fn begin(self: &Arc<Gate>, principal: &Principal) -> Result<InProgress, NotStarted> {
        let mut state = self.state();
        if self.check(&mut state) == Due::Now {
            return Err(NotStarted::Closing);
        }
        match &state.rule {
            Rule::Unproven { .. } => match self.prove(&mut state, Proof::Api(principal.clone())) {
                Ok(()) => {}
                Err(Refused::Full) => return Err(NotStarted::Full),
                Err(Refused::Gone) => return Err(NotStarted::Closing),
            },
            Rule::Api { .. } => {}
            Rule::Stream { .. } => return Err(NotStarted::Closing),
        }
        let ends = self.clock.now().saturating_add(self.call);
        add(&mut state.calls, ends);
        Ok(InProgress {
            gate: Arc::clone(self),
            ends,
        })
    }

    /// Proves a `stream` connection, moving its place to the `stream`
    /// count. `false` when that count is full or the connection must close;
    /// the connection then closes.
    pub(crate) fn prove_stream(&self) -> bool {
        let mut state = self.state();
        match &state.rule {
            Rule::Stream { .. } => return true,
            Rule::Api { .. } => return false,
            Rule::Unproven { .. } => {}
        }
        if self.check(&mut state) == Due::Now {
            return false;
        }
        if self.prove(&mut state, Proof::Stream).is_ok() {
            return true;
        }
        state.closing = true;
        self.changed.notify_one();
        false
    }

    /// Completes when the connection must close. From then on it takes no
    /// call.
    pub(crate) async fn closed(&self) {
        loop {
            let notified = self.changed.notified();
            let due = self.check(&mut self.state());
            let evicted = async {
                match &self.evicted {
                    Some(evicted) => evicted.wait().await,
                    None => std::future::pending().await,
                }
            };
            match due {
                Due::Now => return,
                Due::At(at) => {
                    let sleep = self.clock.sleep_until(at);
                    crate::server::first(crate::server::first(notified, sleep), evicted).await;
                }
                Due::Never => {
                    crate::server::first(notified, evicted).await;
                }
            }
        }
    }
}

/// One authenticated call in progress on a connection, until it is dropped
/// or `ends`.
#[derive(Debug)]
pub(crate) struct InProgress {
    gate: Arc<Gate>,
    ends: Instant,
}

impl Drop for InProgress {
    fn drop(&mut self) {
        let now = self.gate.clock.now();
        let mut state = self.gate.state();
        remove(&mut state.calls, &self.ends);
        state.since = state.since.max(now.min(self.ends));
        drop(state);
        self.gate.changed.notify_one();
    }
}

#[cfg(test)]
mod tests;
