//! The `Runtime` bundle and its helpers, clauses 2.5 and 3.1.14. They have
//! no suite of their own in section 5; they run with every implementation
//! because they are built from its five instances.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::time::Duration;

use alloy_rt::{
    ClockSeam, DiskIoSeam, Elapsed, JoinError, NetSeam, RngSeam, Runtime, RuntimeConfig, SpawnSeam,
};
use alloy_seam::{
    Binding, BuildError, ConfigValue, Detail, Factory, ImplName, RegistryBuilder, Seam,
};

use crate::support::{
    Ctx, Gate, SETTLE, SetOnDrop, eventually, flag, is_set, name, poll_once, sleep, within,
};
use crate::{Applies, Check, Env, Implementation, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0005#2.5",
            Applies::All,
            arfc0005_s2_5_runtime_new_holds_the_given_instances
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_from_registry_builds_in_order
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_from_registry_stops_at_the_first_failure
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_spawn_goes_through_the_spawn_instance
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_join_handle_returns_the_output_or_cancelled
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_join_handle_returns_panicked,
            panics_a_task
        ),
        check!(
            "arfc-0005#2.5",
            Applies::All,
            arfc0005_s2_5_dropping_a_join_handle_detaches
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_sleep_waits_its_duration
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_sleep_deadline_is_fixed_at_the_call
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_timeout_returns_ok_when_the_future_is_ready
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_timeout_zero_elapses_on_first_poll_unless_ready
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_timeout_deadline_is_fixed_at_the_call
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_timeout_polls_the_future_before_the_timer
        ),
        check!(
            "arfc-0005#3.1.14",
            Applies::All,
            arfc0005_s3_1_14_timeout_elapses_at_its_deadline_and_drops_the_future
        ),
    ]
}

/// The order `from_registry` builds the five instances in.
const BUILD_ORDER: [&str; 5] = ["clock", "rng", "spawn", "net", "disk-io"];

type Log = Arc<Mutex<Vec<&'static str>>>;

/// For each instance a wrapped factory handed out: its kind, its strong
/// count when handed out, and a probe of its current strong count.
type Probes = Arc<Mutex<Vec<(&'static str, usize, Box<dyn Fn() -> usize + Send + Sync>)>>>;

/// What `build_logged` observed: the result of `from_registry`, the
/// factory calls in order, and the instances the factories handed out.
type Built = (Result<Runtime, BuildError>, Vec<&'static str>, Probes);

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A factory for seam `S` that records its call in `log`, then fails with
/// `BuildError::Failed(kind)` when `fails`, and otherwise builds a real
/// instance of the implementation under test from the harness's registry.
fn logged<S: Seam<Context = ()>>(
    env: &Env,
    real: &Binding,
    kind: &'static str,
    fails: bool,
    log: &Log,
    probes: &Probes,
) -> Factory<S> {
    let registry = env.registry.clone();
    let real_name = real.name.clone();
    let log = log.clone();
    let probes = probes.clone();
    Arc::new(move |_: &(), config: &ConfigValue| {
        locked(&log).push(kind);
        if fails {
            return Err(BuildError::Failed(Detail::new(kind)));
        }
        let instance = registry.instantiate::<S>(&(), &real_name, config)?;
        let weak = Arc::downgrade(&instance);
        locked(&probes).push((
            kind,
            Arc::strong_count(&instance),
            Box::new(move || weak.strong_count()),
        ));
        Ok(instance)
    })
}

/// Runs `Runtime::from_registry` over a registry whose factories log their
/// calls and, for `failing`, fail.
fn build_logged(env: &Env, failing: Option<&'static str>) -> Result<Built, crate::Failure> {
    let log: Log = Arc::default();
    let probes: Probes = Arc::default();
    let test_name = ImplName::new("conformance").ctx("implementation name")?;
    let fails = |kind: &str| failing == Some(kind);
    let config = &env.config;
    let mut builder = RegistryBuilder::new();
    builder
        .register::<ClockSeam>(
            test_name.clone(),
            logged::<ClockSeam>(env, &config.clock, "clock", fails("clock"), &log, &probes),
        )
        .ctx("register clock")?;
    builder
        .register::<NetSeam>(
            test_name.clone(),
            logged::<NetSeam>(env, &config.net, "net", fails("net"), &log, &probes),
        )
        .ctx("register net")?;
    builder
        .register::<DiskIoSeam>(
            test_name.clone(),
            logged::<DiskIoSeam>(
                env,
                &config.disk_io,
                "disk-io",
                fails("disk-io"),
                &log,
                &probes,
            ),
        )
        .ctx("register disk-io")?;
    builder
        .register::<RngSeam>(
            test_name.clone(),
            logged::<RngSeam>(env, &config.rng, "rng", fails("rng"), &log, &probes),
        )
        .ctx("register rng")?;
    builder
        .register::<SpawnSeam>(
            test_name.clone(),
            logged::<SpawnSeam>(env, &config.spawn, "spawn", fails("spawn"), &log, &probes),
        )
        .ctx("register spawn")?;
    let registry = builder.build();
    let rebind = |binding: &Binding| Binding {
        name: test_name.clone(),
        config: binding.config.clone(),
    };
    let wrapped = RuntimeConfig {
        clock: rebind(&config.clock),
        net: rebind(&config.net),
        disk_io: rebind(&config.disk_io),
        rng: rebind(&config.rng),
        spawn: rebind(&config.spawn),
    };
    let result = Runtime::from_registry(&registry, &wrapped);
    let calls = locked(&log).clone();
    Ok((result, calls, probes))
}

/// `Runtime::new` holds the instances it was given, and clones share them.
async fn arfc0005_s2_5_runtime_new_holds_the_given_instances(env: Env) -> Outcome {
    let source = &env.runtime;
    let built = Runtime::new(
        source.clock().clone(),
        source.net().clone(),
        source.disk_io().clone(),
        source.rng().clone(),
        source.spawner().clone(),
    );
    for runtime in [&built, &built.clone()] {
        ensure!(
            Arc::ptr_eq(runtime.clock(), source.clock()),
            "clock instance"
        );
        ensure!(Arc::ptr_eq(runtime.net(), source.net()), "net instance");
        ensure!(
            Arc::ptr_eq(runtime.disk_io(), source.disk_io()),
            "disk-io instance"
        );
        ensure!(Arc::ptr_eq(runtime.rng(), source.rng()), "rng instance");
        ensure!(
            Arc::ptr_eq(runtime.spawner(), source.spawner()),
            "spawn instance"
        );
    }
    Ok(())
}

/// `from_registry` builds the five instances in the order clock, rng, spawn,
/// net, disk-io.
async fn arfc0005_s3_1_14_from_registry_builds_in_order(env: Env) -> Outcome {
    let (result, calls, _probes) = build_logged(&env, None)?;
    result.ctx("from_registry")?;
    ensure_eq!(
        calls,
        BUILD_ORDER.to_vec(),
        "factory calls of from_registry"
    );
    Ok(())
}

/// On the first failure `from_registry` drops what it built and returns that
/// error, calling no later factory.
async fn arfc0005_s3_1_14_from_registry_stops_at_the_first_failure(env: Env) -> Outcome {
    for (index, failing) in BUILD_ORDER.iter().enumerate() {
        let (result, calls, probes) = build_logged(&env, Some(failing))?;
        match result {
            Err(error) => ensure_eq!(
                error,
                BuildError::Failed(Detail::new(failing)),
                "from_registry with {failing} failing"
            ),
            Ok(_) => {
                return Err(crate::support::fail(format!(
                    "from_registry succeeded with {failing} failing"
                )));
            }
        }
        ensure_eq!(
            calls,
            BUILD_ORDER[..=index].to_vec(),
            "factory calls with {failing} failing"
        );
        for (kind, handed_out, probe) in locked(&probes).iter() {
            ensure_eq!(
                probe(),
                handed_out - 1,
                "references to the {kind} instance after from_registry failed on {failing}"
            );
        }
    }
    Ok(())
}

/// `Runtime::spawn` spawns through the `spawn` instance: its ids come from
/// that instance and are never reused by it.
async fn arfc0005_s3_1_14_spawn_goes_through_the_spawn_instance(env: Env) -> Outcome {
    let runtime = env.runtime.clone();
    let direct = runtime
        .spawner()
        .spawn(name("direct-1")?, Box::pin(async {}))
        .ctx("spawn")?;
    let typed = runtime
        .spawn(name("typed")?, async {})
        .ctx("Runtime::spawn")?;
    let after = runtime
        .spawner()
        .spawn(name("direct-2")?, Box::pin(async {}))
        .ctx("spawn")?;
    let ids = [direct.id(), typed.id(), after.id()];
    ensure!(
        ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2],
        "ids {ids:?} repeat within one spawn instance"
    );
    if env.implementation == Implementation::Mem {
        ensure!(
            ids[1].0 == ids[0].0 + 1 && ids[2].0 == ids[1].0 + 1,
            "mem ids {ids:?} are not consecutive, so Runtime::spawn did not use the host's spawn instance"
        );
    }
    direct.join().await.ctx("join")?;
    typed.join().await.ctx("join")?;
    after.join().await.ctx("join")?;
    Ok(())
}

/// `JoinHandle::join` returns the task's output, or `Cancelled` for a task
/// cancelled before it completed.
async fn arfc0005_s3_1_14_join_handle_returns_the_output_or_cancelled(env: Env) -> Outcome {
    let runtime = env.runtime.clone();
    let number = runtime
        .spawn(name("number")?, async { 41u32 + 1 })
        .ctx("spawn")?;
    ensure_eq!(number.join().await, Ok(42), "join of a task returning 42");
    let text = runtime
        .spawn(name("text")?, async { String::from("alloy") })
        .ctx("spawn")?;
    ensure_eq!(
        text.join().await,
        Ok(String::from("alloy")),
        "join of a task returning text"
    );
    let gate = Arc::new(Gate::default());
    let waiting = runtime
        .spawn(name("waiting")?, {
            let gate = gate.clone();
            async move {
                gate.wait().await;
                7u8
            }
        })
        .ctx("spawn")?;
    waiting.cancel();
    waiting.cancel();
    ensure_eq!(
        waiting.join().await,
        Err(JoinError::Cancelled),
        "join of a cancelled typed task"
    );
    Ok(())
}

/// The suite's deliberate panic, with a value type.
fn explode() -> u8 {
    panic!("conformance-rt: this task panics on purpose (ARFC 0005 clause 3.1.14)");
}

/// `JoinHandle::join` returns `Panicked` for a task that panicked.
async fn arfc0005_s3_1_14_join_handle_returns_panicked(env: Env) -> Outcome {
    let handle = env
        .runtime
        .spawn(name("typed-panic")?, async { explode() })
        .ctx("spawn")?;
    ensure_eq!(
        handle.join().await,
        Err(JoinError::Panicked),
        "join of a typed task that panicked"
    );
    Ok(())
}

/// Dropping a `JoinHandle` detaches the task, which keeps running.
async fn arfc0005_s2_5_dropping_a_join_handle_detaches(env: Env) -> Outcome {
    let gate = Arc::new(Gate::default());
    let done = flag();
    let handle = env
        .runtime
        .spawn(name("typed-detached")?, {
            let gate = gate.clone();
            let done = done.clone();
            async move {
                gate.wait().await;
                done.store(true, Ordering::SeqCst);
            }
        })
        .ctx("spawn")?;
    drop(handle);
    gate.open();
    eventually(&env, "the detached typed task completes", || is_set(&done)).await
}

/// `sleep(d)` completes no earlier than `d` after the call.
async fn arfc0005_s3_1_14_sleep_waits_its_duration(env: Env) -> Outcome {
    let runtime = env.runtime.clone();
    for wait in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_millis(2),
        Duration::from_millis(20),
    ] {
        let before = runtime.clock().now();
        runtime.sleep(wait).await;
        let now = runtime.clock().now();
        ensure!(
            now >= before.saturating_add(wait),
            "sleep({wait:?}) called at {before:?} completed at {now:?}"
        );
    }
    let mut forever = runtime.sleep(Duration::MAX);
    ensure!(
        poll_once(&mut forever).await.is_pending(),
        "sleep(Duration::MAX) was ready at once"
    );
    ensure!(
        within(&env, SETTLE, forever).await.is_none(),
        "sleep(Duration::MAX) completed"
    );
    Ok(())
}

/// `sleep`'s deadline is fixed when it is called, not at its first poll.
async fn arfc0005_s3_1_14_sleep_deadline_is_fixed_at_the_call(env: Env) -> Outcome {
    let mut stored = env.runtime.sleep(Duration::from_millis(20));
    sleep(&env, Duration::from_millis(40)).await;
    ensure!(
        poll_once(&mut stored).await.is_ready(),
        "a sleep first polled after its deadline was not ready"
    );
    Ok(())
}

/// `timeout` returns `Ok` with the future's output when it is ready before
/// the deadline, including a deadline of `Duration::MAX`.
async fn arfc0005_s3_1_14_timeout_returns_ok_when_the_future_is_ready(env: Env) -> Outcome {
    let runtime = env.runtime.clone();
    let quick = runtime.timeout(Duration::from_secs(1), async { 9u8 }).await;
    ensure_eq!(quick, Ok(9), "timeout of a ready future");
    let later = runtime
        .timeout(Duration::from_millis(500), {
            let env = env.clone();
            async move {
                sleep(&env, Duration::from_millis(5)).await;
                3u8
            }
        })
        .await;
    ensure_eq!(later, Ok(3), "timeout of a future that completes in time");
    let unbounded = runtime.timeout(Duration::MAX, async { 1u8 }).await;
    ensure_eq!(unbounded, Ok(1), "timeout with Duration::MAX");
    Ok(())
}

/// `timeout(Duration::ZERO, f)` returns `Elapsed` on its first poll unless
/// `f` is ready then.
async fn arfc0005_s3_1_14_timeout_zero_elapses_on_first_poll_unless_ready(env: Env) -> Outcome {
    let runtime = env.runtime.clone();
    let mut pending = runtime.timeout(Duration::ZERO, std::future::pending::<()>());
    ensure_eq!(
        poll_once(&mut pending).await,
        Poll::Ready(Err(Elapsed)),
        "first poll of timeout(ZERO, pending)"
    );
    let mut ready = runtime.timeout(Duration::ZERO, async { 5u8 });
    ensure_eq!(
        poll_once(&mut ready).await,
        Poll::Ready(Ok(5)),
        "first poll of timeout(ZERO, ready)"
    );
    Ok(())
}

/// `timeout`'s timer starts at the call: one first polled after its deadline
/// returns `Elapsed` on that poll when the future is not ready.
async fn arfc0005_s3_1_14_timeout_deadline_is_fixed_at_the_call(env: Env) -> Outcome {
    let mut stored = env
        .runtime
        .timeout(Duration::from_millis(20), std::future::pending::<()>());
    sleep(&env, Duration::from_millis(40)).await;
    ensure_eq!(
        poll_once(&mut stored).await,
        Poll::Ready(Err(Elapsed)),
        "first poll of a timeout after its deadline"
    );
    Ok(())
}

/// `timeout` polls the future before the timer: one first polled after its
/// deadline returns `Ok` when the future is ready on that poll.
async fn arfc0005_s3_1_14_timeout_polls_the_future_before_the_timer(env: Env) -> Outcome {
    let ready = flag();
    let future = {
        let ready = ready.clone();
        std::future::poll_fn(move |_| {
            if is_set(&ready) {
                Poll::Ready(11u8)
            } else {
                Poll::Pending
            }
        })
    };
    let mut stored = env.runtime.timeout(Duration::from_millis(20), future);
    ensure!(
        poll_once(&mut stored).await.is_pending(),
        "a timeout before its deadline with a pending future"
    );
    sleep(&env, Duration::from_millis(40)).await;
    ready.store(true, Ordering::SeqCst);
    ensure_eq!(
        poll_once(&mut stored).await,
        Poll::Ready(Ok(11)),
        "a poll after the deadline with the future ready"
    );
    Ok(())
}

/// `timeout` returns `Elapsed` no earlier than its deadline, and drops the
/// future when it does.
async fn arfc0005_s3_1_14_timeout_elapses_at_its_deadline_and_drops_the_future(
    env: Env,
) -> Outcome {
    let runtime = env.runtime.clone();
    let dropped = flag();
    let future = {
        let guard = SetOnDrop(dropped.clone());
        async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        }
    };
    let before = runtime.clock().now();
    let limit = Duration::from_millis(30);
    let result = runtime.timeout(limit, future).await;
    let now = runtime.clock().now();
    ensure_eq!(result, Err(Elapsed), "timeout of a pending future");
    ensure!(
        now >= before.saturating_add(limit),
        "timeout({limit:?}) called at {before:?} elapsed at {now:?}"
    );
    ensure!(
        is_set(&dropped),
        "timeout returned Elapsed without dropping the future"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_rt::{
        Clock, ClockSeam, DiskIo, DiskIoSeam, Elapsed, Net, NetSeam, Rng, RngSeam, RuntimeConfig,
        Spawn, SpawnSeam,
    };
    use alloy_seam::{Binding, ConfigValue, ImplName, Seam, SwapClass};

    fn marker<S: Seam<Context = ()>>(kind: &str) {
        assert_eq!(S::KIND, kind);
        assert_eq!(S::SWAP, SwapClass::Restart);
    }

    fn object<S: Seam<Object = T>, T: ?Sized>() {}

    #[test]
    fn arfc0005_s2_5_seam_markers_declare_kind_context_swap_and_object() {
        marker::<ClockSeam>("clock");
        marker::<NetSeam>("net");
        marker::<DiskIoSeam>("disk-io");
        marker::<RngSeam>("rng");
        marker::<SpawnSeam>("spawn");
        object::<ClockSeam, dyn Clock>();
        object::<NetSeam, dyn Net>();
        object::<DiskIoSeam, dyn DiskIo>();
        object::<RngSeam, dyn Rng>();
        object::<SpawnSeam, dyn Spawn>();
    }

    #[test]
    fn arfc0005_s2_5_runtime_config_all_binds_every_seam_to_defaults() {
        let name = ImplName::new("mem").expect("a valid name");
        let expected = Binding {
            name: name.clone(),
            config: ConfigValue::defaults(1).expect("version 1 is valid"),
        };
        let config = RuntimeConfig::all(name);
        for binding in [
            &config.clock,
            &config.net,
            &config.disk_io,
            &config.rng,
            &config.spawn,
        ] {
            assert_eq!(binding, &expected);
        }
    }

    #[test]
    fn arfc0005_s2_5_elapsed_text() {
        assert_eq!(Elapsed.to_string(), "deadline elapsed");
    }
}
