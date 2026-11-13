//! The state of one run that world handles, `mem` instances, and the run
//! loop share: world time, the executor, the hosts, timers, and pending
//! events.
//!
//! Locks, in the only order they are taken: `trace`, then `core`, then the
//! leaves (`pending`, `scenario`, the executor's ready queue, and each
//! joint). `core` is never held while task code, a boot closure, or a
//! destructor of a task runs: such work is collected in [`After`] and done
//! once `core` is released. The same holds for every waker that task code
//! passed in: no simulator lock is held while one is woken, cloned, or
//! dropped, since a waker may call a seam or a world method. Only the
//! executor's own task wakers, which take nothing but the ready queue, are
//! made and dropped under `core`. Records are queued in `pending` while `core` is
//! held, in the order the simulator processes their events, and written to
//! the trace by [`Shared::flush`] after `core` is released. Observers run
//! under `trace` only, so a `mem` instance an observer calls cannot deadlock
//! with the call that wrote the record.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use alloy_rt::{BoxFuture, JoinError, Runtime};
use alloy_seam::Detail;

use crate::exec::{Executor, Joint, Owner, Polled, TaskKey, drop_quietly, lock, poll_caught};
use crate::fault::Fault;
use crate::hook;
use crate::host::{Boot, Host, HostBoot};
use crate::stream::Stream;
use crate::trace::{KIND_TASK_END, KIND_TASK_POLL, KIND_TIMER_FIRE, Trace};
use crate::world::{HostId, WorldError};

/// The latest world time, 2^63 - 1 nanoseconds (section 7).
pub(crate) const MAX_NANOS: u64 = (1 << 63) - 1;

/// A record or a cause of `fail`, waiting to reach the trace.
enum Pending {
    Record {
        time: u64,
        host: Option<HostId>,
        kind: u16,
        body: Vec<u8>,
    },
    Fail(Detail),
}

/// How a task ended, with its `task-end` outcome byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    Completed = 0,
    Cancelled = 1,
    Panicked = 2,
    Crashed = 3,
}

/// A timer of a host's clock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HostTimer {
    pub(crate) host: HostId,
    pub(crate) start: u32,
    /// The deadline on the host's clock.
    pub(crate) deadline: u64,
    /// The task that last polled the sleep.
    pub(crate) task: Option<TaskKey>,
}

/// A pending timer: of the world when `host` is `None`.
#[derive(Debug)]
pub(crate) struct Timer {
    /// Its event, or `None` when the deadline is never reached.
    at: Option<(u64, u64)>,
    waker: Waker,
    host: Option<HostTimer>,
}

/// Something due at a world instant.
#[derive(Debug)]
pub(crate) enum Event {
    /// The timer of that id fires.
    Timer(u64),
    /// A scheduled fault applies.
    Fault(Fault),
    /// The host's stall may have ended.
    StallEnd(HostId),
}

/// A host start whose `boot` closure is still to be called.
pub(crate) struct BootJob {
    pub(crate) key: TaskKey,
    pub(crate) boot: Boot,
    pub(crate) arg: HostBoot,
}

/// Work to do once `core` is released, in field order.
#[derive(Default)]
pub(crate) struct After {
    /// Wakers of fired timers, in firing order.
    pub(crate) wakes: Vec<Waker>,
    pub(crate) drops: Vec<BoxFuture<'static, ()>>,
    pub(crate) runtimes: Vec<Runtime>,
    pub(crate) joints: Vec<(Arc<Joint>, Result<(), JoinError>)>,
    pub(crate) boots: Vec<BootJob>,
}

/// The world's state behind the `core` lock.
pub(crate) struct Core {
    pub(crate) exec: Executor,
    pub(crate) hosts: Vec<Host>,
    timers: BTreeMap<u64, Timer>,
    events: BTreeMap<(u64, u64), Event>,
    /// The next timer id and event sequence number.
    next_seq: u64,
    /// Scheduled faults that have not applied yet.
    pub(crate) scheduled: usize,
    /// The task being polled.
    pub(crate) polling: Option<TaskKey>,
    /// Polls started in the run.
    pub(crate) polls: u64,
}

/// The state of one run that world handles share.
pub(crate) struct Shared {
    seed: u64,
    /// `RunConfig::wall_start` in nanoseconds.
    wall_start: i64,
    /// True once the run has ended or is ending.
    ended: AtomicBool,
    now: AtomicU64,
    scenario: Mutex<Stream>,
    pub(crate) core: Mutex<Core>,
    pending: Mutex<VecDeque<Pending>>,
    pub(crate) trace: Mutex<Trace>,
}

impl Shared {
    /// The state of a new run of `seed`, at world time 0, with the trace
    /// header written.
    pub(crate) fn new(seed: u64, wall_start: i64) -> Shared {
        Shared {
            seed,
            wall_start,
            ended: AtomicBool::new(false),
            now: AtomicU64::new(0),
            scenario: Mutex::new(Stream::new(seed, b"scenario", &[])),
            core: Mutex::new(Core {
                exec: Executor::new(),
                hosts: Vec::new(),
                timers: BTreeMap::new(),
                events: BTreeMap::new(),
                next_seq: 0,
                scheduled: 0,
                polling: None,
                polls: 0,
            }),
            pending: Mutex::new(VecDeque::new()),
            trace: Mutex::new(Trace::new(seed)),
        }
    }

    pub(crate) fn seed(&self) -> u64 {
        self.seed
    }

    pub(crate) fn wall_start(&self) -> i64 {
        self.wall_start
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

    /// The next value of the scenario's random stream.
    pub(crate) fn scenario_u64(&self) -> u64 {
        lock(&self.scenario).next_u64()
    }

    /// `Busy` inside a hook of any world, `Ended` after the run.
    pub(crate) fn check(&self) -> Result<(), WorldError> {
        if hook::inside() {
            Err(WorldError::Busy)
        } else if self.has_ended() {
            Err(WorldError::Ended)
        } else {
            Ok(())
        }
    }

    /// Queues one record at the current world time.
    pub(crate) fn push(&self, host: Option<HostId>, kind: u16, body: &[u8]) {
        lock(&self.pending).push_back(Pending::Record {
            time: self.now_nanos(),
            host,
            kind,
            body: body.to_vec(),
        });
    }

    /// Queues a cause of `fail`.
    pub(crate) fn fail(&self, cause: Detail) {
        lock(&self.pending).push_back(Pending::Fail(cause));
    }

    /// Writes every queued record to the trace, in order. Does nothing
    /// inside a hook: the flush that runs the hook writes what the hook
    /// queued. An observer panic ends the run.
    pub(crate) fn flush(&self) {
        if hook::inside() {
            return;
        }
        let mut trace = lock(&self.trace);
        loop {
            let next = lock(&self.pending).pop_front();
            match next {
                None => return,
                Some(Pending::Fail(cause)) => trace.outcome.fail(cause),
                Some(Pending::Record {
                    time,
                    host,
                    kind,
                    body,
                }) => {
                    if trace.write(time, host, kind, &body, &[]) {
                        self.end();
                    }
                }
            }
        }
    }

    /// Does the work `after` holds, then flushes. Called with no lock held.
    pub(crate) fn settle(self: &Arc<Self>, after: After) {
        let After {
            wakes,
            drops,
            runtimes,
            joints,
            boots,
        } = after;
        for waker in wakes {
            waker.wake();
        }
        for future in drops {
            drop_quietly(future);
        }
        drop_quietly(runtimes);
        for (joint, result) in joints {
            joint.finish(result);
        }
        for job in boots {
            let boot = job.boot;
            let arg = job.arg;
            // A boot closure that panics gives the boot task a future that
            // panics on its first poll, so the panic ends that task as any
            // task panic does (section 6).
            let future: BoxFuture<'static, ()> =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| boot(arg))) {
                    Ok(future) => future,
                    Err(payload) => Box::pin(async move { std::panic::resume_unwind(payload) }),
                };
            let refused = lock(&self.core).exec.install(job.key, future);
            if let Err(future) = refused {
                drop_quietly(future);
            }
            drop_quietly(boot);
        }
        self.flush();
    }

    /// Polls one runnable task, picked with `schedule`, and returns it with
    /// what the poll did. `None` when no task is runnable.
    pub(crate) fn poll_one(self: &Arc<Self>, schedule: &mut Stream) -> Option<(TaskKey, Polled)> {
        let (key, owner, number, mut future, waker) = {
            let mut core = lock(&self.core);
            core.route_woken(self.now_nanos());
            let key = core.exec.pick(schedule)?;
            let owner = core.exec.task(key)?.owner;
            let (future, waker) = core.exec.take(key)?;
            core.polls += 1;
            let number = core.polls;
            core.polling = Some(key);
            if let Owner::Host { host, id } = owner {
                self.push(Some(host), KIND_TASK_POLL, &id.0.to_le_bytes());
            }
            (key, owner, number, future, waker)
        };
        self.flush();
        let result = poll_caught(&mut future, &waker);
        drop(waker);
        let mut after = After::default();
        let polled = {
            let mut core = lock(&self.core);
            core.polling = None;
            core.end_poll(self, key, owner, number, future, result, &mut after)
        };
        self.settle(after);
        Some((key, polled))
    }

    /// The world time of the earliest pending event.
    pub(crate) fn next_event(&self) -> Option<u64> {
        lock(&self.core)
            .events
            .first_key_value()
            .map(|(at, _)| at.0)
    }

    /// Processes every event due at or before now, in an order drawn from
    /// `schedule` (clause 3.2.2).
    pub(crate) fn process_due(self: &Arc<Self>, schedule: &mut Stream) {
        let mut due = {
            let mut core = lock(&self.core);
            let later = core
                .events
                .split_off(&(self.now_nanos().saturating_add(1), 0));
            std::mem::replace(&mut core.events, later)
                .into_values()
                .collect::<Vec<Event>>()
        };
        schedule.shuffle(&mut due);
        for event in due {
            let mut after = After::default();
            lock(&self.core).process(self, event, &mut after);
            self.settle(after);
        }
    }

    /// Ends every task and drops every object of the world (clause 3.2.7
    /// step 5). Called once the trace is closed.
    pub(crate) fn clear(&self) {
        let (tasks, hosts, timers, events) = {
            let mut core = lock(&self.core);
            let tasks = core.exec.clear();
            let hosts = std::mem::take(&mut core.hosts);
            let timers = std::mem::take(&mut core.timers);
            let events = std::mem::take(&mut core.events);
            (tasks, hosts, timers, events)
        };
        for task in tasks {
            let (future, _, joint) = task.into_parts();
            drop_quietly(future);
            if let Some(joint) = joint {
                joint.finish(Err(JoinError::Cancelled));
            }
        }
        drop_quietly((hosts, timers, events));
        lock(&self.pending).clear();
    }
}

impl Core {
    /// The host `id`.
    ///
    /// # Errors
    ///
    /// `Unknown` when the world has no such host.
    pub(crate) fn host(&self, id: HostId) -> Result<&Host, WorldError> {
        self.hosts
            .get(id.0 as usize)
            .ok_or_else(|| WorldError::Unknown(Detail::new(&format!("host {}", id.0))))
    }

    pub(crate) fn host_mut(&mut self, id: HostId) -> Result<&mut Host, WorldError> {
        self.hosts
            .get_mut(id.0 as usize)
            .ok_or_else(|| WorldError::Unknown(Detail::new(&format!("host {}", id.0))))
    }

    /// Makes every woken task runnable, or parks it while its host stalls.
    fn route_woken(&mut self, now: u64) {
        for key in self.exec.woken() {
            let stalled = match self.exec.task(key).map(|task| task.owner) {
                Some(Owner::Host { host, .. }) => self
                    .hosts
                    .get_mut(host.0 as usize)
                    .filter(|h| h.stalled(now)),
                _ => None,
            };
            match stalled {
                Some(host) => host.parked.push(key),
                None => self.exec.push_runnable(key),
            }
        }
    }

    /// Handles the end of the poll number `number` of task `key`.
    #[expect(
        clippy::too_many_arguments,
        reason = "the parts of one poll, taken apart so that no lock is held while it runs"
    )]
    fn end_poll(
        &mut self,
        shared: &Arc<Shared>,
        key: TaskKey,
        owner: Owner,
        number: u64,
        future: BoxFuture<'static, ()>,
        result: Result<Poll<()>, String>,
        after: &mut After,
    ) -> Polled {
        let polled = match result {
            Ok(Poll::Pending) => {
                if let Some(future) = self.exec.put_back(key, future) {
                    after.drops.push(future);
                    self.end_task(shared, key, End::Cancelled, after);
                }
                Polled::Pending
            }
            Ok(Poll::Ready(())) => {
                after.drops.push(future);
                self.end_task(shared, key, End::Completed, after);
                Polled::Completed
            }
            Err(text) => {
                after.drops.push(future);
                self.end_task(shared, key, End::Panicked, after);
                if let Owner::Host { host, id } = owner {
                    shared.fail(Detail::new(&format!(
                        "task {} of host {} panicked: {text}",
                        id.0, host.0
                    )));
                }
                Polled::Panicked(text)
            }
        };
        if let Owner::Host { host, .. } = owner {
            self.after_host_poll(shared, host, number, after);
        }
        polled
    }

    /// Ends the task `key`: writes its `task-end` record, and leaves its
    /// future to drop and its joint to finish in `after`.
    pub(crate) fn end_task(&mut self, shared: &Shared, key: TaskKey, end: End, after: &mut After) {
        let Some(task) = self.exec.remove(key) else {
            return;
        };
        let (future, owner, joint) = task.into_parts();
        if let Some(future) = future {
            after.drops.push(future);
        }
        if let Owner::Host { host, id } = owner {
            let mut body = id.0.to_le_bytes().to_vec();
            body.push(end as u8);
            shared.push(Some(host), KIND_TASK_END, &body);
        }
        if let Some(joint) = joint {
            let result = match end {
                End::Completed => Ok(()),
                End::Cancelled | End::Crashed => Err(JoinError::Cancelled),
                End::Panicked => Err(JoinError::Panicked),
            };
            after.joints.push((joint, result));
        }
    }

    /// Cancels the task `key` (clause 3.1.6). A task being polled is ended
    /// when its poll returns, unless the poll completes it.
    pub(crate) fn cancel(&mut self, shared: &Shared, key: TaskKey, after: &mut After) {
        let polled = self.polling == Some(key);
        match self.exec.task_mut(key) {
            Some(task) if polled => task.cancelled = true,
            Some(_) => self.end_task(shared, key, End::Cancelled, after),
            None => {}
        }
    }

    /// Adds a timer that fires at `at`, or never for `None`, and returns its
    /// id.
    pub(crate) fn add_timer(
        &mut self,
        at: Option<u64>,
        waker: Waker,
        host: Option<HostTimer>,
    ) -> u64 {
        let id = self.next_seq;
        self.next_seq += 1;
        let at = at.map(|time| (time, id));
        if let Some(key) = at {
            self.events.insert(key, Event::Timer(id));
        }
        self.timers.insert(id, Timer { at, waker, host });
        id
    }

    /// Refreshes the waker of timer `id` with `waker`, a clone made before
    /// `core` was taken, and the task of a host timer. `Ok` holds the waker
    /// left over, the old one or `waker` itself, to drop once no lock is
    /// held. `Err` gives `waker` back when the timer has fired or been
    /// removed.
    pub(crate) fn touch_timer(&mut self, id: u64, waker: Waker) -> Result<Waker, Waker> {
        let polling = self.polling;
        match self.timers.get_mut(&id) {
            Some(timer) => {
                if let Some(host) = timer.host.as_mut() {
                    host.task = polling;
                }
                if timer.waker.will_wake(&waker) {
                    Ok(waker)
                } else {
                    Ok(std::mem::replace(&mut timer.waker, waker))
                }
            }
            None => Err(waker),
        }
    }

    /// Removes timer `id`. Its waker is returned to drop once no lock is
    /// held.
    pub(crate) fn remove_timer(&mut self, id: u64) -> Option<Waker> {
        let timer = self.timers.remove(&id)?;
        if let Some(at) = timer.at {
            self.events.remove(&at);
        }
        Some(timer.waker)
    }

    /// Moves every timer of `host` to the world time its deadline is now
    /// reached at, after a change of the host's clock rate.
    pub(crate) fn retime(&mut self, host: HostId) {
        let Ok(owner) = self.host(host) else {
            return;
        };
        let clock = lock(&owner.clock);
        let mut moves = Vec::new();
        for (id, timer) in &self.timers {
            if let Some(timer_host) = timer.host.filter(|t| t.host == host) {
                let fire = clock.fire_at(timer_host.start, timer_host.deadline);
                moves.push((*id, timer.at, fire.map(|time| (time, *id))));
            }
        }
        drop(clock);
        for (id, old, new) in moves {
            if let Some(old) = old {
                self.events.remove(&old);
            }
            if let Some(new) = new {
                self.events.insert(new, Event::Timer(id));
            }
            if let Some(timer) = self.timers.get_mut(&id) {
                timer.at = new;
            }
        }
    }

    /// Adds an event at `at` and returns its key.
    pub(crate) fn add_event(&mut self, at: u64, event: Event) -> (u64, u64) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.events.insert((at, seq), event);
        (at, seq)
    }

    /// Removes the event of key `key`, if it is still pending.
    pub(crate) fn remove_event(&mut self, key: (u64, u64)) {
        self.events.remove(&key);
    }

    /// Processes one due event.
    fn process(&mut self, shared: &Arc<Shared>, event: Event, after: &mut After) {
        match event {
            Event::Timer(id) => {
                let Some(timer) = self.timers.remove(&id) else {
                    return;
                };
                if let Some(fired) = timer.host {
                    let woken = fired
                        .task
                        .and_then(|key| self.exec.task(key))
                        .and_then(|task| match task.owner {
                            Owner::Host { host, id } if host == fired.host => Some(id.0),
                            _ => None,
                        })
                        .unwrap_or(0);
                    let mut body = woken.to_le_bytes().to_vec();
                    body.extend_from_slice(&fired.deadline.to_le_bytes());
                    shared.push(Some(fired.host), KIND_TIMER_FIRE, &body);
                }
                after.wakes.push(timer.waker);
            }
            Event::Fault(fault) => {
                self.scheduled = self.scheduled.saturating_sub(1);
                self.apply_scheduled(shared, fault, after);
            }
            Event::StallEnd(host) => {
                let now = shared.now_nanos();
                if let Some(owner) = self.hosts.get_mut(host.0 as usize)
                    && !owner.stalled(now)
                {
                    owner.stall_event = None;
                    for key in std::mem::take(&mut owner.parked) {
                        self.exec.push_runnable(key);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_reach_the_trace_in_queue_order_and_not_from_a_hook() {
        let shared = Shared::new(1, 0);
        shared.push(None, 0x00F0, b"\x01\x00a");
        {
            let _inside = hook::enter();
            shared.flush();
            assert_eq!(lock(&shared.pending).len(), 1);
        }
        shared.fail(Detail::new("first"));
        shared.flush();
        assert!(lock(&shared.pending).is_empty());
        let closed = lock(&shared.trace).finish(0);
        assert_eq!(closed.events, 2);
        assert_eq!(closed.failure, Some(Detail::new("first")));
    }

    #[test]
    fn timers_fire_by_time_and_a_removed_timer_leaves_no_event() {
        let shared = Arc::new(Shared::new(1, 0));
        let (first, second) = {
            let mut core = lock(&shared.core);
            let first = core.add_timer(Some(9), Waker::noop().clone(), None);
            let second = core.add_timer(Some(5), Waker::noop().clone(), None);
            core.add_timer(None, Waker::noop().clone(), None);
            (first, second)
        };
        assert_eq!(shared.next_event(), Some(5));
        assert!(lock(&shared.core).remove_timer(second).is_some());
        assert_eq!(shared.next_event(), Some(9));
        assert!(
            lock(&shared.core)
                .touch_timer(first, Waker::noop().clone())
                .is_ok()
        );
        shared.set_now(9);
        shared.process_due(&mut Stream::new(1, b"t", &[]));
        assert_eq!(shared.next_event(), None);
        assert!(
            lock(&shared.core)
                .touch_timer(first, Waker::noop().clone())
                .is_err()
        );
    }
}
