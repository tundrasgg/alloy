//! The single-thread executor: a task slab, wakers, and the seeded pick of
//! the next runnable task (ARFC 0005 clause 3.2.2).
//!
//! The executor owns every task future of a run and polls them on the run's
//! thread. A waker may be called from any thread: it only appends its task to
//! a shared ready queue, which the executor drains in order before each pick.
//! A task is in the queue or the runnable set at most once, so both are
//! bounded by the number of live tasks.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll, Wake, Waker};

use alloy_rt::BoxFuture;

use crate::stream::Stream;

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

/// Identifies one task of an executor. The generation tells a task apart
/// from a later task in the same slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TaskKey {
    slot: usize,
    generation: u64,
}

/// What one poll of a task did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Polled {
    /// The task is still running.
    Pending,
    /// The task's future completed and was dropped.
    Completed,
    /// The task's future panicked with this message and was dropped.
    Panicked(String),
}

/// Tasks woken since the executor last drained the queue, in wake order.
type ReadyQueue = Mutex<Vec<TaskKey>>;

/// The waker of one task.
#[derive(Debug)]
struct TaskWaker {
    key: TaskKey,
    queue: Weak<ReadyQueue>,
    /// True while the task is in the ready queue or the runnable set, and
    /// for ever once the task has ended, so a wake then does nothing.
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

struct Task {
    future: BoxFuture<'static, ()>,
    waker: Arc<TaskWaker>,
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
    /// Tasks that may be polled, in the order they became runnable.
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

    /// Adds `future` as a task that is runnable at once. It is not polled
    /// here.
    pub(crate) fn spawn(&mut self, future: BoxFuture<'static, ()>) -> TaskKey {
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
            queued: AtomicBool::new(true),
        });
        entry.task = Some(Task { future, waker });
        self.runnable.push(key);
        key
    }

    /// The number of tasks that have not ended.
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.slots.iter().filter(|slot| slot.task.is_some()).count()
    }

    /// Takes the next task to poll: one of the runnable tasks, picked with
    /// `stream`. `None` when no task is runnable.
    pub(crate) fn pick(&mut self, stream: &mut Stream) -> Option<TaskKey> {
        let woken = std::mem::take(&mut *lock(&self.queue));
        for key in woken {
            if self.task(key).is_some() {
                self.runnable.push(key);
            }
        }
        if self.runnable.is_empty() {
            return None;
        }
        let index = stream.below(self.runnable.len());
        Some(self.runnable.swap_remove(index))
    }

    fn task(&mut self, key: TaskKey) -> Option<&mut Task> {
        self.slots
            .get_mut(key.slot)
            .filter(|slot| slot.generation == key.generation)
            .and_then(|slot| slot.task.as_mut())
    }

    /// Polls the task `key` once. A task that completes or panics is
    /// removed, and its future is dropped before this returns. A key whose
    /// task has ended gives `Completed` without polling anything.
    pub(crate) fn poll(&mut self, key: TaskKey) -> Polled {
        let Some(task) = self.task(key) else {
            return Polled::Completed;
        };
        task.waker.queued.store(false, Ordering::Release);
        let waker = Waker::from(Arc::clone(&task.waker));
        let mut cx = Context::from_waker(&waker);
        let future = &mut task.future;
        let polled = match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut cx))) {
            Ok(Poll::Pending) => return Polled::Pending,
            Ok(Poll::Ready(())) => Polled::Completed,
            Err(payload) => Polled::Panicked(panic_text(payload.as_ref())),
        };
        self.remove(key);
        polled
    }

    /// Ends the task `key` and drops its future, if it has not ended. A panic
    /// in the future's destructor is caught.
    fn remove(&mut self, key: TaskKey) {
        let Some(slot) = self
            .slots
            .get_mut(key.slot)
            .filter(|slot| slot.generation == key.generation)
        else {
            return;
        };
        let Some(task) = slot.task.take() else {
            return;
        };
        slot.generation += 1;
        self.free.push(key.slot);
        self.runnable.retain(|k| *k != key);
        task.waker.queued.store(true, Ordering::Release);
        drop_quietly(task.future);
    }

    /// Ends every task, in slot order, dropping each future.
    pub(crate) fn clear(&mut self) {
        let keys: Vec<TaskKey> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.task.is_some())
            .map(|(slot, entry)| TaskKey {
                slot,
                generation: entry.generation,
            })
            .collect();
        for key in keys {
            self.remove(key);
        }
        lock(&self.queue).clear();
    }
}

/// Drops `value`, catching a panic in its destructor. Used where the run has
/// already decided its result and a destructor can no longer change it.
pub(crate) fn drop_quietly<T>(value: T) {
    // A destructor that panics has nothing left to report to: the panic hook
    // has printed it, and the value is gone either way.
    let _ = catch_unwind(AssertUnwindSafe(move || drop(value)));
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

    fn drive(exec: &mut Executor, stream: &mut Stream) -> usize {
        let mut polls = 0;
        while let Some(key) = exec.pick(stream) {
            exec.poll(key);
            polls += 1;
        }
        polls
    }

    #[test]
    fn every_task_runs_to_completion() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut exec = Executor::new();
        for name in 0..5 {
            exec.spawn(counter(&log, name, name));
        }
        assert_eq!(exec.live(), 5);
        let polls = drive(&mut exec, &mut Stream::new(1, b"t", &[]));
        assert_eq!(polls, 15);
        assert_eq!(exec.live(), 0);
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
                exec.spawn(counter(&log, name, 3));
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
        exec.spawn(Box::pin(poll_fn(move |cx| {
            polled += 1;
            if polled == 2 {
                return Poll::Ready(());
            }
            *lock(&held) = Some(cx.waker().clone());
            Poll::Pending
        })));
        let mut stream = Stream::new(1, b"t", &[]);
        assert_eq!(drive(&mut exec, &mut stream), 1);
        assert!(exec.pick(&mut stream).is_none());
        let waker = lock(&stash).take();
        let waker = waker.expect("the task stored its waker");
        // Waking twice queues the task once.
        waker.wake_by_ref();
        waker.wake_by_ref();
        assert_eq!(drive(&mut exec, &mut stream), 1);
        assert_eq!(exec.live(), 0);
        // A wake after the task ended does nothing.
        waker.wake();
        assert!(exec.pick(&mut stream).is_none());
    }

    #[test]
    fn a_panicking_task_ends_and_others_continue() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut exec = Executor::new();
        let bad = exec.spawn(Box::pin(poll_fn(|_| -> Poll<()> { panic!("boom") })));
        exec.spawn(counter(&log, 7, 2));
        let mut stream = Stream::new(1, b"t", &[]);
        let mut panics = Vec::new();
        while let Some(key) = exec.pick(&mut stream) {
            if let Polled::Panicked(text) = exec.poll(key) {
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
        let first = exec.spawn(counter(&log, 0, 0));
        let mut stream = Stream::new(1, b"t", &[]);
        drive(&mut exec, &mut stream);
        let second = exec.spawn(counter(&log, 1, 0));
        assert_eq!(first.slot, second.slot);
        assert_ne!(first, second);
        // The old key reaches nothing.
        assert_eq!(exec.poll(first), Polled::Completed);
        assert_eq!(exec.live(), 1);
        assert_eq!(drive(&mut exec, &mut stream), 1);
    }

    #[test]
    fn clear_drops_every_future() {
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
            exec.spawn(Box::pin(async move {
                let _flag = flag;
                std::future::pending::<()>().await;
            }));
        }
        exec.clear();
        assert_eq!(exec.live(), 0);
        assert!(dropped.iter().all(|flag| flag.load(Ordering::Acquire)));
        assert!(exec.pick(&mut Stream::new(1, b"t", &[])).is_none());
    }
}
