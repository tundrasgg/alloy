//! Suite `clock` (ARFC 0005 section 5): the `Clock` seam and `Instant`,
//! clauses 2.1, 3.1.3, and 3.1.4.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy_rt::Instant;

use crate::support::{Ctx, SETTLE, name, poll_once, sleep, within};
use crate::{Applies, Check, Env, Implementation, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0005#3.1.4",
            Applies::All,
            arfc0005_s3_1_4_now_never_decreases
        ),
        check!(
            "arfc-0005#3.1.4",
            Applies::All,
            arfc0005_s3_1_4_sleep_until_completes_only_at_its_deadline
        ),
        check!(
            "arfc-0005#3.1.4",
            Applies::All,
            arfc0005_s3_1_4_sleep_until_a_due_deadline_is_ready_on_first_poll
        ),
        check!(
            "arfc-0005#3.1.4",
            Applies::All,
            arfc0005_s3_1_4_sleep_until_max_never_completes
        ),
        check!(
            "arfc-0005#3.1.4",
            Applies::Only(Implementation::Mem),
            arfc0005_s3_1_4_mem_woken_sleeper_reads_its_deadline
        ),
        check!(
            "arfc-0005#3.1.3",
            Applies::All,
            arfc0005_s3_1_3_dropped_sleeps_cancel_without_panic
        ),
    ]
}

/// `now` never returns a value lower than an earlier call returned, from any
/// task.
async fn arfc0005_s3_1_4_now_never_decreases(env: Env) -> Outcome {
    let clock = env.runtime.clock().clone();
    let mut last = clock.now();
    for _ in 0..1_000 {
        let now = clock.now();
        ensure!(now >= last, "now went back from {last:?} to {now:?}");
        last = now;
    }

    // Each task reads the highest value any task has seen before it calls
    // `now`, so that value came from a call that returned before this one.
    let seen = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for task in 0..4 {
        let clock = clock.clone();
        let seen = seen.clone();
        let spawner = env.runtime.spawner().clone();
        let body = async move {
            let mut ordered = true;
            for _ in 0..200 {
                let before = seen.load(Ordering::SeqCst);
                let now = clock.now().as_nanos();
                ordered &= now >= before;
                seen.fetch_max(now, Ordering::SeqCst);
                spawner.yield_now().await;
            }
            ordered
        };
        let handle = env
            .runtime
            .spawn(name(&format!("clock-{task}"))?, body)
            .ctx("spawn")?;
        handles.push(handle);
    }
    for handle in handles {
        let ordered = handle.join().await.ctx("join")?;
        ensure!(
            ordered,
            "a task read a value of now below one that another task had already read"
        );
    }
    Ok(())
}

/// `sleep_until(d)` completes only when `now() >= d`, for one sleeper and for
/// several concurrent ones.
async fn arfc0005_s3_1_4_sleep_until_completes_only_at_its_deadline(env: Env) -> Outcome {
    let clock = env.runtime.clock().clone();
    for wait in [
        Duration::from_nanos(1),
        Duration::from_micros(10),
        Duration::from_millis(1),
        Duration::from_millis(20),
    ] {
        let deadline = clock.now().saturating_add(wait);
        clock.sleep_until(deadline).await;
        let now = clock.now();
        ensure!(
            now >= deadline,
            "sleep_until({deadline:?}) completed at {now:?}"
        );
    }

    let mut handles = Vec::new();
    for task in 0..8u64 {
        let clock = clock.clone();
        let deadline = clock
            .now()
            .saturating_add(Duration::from_millis(1 + (task * 7) % 13));
        let body = async move {
            clock.sleep_until(deadline).await;
            clock.now() >= deadline
        };
        let handle = env
            .runtime
            .spawn(name(&format!("sleeper-{task}"))?, body)
            .ctx("spawn")?;
        handles.push(handle);
    }
    for handle in handles {
        ensure!(
            handle.join().await.ctx("join")?,
            "a concurrent sleeper woke before its deadline"
        );
    }
    Ok(())
}

/// With `d` at or before `now()`, `sleep_until(d)` completes on its first
/// poll.
async fn arfc0005_s3_1_4_sleep_until_a_due_deadline_is_ready_on_first_poll(env: Env) -> Outcome {
    let clock = env.runtime.clock().clone();
    let earlier = clock.now();
    sleep(&env, Duration::from_millis(1)).await;
    for deadline in [Instant::ZERO, earlier, clock.now()] {
        let mut sleeper = clock.sleep_until(deadline);
        ensure!(
            poll_once(&mut sleeper).await.is_ready(),
            "sleep_until({deadline:?}) was not ready on its first poll"
        );
    }
    Ok(())
}

/// `sleep_until(Instant::MAX)` never completes.
async fn arfc0005_s3_1_4_sleep_until_max_never_completes(env: Env) -> Outcome {
    let clock = env.runtime.clock().clone();
    let mut sleeper = clock.sleep_until(Instant::MAX);
    ensure!(
        poll_once(&mut sleeper).await.is_pending(),
        "sleep_until(MAX) was ready on its first poll"
    );
    ensure!(
        within(&env, Duration::from_millis(100), sleeper)
            .await
            .is_none(),
        "sleep_until(MAX) completed"
    );
    Ok(())
}

/// `mem`: with a rate offset of 0, a task woken by `sleep_until(d)` reads `d`
/// itself from `now`.
async fn arfc0005_s3_1_4_mem_woken_sleeper_reads_its_deadline(env: Env) -> Outcome {
    let clock = env.runtime.clock().clone();
    for wait in [
        Duration::from_nanos(1),
        Duration::from_nanos(999),
        Duration::from_millis(3),
        Duration::from_secs(5),
    ] {
        let deadline = clock.now().saturating_add(wait);
        clock.sleep_until(deadline).await;
        ensure_eq!(
            clock.now(),
            deadline,
            "now read on the poll that a sleep's timer woke"
        );
    }
    Ok(())
}

/// Dropping a `sleep_until` future, polled or not, cancels it without a
/// panic, and the clock keeps serving other sleepers.
async fn arfc0005_s3_1_3_dropped_sleeps_cancel_without_panic(env: Env) -> Outcome {
    let clock = env.runtime.clock().clone();
    for index in 0..200u64 {
        let mut sleeper = clock.sleep_until(
            clock
                .now()
                .saturating_add(Duration::from_millis(1 + index % 5)),
        );
        if index % 2 == 0 {
            let _ = poll_once(&mut sleeper).await;
        }
        drop(sleeper);
    }
    sleep(&env, SETTLE).await;
    let deadline = clock.now().saturating_add(Duration::from_millis(1));
    clock.sleep_until(deadline).await;
    ensure!(
        clock.now() >= deadline,
        "a sleep after dropped sleeps woke early"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alloy_rt::Instant;

    use crate::support::Gen;

    const MAX_NANOS: u64 = (1 << 63) - 1;

    /// `Instant` is usable in constant expressions (section 2.1).
    const THREE: Instant = Instant::from_nanos(3);
    const THREE_NANOS: u64 = THREE.as_nanos();

    #[test]
    fn arfc0005_s2_1_instant_bounds() {
        assert_eq!(Instant::ZERO.as_nanos(), 0);
        assert_eq!(Instant::MAX.as_nanos(), MAX_NANOS);
        assert_eq!(THREE_NANOS, 3);
        assert_eq!(Instant::from_nanos(MAX_NANOS), Instant::MAX);
        assert_eq!(Instant::from_nanos(MAX_NANOS + 1), Instant::MAX);
        assert_eq!(Instant::from_nanos(u64::MAX), Instant::MAX);
        assert!(Instant::ZERO < Instant::MAX);
    }

    #[test]
    fn arfc0005_s2_1_instant_checked_add_is_none_above_max() {
        let near = Instant::from_nanos(MAX_NANOS - 1);
        assert_eq!(
            near.checked_add(Duration::from_nanos(1)),
            Some(Instant::MAX)
        );
        assert_eq!(near.checked_add(Duration::from_nanos(2)), None);
        assert_eq!(Instant::ZERO.checked_add(Duration::MAX), None);
        assert_eq!(Instant::MAX.checked_add(Duration::ZERO), Some(Instant::MAX));
    }

    #[test]
    fn arfc0005_s2_1_instant_saturating_add_gives_max_above_max() {
        let near = Instant::from_nanos(MAX_NANOS - 1);
        assert_eq!(near.saturating_add(Duration::from_nanos(5)), Instant::MAX);
        assert_eq!(Instant::ZERO.saturating_add(Duration::MAX), Instant::MAX);
        assert_eq!(
            Instant::from_nanos(7).saturating_add(Duration::from_nanos(8)),
            Instant::from_nanos(15)
        );
    }

    #[test]
    fn arfc0005_s2_1_instant_saturating_duration_since_is_zero_when_earlier_is_later() {
        let ten = Instant::from_nanos(10);
        let four = Instant::from_nanos(4);
        assert_eq!(ten.saturating_duration_since(four), Duration::from_nanos(6));
        assert_eq!(four.saturating_duration_since(ten), Duration::ZERO);
        assert_eq!(ten.saturating_duration_since(ten), Duration::ZERO);
        assert_eq!(
            Instant::MAX.saturating_duration_since(Instant::ZERO),
            Duration::from_nanos(MAX_NANOS)
        );
    }

    /// The arithmetic agrees with a 128-bit model on generated values.
    #[test]
    fn arfc0005_s2_1_instant_arithmetic_matches_a_model() {
        for seed in 0..16 {
            let mut generator = Gen::new(seed);
            for _ in 0..1_000 {
                let a = generator.next_u64() >> (generator.below(64) as u32);
                let b = generator.next_u64() >> (generator.below(64) as u32);
                let d = Duration::new(
                    generator.next_u64() >> (generator.below(64) as u32),
                    (generator.below(1_000_000_000)) as u32,
                );
                let instant = Instant::from_nanos(a);
                let model_a = a.min(MAX_NANOS);
                assert_eq!(instant.as_nanos(), model_a);

                let sum = u128::from(model_a) + d.as_nanos();
                let expected = if sum > u128::from(MAX_NANOS) {
                    None
                } else {
                    Some(Instant::from_nanos(sum as u64))
                };
                assert_eq!(instant.checked_add(d), expected);
                assert_eq!(instant.saturating_add(d), expected.unwrap_or(Instant::MAX));

                let other = Instant::from_nanos(b);
                let model_b = b.min(MAX_NANOS);
                assert_eq!(
                    instant.saturating_duration_since(other),
                    Duration::from_nanos(model_a.saturating_sub(model_b))
                );
            }
        }
    }
}
