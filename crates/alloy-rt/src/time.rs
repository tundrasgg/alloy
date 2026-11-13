//! The `clock` seam (ARFC 0005 section 2.1 and clause 3.1.4).

use std::fmt;
use std::time::Duration;

use crate::BoxFuture;

/// A point on one [`Clock`] instance's monotonic timeline, in nanoseconds from
/// an origin fixed when the instance is built. Values from different instances
/// are not comparable.
///
/// Ranges from [`Instant::ZERO`] to [`Instant::MAX`], 2^63 - 1 nanoseconds
/// (about 292 years). Arithmetic never wraps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
    /// The origin.
    pub const ZERO: Instant = Instant(0);
    /// 2^63 - 1 nanoseconds.
    pub const MAX: Instant = Instant(u64::MAX >> 1);

    /// The instant `nanos` nanoseconds after the origin. Values above
    /// [`Instant::MAX`] give `MAX`.
    pub const fn from_nanos(nanos: u64) -> Instant {
        if nanos > Self::MAX.0 {
            Self::MAX
        } else {
            Instant(nanos)
        }
    }

    /// Nanoseconds since the origin, at most 2^63 - 1.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// `self + duration`, or `None` when the sum is above [`Instant::MAX`].
    pub fn checked_add(self, duration: Duration) -> Option<Instant> {
        let sum = u128::from(self.0).checked_add(duration.as_nanos())?;
        u64::try_from(sum)
            .ok()
            .filter(|nanos| *nanos <= Self::MAX.0)
            .map(Instant)
    }

    /// `self + duration`, or [`Instant::MAX`] when the sum is above it.
    pub fn saturating_add(self, duration: Duration) -> Instant {
        self.checked_add(duration).unwrap_or(Self::MAX)
    }

    /// The time from `earlier` to `self`, or zero when `earlier` is later than
    /// `self`.
    pub fn saturating_duration_since(self, earlier: Instant) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

/// Wall-clock time in nanoseconds since the Unix epoch, leap seconds ignored.
/// Negative values are before the epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WallTime(pub i64);

/// Seam `clock`: monotonic time, wall time, and timers (ARFC 0005 clause
/// 3.1.4).
///
/// Methods take `&self`, may be called concurrently from any task, and never
/// panic.
pub trait Clock: Send + Sync + fmt::Debug + 'static {
    /// The current instant. Never lower than any value an earlier call on the
    /// same instance returned, from any thread.
    fn now(&self) -> Instant;

    /// The current wall time. May move backward or forward between calls.
    fn wall(&self) -> WallTime;

    /// Completes once `now() >= deadline`: on its first poll when `deadline` is
    /// at or before `now()`, and never for [`Instant::MAX`]. Dropping it
    /// removes the timer.
    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_nanos_saturates_at_max() {
        assert_eq!(Instant::MAX.as_nanos(), (1u64 << 63) - 1);
        assert_eq!(Instant::from_nanos(5).as_nanos(), 5);
        assert_eq!(Instant::from_nanos(1 << 63), Instant::MAX);
        assert_eq!(Instant::from_nanos(u64::MAX), Instant::MAX);
        assert_eq!(Instant::ZERO.as_nanos(), 0);
    }

    #[test]
    fn checked_and_saturating_add() {
        let t = Instant::from_nanos(10);
        assert_eq!(
            t.checked_add(Duration::from_nanos(5)),
            Some(Instant::from_nanos(15))
        );
        assert_eq!(Instant::MAX.checked_add(Duration::ZERO), Some(Instant::MAX));
        assert_eq!(Instant::MAX.checked_add(Duration::from_nanos(1)), None);
        assert_eq!(t.checked_add(Duration::MAX), None);
        assert_eq!(t.saturating_add(Duration::MAX), Instant::MAX);
        assert_eq!(
            t.saturating_add(Duration::from_secs(1)).as_nanos(),
            1_000_000_010
        );
    }

    #[test]
    fn duration_since_is_zero_for_a_later_earlier() {
        let a = Instant::from_nanos(100);
        let b = Instant::from_nanos(40);
        assert_eq!(a.saturating_duration_since(b), Duration::from_nanos(60));
        assert_eq!(b.saturating_duration_since(a), Duration::ZERO);
        assert_eq!(
            Instant::MAX
                .saturating_duration_since(Instant::ZERO)
                .as_nanos(),
            (1u128 << 63) - 1
        );
    }
}
