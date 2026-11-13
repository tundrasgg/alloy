//! The single-thread executor: a task slab, wakers, and the seeded pick of
//! the next runnable task (ARFC 0005 clause 3.2.2).
//!
//! The executor owns every task future of a run. The run loop takes a
//! task's future out of its slot to poll it, so that no lock is held while
//! task code runs and a task may spawn, cancel, or crash tasks during its own
//! poll. A waker may be called from any thread: it only appends its task to a
//! shared ready queue. A task is in the queue, the runnable set, or a stalled
//! host's parked list at most once, so all three are bounded by the number
//! of live tasks.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll, Wake, Waker};

use alloy_rt::{BoxFuture, JoinError, TaskId};

use crate::stream::Stream;
use crate::world::HostId;

/// Locks `mutex`, also when a panic poisoned it. The simulator's state stays
/// consistent across a caught panic, because no user code runs while one of
/// its locks is held except observers, whose panics are caught inside the
/// lock.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The text of a panic payload: its message when it is a string, otherwise a
/// fixed text.
pub(crate) fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic with a non-string payload".to_owned()
    }
}

/// Drops `value`, catching a panic in its destructor. Used where the run has
/// already decided what the drop means and a destructor can no longer change
/// it.
pub(crate) fn drop_quietly<T>(value: T) {
    // A destructor that panics has nothing left to report to: the panic hook
    // has printed it, and the value is gone either way.
    let _ = catch_unwind(AssertUnwindSafe(move || drop(value)));
}

/// Polls `future` once with `waker`, catching a panic.
pub(crate) fn poll_caught(
    future: &mut BoxFuture<'static, ()>,
    waker: &Waker,
) -> Result<Poll<()>, String> {
    let mut cx = Context::from_waker(waker);
    catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut cx)))
        .map_err(|payload| panic_text(payload.as_ref()))
}

/// Identifies one task of an executor. The generation tells a task apart
/// from a later task in the same slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TaskKey {
    slot: usize,
    generation: u64,
}

/// Whose task it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// The control task, which belongs to no host and writes no records.
    Control,
    /// A task of `host`, numbered `id` on it.
    Host { host: HostId, id: TaskId },
}

/// What one poll of a task did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Polled {
    /// The task is still running.
    Pending,
    /// The task's future completed.
    Completed,
    /// The task's future panicked with this message.
    Panicked(String),
}

/// The end of a task that a handle can join, and the live count of the
/// `spawn` instance that started it.
#[derive(Debug)]
pub(crate) struct Joint {
    state: Mutex<JoinState>,
    live: Arc<AtomicUsize>,
}

/// How a joined task ended, and the waker of a join waiting for it.
#[derive(Debug, Default)]
struct JoinState {
    end: Option<Result<(), JoinError>>,
    waker: Option<Waker>,
}

impl Joint {
    /// A joint counted in `live`, which the caller has already raised.
    pub(crate) fn new(live: Arc<AtomicUsize>) -> Joint {
        Joint {
            state: Mutex::default(),
            live,
        }
    }

    /// Records how the task ended, wakes a waiting join, and frees the task's
    /// place in its instance. Called after the task's future was dropped.
    pub(crate) fn finish(&self, result: Result<(), JoinError>) {
        let waker = {
            let mut state = lock(&self.state);
            if state.end.is_some() {
                return;
            }
            state.end = Some(result);
            state.waker.take()
        };
        self.live.fetch_sub(1, Ordering::AcqRel);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// How the task ended, or `None` with `waker` stored while it runs.
    /// The waker is cloned, and the one it replaces dropped, with no lock
    /// held, since a waker may call back into the simulator.
    pub(crate) fn poll_end(&self, waker: &Waker) -> Option<Result<(), JoinError>> {
        let stored = lock(&self.state)
            .waker
            .as_ref()
            .map(|old| old.will_wake(waker));
        if stored == Some(true) {
            return lock(&self.state).end.clone();
        }
        let waker = waker.clone();
        let (end, left) = {
            let mut state = lock(&self.state);
            match state.end.clone() {
                Some(end) => (Some(end), Some(waker)),
                None => (None, state.waker.replace(waker)),
            }
        };
        drop(left);
        end
    }
}

/// Tasks woken since the executor last drained the queue, in wake order.
type ReadyQueue = Mutex<Vec<TaskKey>>;

/// The waker of one task.
#[derive(Debug)]
struct TaskWaker {
    key: TaskKey,
    queue: Weak<ReadyQueue>,
    /// True while the task is in the ready queue, the runnable set, or a
    /// parked list, and for ever once the task has ended, so a wake then
    /// does nothing.
    queued: AtomicBool,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if !self.queued.swap(true, Ordering::AcqRel)
            && let Some(queue) = self.queue.upgrade()
        {
            lock(&queue).push(self.key);
        }
    }
}

/// One live task.
pub(crate) struct Task {
    /// `None` while the task is being polled, and before the future of a
    /// host's `boot` task is installed.
    future: Option<BoxFuture<'static, ()>>,
    waker: Arc<TaskWaker>,
    pub(crate) owner: Owner,
    /// Set by a cancel that came during the task's poll.
    pub(crate) cancelled: bool,
    pub(crate) joint: Option<Arc<Joint>>,
}

impl Task {
    /// The task's future, when it is in its slot, its owner, and its joint.
    pub(crate) fn into_parts(self) -> (Option<BoxFuture<'static, ()>>, Owner, Option<Arc<Joint>>) {
        (self.future, self.owner, self.joint)
    }
}

struct Slot {
    generation: u64,
    task: Option<Task>,
}

/// The task slab and the runnable set of one run.
pub(crate) struct Executor {
    slots: Vec<Slot>,
    /// Empty slots, reused last in first out.
    free: Vec<usize>,
    /// Tasks that may be polled, in the order they became runnable. A key
    /// whose task has ended since is skipped when picked.
    runnable: Vec<TaskKey>,
    queue: Arc<ReadyQueue>,
}

impl Executor {
    pub(crate) fn new() -> Executor {
        Executor {
            slots: Vec::new(),
            free: Vec::new(),
            runnable: Vec::new(),
            queue: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Adds a task. A task with a future is woken at once; one without
    /// waits for [`Executor::install`]. It is not polled here.
    pub(crate) fn insert(
        &mut self,
        owner: Owner,
        future: Option<BoxFuture<'static, ()>>,
        joint: Option<Arc<Joint>>,
    ) -> TaskKey {
        let slot = match self.free.pop() {
            Some(slot) => slot,
            None => {
                self.slots.push(Slot {
                    generation: 0,
                    task: None,
                });
                self.slots.len() - 1
            }
        };
        let entry = &mut self.slots[slot];
        let key = TaskKey {
            slot,
            generation: entry.generation,
        };
        let waker = Arc::new(TaskWaker {
            key,
            queue: Arc::downgrade(&self.queue),
            queued: AtomicBool::new(false),
        });
        let woken = future.is_some();
        entry.task = Some(Task {
            future,
            waker: Arc::clone(&waker),
            owner,
            cancelled: false,
            joint,
        });
        if woken {
            waker.wake_by_ref();
        }
        key
    }

    /// Gives the task `key` its future and wakes it. Returns the future when
    /// the task has ended or already has one.
    pub(crate) fn install(
        &mut self,
        key: TaskKey,
        future: BoxFuture<'static, ()>,
    ) -> Result<(), BoxFuture<'static, ()>> {
        match self.task_mut(key) {
            Some(task) if task.future.is_none() => {
                task.future = Some(future);
                Arc::clone(&task.waker).wake_by_ref();
                Ok(())
            }
            _ => Err(future),
        }
    }

    /// The live tasks woken since the last call, in wake order.
    pub(crate) fn woken(&mut self) -> Vec<TaskKey> {
        let woken = std::mem::take(&mut *lock(&self.queue));
        woken
            .into_iter()
            .filter(|key| self.task(*key).is_some())
            .collect()
    }

    /// Makes a woken task runnable.
    pub(crate) fn push_runnable(&mut self, key: TaskKey) {
        self.runnable.push(key);
    }

    /// Removes from the runnable set, in order, every task `park` accepts,
    /// and returns them.
    pub(crate) fn take_runnable(&mut self, mut park: impl FnMut(&Task) -> bool) -> Vec<TaskKey> {
        let mut taken = Vec::new();
        let mut kept = Vec::with_capacity(self.runnable.len());
        for key in std::mem::take(&mut self.runnable) {
            match self.task(key) {
                Some(task) if park(task) => taken.push(key),
                Some(_) => kept.push(key),
                None => {}
            }
        }
        self.runnable = kept;
        taken
    }

    /// Takes the next task to poll: one of the runnable tasks, picked with
    /// `stream`. `None` when no task is runnable.
    pub(crate) fn pick(&mut self, stream: &mut Stream) -> Option<TaskKey> {
        while !self.runnable.is_empty() {
            let index = stream.below(self.runnable.len());
            let key = self.runnable.swap_remove(index);
            if self.task(key).is_some_and(|task| task.future.is_some()) {
                return Some(key);
            }
        }
        None
    }

    pub(crate) fn task(&self, key: TaskKey) -> Option<&Task> {
        self.slots
            .get(key.slot)
            .filter(|slot| slot.generation == key.generation)
            .and_then(|slot| slot.task.as_ref())
    }

    pub(crate) fn task_mut(&mut self, key: TaskKey) -> Option<&mut Task> {
        self.slots
            .get_mut(key.slot)
            .filter(|slot| slot.generation == key.generation)
            .and_then(|slot| slot.task.as_mut())
    }

    /// Takes the future of the task `key` out to poll it, with its waker.
    /// A wake from now on queues the task again.
    pub(crate) fn take(&mut self, key: TaskKey) -> Option<(BoxFuture<'static, ()>, Waker)> {
        let task = self.task_mut(key)?;
        let future = task.future.take()?;
        task.waker.queued.store(false, Ordering::Release);
        Some((future, Waker::from(Arc::clone(&task.waker))))
    }

    /// Puts a polled, pending future back. Returns it when the task has
    /// ended or was cancelled during the poll.
    pub(crate) fn put_back(
        &mut self,
        key: TaskKey,
        future: BoxFuture<'static, ()>,
    ) -> Option<BoxFuture<'static, ()>> {
        match self.task_mut(key) {
            Some(task) if !task.cancelled && task.future.is_none() => {
                task.future = Some(future);
                None
            }
            _ => Some(future),
        }
    }

    /// Ends the task `key`, if it has not ended, and returns it. Its future
    /// is the caller's to drop.
    pub(crate) fn remove(&mut self, key: TaskKey) -> Option<Task> {
        let slot = self
            .slots
            .get_mut(key.slot)
            .filter(|slot| slot.generation == key.generation)?;
        let task = slot.task.take()?;
        slot.generation += 1;
        self.free.push(key.slot);
        task.waker.queued.store(true, Ordering::Release);
        Some(task)
    }

    /// Every live task, in slot order.
    pub(crate) fn keys(&self) -> Vec<TaskKey> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.task.is_some())
            .map(|(slot, entry)| TaskKey {
                slot,
                generation: entry.generation,
            })
            .collect()
    }

    /// Ends every task and returns them, in slot order.
    pub(crate) fn clear(&mut self) -> Vec<Task> {
        let tasks = self
            .keys()
            .into_iter()
            .filter_map(|key| self.remove(key))
            .collect();
        self.runnable.clear();
        lock(&self.queue).clear();
        tasks
    }
}

#[cfg(test)]
mod tests {
    use std::future::poll_fn;

    use super::*;

    /// A task that records its name each time it is polled and is pending
    /// for `polls` polls, waking itself each time.
    fn counter(log: &Arc<Mutex<Vec<u32>>>, name: u32, polls: u32) -> BoxFuture<'static, ()> {
        let log = Arc::clone(log);
        let mut left = polls;
        Box::pin(poll_fn(move |cx| {
            lock(&log).push(name);
            if left == 0 {
                Poll::Ready(())
            } else {
                left -= 1;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }))
    }

    fn spawn(exec: &mut Executor, future: BoxFuture<'static, ()>) -> TaskKey {
        exec.insert(Owner::Control, Some(future), None)
    }

    /// Polls `key` once the way the run loop does, removing it when it ends.
    fn poll(exec: &mut Executor, key: TaskKey) -> Polled {
        let Some((mut future, waker)) = exec.take(key) else {
            return Polled::Completed;
        };
        let polled = match poll_caught(&mut future, &waker) {
            Ok(Poll::Pending) => {
                if let Some(future) = exec.put_back(key, future) {
                    drop(future);
                }
                return Polled::Pending;
            }
            Ok(Poll::Ready(())) => Polled::Completed,
            Err(text) => Polled::Panicked(text),
        };
        exec.remove(key);
        polled
    }

    fn next(exec: &mut Executor, stream: &mut Stream) -> Option<TaskKey> {
        for key in exec.woken() {
            exec.push_runnable(key);
        }
        exec.pick(stream)
    }

    fn drive(exec: &mut Executor, stream: &mut Stream) -> usize {
        let mut polls = 0;
        while let Some(key) = next(exec, stream) {
            poll(exec, key);
            polls += 1;
        }
        polls
    }

    #[test]
    fn every_task_runs_to_completion() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut exec = Executor::new();
        for name in 0..5 {
            spawn(&mut exec, counter(&log, name, name));
        }
        assert_eq!(exec.keys().len(), 5);
        let polls = drive(&mut exec, &mut Stream::new(1, b"t", &[]));
        assert_eq!(polls, 15);
        assert!(exec.keys().is_empty());
        let mut seen = lock(&log).clone();
        seen.sort_unstable();
        assert_eq!(seen, [0, 1, 1, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 4]);
    }

    #[test]
    fn the_pick_order_follows_the_seed() {
        let order = |seed: u64| {
            let log = Arc::new(Mutex::new(Vec::new()));
            let mut exec = Executor::new();
            for name in 0..8 {
                spawn(&mut exec, counter(&log, name, 3));
            }
            drive(&mut exec, &mut Stream::new(seed, b"t", &[]));
            lock(&log).clone()
        };
        assert_eq!(order(1), order(1));
        assert_ne!(order(1), order(2));
    }

    #[test]
    fn a_pending_task_without_a_wake_is_not_runnable() {
        let mut exec = Executor::new();
        let stash: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let held = Arc::clone(&stash);
        let mut polled = 0;
        spawn(
            &mut exec,
            Box::pin(poll_fn(move |cx| {
                polled += 1;
                if polled == 2 {
                    return Poll::Ready(());
                }
                *lock(&held) = Some(cx.waker().clone());
                Poll::Pending
            })),
        );
        let mut stream = Stream::new(1, b"t", &[]);
        assert_eq!(drive(&mut exec, &mut stream), 1);
        assert!(next(&mut exec, &mut stream).is_none());
        let waker = lock(&stash).take();
        let waker = waker.expect("the task stored its waker");
        // Waking twice queues the task once.
        waker.wake_by_ref();
        waker.wake_by_ref();
        assert_eq!(drive(&mut exec, &mut stream), 1);
        assert!(exec.keys().is_empty());
        // A wake after the task ended does nothing.
        waker.wake();
        assert!(next(&mut exec, &mut stream).is_none());
    }

    #[test]
    fn a_panicking_task_ends_and_others_continue() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut exec = Executor::new();
        let bad = spawn(
            &mut exec,
            Box::pin(poll_fn(|_| -> Poll<()> { panic!("boom") })),
        );
        spawn(&mut exec, counter(&log, 7, 2));
        let mut stream = Stream::new(1, b"t", &[]);
        let mut panics = Vec::new();
        while let Some(key) = next(&mut exec, &mut stream) {
            if let Polled::Panicked(text) = poll(&mut exec, key) {
                panics.push((key, text));
            }
        }
        assert_eq!(panics, [(bad, "boom".to_owned())]);
        assert_eq!(*lock(&log), [7, 7, 7]);
    }

    #[test]
    fn slots_are_reused_with_a_new_generation() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut exec = Executor::new();
        let first = spawn(&mut exec, counter(&log, 0, 0));
        let mut stream = Stream::new(1, b"t", &[]);
        drive(&mut exec, &mut stream);
        let second = spawn(&mut exec, counter(&log, 1, 0));
        assert_eq!(first.slot, second.slot);
        assert_ne!(first, second);
        // The old key reaches nothing.
        assert!(exec.take(first).is_none());
        assert!(exec.task(first).is_none());
        assert_eq!(exec.keys(), [second]);
        assert_eq!(drive(&mut exec, &mut stream), 1);
    }

    #[test]
    fn a_task_without_a_future_waits_for_install() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut exec = Executor::new();
        let key = exec.insert(Owner::Control, None, None);
        let mut stream = Stream::new(1, b"t", &[]);
        assert_eq!(drive(&mut exec, &mut stream), 0);
        assert!(exec.install(key, counter(&log, 3, 0)).is_ok());
        assert!(exec.install(key, counter(&log, 4, 0)).is_err());
        assert_eq!(drive(&mut exec, &mut stream), 1);
        assert_eq!(*lock(&log), [3]);
        assert!(exec.install(key, counter(&log, 5, 0)).is_err());
    }

    #[test]
    fn take_runnable_parks_in_order_and_skips_ended_tasks() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut exec = Executor::new();
        let keys: Vec<TaskKey> = (0..4)
            .map(|n| spawn(&mut exec, counter(&log, n, 0)))
            .collect();
        for key in exec.woken() {
            exec.push_runnable(key);
        }
        exec.remove(keys[2]);
        let parked = exec.take_runnable(|task| task.owner == Owner::Control);
        assert_eq!(parked, [keys[0], keys[1], keys[3]]);
        assert!(exec.pick(&mut Stream::new(1, b"t", &[])).is_none());
    }

    #[test]
    fn clear_returns_every_task() {
        struct Flag(Arc<AtomicBool>);
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let dropped: Vec<Arc<AtomicBool>> =
            (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
        let mut exec = Executor::new();
        for flag in &dropped {
            let flag = Flag(Arc::clone(flag));
            spawn(
                &mut exec,
                Box::pin(async move {
                    let _flag = flag;
                    std::future::pending::<()>().await;
                }),
            );
        }
        let tasks = exec.clear();
        assert_eq!(tasks.len(), 3);
        drop(tasks);
        assert!(exec.keys().is_empty());
        assert!(dropped.iter().all(|flag| flag.load(Ordering::Acquire)));
        assert!(next(&mut exec, &mut Stream::new(1, b"t", &[])).is_none());
    }

    #[test]
    fn a_joint_finishes_once_and_frees_its_place() {
        let live = Arc::new(AtomicUsize::new(1));
        let joint = Joint::new(Arc::clone(&live));
        assert_eq!(joint.poll_end(Waker::noop()), None);
        joint.finish(Err(JoinError::Cancelled));
        joint.finish(Ok(()));
        assert_eq!(
            joint.poll_end(Waker::noop()),
            Some(Err(JoinError::Cancelled))
        );
        assert_eq!(live.load(Ordering::Acquire), 0);
    }
}
