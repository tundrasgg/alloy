//! Suite `spawn` (ARFC 0005 section 5): the `Spawn` seam and `TaskHandle`,
//! clauses 2.2, 3.1.3, and 3.1.6.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::Context;
use std::time::Duration;

use alloy_rt::{BoxFuture, JoinError, Spawn, SpawnError, SpawnSeam};

use crate::support::{
    Ctx, Gate, SETTLE, SetOnDrop, counting_waker, eventually, fail, flag, fresh, is_set, name,
    poll_once, sleep,
};
use crate::{Applies, Check, Env, Implementation, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0005#3.1.6",
            Applies::All,
            arfc0005_s3_1_6_spawn_does_not_poll_on_the_callers_stack
        ),
        check!(
            "arfc-0005#3.1.6",
            Applies::All,
            arfc0005_s3_1_6_completed_task_joins_ok
        ),
        check!(
            "arfc-0005#3.1.6",
            Applies::All,
            arfc0005_s3_1_6_cancelled_task_is_not_polled_again_and_joins_cancelled
        ),
        check!(
            "arfc-0005#2.2",
            Applies::All,
            arfc0005_s2_2_cancel_is_idempotent_and_spares_ended_tasks
        ),
        check!(
            "arfc-0005#3.1.6",
            Applies::All,
            arfc0005_s3_1_6_panicking_task_joins_panicked_and_others_run,
            panics_a_task
        ),
        check!(
            "arfc-0005#3.1.6",
            Applies::All,
            arfc0005_s3_1_6_join_completes_after_the_future_is_dropped
        ),
        check!(
            "arfc-0005#3.1.6",
            Applies::All,
            arfc0005_s3_1_6_dropping_the_task_handle_detaches
        ),
        check!(
            "arfc-0005#3.1.6",
            Applies::All,
            arfc0005_s3_1_6_dropping_the_join_future_detaches
        ),
        check!(
            "arfc-0005#3.1.6",
            Applies::All,
            arfc0005_s3_1_6_an_instance_holds_at_most_1048576_live_tasks
        ),
        check!(
            "arfc-0005#2.2",
            Applies::All,
            arfc0005_s2_2_task_ids_are_never_reused
        ),
        check!(
            "arfc-0005#3.1.6",
            Applies::Only(Implementation::Mem),
            arfc0005_s3_1_6_mem_task_ids_count_up_by_one
        ),
        check!(
            "arfc-0005#2.2",
            Applies::All,
            arfc0005_s2_2_yield_now_completes_on_its_second_poll
        ),
    ]
}

/// The live task limit of one `spawn` instance (clause 3.1.6).
const LIVE_TASKS: usize = 1_048_576;

/// A task that never completes until cancelled.
fn forever() -> BoxFuture<'static, ()> {
    Box::pin(std::future::pending())
}

/// A task that counts the times it runs and sets `dropped` when its future
/// is dropped. It sleeps 1 ms between runs, so that a `mem` scheduler can
/// advance time, and never completes on its own.
fn counted(env: &Env, polls: &Arc<AtomicU64>, dropped: &Arc<AtomicBool>) -> BoxFuture<'static, ()> {
    let clock = env.runtime.clock().clone();
    let polls = polls.clone();
    let guard = SetOnDrop(dropped.clone());
    Box::pin(async move {
        let _guard = guard;
        loop {
            polls.fetch_add(1, Ordering::SeqCst);
            clock
                .sleep_until(clock.now().saturating_add(Duration::from_millis(1)))
                .await;
        }
    })
}

thread_local! {
    /// Set only on the calling thread, and only while that thread is inside
    /// `spawn`. A task that reads it as set is being polled on the caller's
    /// stack: a poll on any other thread, or after `spawn` returned, reads it
    /// as clear.
    static INSIDE_SPAWN: Cell<bool> = const { Cell::new(false) };
}

/// `spawn` returns without polling the task on the caller's stack.
async fn arfc0005_s3_1_6_spawn_does_not_poll_on_the_callers_stack(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let polled_inside = flag();
    let task: BoxFuture<'static, ()> = {
        let polled_inside = polled_inside.clone();
        Box::pin(async move {
            if INSIDE_SPAWN.with(Cell::get) {
                polled_inside.store(true, Ordering::SeqCst);
            }
        })
    };
    INSIDE_SPAWN.with(|inside| inside.set(true));
    let handle = spawner.spawn(name("on-stack")?, task);
    INSIDE_SPAWN.with(|inside| inside.set(false));
    handle.ctx("spawn")?.join().await.ctx("join")?;
    ensure!(
        !is_set(&polled_inside),
        "the task was polled inside spawn, on the caller's stack"
    );
    Ok(())
}

/// A task is polled until it completes, and `join` then returns `Ok(())`.
async fn arfc0005_s3_1_6_completed_task_joins_ok(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let done = flag();
    let task: BoxFuture<'static, ()> = {
        let done = done.clone();
        let spawner = spawner.clone();
        Box::pin(async move {
            for _ in 0..10 {
                spawner.yield_now().await;
            }
            done.store(true, Ordering::SeqCst);
        })
    };
    let handle = spawner.spawn(name("completes")?, task).ctx("spawn")?;
    ensure_eq!(handle.join().await, Ok(()), "join of a completed task");
    ensure!(
        is_set(&done),
        "join returned before the task ran to its end"
    );
    Ok(())
}

/// After `cancel` returns, the task is not polled again (one poll already
/// running may finish), its future is dropped before `join` completes, and
/// `join` returns `Cancelled`.
async fn arfc0005_s3_1_6_cancelled_task_is_not_polled_again_and_joins_cancelled(
    env: Env,
) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let polls = Arc::new(AtomicU64::new(0));
    let dropped = flag();
    let handle = spawner
        .spawn(name("cancelled")?, counted(&env, &polls, &dropped))
        .ctx("spawn")?;
    eventually(&env, "the task is polled five times", || {
        polls.load(Ordering::SeqCst) >= 5
    })
    .await?;
    handle.cancel();
    let at_cancel = polls.load(Ordering::SeqCst);
    let joined = handle.join().await;
    ensure_eq!(
        joined,
        Err(JoinError::Cancelled),
        "join of a cancelled task"
    );
    ensure!(
        is_set(&dropped),
        "join completed before the task's future was dropped"
    );
    let at_join = polls.load(Ordering::SeqCst);
    ensure!(
        at_join <= at_cancel + 1,
        "the task was polled {} times after cancel returned",
        at_join - at_cancel
    );
    sleep(&env, SETTLE).await;
    ensure_eq!(
        polls.load(Ordering::SeqCst),
        at_join,
        "polls of a task after join returned Cancelled"
    );
    Ok(())
}

/// `cancel` is idempotent and has no effect on a task that has ended.
async fn arfc0005_s2_2_cancel_is_idempotent_and_spares_ended_tasks(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let done = flag();
    let task: BoxFuture<'static, ()> = {
        let done = done.clone();
        Box::pin(async move { done.store(true, Ordering::SeqCst) })
    };
    let ended = spawner.spawn(name("ended")?, task).ctx("spawn")?;
    eventually(&env, "the task ends", || is_set(&done)).await?;
    sleep(&env, Duration::from_millis(1)).await;
    for _ in 0..3 {
        ended.cancel();
    }
    ensure_eq!(
        ended.join().await,
        Ok(()),
        "join of an ended task after cancel"
    );

    let pending = spawner.spawn(name("pending")?, forever()).ctx("spawn")?;
    for _ in 0..3 {
        pending.cancel();
    }
    ensure_eq!(
        pending.join().await,
        Err(JoinError::Cancelled),
        "join of a task cancelled three times"
    );
    Ok(())
}

/// The suite's deliberate panic.
fn explode() {
    panic!("conformance-rt: this task panics on purpose (ARFC 0005 clause 3.1.6)");
}

/// A task that panics ends with `join` returning `Panicked`, and every other
/// task keeps running.
async fn arfc0005_s3_1_6_panicking_task_joins_panicked_and_others_run(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let gate = Arc::new(Gate::default());
    let survivor_done = flag();
    let survivor: BoxFuture<'static, ()> = {
        let gate = gate.clone();
        let done = survivor_done.clone();
        Box::pin(async move {
            gate.wait().await;
            done.store(true, Ordering::SeqCst);
        })
    };
    let survivor = spawner.spawn(name("survivor")?, survivor).ctx("spawn")?;
    let panicking: BoxFuture<'static, ()> = Box::pin(async move { explode() });
    let panicking = spawner.spawn(name("panics")?, panicking).ctx("spawn")?;
    ensure_eq!(
        panicking.join().await,
        Err(JoinError::Panicked),
        "join of a task that panicked"
    );
    gate.open();
    ensure_eq!(
        survivor.join().await,
        Ok(()),
        "join of a task that ran beside a panicking one"
    );
    ensure!(is_set(&survivor_done), "the surviving task did not finish");
    let after: BoxFuture<'static, ()> = Box::pin(async {});
    let after = spawner
        .spawn(name("after")?, after)
        .ctx("spawn after a panic")?;
    ensure_eq!(
        after.join().await,
        Ok(()),
        "join of a task spawned after a panic"
    );
    Ok(())
}

/// `join` completes after the task has ended and its future has been
/// dropped.
async fn arfc0005_s3_1_6_join_completes_after_the_future_is_dropped(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let dropped = flag();
    let task: BoxFuture<'static, ()> = {
        let guard = SetOnDrop(dropped.clone());
        let spawner = spawner.clone();
        Box::pin(async move {
            let _guard = guard;
            spawner.yield_now().await;
        })
    };
    let handle = spawner.spawn(name("drops")?, task).ctx("spawn")?;
    ensure_eq!(handle.join().await, Ok(()), "join");
    ensure!(
        is_set(&dropped),
        "join completed before the task's future was dropped"
    );
    Ok(())
}

/// A task whose completion sets `done` once `gate` opens.
fn gated(gate: &Arc<Gate>, done: &Arc<AtomicBool>) -> BoxFuture<'static, ()> {
    let gate = gate.clone();
    let done = done.clone();
    Box::pin(async move {
        gate.wait().await;
        done.store(true, Ordering::SeqCst);
    })
}

/// Dropping a `TaskHandle` detaches the task, which keeps running.
async fn arfc0005_s3_1_6_dropping_the_task_handle_detaches(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let gate = Arc::new(Gate::default());
    let done = flag();
    let handle = spawner
        .spawn(name("detached")?, gated(&gate, &done))
        .ctx("spawn")?;
    drop(handle);
    sleep(&env, Duration::from_millis(1)).await;
    gate.open();
    eventually(&env, "the detached task completes", || is_set(&done)).await
}

/// Dropping a `join` future detaches the task, which keeps running.
async fn arfc0005_s3_1_6_dropping_the_join_future_detaches(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let gate = Arc::new(Gate::default());
    let done = flag();
    let handle = spawner
        .spawn(name("join-dropped")?, gated(&gate, &done))
        .ctx("spawn")?;
    let mut join = handle.join();
    ensure!(
        poll_once(&mut join).await.is_pending(),
        "join was ready before the task could end"
    );
    drop(join);
    gate.open();
    eventually(&env, "the task whose join was dropped completes", || {
        is_set(&done)
    })
    .await
}

/// An instance holds at most 1,048,576 live tasks; `spawn` beyond that
/// returns `Limit`, and a task that ends frees its place.
async fn arfc0005_s3_1_6_an_instance_holds_at_most_1048576_live_tasks(env: Env) -> Outcome {
    let spawner: Arc<dyn Spawn> = fresh::<SpawnSeam>(&env, &env.config.spawn)?;
    let gate = Arc::new(Gate::default());
    let task_name = name("live")?;
    let mut handles = Vec::with_capacity(LIVE_TASKS);
    let refused = loop {
        if handles.len() > LIVE_TASKS {
            return Err(fail("an instance accepted more than 1048576 live tasks"));
        }
        let task: BoxFuture<'static, ()> = Box::pin(gate.clone().wait());
        match spawner.spawn(task_name.clone(), task) {
            Ok(handle) => handles.push(handle),
            Err(error) => break error,
        }
    };
    ensure_eq!(
        refused,
        SpawnError::Limit,
        "spawn beyond the live task limit"
    );
    ensure_eq!(
        handles.len(),
        LIVE_TASKS,
        "live tasks a fresh instance accepted"
    );
    let last = handles.pop().ctx("the last handle")?;
    last.cancel();
    ensure_eq!(last.join().await, Err(JoinError::Cancelled), "join");
    let again = spawner
        .spawn(task_name.clone(), forever())
        .ctx("spawn after a live task ended")?;
    again.cancel();
    gate.open();
    drop(handles);
    ensure_eq!(again.join().await, Err(JoinError::Cancelled), "join");
    Ok(())
}

/// `TaskId`s are never reused by one instance.
async fn arfc0005_s2_2_task_ids_are_never_reused(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let mut ids = BTreeSet::new();
    for round in 0..10 {
        let mut handles = Vec::new();
        for task in 0..100 {
            let body: BoxFuture<'static, ()> = Box::pin(async {});
            let handle = spawner
                .spawn(name(&format!("id-{round}-{task}"))?, body)
                .ctx("spawn")?;
            ensure!(
                ids.insert(handle.id()),
                "task id {:?} was given twice",
                handle.id()
            );
            handles.push(handle);
        }
        for handle in handles {
            handle.join().await.ctx("join")?;
        }
    }
    Ok(())
}

/// `mem`: task ids count up by one per spawn on a host.
async fn arfc0005_s3_1_6_mem_task_ids_count_up_by_one(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let mut handles = Vec::new();
    for task in 0..16 {
        handles.push(
            spawner
                .spawn(name(&format!("count-{task}"))?, forever())
                .ctx("spawn")?,
        );
    }
    let ids: Vec<u64> = handles.iter().map(|handle| handle.id().0).collect();
    ensure!(ids[0] >= 1, "task ids start at 1, got {}", ids[0]);
    for pair in ids.windows(2) {
        ensure_eq!(pair[1], pair[0] + 1, "the id after {}", pair[0]);
    }
    for handle in handles {
        handle.cancel();
        handle.join().await.ok();
    }
    Ok(())
}

/// `yield_now` completes on its second poll, having woken its task on the
/// first.
async fn arfc0005_s2_2_yield_now_completes_on_its_second_poll(env: Env) -> Outcome {
    let spawner = env.runtime.spawner().clone();
    let (count, waker) = counting_waker();
    let mut cx = Context::from_waker(&waker);
    let mut yielding = spawner.yield_now();
    ensure!(
        yielding.as_mut().poll(&mut cx).is_pending(),
        "yield_now was ready on its first poll"
    );
    ensure!(
        count.get() >= 1,
        "yield_now did not wake its task on the first poll"
    );
    ensure!(
        yielding.as_mut().poll(&mut cx).is_ready(),
        "yield_now was not ready on its second poll"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_rt::{JoinError, SpawnError, TaskId, TaskName};

    use crate::support::Gen;

    fn valid(name: &str) -> bool {
        (1..=64).contains(&name.len()) && name.bytes().all(|byte| (0x21..=0x7E).contains(&byte))
    }

    fn check(name: &str) {
        match TaskName::new(name) {
            Ok(task_name) => {
                assert!(valid(name), "{name:?} was accepted");
                assert_eq!(task_name.as_str(), name);
            }
            Err(error) => {
                assert!(!valid(name), "{name:?} was refused");
                assert_eq!(error, SpawnError::InvalidName);
            }
        }
    }

    #[test]
    fn arfc0005_s2_2_task_name_accepts_exactly_its_rules() {
        let tilde64 = "~".repeat(64);
        let bang65 = "!".repeat(65);
        for name in [
            "", "a", "!", "~", "boot", "a b", "a\tb", "\u{7f}", "é", "a\0", &tilde64, &bang65,
        ] {
            check(name);
        }
        let alphabet = [
            'a', 'Z', '0', '!', '~', ' ', '\u{7f}', '\0', 'é', '\u{20}', '\u{21}',
        ];
        for seed in 0..32 {
            let mut generator = Gen::new(seed);
            for _ in 0..500 {
                let len = generator.below(70) as usize;
                let name: String = (0..len)
                    .map(|_| alphabet[generator.below(alphabet.len() as u64) as usize])
                    .collect();
                check(&name);
            }
        }
    }

    #[test]
    fn arfc0005_s2_2_errors_and_ids() {
        assert_eq!(
            SpawnError::InvalidName.to_string(),
            "task name must be 1 to 64 bytes from 0x21 to 0x7E"
        );
        assert_eq!(SpawnError::Limit.to_string(), "1048576 tasks are live");
        assert_eq!(
            SpawnError::ShuttingDown.to_string(),
            "the runtime is shutting down"
        );
        assert_eq!(JoinError::Cancelled.to_string(), "task was cancelled");
        assert_eq!(JoinError::Panicked.to_string(), "task panicked");
        assert_eq!(TaskId(7).0, 7);
        assert!(TaskId(1) < TaskId(2));
    }
}
