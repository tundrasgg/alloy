//! `openraft`'s `AsyncRuntime` on `alloy-rt` (ADR 0064): every timer, task,
//! and random draw of a group goes through the [`Runtime`] it was opened with.
//!
//! `AsyncRuntime` has no receiver: `openraft` calls `spawn`, `sleep`, and
//! `Instant::now` as associated functions. So the runtime is found through a
//! scope: [`scoped`] makes an [`RtScope`] current on the thread for each poll
//! of a future, and every task spawned from inside a scope runs in the same
//! scope. `Group` enters its scope around every call into `openraft`, so
//! `openraft` never runs outside one. A call outside a scope has no runtime:
//! a spawn then fails its join handle, a sleep never completes, the clock
//! reads [`Instant::ZERO`], and the generator gives zeros.
//!
//! A scope also tracks the tasks spawned in it ([`Tasks`]), so that a group
//! that stops can end every task it started and know when they have ended,
//! and with them every reference they held to the stores.
//!
//! Channels and the mutex are `tokio::sync` primitives, which read no clock,
//! start no task, and draw no randomness.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use alloy_rt::{Elapsed, Instant, JoinError, Runtime, TaskName};
use futures_util::future::{AbortHandle, AbortRegistration, Abortable, Aborted};
use futures_util::{FutureExt, TryFutureExt};
use openraft::OptionalSend;
use openraft::async_runtime::{
    Mpsc, MpscReceiver, MpscSender, MpscWeakSender, Oneshot, OneshotSender, SendError,
    TryRecvError, watch,
};
use tokio::sync::{mpsc, oneshot, watch as tokio_watch};

thread_local! {
    /// The scope being polled on this thread, if any.
    static CURRENT: RefCell<Option<RtScope>> = const { RefCell::new(None) };
}

fn current() -> Option<RtScope> {
    CURRENT.with(|current| current.borrow().clone())
}

fn current_runtime() -> Option<Runtime> {
    current().map(|scope| scope.runtime)
}

/// The runtime a group runs on and the tasks it has spawned.
#[derive(Clone, Debug)]
pub(crate) struct RtScope {
    pub(crate) runtime: Runtime,
    pub(crate) tasks: Arc<Tasks>,
}

impl RtScope {
    /// A scope on `runtime` with no tasks yet.
    pub(crate) fn new(runtime: &Runtime) -> RtScope {
        RtScope {
            runtime: runtime.clone(),
            tasks: Arc::new(Tasks(tokio_watch::channel(TaskSet::default()).0)),
        }
    }
}

/// The live tasks of one scope: at most as many as the `spawn` seam allows,
/// 1,048,576.
#[derive(Debug)]
pub(crate) struct Tasks(tokio_watch::Sender<TaskSet>);

#[derive(Debug, Default)]
struct TaskSet {
    /// The id of the next task. 2^64 ids are never used up.
    next: u64,
    /// Set by [`Tasks::close`]: no task starts after it.
    closed: bool,
    /// Each live task's abort handle, by id.
    live: BTreeMap<u64, AbortHandle>,
}

impl Tasks {
    /// Records a new task, unless the scope is closed.
    fn register(&self) -> Option<(u64, AbortRegistration)> {
        let mut registered = None;
        self.0.send_modify(|set| {
            if set.closed {
                return;
            }
            let (handle, registration) = AbortHandle::new_pair();
            let id = set.next;
            set.next = set.next.wrapping_add(1);
            set.live.insert(id, handle);
            registered = Some((id, registration));
        });
        registered
    }

    fn ended_one(&self, id: u64) {
        self.0.send_modify(|set| {
            set.live.remove(&id);
        });
    }

    /// Ends every task of the scope: none starts after this call, and each
    /// live one is dropped, unfinished, at its next poll.
    pub(crate) fn close(&self) {
        self.0.send_modify(|set| {
            set.closed = true;
            for handle in set.live.values() {
                handle.abort();
            }
        });
    }

    /// Completes once no task of the scope is live: each has ended and its
    /// future has been dropped.
    pub(crate) async fn ended(&self) {
        let mut set = self.0.subscribe();
        // The sender is `self`, which outlives this call, so the wait never
        // fails.
        let _ = set.wait_for(|set| set.live.is_empty()).await;
    }
}

/// Marks its task ended when dropped.
struct Ended {
    tasks: Arc<Tasks>,
    id: u64,
}

impl Drop for Ended {
    fn drop(&mut self) {
        self.tasks.ended_one(self.id);
    }
}

/// A spawned task's future. Fields drop in declaration order, so the task's
/// future, and all it holds, is gone before the task counts as ended.
struct Tracked<F> {
    future: Pin<Box<Abortable<F>>>,
    _ended: Ended,
}

impl<F: Future> Future for Tracked<F> {
    type Output = Result<F::Output, Aborted>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(cx)
    }
}

/// A future polled with its scope current.
pub(crate) struct Scoped<F> {
    scope: RtScope,
    future: Pin<Box<F>>,
}

/// Runs `future` with `scope` current on every poll.
pub(crate) fn scoped<F: Future>(scope: &RtScope, future: F) -> Scoped<F> {
    Scoped {
        scope: scope.clone(),
        future: Box::pin(future),
    }
}

/// Restores the scope that was current before a poll, also on unwind.
struct Restore(Option<RtScope>);

impl Drop for Restore {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT.with(|current| *current.borrow_mut() = previous);
    }
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let scope = self.scope.clone();
        let _restore = Restore(CURRENT.with(|current| current.replace(Some(scope))));
        self.future.as_mut().poll(cx)
    }
}

impl<F> fmt::Debug for Scoped<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scoped").finish_non_exhaustive()
    }
}

/// `openraft`'s runtime over the current [`Runtime`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AlloyRt;

/// Why a spawned task gave no output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TaskError {
    /// The task was cancelled or panicked.
    #[error(transparent)]
    Join(#[from] JoinError),
    /// The task never started: no scope was current, the scope was closed,
    /// or the runtime refused.
    #[error("the task could not be spawned")]
    Spawn,
}

type Boxed<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A spawned task's output.
pub(crate) struct RtJoin<T>(Boxed<Result<T, TaskError>>);

impl<T> Future for RtJoin<T> {
    type Output = Result<T, TaskError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.as_mut().poll(cx)
    }
}

/// A future with a deadline.
pub(crate) struct RtTimeout<T> {
    future: Pin<Box<T>>,
    sleep: Boxed<()>,
}

impl<T: Future> Future for RtTimeout<T> {
    type Output = Result<T::Output, Elapsed>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(value) = self.future.as_mut().poll(cx) {
            return Poll::Ready(Ok(value));
        }
        self.sleep.as_mut().poll(cx).map(|()| Err(Elapsed))
    }
}

/// An instant on the current runtime's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RtInstant(Instant);

impl Add<Duration> for RtInstant {
    type Output = RtInstant;
    fn add(self, rhs: Duration) -> RtInstant {
        RtInstant(self.0.saturating_add(rhs))
    }
}

impl AddAssign<Duration> for RtInstant {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl Sub<Duration> for RtInstant {
    type Output = RtInstant;
    fn sub(self, rhs: Duration) -> RtInstant {
        let nanos = u64::try_from(rhs.as_nanos()).unwrap_or(u64::MAX);
        RtInstant(Instant::from_nanos(self.0.as_nanos().saturating_sub(nanos)))
    }
}

impl SubAssign<Duration> for RtInstant {
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl Sub<RtInstant> for RtInstant {
    type Output = Duration;
    fn sub(self, rhs: RtInstant) -> Duration {
        self.0.saturating_duration_since(rhs.0)
    }
}

impl openraft::Instant for RtInstant {
    fn now() -> RtInstant {
        RtInstant(current_runtime().map_or(Instant::ZERO, |rt| rt.clock().now()))
    }
}

/// Draws from the current runtime's `rng` seam.
#[derive(Debug)]
pub(crate) struct RtRng;

impl rand_core::TryRng for RtRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        let mut bytes = [0; 4];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        let mut bytes = [0; 8];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    /// A failed draw leaves zeros: `openraft` draws only election timeouts
    /// and tick jitter, which then take the low end of their range.
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        dst.fill(0);
        if let Some(rt) = current_runtime()
            && rt.rng().fill(dst).is_err()
        {
            dst.fill(0);
        }
        Ok(())
    }
}

fn sleep_until(deadline: Instant) -> Boxed<()> {
    match current_runtime() {
        Some(rt) => rt.clock().sleep_until(deadline),
        None => Box::pin(std::future::pending()),
    }
}

impl openraft::AsyncRuntime for AlloyRt {
    type JoinError = TaskError;
    type JoinHandle<T: OptionalSend + 'static> = RtJoin<T>;
    type Sleep = Boxed<()>;
    type Instant = RtInstant;
    type TimeoutError = Elapsed;
    type Timeout<R, T: Future<Output = R> + OptionalSend> = RtTimeout<T>;
    type ThreadLocalRng = RtRng;

    fn spawn<T>(future: T) -> RtJoin<T::Output>
    where
        T: Future + OptionalSend + 'static,
        T::Output: OptionalSend + 'static,
    {
        let spawned = current().and_then(|scope| {
            let (id, registration) = scope.tasks.register()?;
            let tracked = Tracked {
                future: Box::pin(Abortable::new(future, registration)),
                _ended: Ended {
                    tasks: Arc::clone(&scope.tasks),
                    id,
                },
            };
            // "raft" satisfies the rules of TaskName::new.
            let name = TaskName::new("raft").expect("a fixed valid task name");
            scope.runtime.spawn(name, scoped(&scope, tracked)).ok()
        });
        match spawned {
            Some(handle) => RtJoin(
                handle
                    .join()
                    .map(|joined| match joined {
                        Ok(Ok(output)) => Ok(output),
                        Ok(Err(Aborted)) => Err(TaskError::Join(JoinError::Cancelled)),
                        Err(error) => Err(TaskError::Join(error)),
                    })
                    .boxed(),
            ),
            None => RtJoin(Box::pin(async { Err(TaskError::Spawn) })),
        }
    }

    fn sleep(duration: Duration) -> Boxed<()> {
        sleep_until((<RtInstant as openraft::Instant>::now() + duration).0)
    }

    fn sleep_until(deadline: RtInstant) -> Boxed<()> {
        sleep_until(deadline.0)
    }

    fn timeout<R, F: Future<Output = R> + OptionalSend>(
        duration: Duration,
        future: F,
    ) -> RtTimeout<F> {
        Self::timeout_at(<RtInstant as openraft::Instant>::now() + duration, future)
    }

    fn timeout_at<R, F: Future<Output = R> + OptionalSend>(
        deadline: RtInstant,
        future: F,
    ) -> RtTimeout<F> {
        RtTimeout {
            future: Box::pin(future),
            sleep: sleep_until(deadline.0),
        }
    }

    fn is_panic(join_error: &TaskError) -> bool {
        *join_error == TaskError::Join(JoinError::Panicked)
    }

    fn thread_rng() -> RtRng {
        RtRng
    }

    type Mpsc = RtMpsc;
    type Watch = RtWatch;
    type Oneshot = RtOneshot;
    type Mutex<T: OptionalSend + 'static> = RtMutex<T>;

    fn new(_threads: usize) -> AlloyRt {
        AlloyRt
    }

    /// Blocks the thread until `future` completes, polling it in the scope of
    /// the runtime current on this thread. `openraft` calls it only through
    /// `AsyncRuntime::run`, which no code here uses.
    fn block_on<F, T>(&mut self, future: F) -> T
    where
        F: Future<Output = T>,
    {
        let signal = Arc::new(Signal::default());
        let waker = Waker::from(Arc::clone(&signal));
        let mut cx = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
            signal.wait();
        }
    }
}

/// Wakes a thread blocked in `block_on`.
#[derive(Default)]
struct Signal {
    woken: Mutex<bool>,
    condvar: Condvar,
}

impl Signal {
    fn wait(&self) {
        let mut woken = self.woken.lock().unwrap_or_else(PoisonError::into_inner);
        while !*woken {
            woken = self
                .condvar
                .wait(woken)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *woken = false;
    }
}

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        *self.woken.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.condvar.notify_one();
    }
}

/// Bounded channels over `tokio::sync::mpsc`.
#[derive(Debug)]
pub(crate) struct RtMpsc;

/// The sending half of an [`RtMpsc`] channel.
pub(crate) struct RtSender<T>(mpsc::Sender<T>);
/// The receiving half of an [`RtMpsc`] channel.
pub(crate) struct RtReceiver<T>(mpsc::Receiver<T>);
/// A sender that does not keep its channel open.
pub(crate) struct RtWeakSender<T>(mpsc::WeakSender<T>);

impl<T> Clone for RtSender<T> {
    fn clone(&self) -> Self {
        RtSender(self.0.clone())
    }
}

impl<T> Clone for RtWeakSender<T> {
    fn clone(&self) -> Self {
        RtWeakSender(self.0.clone())
    }
}

impl Mpsc for RtMpsc {
    type Sender<T: OptionalSend> = RtSender<T>;
    type Receiver<T: OptionalSend> = RtReceiver<T>;
    type WeakSender<T: OptionalSend> = RtWeakSender<T>;

    fn channel<T: OptionalSend>(buffer: usize) -> (RtSender<T>, RtReceiver<T>) {
        let (tx, rx) = mpsc::channel(buffer.max(1));
        (RtSender(tx), RtReceiver(rx))
    }
}

impl<T: OptionalSend> MpscSender<RtMpsc, T> for RtSender<T> {
    fn send(&self, msg: T) -> impl Future<Output = Result<(), SendError<T>>> + OptionalSend {
        self.0.send(msg).map_err(|error| SendError(error.0))
    }

    fn downgrade(&self) -> RtWeakSender<T> {
        RtWeakSender(self.0.downgrade())
    }
}

impl<T: OptionalSend> MpscReceiver<T> for RtReceiver<T> {
    fn recv(&mut self) -> impl Future<Output = Option<T>> + OptionalSend {
        self.0.recv()
    }

    fn try_recv(&mut self) -> Result<T, TryRecvError> {
        self.0.try_recv().map_err(|error| match error {
            mpsc::error::TryRecvError::Empty => TryRecvError::Empty,
            mpsc::error::TryRecvError::Disconnected => TryRecvError::Disconnected,
        })
    }
}

impl<T: OptionalSend> MpscWeakSender<RtMpsc, T> for RtWeakSender<T> {
    fn upgrade(&self) -> Option<RtSender<T>> {
        self.0.upgrade().map(RtSender)
    }
}

/// Watch channels over `tokio::sync::watch`.
#[derive(Debug)]
pub(crate) struct RtWatch;

/// The sending half of an [`RtWatch`] channel.
pub(crate) struct RtWatchSender<T>(tokio_watch::Sender<T>);
/// A receiving half of an [`RtWatch`] channel.
pub(crate) struct RtWatchReceiver<T>(tokio_watch::Receiver<T>);

impl<T> Clone for RtWatchSender<T> {
    fn clone(&self) -> Self {
        RtWatchSender(self.0.clone())
    }
}

impl<T> Clone for RtWatchReceiver<T> {
    fn clone(&self) -> Self {
        RtWatchReceiver(self.0.clone())
    }
}

impl watch::Watch for RtWatch {
    type Sender<T: OptionalSend + Sync> = RtWatchSender<T>;
    type Receiver<T: OptionalSend + Sync> = RtWatchReceiver<T>;
    type Ref<'a, T: OptionalSend + 'a> = tokio_watch::Ref<'a, T>;

    fn channel<T: OptionalSend + Sync>(init: T) -> (RtWatchSender<T>, RtWatchReceiver<T>) {
        let (tx, rx) = tokio_watch::channel(init);
        (RtWatchSender(tx), RtWatchReceiver(rx))
    }
}

impl<T: OptionalSend + Sync> watch::WatchSender<RtWatch, T> for RtWatchSender<T> {
    fn send(&self, value: T) -> Result<(), watch::SendError<T>> {
        self.0
            .send(value)
            .map_err(|error| watch::SendError(error.0))
    }

    fn send_if_modified<F: FnOnce(&mut T) -> bool>(&self, modify: F) -> bool {
        self.0.send_if_modified(modify)
    }

    fn borrow_watched(&self) -> tokio_watch::Ref<'_, T> {
        self.0.borrow()
    }

    fn subscribe(&self) -> RtWatchReceiver<T> {
        RtWatchReceiver(self.0.subscribe())
    }
}

impl<T: OptionalSend + Sync> watch::WatchReceiver<RtWatch, T> for RtWatchReceiver<T> {
    async fn changed(&mut self) -> Result<(), watch::RecvError> {
        self.0.changed().await.map_err(|_| watch::RecvError(()))
    }

    fn borrow_watched(&self) -> tokio_watch::Ref<'_, T> {
        self.0.borrow()
    }

    fn borrow_and_update(&mut self) -> tokio_watch::Ref<'_, T> {
        self.0.borrow_and_update()
    }
}

/// One-shot channels over `tokio::sync::oneshot`.
#[derive(Debug)]
pub(crate) struct RtOneshot;

/// The sending half of an [`RtOneshot`] channel.
pub(crate) struct RtOneshotSender<T>(oneshot::Sender<T>);

impl Oneshot for RtOneshot {
    type Sender<T: OptionalSend> = RtOneshotSender<T>;
    type Receiver<T: OptionalSend> = oneshot::Receiver<T>;
    type ReceiverError = oneshot::error::RecvError;

    fn channel<T: OptionalSend>() -> (RtOneshotSender<T>, oneshot::Receiver<T>) {
        let (tx, rx) = oneshot::channel();
        (RtOneshotSender(tx), rx)
    }
}

impl<T: OptionalSend> OneshotSender<T> for RtOneshotSender<T> {
    fn send(self, t: T) -> Result<(), T> {
        self.0.send(t)
    }
}

/// An asynchronous mutex over `tokio::sync::Mutex`.
pub(crate) struct RtMutex<T>(tokio::sync::Mutex<T>);

impl<T: OptionalSend + 'static> openraft::async_runtime::mutex::Mutex<T> for RtMutex<T> {
    type Guard<'a> = tokio::sync::MutexGuard<'a, T>;

    fn new(value: T) -> RtMutex<T> {
        RtMutex(tokio::sync::Mutex::new(value))
    }

    fn lock(&self) -> impl Future<Output = Self::Guard<'_>> + OptionalSend {
        self.0.lock()
    }
}
