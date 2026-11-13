//! The world of one run and the calls of ARFC 0005 section 2.7: world time,
//! world timers, the scenario's random stream, notes, observers, hosts,
//! faults, and histories.

use std::fmt;
use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use alloy_rt::{BoxFuture, Runtime};
use alloy_seam::Detail;

use crate::core::{After, MAX_NANOS, Shared};
use crate::disk::Disk;
use crate::exec::lock;
use crate::fault::Fault;
use crate::history::History;
use crate::hook;
use crate::host::{Host, HostConfig, HostState, MAX_HOSTS, MAX_PPM};
use crate::stream::Stream;
use crate::trace::{KIND_NOTE, Observer, text_field};

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
    /// The call was made from inside an observer or a middlebox, of this
    /// world or another, and changed nothing (clause 3.2.9).
    #[error("called from an observer or a middlebox")]
    Busy,
    /// The run has ended.
    #[error("the run has ended")]
    Ended,
}

/// One run's world. Clones share it.
///
/// World calls may be made from the control task, from any task that holds a
/// `World`, and from any thread. Once the run has ended, every method that
/// returns a `Result` returns [`WorldError::Ended`]. Inside an observer of
/// any world they return [`WorldError::Busy`] and change nothing (clause
/// 3.2.9).
#[derive(Clone)]
pub struct World(Arc<Shared>);

impl fmt::Debug for World {
    /// The world time and the host count.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hosts = lock(&self.0.core).hosts.len();
        f.debug_struct("World")
            .field("now", &self.now())
            .field("hosts", &hosts)
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
            Ok(deadline) if deadline <= MAX_NANOS && !hook::inside() => Box::pin(Sleep {
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
        if hook::inside() {
            return 0;
        }
        self.0.scenario_u64()
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
        self.0.push(None, KIND_NOTE, &text_field(text));
        self.0.flush();
        Ok(())
    }

    /// Creates a host with its devices and starts it at the current world
    /// time: it writes `host-start` and the `task-spawn` of its `boot` task,
    /// whose future `config.boot` builds before this returns (clause 3.3.4).
    ///
    /// # Errors
    ///
    /// [`WorldError::Invalid`] for a `clock_ppm` outside -500,000 to 500,000,
    /// a `block_size` other than 512 or 4,096, more than 64 devices, and a
    /// device that clause 3.3.7 rejects: one with a length outside its
    /// range, or whose path is the root, another device's, or below one.
    /// [`WorldError::Limit`] when the world holds 10,000 hosts,
    /// [`WorldError::Busy`] inside an observer, and [`WorldError::Ended`]
    /// after the run.
    pub fn add_host(&self, config: HostConfig) -> Result<HostId, WorldError> {
        self.0.check()?;
        if !(-MAX_PPM..=MAX_PPM).contains(&config.clock_ppm) {
            return Err(WorldError::Invalid(Detail::new(
                "clock_ppm must be -500,000 to 500,000",
            )));
        }
        let mut after = After::default();
        let id = {
            let mut core = lock(&self.0.core);
            if core.hosts.len() >= MAX_HOSTS {
                return Err(WorldError::Limit(Detail::new("10000 hosts")));
            }
            // At most 10,000 hosts, so the id fits a u32.
            let id = HostId(core.hosts.len() as u32);
            let reads = Stream::new(self.0.seed(), b"disk-read", &[u64::from(id.0)]);
            let disk = Disk::new(config.block_size, config.capacity, &config.devices, reads)?;
            let host = Host::new(config, disk, self.0.now_nanos(), self.0.wall_start());
            core.hosts.push(host);
            core.start(&self.0, id, None, &mut after);
            id
        };
        self.0.settle(after);
        Ok(id)
    }

    /// The state of `host`.
    ///
    /// # Errors
    ///
    /// [`WorldError::Unknown`] for a host the world does not have,
    /// [`WorldError::Busy`] inside an observer, and [`WorldError::Ended`]
    /// after the run.
    pub fn host_state(&self, host: HostId) -> Result<HostState, WorldError> {
        self.0.check()?;
        let core = lock(&self.0.core);
        Ok(core.host(host)?.state(self.0.now_nanos()))
    }

    /// The runtime of the host's current start.
    ///
    /// # Errors
    ///
    /// [`WorldError::State`] while the host is crashed,
    /// [`WorldError::Unknown`] for a host the world does not have,
    /// [`WorldError::Busy`] inside an observer, and [`WorldError::Ended`]
    /// after the run.
    pub fn runtime(&self, host: HostId) -> Result<Runtime, WorldError> {
        self.0.check()?;
        let core = lock(&self.0.core);
        core.host(host)?
            .runtime
            .clone()
            .ok_or_else(|| WorldError::State(Detail::new("the host has crashed")))
    }

    /// Applies `fault` now (clause 3.3.8).
    ///
    /// # Errors
    ///
    /// [`WorldError::Unknown`] for a host the world does not have,
    /// [`WorldError::Invalid`] for a value out of range, and
    /// [`WorldError::State`] when the fault does not fit the host's state:
    /// a `Crash` of a crashed host, a `Restart` of a running host, a `Stall`
    /// of a crashed host. [`WorldError::Busy`] inside an observer and
    /// [`WorldError::Ended`] after the run. On an error nothing changes.
    pub fn apply(&self, fault: Fault) -> Result<(), WorldError> {
        self.0.check()?;
        let mut after = After::default();
        let applied = lock(&self.0.core).apply_fault(&self.0, fault, &mut after);
        self.0.settle(after);
        applied
    }

    /// Applies `fault` at world time `at`, at or after now. What does not
    /// depend on state is checked at once; a fault whose state check fails
    /// at `at` is skipped, and a `fault` record says so (clause 3.3.8).
    ///
    /// # Errors
    ///
    /// [`WorldError::Invalid`] for an `at` before now or above 2^63 - 1 ns,
    /// or a value out of range, [`WorldError::Unknown`] for a host the world
    /// does not have, [`WorldError::Limit`] when 1,048,576 faults are
    /// scheduled, [`WorldError::Busy`] inside an observer, and
    /// [`WorldError::Ended`] after the run.
    pub fn schedule(&self, at: Duration, fault: Fault) -> Result<(), WorldError> {
        self.0.check()?;
        let now = self.0.now_nanos();
        lock(&self.0.core).schedule_fault(now, at, fault)
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

    /// A new, empty history (clause 3.6.2).
    pub fn history<Op, Ret>(&self) -> History<Op, Ret>
    where
        Op: Clone + Send + 'static,
        Ret: Clone + Send + 'static,
    {
        History::new(Arc::clone(&self.0))
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
        // The waker is cloned, and what is left of it dropped, with no lock
        // held (module `core`).
        let waker = cx.waker().clone();
        let mut core = lock(&shared.core);
        let waker = match self.timer {
            Some(id) => core.touch_timer(id, waker),
            None => Err(waker),
        };
        let left = match waker {
            Ok(left) => Some(left),
            Err(waker) => {
                self.timer = Some(core.add_timer(Some(self.deadline), waker, None));
                None
            }
        };
        drop(core);
        drop(left);
        Poll::Pending
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let Some(id) = self.timer {
            let waker = lock(&self.shared.core).remove_timer(id);
            drop(waker);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::task::Waker;

    use super::*;

    #[test]
    fn a_dropped_sleep_removes_its_timer() {
        let shared = Arc::new(Shared::new(1, 0));
        let world = World::new(Arc::clone(&shared));
        let mut sleep = world.sleep(Duration::from_nanos(10));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(sleep.as_mut().poll(&mut cx).is_pending());
        assert!(sleep.as_mut().poll(&mut cx).is_pending());
        assert_eq!(shared.next_event(), Some(10));
        drop(sleep);
        assert_eq!(shared.next_event(), None);
    }

    #[test]
    fn sleeps_past_the_end_of_time_hold_no_timer() {
        let shared = Arc::new(Shared::new(1, 0));
        let world = World::new(Arc::clone(&shared));
        let mut cx = Context::from_waker(Waker::noop());
        let mut never = world.sleep_until(Duration::from_nanos(MAX_NANOS + 1));
        assert!(never.as_mut().poll(&mut cx).is_pending());
        let mut last = world.sleep_until(Duration::from_nanos(MAX_NANOS));
        assert!(last.as_mut().poll(&mut cx).is_pending());
        assert_eq!(shared.next_event(), Some(MAX_NANOS));
        let mut now = world.sleep(Duration::ZERO);
        assert!(now.as_mut().poll(&mut cx).is_ready());
    }

    #[test]
    fn calls_from_a_hook_are_busy_and_after_the_end_ended() {
        let shared = Arc::new(Shared::new(1, 0));
        let world = World::new(Arc::clone(&shared));
        let other = World::new(Arc::new(Shared::new(2, 0)));
        let first = world.random_u64();
        let inside = hook::enter();
        assert_eq!(world.note("x"), Err(WorldError::Busy));
        assert_eq!(other.note("x"), Err(WorldError::Busy));
        assert_eq!(world.random_u64(), 0);
        assert_eq!(other.random_u64(), 0);
        assert_eq!(world.host_state(HostId(0)), Err(WorldError::Busy));
        assert!(world.history::<u8, u8>().entries().is_empty());
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            world
                .sleep(Duration::ZERO)
                .as_mut()
                .poll(&mut cx)
                .is_pending()
        );
        drop(inside);
        let mut expected = Stream::new(1, b"scenario", &[]);
        assert_eq!(first, expected.next_u64());
        assert_eq!(world.random_u64(), expected.next_u64());
        shared.end();
        assert_eq!(world.note("x"), Err(WorldError::Ended));
        assert_eq!(format!("{world:?}"), "World { now: 0ns, hosts: 0 }");
    }
}
