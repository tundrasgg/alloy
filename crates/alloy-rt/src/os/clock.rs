//! The `os` clock (ARFC 0005 clause 3.1.4): the monotonic clock that
//! `std::time::Instant` reads, the system real-time clock, and `tokio` timers.

use std::future::{Future, pending, ready};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Host, SleepUntil};
use crate::{BoxFuture, Clock, Instant, WallTime};

/// Seam `clock`, implementation `os`.
#[derive(Debug)]
pub(super) struct OsClock(Arc<Timeline>);

/// The instance's monotonic timeline, shared with its sleeps.
#[derive(Debug)]
struct Timeline {
    host: Host,
    /// The origin: [`Instant::ZERO`] of this instance.
    origin: std::time::Instant,
    /// The highest value `now` has returned, so that no later call returns
    /// less, from any thread, whatever the platform clock does.
    latest: AtomicU64,
}

impl OsClock {
    pub(super) fn new(host: Host) -> OsClock {
        OsClock(Arc::new(Timeline {
            host,
            origin: std::time::Instant::now(),
            latest: AtomicU64::new(0),
        }))
    }
}

impl Timeline {
    fn now(&self) -> Instant {
        let nanos = u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let nanos = Instant::from_nanos(nanos).as_nanos();
        let before = self.latest.fetch_max(nanos, Ordering::SeqCst);
        Instant::from_nanos(before.max(nanos))
    }

    /// The platform instant of `deadline`, or `None` when the platform clock
    /// cannot represent it.
    fn platform(&self, deadline: Instant) -> Option<std::time::Instant> {
        self.origin
            .checked_add(Duration::from_nanos(deadline.as_nanos()))
    }
}

impl Clock for OsClock {
    fn now(&self) -> Instant {
        self.0.now()
    }

    fn wall(&self) -> WallTime {
        let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
            Err(before) => i64::try_from(before.duration().as_nanos())
                .map_or(i64::MIN, |nanos| nanos.saturating_neg()),
        };
        WallTime(nanos)
    }

    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()> {
        if deadline == Instant::MAX {
            return Box::pin(pending());
        }
        if self.0.now() >= deadline {
            return Box::pin(ready(()));
        }
        match self.0.platform(deadline) {
            Some(at) => Box::pin(Sleep {
                timeline: Arc::clone(&self.0),
                deadline,
                timer: self.0.host.sleep_until(at),
            }),
            None => Box::pin(pending()),
        }
    }
}

/// The future of [`OsClock::sleep_until`]. Completes only once `now()` is at
/// least its deadline: a timer that fires early is armed again.
#[derive(Debug)]
struct Sleep {
    timeline: Arc<Timeline>,
    deadline: Instant,
    timer: SleepUntil,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        loop {
            if self.timeline.now() >= self.deadline {
                return Poll::Ready(());
            }
            if Pin::new(&mut self.timer).poll(cx).is_pending() {
                return Poll::Pending;
            }
            let Some(at) = self.timeline.platform(self.deadline) else {
                return Poll::Pending;
            };
            self.timer = self.timeline.host.sleep_until(at);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::task::Waker;

    use super::*;
    use crate::os::tests::host;

    #[test]
    fn now_never_decreases_across_threads() {
        let host = host();
        let clock = Arc::new(OsClock::new(host.host()));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let clock = Arc::clone(&clock);
                std::thread::spawn(move || {
                    let mut last = Instant::ZERO;
                    for _ in 0..10_000 {
                        let now = clock.now();
                        assert!(now >= last);
                        last = now;
                    }
                    last
                })
            })
            .collect();
        let latest = workers.into_iter().map(|w| w.join().unwrap()).max();
        assert!(clock.now() >= latest.unwrap());
        host.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn wall_is_after_2020() {
        let host = host();
        let clock = OsClock::new(host.host());
        // 2020-01-01T00:00:00Z.
        assert!(clock.wall().0 > 1_577_836_800 * 1_000_000_000);
        host.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn sleep_until_completes_at_its_deadline() {
        let host = host();
        let clock = OsClock::new(host.host());
        let mut cx = Context::from_waker(Waker::noop());
        let start = clock.now();
        assert!(clock.sleep_until(start).as_mut().poll(&mut cx).is_ready());
        assert!(
            clock
                .sleep_until(Instant::ZERO)
                .as_mut()
                .poll(&mut cx)
                .is_ready()
        );
        let deadline = start.saturating_add(Duration::from_millis(5));
        host.block_on(clock.sleep_until(deadline));
        assert!(clock.now() >= deadline);
        let mut never = clock.sleep_until(Instant::MAX);
        assert!(never.as_mut().poll(&mut cx).is_pending());
        drop(never);
        // Polled from a thread outside the runtime, then dropped unfinished.
        let mut late = clock.sleep_until(clock.now().saturating_add(Duration::from_secs(60)));
        assert!(late.as_mut().poll(&mut cx).is_pending());
        drop(late);
        host.shutdown(Duration::from_secs(1));
    }
}
