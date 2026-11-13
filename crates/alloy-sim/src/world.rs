//! The world of one run: world time, world timers, the scenario's random
//! stream, and the calls of ARFC 0005 section 2.7 that use them.

use std::collections::BTreeMap;
use std::fmt;
use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use alloy_rt::BoxFuture;
use alloy_seam::Detail;

use crate::exec::lock;
use crate::stream::Stream;
use crate::trace::{Hook, KIND_NOTE, Observer, Trace, text_field};

/// The latest world time, 2^63 - 1 nanoseconds (section 7).
pub(crate) const MAX_NANOS: u64 = (1 << 63) - 1;

/// Identifies a host of a world. Ids are given in creation order from 0, and
/// a `HostId` is the `host` field of trace records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId(pub u32);

/// Why a world call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorldError {
    /// The call named a host, network, or other object the world does not
    /// have.
    #[error("no such {0}")]
    Unknown(Detail),
    /// An argument was outside its range.
    #[error("invalid argument: {0}")]
    Invalid(Detail),
    /// The call is not allowed in the object's current state.
    #[error("not allowed in the current state: {0}")]
    State(Detail),
    /// A bound of section 7 was reached.
    #[error("limit reached: {0}")]
    Limit(Detail),
    /// The call was made from inside an observer or a middlebox, and changed
    /// nothing (clause 3.2.9).
    #[error("called from an observer or a middlebox")]
    Busy,
    /// The run has ended.
    #[error("the run has ended")]
    Ended,
}

/// World timers by deadline, then by creation, each with the waker of the
/// future that waits for it.
#[derive(Debug, Default)]
struct Timers {
    next: u64,
    by_deadline: BTreeMap<(u64, u64), Waker>,
}

/// The state of one run that world handles share.
///
/// No lock is held while user code runs, except `trace` while observers run.
/// A world call made from an observer takes only the hook's own lock and
/// returns, so the only other lock taken under `trace` is `timers`, when an
/// observer drops a world sleep. Nothing takes `trace` while holding another
/// lock.
pub(crate) struct Shared {
    seed: u64,
    /// The threads that are running an observer of this world.
    hook: Hook,
    /// True once the run has ended or is ending.
    ended: AtomicBool,
    now: AtomicU64,
    timers: Mutex<Timers>,
    scenario: Mutex<Stream>,
    pub(crate) trace: Mutex<Trace>,
}

impl Shared {
    /// The state of a new run of `seed`, at world time 0, with the trace
    /// header written.
    pub(crate) fn new(seed: u64) -> Shared {
        Shared {
            seed,
            hook: Hook::default(),
            ended: AtomicBool::new(false),
            now: AtomicU64::new(0),
            timers: Mutex::default(),
            scenario: Mutex::new(Stream::new(seed, b"scenario", &[])),
            trace: Mutex::new(Trace::new(seed)),
        }
    }

    pub(crate) fn seed(&self) -> u64 {
        self.seed
    }

    pub(crate) fn now_nanos(&self) -> u64 {
        self.now.load(Ordering::Acquire)
    }

    pub(crate) fn set_now(&self, nanos: u64) {
        self.now.store(nanos, Ordering::Release);
    }

    pub(crate) fn has_ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// Ends the run: from now on, world calls that return a `Result` return
    /// `Ended`.
    pub(crate) fn end(&self) {
        self.ended.store(true, Ordering::Release);
    }

    pub(crate) fn hook(&self) -> &Hook {
        &self.hook
    }

    /// True when the calling thread is inside an observer or a middlebox of
    /// this world, whatever thread that is.
    fn in_hook(&self) -> bool {
        self.hook.is_current()
    }

    /// `Busy` inside a hook, `Ended` after the run.
    fn check(&self) -> Result<(), WorldError> {
        if self.in_hook() {
            Err(WorldError::Busy)
        } else if self.has_ended() {
            Err(WorldError::Ended)
        } else {
            Ok(())
        }
    }

    /// Writes one record at the current world time. An observer panic ends
    /// the run.
    pub(crate) fn record(&self, host: Option<HostId>, kind: u16, body: &[u8], data: &[u8]) {
        let panicked =
            lock(&self.trace).write(&self.hook, self.now_nanos(), host, kind, body, data);
        if panicked {
            self.end();
        }
    }

    /// The earliest world timer deadline.
    pub(crate) fn next_deadline(&self) -> Option<u64> {
        lock(&self.timers)
            .by_deadline
            .first_key_value()
            .map(|((deadline, _), _)| *deadline)
    }

    /// Removes the world timers due at `deadline` and returns their wakers in
    /// creation order.
    pub(crate) fn take_due(&self, deadline: u64) -> Vec<Waker> {
        let mut timers = lock(&self.timers);
        let later = timers
            .by_deadline
            .split_off(&(deadline.saturating_add(1), 0));
        std::mem::replace(&mut timers.by_deadline, later)
            .into_values()
            .collect()
    }

    /// Removes every world timer and returns the wakers, to be dropped once
    /// no lock is held.
    pub(crate) fn clear_timers(&self) -> Vec<Waker> {
        std::mem::take(&mut lock(&self.timers).by_deadline)
            .into_values()
            .collect()
    }
}

/// One run's world. Clones share it.
///
/// World calls may be made from the control task, from any task that holds a
/// `World`, and from any thread. Once the run has ended, every method that
/// returns a `Result` returns [`WorldError::Ended`]. Inside an observer they
/// return [`WorldError::Busy`] and change nothing (clause 3.2.9).
#[derive(Clone)]
pub struct World(Arc<Shared>);

impl fmt::Debug for World {
    /// The world time and the host count.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("World")
            .field("now", &self.now())
            .field("hosts", &0u32)
            .finish()
    }
}

impl World {
    pub(crate) fn new(shared: Arc<Shared>) -> World {
        World(shared)
    }

    /// The current world time.
    pub fn now(&self) -> Duration {
        Duration::from_nanos(self.0.now_nanos())
    }

    /// Completes at the first world instant at or after `t`: on its first
    /// poll when `t` is not later than now. A `t` above 2^63 - 1 ns never
    /// comes, and neither does completion. Inside an observer it returns a
    /// future that never completes.
    ///
    /// The timer of a world sleep writes no trace record, whichever task
    /// awaits it.
    pub fn sleep_until(&self, t: Duration) -> BoxFuture<'static, ()> {
        match u64::try_from(t.as_nanos()) {
            Ok(deadline) if deadline <= MAX_NANOS && !self.0.in_hook() => Box::pin(Sleep {
                shared: Arc::clone(&self.0),
                deadline,
                timer: None,
            }),
            _ => Box::pin(pending()),
        }
    }

    /// [`World::sleep_until`] the current world time plus `d`. The deadline
    /// is fixed at the call.
    pub fn sleep(&self, d: Duration) -> BoxFuture<'static, ()> {
        self.sleep_until(self.now().saturating_add(d))
    }

    /// The next value of the scenario's random stream, which no host, router,
    /// or link shares. Inside an observer it returns 0 and does not advance
    /// the stream.
    pub fn random_u64(&self) -> u64 {
        if self.0.in_hook() {
            return 0;
        }
        lock(&self.0.scenario).next_u64()
    }

    /// Writes a `note` record with `text`, cut to 1,024 bytes at a character
    /// boundary.
    ///
    /// # Errors
    ///
    /// [`WorldError::Busy`] inside an observer and [`WorldError::Ended`]
    /// after the run.
    pub fn note(&self, text: &str) -> Result<(), WorldError> {
        self.0.check()?;
        self.0.record(None, KIND_NOTE, &text_field(text), &[]);
        Ok(())
    }

    /// Adds an observer, which from then on receives every record in trace
    /// order (clause 3.6.1).
    ///
    /// # Errors
    ///
    /// [`WorldError::Limit`] when the world holds 64 observers,
    /// [`WorldError::Busy`] inside an observer, and [`WorldError::Ended`]
    /// after the run.
    pub fn observe(&self, observer: Box<dyn Observer>) -> Result<(), WorldError> {
        self.0.check()?;
        lock(&self.0.trace).add_observer(observer)
    }
}

/// The future of a world sleep. It holds a timer from its first pending poll
/// until it completes or is dropped.
struct Sleep {
    shared: Arc<Shared>,
    deadline: u64,
    timer: Option<u64>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.shared.now_nanos() >= self.deadline {
            return Poll::Ready(());
        }
        if self.shared.has_ended() {
            return Poll::Pending;
        }
        let shared = Arc::clone(&self.shared);
        let mut timers = lock(&shared.timers);
        let id = match self.timer {
            Some(id) => id,
            None => {
                let id = timers.next;
                timers.next += 1;
                self.timer = Some(id);
                id
            }
        };
        timers
            .by_deadline
            .entry((self.deadline, id))
            .and_modify(|waker| waker.clone_from(cx.waker()))
            .or_insert_with(|| cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let Some(id) = self.timer {
            let waker = lock(&self.shared.timers)
                .by_deadline
                .remove(&(self.deadline, id));
            drop(waker);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn due_timers_are_taken_in_creation_order() {
        let shared = Shared::new(1);
        {
            let mut timers = lock(&shared.timers);
            for (deadline, id) in [(5, 2), (5, 0), (9, 1), (5, 3)] {
                timers
                    .by_deadline
                    .insert((deadline, id), Waker::noop().clone());
            }
        }
        assert_eq!(shared.next_deadline(), Some(5));
        assert_eq!(shared.take_due(5).len(), 3);
        assert_eq!(shared.next_deadline(), Some(9));
        assert_eq!(shared.take_due(9).len(), 1);
        assert_eq!(shared.next_deadline(), None);
    }

    #[test]
    fn a_dropped_sleep_removes_its_timer() {
        let shared = Arc::new(Shared::new(1));
        let world = World::new(Arc::clone(&shared));
        let mut sleep = world.sleep(Duration::from_nanos(10));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(sleep.as_mut().poll(&mut cx).is_pending());
        assert!(sleep.as_mut().poll(&mut cx).is_pending());
        assert_eq!(lock(&shared.timers).by_deadline.len(), 1);
        drop(sleep);
        assert_eq!(shared.next_deadline(), None);
    }

    #[test]
    fn sleeps_past_the_end_of_time_hold_no_timer() {
        let shared = Arc::new(Shared::new(1));
        let world = World::new(Arc::clone(&shared));
        let mut cx = Context::from_waker(Waker::noop());
        let mut never = world.sleep_until(Duration::from_nanos(MAX_NANOS + 1));
        assert!(never.as_mut().poll(&mut cx).is_pending());
        let mut last = world.sleep_until(Duration::from_nanos(MAX_NANOS));
        assert!(last.as_mut().poll(&mut cx).is_pending());
        assert_eq!(shared.next_deadline(), Some(MAX_NANOS));
        let mut now = world.sleep(Duration::ZERO);
        assert!(now.as_mut().poll(&mut cx).is_ready());
    }

    #[test]
    fn calls_from_a_hook_are_busy_and_after_the_end_ended() {
        let shared = Arc::new(Shared::new(1));
        let world = World::new(Arc::clone(&shared));
        let first = world.random_u64();
        let inside = shared.hook.enter();
        assert_eq!(world.note("x"), Err(WorldError::Busy));
        assert_eq!(world.random_u64(), 0);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            world
                .sleep(Duration::ZERO)
                .as_mut()
                .poll(&mut cx)
                .is_pending()
        );
        // Another thread is outside the hook while this one is inside it.
        let other = thread::scope(|scope| scope.spawn(|| world.random_u64()).join());
        drop(inside);
        let mut expected = Stream::new(1, b"scenario", &[]);
        assert_eq!(first, expected.next_u64());
        assert_eq!(other.ok(), Some(expected.next_u64()));
        assert_eq!(world.random_u64(), expected.next_u64());
        shared.end();
        assert_eq!(world.note("x"), Err(WorldError::Ended));
        assert_eq!(format!("{world:?}"), "World { now: 0ns, hosts: 0 }");
    }
}
