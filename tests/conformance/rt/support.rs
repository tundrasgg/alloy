//! Helpers the checks share. Nothing here implements a seam: every check
//! reaches time, the network, storage, randomness, and tasks only through the
//! instances its [`Env`] gives.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use alloy_rt::{BoxFuture, TaskName};
use alloy_seam::{Binding, Seam};

use crate::{Env, Failure, Outcome};

/// How long a check waits to see that something does not happen.
pub(crate) const SETTLE: Duration = Duration::from_millis(50);

/// How long a check waits for something that must happen. Only a failing
/// implementation makes a check wait this long.
pub(crate) const PATIENCE: Duration = Duration::from_secs(10);

/// Turns an unexpected error or a missing value into a failure.
pub(crate) trait Ctx<T> {
    fn ctx(self, what: &str) -> Result<T, Failure>;
}

impl<T, E: fmt::Debug> Ctx<T> for Result<T, E> {
    fn ctx(self, what: &str) -> Result<T, Failure> {
        self.map_err(|error| Failure::Failed(format!("{what}: unexpected error {error:?}")))
    }
}

impl<T> Ctx<T> for Option<T> {
    fn ctx(self, what: &str) -> Result<T, Failure> {
        self.ok_or_else(|| Failure::Failed(format!("{what}: did not complete within {PATIENCE:?}")))
    }
}

/// A `Failed` failure.
pub(crate) fn fail(text: impl Into<String>) -> Failure {
    Failure::Failed(text.into())
}

/// A `Skipped` outcome.
pub(crate) fn skip(text: impl Into<String>) -> Outcome {
    Err(Failure::Skipped(text.into()))
}

/// A task name, from a literal the check knows is valid.
pub(crate) fn name(text: &str) -> Result<TaskName, Failure> {
    TaskName::new(text).ctx("a task name of the suite")
}

/// A new instance of seam `S` built from the harness's registry with
/// `binding`, so that limits per instance start from zero.
pub(crate) fn fresh<S: Seam<Context = ()>>(
    env: &Env,
    binding: &Binding,
) -> Result<Arc<S::Object>, Failure> {
    env.registry
        .instantiate::<S>(&(), &binding.name, &binding.config)
        .ctx("building a fresh instance from the registry")
}

/// Sleeps for `duration` on the environment's clock.
pub(crate) async fn sleep(env: &Env, duration: Duration) {
    let clock = env.runtime.clock();
    let deadline = clock.now().saturating_add(duration);
    clock.sleep_until(deadline).await;
}

/// Runs `future` until it completes or `limit` passes on the environment's
/// clock, whichever is first. The future is polled before the timer on every
/// poll and is dropped when the timer wins.
pub(crate) async fn within<F: Future>(env: &Env, limit: Duration, future: F) -> Option<F::Output> {
    let clock = env.runtime.clock();
    let timer = clock.sleep_until(clock.now().saturating_add(limit));
    Within {
        future: Box::pin(future),
        timer,
    }
    .await
}

struct Within<F> {
    future: Pin<Box<F>>,
    timer: BoxFuture<'static, ()>,
}

impl<F: Future> Future for Within<F> {
    type Output = Option<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(value) = self.future.as_mut().poll(cx) {
            return Poll::Ready(Some(value));
        }
        match self.timer.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Polls `future` exactly once, with the calling task's context, and returns
/// what that poll returned.
pub(crate) fn poll_once<F: Future + Unpin>(future: &mut F) -> PollOnce<'_, F> {
    PollOnce(future)
}

pub(crate) struct PollOnce<'a, F>(&'a mut F);

impl<F: Future + Unpin> Future for PollOnce<'_, F> {
    type Output = Poll<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(Pin::new(&mut *self.0).poll(cx))
    }
}

/// Waits until `condition` holds, checking it every millisecond of the
/// environment's clock, for at most [`PATIENCE`].
pub(crate) async fn eventually(
    env: &Env,
    what: &str,
    mut condition: impl FnMut() -> bool,
) -> Outcome {
    let clock = env.runtime.clock();
    let deadline = clock.now().saturating_add(PATIENCE);
    loop {
        if condition() {
            return Ok(());
        }
        if clock.now() >= deadline {
            return Err(fail(format!("{what}: did not happen within {PATIENCE:?}")));
        }
        clock
            .sleep_until(clock.now().saturating_add(Duration::from_millis(1)))
            .await;
    }
}

/// A waker that counts its wakes.
#[derive(Debug, Default)]
pub(crate) struct WakeCount(AtomicUsize);

impl WakeCount {
    pub(crate) fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A counting waker and its counter.
pub(crate) fn counting_waker() -> (Arc<WakeCount>, Waker) {
    let count = Arc::new(WakeCount::default());
    (count.clone(), Waker::from(count))
}

/// A one-way gate tasks wait on. Opening it wakes every waiter.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    open: AtomicBool,
    waiters: Mutex<Vec<Waker>>,
}

impl Gate {
    pub(crate) fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        let waiters =
            std::mem::take(&mut *self.waiters.lock().unwrap_or_else(PoisonError::into_inner));
        for waker in waiters {
            waker.wake();
        }
    }

    /// Completes once the gate is open.
    pub(crate) fn wait(self: Arc<Self>) -> GateWait {
        GateWait(self)
    }
}

pub(crate) struct GateWait(Arc<Gate>);

impl Future for GateWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let gate = &self.0;
        if gate.open.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        let mut waiters = gate.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        if !waiters.iter().any(|waker| waker.will_wake(cx.waker())) {
            waiters.push(cx.waker().clone());
        }
        drop(waiters);
        if gate.open.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// Sets its flag when dropped, to observe when a task's future is dropped.
#[derive(Debug)]
pub(crate) struct SetOnDrop(pub(crate) Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A new flag, false.
pub(crate) fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Whether `flag` is set.
pub(crate) fn is_set(flag: &AtomicBool) -> bool {
    flag.load(Ordering::SeqCst)
}

/// A deterministic generator for test data (SplitMix64). It is test data
/// only, never a stand-in for the `rng` seam.
#[derive(Clone, Debug)]
pub(crate) struct Gen(u64);

impl Gen {
    pub(crate) fn new(seed: u64) -> Gen {
        Gen(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`. `bound` is not 0.
    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u64().to_le_bytes()[0]).collect()
    }
}
