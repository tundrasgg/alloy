//! Hosts through the public interface: starts and their records, the `mem`
//! clock, rng, and spawn, crash and restart, stalls, clock faults, `apply`
//! and `schedule`, and histories (ARFC 0005 clauses 3.1.4 to 3.1.6, 3.3.4 to
//! 3.3.6, 3.3.8, and 3.6.2).

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use alloy_rt::{
    BoxFuture, DiskError, Instant, JoinError, NetError, RtPath, Runtime, SpawnError, TaskName,
    UdpOptions, WallTime,
};
use alloy_seam::Detail;
use alloy_sim::{
    Boot, Call, ClientId, Entry, Failure, Fault, HostBoot, HostConfig, HostId, HostState, Observer,
    Record, RunConfig, RunResult, RunSummary, Unsynced, World, WorldError, run,
};

const HOST_START: u16 = 0x0001;
const HOST_CRASH: u16 = 0x0002;
const HOST_STALL: u16 = 0x0003;
const TASK_SPAWN: u16 = 0x0010;
const TASK_POLL: u16 = 0x0011;
const TASK_END: u16 = 0x0012;
const TIMER_FIRE: u16 = 0x0020;
const CLOCK_FAULT: u16 = 0x0021;
const RNG: u16 = 0x0060;
const FAULT: u16 = 0x0070;
const NOTE: u16 = 0x00F0;
const RUN_END: u16 = 0x00FF;

const WALL_START: i64 = 1_800_000_000_000_000_000;

/// A record as an observer saw it: time in ns, host, kind, body.
type Seen = (u64, Option<u32>, u16, Vec<u8>);

/// Collects every record it sees.
#[derive(Clone, Default)]
struct Collect(Arc<Mutex<Vec<Seen>>>);

impl Collect {
    fn records(&self) -> Vec<Seen> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The records of `kind`, without their kind.
    fn of(&self, kind: u16) -> Vec<(u64, Option<u32>, Vec<u8>)> {
        self.records()
            .into_iter()
            .filter(|r| r.2 == kind)
            .map(|(time, host, _, body)| (time, host, body))
            .collect()
    }
}

impl Observer for Collect {
    fn record(&mut self, record: &Record<'_>) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push((
            record.time.as_nanos() as u64,
            record.host.map(|h| h.0),
            record.kind,
            record.body.to_vec(),
        ));
    }
}

/// A shared value the scenario and the test both see.
type Shared<T> = Arc<Mutex<T>>;

fn get<T: Clone>(shared: &Shared<T>) -> T {
    shared
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn set<T>(shared: &Shared<T>, value: T) {
    *shared.lock().unwrap_or_else(PoisonError::into_inner) = value;
}

fn boot<F, Fut>(f: F) -> Boot
where
    F: Fn(HostBoot) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |b| -> BoxFuture<'static, ()> { Box::pin(f(b)) })
}

fn fail(error: WorldError) -> Failure {
    Failure(Detail::new(&error.to_string()))
}

/// Runs `scenario` with an observer that collects every record.
fn observed<S, F>(seed: u64, scenario: S) -> (RunSummary, Collect)
where
    S: FnOnce(World) -> F,
    F: Future<Output = Result<(), Failure>> + Send + 'static,
{
    let seen = Collect::default();
    let kept = seen.clone();
    let summary = run(RunConfig::new(seed), move |world| {
        world.observe(Box::new(kept)).expect("room");
        scenario(world)
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
    (summary, seen)
}

fn u64s(values: &[u64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn spawn_body(id: u64, name: &str) -> Vec<u8> {
    let mut body = id.to_le_bytes().to_vec();
    body.push(name.len() as u8);
    body.extend_from_slice(name.as_bytes());
    body
}

fn end_body(id: u64, outcome: u8) -> Vec<u8> {
    let mut body = id.to_le_bytes().to_vec();
    body.push(outcome);
    body
}

fn clock_body(ppm: i32, step: i64) -> Vec<u8> {
    let mut body = ppm.to_le_bytes().to_vec();
    body.extend_from_slice(&step.to_le_bytes());
    body
}

fn name(text: &str) -> TaskName {
    TaskName::new(text).expect("a valid task name")
}

#[test]
fn a_host_start_writes_its_records_and_runs_boot() {
    let started: Shared<Option<(u32, u32, String)>> = Arc::default();
    let kept = Arc::clone(&started);
    let (summary, seen) = observed(1, move |world| async move {
        let config = HostConfig::new(boot(move |b: HostBoot| {
            set(&kept, Some((b.host.0, b.start, format!("{b:?}"))));
            async move {
                b.runtime.sleep(Duration::from_secs(1)).await;
            }
        }));
        assert_eq!(
            format!("{config:?}"),
            "HostConfig { clock_ppm: 0, wall_offset: 0, devices: [], capacity: 1073741824, \
             block_size: 4096, .. }"
        );
        let host = world.add_host(config).map_err(fail)?;
        assert_eq!(host, HostId(0));
        assert_eq!(world.host_state(host), Ok(HostState::Running));
        assert_eq!(format!("{world:?}"), "World { now: 0ns, hosts: 1 }");
        world.sleep(Duration::from_secs(2)).await;
        Ok(())
    });
    assert_eq!(
        get(&started),
        Some((
            0,
            1,
            "HostBoot { host: HostId(0), start: 1, .. }".to_owned()
        ))
    );
    let second = 1_000_000_000;
    assert_eq!(
        seen.records(),
        [
            (0, Some(0), HOST_START, vec![]),
            (0, Some(0), TASK_SPAWN, spawn_body(1, "boot")),
            (0, Some(0), TASK_POLL, u64s(&[1])),
            (second, Some(0), TIMER_FIRE, u64s(&[1, second])),
            (second, Some(0), TASK_POLL, u64s(&[1])),
            (second, Some(0), TASK_END, end_body(1, 0)),
            (2 * second, None, RUN_END, vec![0]),
        ]
    );
    assert_eq!(summary.events, 7);
}

#[test]
fn add_host_checks_its_config_and_unknown_hosts_are_unknown() {
    observed(2, |world| async move {
        let mut config = HostConfig::new(boot(|_| async {}));
        config.clock_ppm = 500_001;
        assert!(matches!(
            world.add_host(config),
            Err(WorldError::Invalid(_))
        ));
        let unknown = Err(WorldError::Unknown(Detail::new("host 0")));
        assert_eq!(world.host_state(HostId(0)), unknown);
        assert!(matches!(
            world.runtime(HostId(0)),
            Err(WorldError::Unknown(_))
        ));
        let fault = Fault::Stall {
            host: HostId(0),
            length: Duration::from_secs(1),
        };
        assert_eq!(world.apply(fault.clone()), unknown.clone().map(|_| ()));
        assert_eq!(
            world.schedule(Duration::from_secs(1), fault),
            unknown.map(|_| ())
        );
        Ok(())
    });
}

#[test]
fn the_mem_clock_follows_rate_offsets_steps_and_retimes_its_timers() {
    let reads: Shared<Vec<(u64, i64)>> = Arc::default();
    let kept = Arc::clone(&reads);
    let (_, seen) = observed(3, move |world| async move {
        world.sleep(Duration::from_nanos(100)).await;
        let mut config = HostConfig::new(boot(move |b: HostBoot| {
            let kept = Arc::clone(&kept);
            async move {
                let clock = b.runtime.clock().clone();
                let read = || (clock.now().as_nanos(), clock.wall().0);
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(read());
                clock.sleep_until(Instant::from_nanos(5)).await;
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(read());
                clock.sleep_until(Instant::from_nanos(1_000)).await;
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(read());
            }
        }));
        config.clock_ppm = 250_000;
        config.wall_offset = -7;
        let host = world.add_host(config).map_err(fail)?;
        world.sleep_until(Duration::from_nanos(200)).await;
        world
            .apply(Fault::ClockRate { host, ppm: 0 })
            .map_err(fail)?;
        world
            .apply(Fault::ClockStep { host, step_ns: -50 })
            .map_err(fail)?;
        let bad = Fault::ClockRate {
            host,
            ppm: -500_001,
        };
        assert!(matches!(world.apply(bad), Err(WorldError::Invalid(_))));
        world.sleep_until(Duration::from_nanos(2_000)).await;
        Ok(())
    });
    // Created at 100 with +25%: the wall clock is W + 100 at creation. The
    // deadline 5 is first reached at 100 + ceil(5 / 1.25) = 104, where the
    // rate clock reads 105. At 200 the host reads 125, the rate clock 225,
    // and the rate becomes 0, so 1,000 is reached at 1,075, where the rate
    // clock reads 1,100 and the wall clock is 50 ns behind it.
    let wall = WALL_START - 7;
    assert_eq!(
        get(&reads),
        [(0, wall + 100), (5, wall + 105), (1_000, wall + 1_100 - 50)]
    );
    assert_eq!(
        seen.of(TIMER_FIRE),
        [
            (104, Some(0), u64s(&[1, 5])),
            (1_075, Some(0), u64s(&[1, 1_000]))
        ]
    );
    assert_eq!(
        seen.of(CLOCK_FAULT),
        [
            (200, Some(0), clock_body(0, 0)),
            (200, Some(0), clock_body(0, -50))
        ]
    );
}

/// What one start of the crash host saw: its start number, its first rng
/// value, and its runtime.
type Starts = Shared<Vec<(u32, u64, Runtime)>>;

fn recording_boot(starts: &Starts, child: &Shared<Option<alloy_rt::JoinHandle<()>>>) -> Boot {
    let (starts, child) = (Arc::clone(starts), Arc::clone(child));
    boot(move |b: HostBoot| {
        let mut bytes = [0u8; 8];
        b.runtime.rng().fill(&mut bytes).expect("mem never fails");
        starts.lock().unwrap_or_else(PoisonError::into_inner).push((
            b.start,
            u64::from_le_bytes(bytes),
            b.runtime.clone(),
        ));
        let handle = b
            .runtime
            .spawn(name("child"), std::future::pending::<()>())
            .expect("spawn");
        set(&child, Some(handle));
        std::future::pending::<()>()
    })
}

#[test]
fn a_crash_ends_every_task_and_a_restart_boots_again_with_rng_replay() {
    let starts: Starts = Arc::default();
    let child: Shared<Option<alloy_rt::JoinHandle<()>>> = Arc::default();
    let (kept, held) = (Arc::clone(&starts), Arc::clone(&child));
    let (_, seen) = observed(4, move |world| async move {
        let host = world
            .add_host(HostConfig::new(recording_boot(&kept, &held)))
            .map_err(fail)?;
        world.sleep(Duration::from_millis(1)).await;
        let crash = Fault::Crash {
            host,
            after_polls: 0,
            unsynced: Unsynced::Lost,
        };
        world.apply(crash.clone()).map_err(fail)?;
        assert_eq!(world.host_state(host), Ok(HostState::Crashed));
        assert!(matches!(world.runtime(host), Err(WorldError::State(_))));
        assert!(matches!(
            world.apply(crash.clone()),
            Err(WorldError::State(_))
        ));
        let stall = Fault::Stall {
            host,
            length: Duration::from_secs(1),
        };
        assert!(matches!(world.apply(stall), Err(WorldError::State(_))));
        let handle = held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("the child's handle");
        assert_eq!(handle.join().await, Err(JoinError::Cancelled));

        // Objects of the crashed start: spawn, net, and disk refuse; the
        // clock and the rng keep working.
        let old = get(&kept)[0].2.clone();
        let spawned = old.spawner().spawn(name("late"), Box::pin(async {}));
        assert_eq!(spawned.err(), Some(SpawnError::ShuttingDown));
        assert_eq!(old.net().interfaces(None).await, Err(NetError::Closed));
        let bound = old
            .net()
            .bind_udp("127.0.0.1:0".parse().expect("addr"), UdpOptions::default());
        assert_eq!(bound.err(), Some(NetError::Closed));
        assert_eq!(
            old.disk_io().metadata(&RtPath::root()).await,
            Err(DiskError::Io)
        );
        assert_eq!(old.clock().now(), Instant::from_nanos(1_000_000));
        assert_eq!(old.rng().fill(&mut [0u8; 4]), Ok(()));

        let restart = |rng_replay| Fault::Restart { host, rng_replay };
        assert!(matches!(
            world.apply(restart(Some(0))),
            Err(WorldError::Invalid(_))
        ));
        assert!(matches!(
            world.apply(restart(Some(2))),
            Err(WorldError::Invalid(_))
        ));
        world.apply(restart(None)).map_err(fail)?;
        assert!(matches!(
            world.apply(restart(None)),
            Err(WorldError::State(_))
        ));
        world.sleep(Duration::from_millis(1)).await;
        world.apply(crash).map_err(fail)?;
        world.apply(restart(Some(1))).map_err(fail)?;
        world.sleep(Duration::from_millis(1)).await;
        Ok(())
    });
    let starts = get(&starts);
    let numbers: Vec<u32> = starts.iter().map(|s| s.0).collect();
    assert_eq!(numbers, [1, 2, 3]);
    assert_ne!(starts[1].1, starts[0].1);
    assert_eq!(starts[2].1, starts[0].1);
    // Task numbers continue across restarts: boot and child are 1 and 2,
    // then 3 and 4, then 5 and 6.
    let spawns: Vec<Vec<u8>> = seen.of(TASK_SPAWN).into_iter().map(|r| r.2).collect();
    assert_eq!(
        spawns,
        [
            spawn_body(1, "boot"),
            spawn_body(2, "child"),
            spawn_body(3, "boot"),
            spawn_body(4, "child"),
            spawn_body(5, "boot"),
            spawn_body(6, "child"),
        ]
    );
    // At a crash: host-crash, then task-end 3 for each live task in
    // ascending task number.
    let records = seen.records();
    let crash = records
        .iter()
        .position(|r| r.2 == HOST_CRASH)
        .expect("a crash");
    assert_eq!(
        records[crash..crash + 3]
            .iter()
            .map(|r| (r.2, r.3.clone()))
            .collect::<Vec<_>>(),
        [
            (HOST_CRASH, vec![]),
            (TASK_END, end_body(1, 3)),
            (TASK_END, end_body(2, 3)),
        ]
    );
    assert_eq!(seen.of(HOST_START).len(), 3);
    // Each fill writes one rng record, also after the crash.
    let fills: Vec<Vec<u8>> = seen.of(RNG).into_iter().map(|r| r.2).collect();
    assert_eq!(
        fills,
        [
            vec![8, 0, 0, 0],
            vec![4, 0, 0, 0],
            vec![8, 0, 0, 0],
            vec![8, 0, 0, 0]
        ]
    );
}

/// A boot task that yields for ever.
fn spinning() -> Boot {
    boot(|b: HostBoot| async move {
        loop {
            b.runtime.spawner().yield_now().await;
        }
    })
}

/// A boot task that sleeps 3 ns on the host's clock for ever.
fn ticking() -> Boot {
    boot(|b: HostBoot| async move {
        loop {
            b.runtime.sleep(Duration::from_nanos(3)).await;
        }
    })
}

/// The `task-poll` records between the first `fault` record and
/// `host-crash`.
fn polls_before_crash(seen: &Collect) -> usize {
    let records = seen.records();
    let armed = records
        .iter()
        .position(|r| r.2 == FAULT)
        .expect("the armed crash record");
    let crash = records
        .iter()
        .position(|r| r.2 == HOST_CRASH)
        .expect("a crash");
    records[armed..crash]
        .iter()
        .filter(|r| r.2 == TASK_POLL)
        .count()
}

#[test]
fn an_armed_crash_counts_the_polls_that_start_after_it() {
    let (_, seen) = observed(5, |world| async move {
        // The host's task is always runnable, so world time advances only
        // once the host has crashed: the crash is armed before any poll.
        let host = world.add_host(HostConfig::new(spinning())).map_err(fail)?;
        let arm = |after_polls| Fault::Crash {
            host,
            after_polls,
            unsynced: Unsynced::Seeded,
        };
        world.apply(arm(9)).map_err(fail)?;
        // A second armed crash replaces the first.
        world.apply(arm(3)).map_err(fail)?;
        world.sleep(Duration::from_nanos(1)).await;
        assert_eq!(world.host_state(host), Ok(HostState::Crashed));
        Ok(())
    });
    assert_eq!(polls_before_crash(&seen), 3);
    let armed: Vec<(Option<u32>, Vec<u8>)> =
        seen.of(FAULT).into_iter().map(|r| (r.1, r.2)).collect();
    let text = |n: &str| {
        let text = format!("crash armed after {n} polls");
        let mut body = (text.len() as u16).to_le_bytes().to_vec();
        body.extend_from_slice(text.as_bytes());
        body
    };
    assert_eq!(armed, [(Some(0), text("9")), (Some(0), text("3"))]);
}

#[test]
fn a_crash_from_the_hosts_own_task_waits_for_the_poll_to_return() {
    let seen_in_poll: Shared<Vec<String>> = Arc::default();
    let kept = Arc::clone(&seen_in_poll);
    let (_, seen) = observed(6, move |world| async move {
        let inner = world.clone();
        let config = HostConfig::new(boot(move |b: HostBoot| {
            let (world, kept) = (inner.clone(), Arc::clone(&kept));
            async move {
                let crash = Fault::Crash {
                    host: b.host,
                    after_polls: 0,
                    unsynced: Unsynced::Torn,
                };
                let first = world.apply(crash.clone());
                let state = world.host_state(b.host);
                let second = world.apply(crash);
                world.note("still running").ok();
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(format!("{first:?} {state:?} {second:?}"));
                std::future::pending::<()>().await;
            }
        }));
        let host = world.add_host(config).map_err(fail)?;
        world.sleep(Duration::from_nanos(1)).await;
        assert_eq!(world.host_state(host), Ok(HostState::Crashed));
        Ok(())
    });
    assert_eq!(get(&seen_in_poll).len(), 1);
    let line = &get(&seen_in_poll)[0];
    assert!(line.starts_with("Ok(()) Ok(Running) Err(State("), "{line}");
    let kinds: Vec<u16> = seen.records().iter().map(|r| r.2).collect();
    assert_eq!(
        kinds,
        [
            HOST_START, TASK_SPAWN, TASK_POLL, NOTE, HOST_CRASH, TASK_END, RUN_END
        ]
    );
    assert_eq!(seen.of(TASK_END)[0].2, end_body(1, 3));
}

#[test]
fn a_stall_holds_polls_but_not_timers_and_a_crash_ends_it() {
    let (_, seen) = observed(7, |world| async move {
        let config = HostConfig::new(boot(|b: HostBoot| async move {
            let clock = b.runtime.clock().clone();
            clock.sleep_until(Instant::from_nanos(50)).await;
            let at = clock.now();
            // The task reads the time it was polled at, after the stall.
            assert_eq!(at, Instant::from_nanos(110));
        }));
        let host = world.add_host(config).map_err(fail)?;
        world.sleep_until(Duration::from_nanos(10)).await;
        let stall = |ns| Fault::Stall {
            host,
            length: Duration::from_nanos(ns),
        };
        world.apply(stall(100)).map_err(fail)?;
        assert_eq!(world.host_state(host), Ok(HostState::Stalled));
        world.sleep_until(Duration::from_nanos(20)).await;
        // Ends at 50, before the stall in force ends at 110.
        world.apply(stall(30)).map_err(fail)?;
        world.sleep_until(Duration::from_nanos(109)).await;
        assert_eq!(world.host_state(host), Ok(HostState::Stalled));
        world.sleep_until(Duration::from_nanos(110)).await;
        assert_eq!(world.host_state(host), Ok(HostState::Running));
        // The task woken at the stall's end is polled at 110, before or
        // after this task by the seed; from 111 it has been.
        world.sleep_until(Duration::from_nanos(111)).await;
        world.apply(stall(1_000)).map_err(fail)?;
        world
            .apply(Fault::Crash {
                host,
                after_polls: 0,
                unsynced: Unsynced::Lost,
            })
            .map_err(fail)?;
        assert_eq!(world.host_state(host), Ok(HostState::Crashed));
        world
            .apply(Fault::Restart {
                host,
                rng_replay: None,
            })
            .map_err(fail)?;
        assert_eq!(world.host_state(host), Ok(HostState::Running));
        Ok(())
    });
    assert_eq!(
        seen.of(HOST_STALL),
        [
            (10, Some(0), u64s(&[100])),
            (20, Some(0), u64s(&[30])),
            (111, Some(0), u64s(&[1_000])),
        ]
    );
    assert_eq!(seen.of(TIMER_FIRE), [(50, Some(0), u64s(&[1, 50]))]);
    let polls: Vec<u64> = seen.of(TASK_POLL).into_iter().map(|r| r.0).collect();
    assert_eq!(polls, [0, 110]);
}

#[test]
fn scheduled_faults_apply_at_their_time_or_are_skipped_with_a_record() {
    let (_, seen) = observed(8, |world| async move {
        let host = world.add_host(HostConfig::new(ticking())).map_err(fail)?;
        world.sleep_until(Duration::from_nanos(10)).await;
        let step = Fault::ClockStep { host, step_ns: 9 };
        assert!(matches!(
            world.schedule(Duration::from_nanos(9), step.clone()),
            Err(WorldError::Invalid(_))
        ));
        assert!(matches!(
            world.schedule(
                Duration::from_nanos(20),
                Fault::ClockRate { host, ppm: 600_000 }
            ),
            Err(WorldError::Invalid(_))
        ));
        world
            .schedule(Duration::from_nanos(20), step)
            .map_err(fail)?;
        let restart = Fault::Restart {
            host,
            rng_replay: None,
        };
        world
            .schedule(Duration::from_nanos(30), restart)
            .map_err(fail)?;
        let crash = Fault::Crash {
            host,
            after_polls: 0,
            unsynced: Unsynced::Lost,
        };
        world
            .schedule(Duration::from_nanos(40), crash)
            .map_err(fail)?;
        world.sleep_until(Duration::from_nanos(50)).await;
        assert_eq!(world.host_state(host), Ok(HostState::Crashed));
        Ok(())
    });
    assert_eq!(seen.of(CLOCK_FAULT), [(20, Some(0), clock_body(0, 9))]);
    let skipped = seen.of(FAULT);
    assert_eq!(skipped.len(), 1);
    assert_eq!((skipped[0].0, skipped[0].1), (30, Some(0)));
    let text = String::from_utf8_lossy(&skipped[0].2[2..]).into_owned();
    assert!(text.starts_with("skipped Restart"), "{text}");
    assert_eq!(seen.of(HOST_CRASH), [(40, Some(0), vec![])]);
}

#[test]
fn the_registry_builds_mem_instances_of_the_start() {
    observed(9, |world| async move {
        let config = HostConfig::new(boot(|b: HostBoot| async move {
            let registry = b.registry.build();
            let mem = alloy_seam::ImplName::new("mem").expect("name");
            let runtime =
                Runtime::from_registry(&registry, &alloy_rt::RuntimeConfig::all(mem.clone()))
                    .expect("mem builds");
            let handle = runtime
                .spawn(name("via-registry"), async { 7u8 })
                .expect("spawn");
            assert_eq!(handle.join().await, Ok(7));
            let mut a = [0u8; 16];
            let mut b2 = [0u8; 16];
            runtime.rng().fill(&mut a).expect("fill");
            b.runtime.rng().fill(&mut b2).expect("fill");
            assert_ne!(a, b2);
            let two = alloy_seam::ConfigValue::defaults(2).expect("v2");
            let built = registry.instantiate::<alloy_rt::ClockSeam>(&(), &mem, &two);
            assert!(built.is_err());
            assert_eq!(runtime.clock().wall(), WallTime(WALL_START));
        }));
        world.add_host(config).map_err(fail)?;
        world.sleep(Duration::from_millis(1)).await;
        Ok(())
    });
}

#[test]
fn a_panicking_host_task_fails_the_run_without_ending_it() {
    let summary = run(RunConfig::new(10), |world| async move {
        world
            .add_host(HostConfig::new(boot(|_| async {
                panic!("host task broke")
            })))
            .map_err(fail)?;
        world.sleep(Duration::from_secs(1)).await;
        world.note("the run went on").map_err(fail)?;
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Fail);
    assert_eq!(
        summary.failure,
        Some(Detail::new("task 1 of host 0 panicked: host task broke"))
    );
    assert_eq!(summary.virtual_time, Duration::from_secs(1));
}

#[test]
fn runs_with_hosts_and_faults_are_deterministic() {
    let scenario = |world: World| async move {
        for _ in 0..4 {
            world.add_host(HostConfig::new(ticking())).map_err(fail)?;
        }
        for round in 0..20u64 {
            let host = HostId((world.random_u64() % 4) as u32);
            let fault = match world.random_u64() % 3 {
                0 => Fault::Stall {
                    host,
                    length: Duration::from_nanos(world.random_u64() % 50),
                },
                1 => Fault::Crash {
                    host,
                    after_polls: (world.random_u64() % 4) as u32,
                    unsynced: Unsynced::Lost,
                },
                _ => Fault::Restart {
                    host,
                    rng_replay: None,
                },
            };
            world
                .schedule(Duration::from_nanos(round * 10 + 5), fault)
                .map_err(fail)?;
        }
        world.sleep(Duration::from_nanos(300)).await;
        Ok(())
    };
    let mut hashes = std::collections::BTreeSet::new();
    for seed in 0..40 {
        let first = run(RunConfig::new(seed), scenario);
        let again = run(RunConfig::new(seed), scenario);
        assert_eq!(
            first.result,
            RunResult::Pass,
            "seed {seed}: {:?}",
            first.failure
        );
        assert_eq!(first, again, "seed {seed}");
        hashes.insert(first.trace_hash);
    }
    assert_eq!(hashes.len(), 40);
}

#[test]
fn histories_log_calls_with_world_times() {
    let history_out: Shared<Option<alloy_sim::History<&'static str, u32>>> = Arc::default();
    let kept = Arc::clone(&history_out);
    let summary = run(RunConfig::new(12), move |world| async move {
        let history = world.history::<&'static str, u32>();
        let other = world.history::<&'static str, u32>();
        world.sleep(Duration::from_nanos(5)).await;
        let write = history.invoke(ClientId(1), "write").map_err(fail)?;
        // The read is never completed.
        let _read = history.invoke(ClientId(2), "read").map_err(fail)?;
        let foreign = other.invoke(ClientId(3), "x").map_err(fail)?;
        world.sleep(Duration::from_nanos(4)).await;
        history.complete(write, 7);
        history.complete(foreign, 1);
        set(&kept, Some(history));
        Ok(())
    });
    assert_eq!(summary.events, 1);
    let history = get(&history_out).expect("the history");
    assert_eq!(
        history.entries(),
        [
            Entry {
                client: ClientId(1),
                op: "write",
                invoked: Duration::from_nanos(5),
                completed: Some((Duration::from_nanos(9), 7)),
            },
            Entry {
                client: ClientId(2),
                op: "read",
                invoked: Duration::from_nanos(5),
                completed: None,
            },
        ]
    );
    assert_eq!(history.invoke(ClientId(1), "late"), Err(WorldError::Ended));
    assert_eq!(format!("{history:?}"), "History { entries: 2 }");
}

#[test]
fn a_replay_of_a_replay_draws_the_streams_of_the_first_start() {
    let starts: Starts = Arc::default();
    let child: Shared<Option<alloy_rt::JoinHandle<()>>> = Arc::default();
    let (kept, held) = (Arc::clone(&starts), Arc::clone(&child));
    observed(13, move |world| async move {
        let host = world
            .add_host(HostConfig::new(recording_boot(&kept, &held)))
            .map_err(fail)?;
        for rng_replay in [Some(1), Some(2), None, Some(3), Some(4)] {
            world.sleep(Duration::from_millis(1)).await;
            world
                .apply(Fault::Crash {
                    host,
                    after_polls: 0,
                    unsynced: Unsynced::Lost,
                })
                .map_err(fail)?;
            world
                .apply(Fault::Restart { host, rng_replay })
                .map_err(fail)?;
        }
        world.sleep(Duration::from_millis(1)).await;
        Ok(())
    });
    let draws: Vec<(u32, u64)> = get(&starts).iter().map(|s| (s.0, s.1)).collect();
    // Start 2 replays 1, start 3 replays 2 and so 1, start 4 draws its own
    // streams, start 5 replays 3 and so 1, and start 6 replays 4.
    let (first, fourth) = (draws[0].1, draws[3].1);
    assert_ne!(first, fourth);
    assert_eq!(
        draws,
        [
            (1, first),
            (2, first),
            (3, first),
            (4, fourth),
            (5, first),
            (6, fourth)
        ]
    );
}

#[test]
fn a_crash_leaves_nothing_of_its_stalls_pending() {
    let summary = run(RunConfig::new(14), |world| async move {
        let host = world
            .add_host(HostConfig::new(boot(|_| std::future::pending::<()>())))
            .map_err(fail)?;
        for secs in [1_000, 2_000] {
            world
                .apply(Fault::Stall {
                    host,
                    length: Duration::from_secs(secs),
                })
                .map_err(fail)?;
        }
        world
            .apply(Fault::Crash {
                host,
                after_polls: 0,
                unsynced: Unsynced::Lost,
            })
            .map_err(fail)?;
        std::future::pending::<Result<(), Failure>>().await
    });
    // Nothing is runnable and nothing is pending, so world time stays at 0
    // (clause 3.2.7 step 4), as it would have without the stalls.
    assert_eq!(summary.result, RunResult::Liveness);
    assert_eq!(summary.virtual_time, Duration::ZERO);
}

/// What a [`Calling`] waker does: a call into the simulator.
type CallBack = Arc<dyn Fn() + Send + Sync>;

/// A waker that calls into the simulator when it is woken and when it is
/// dropped, as a waker of an adapter executor may.
struct Calling {
    call: CallBack,
    task: Waker,
    woken: Arc<AtomicUsize>,
}

impl Wake for Calling {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        (self.call)();
        self.woken.fetch_add(1, Ordering::AcqRel);
        self.task.wake_by_ref();
    }
}

impl Drop for Calling {
    fn drop(&mut self) {
        (self.call)();
    }
}

/// Polls `sleep` with a [`Calling`] waker, then with another, so the first
/// is replaced and dropped, and leaves the second with the sleep's timer.
async fn wire(sleep: &mut BoxFuture<'static, ()>, call: &CallBack, woken: &Arc<AtomicUsize>) {
    poll_fn(|cx| {
        for _ in 0..2 {
            let waker = Waker::from(Arc::new(Calling {
                call: Arc::clone(call),
                task: cx.waker().clone(),
                woken: Arc::clone(woken),
            }));
            let pinned: Pin<&mut _> = sleep.as_mut();
            assert!(pinned.poll(&mut Context::from_waker(&waker)).is_pending());
        }
        Poll::Ready(())
    })
    .await;
}

#[test]
fn wakers_that_call_seams_or_the_world_never_deadlock() {
    let ran = Arc::new(AtomicUsize::new(0));
    let woken = Arc::new(AtomicUsize::new(0));
    let (ran_in, woken_in) = (Arc::clone(&ran), Arc::clone(&woken));
    observed(15, move |world| async move {
        let config = HostConfig::new(boot(move |b: HostBoot| {
            let (ran, woken) = (Arc::clone(&ran_in), Arc::clone(&woken_in));
            async move {
                let runtime = b.runtime.clone();
                let call: CallBack = Arc::new(move || {
                    let ran = Arc::clone(&ran);
                    let spawned = runtime.spawn(name("from-waker"), async move {
                        ran.fetch_add(1, Ordering::AcqRel);
                    });
                    drop(spawned);
                });
                let mut sleep = b.runtime.clock().sleep_until(Instant::from_nanos(100));
                wire(&mut sleep, &call, &woken).await;
                b.runtime.sleep(Duration::from_nanos(200)).await;
            }
        }));
        let host = world.add_host(config).map_err(fail)?;
        let observer = world.clone();
        let call: CallBack = Arc::new(move || {
            assert!(observer.host_state(host).is_ok());
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let mut sleep = world.sleep_until(Duration::from_nanos(50));
        wire(&mut sleep, &call, &calls).await;
        world.sleep_until(Duration::from_nanos(300)).await;
        assert_eq!(calls.load(Ordering::Acquire), 1);
        Ok(())
    });
    // The host's waker was woken once at 100, and the tasks spawned from the
    // replaced waker's drop and from the wake both ran.
    assert_eq!(woken.load(Ordering::Acquire), 1);
    assert!(ran.load(Ordering::Acquire) >= 2);
}

#[test]
fn a_call_completes_only_in_the_history_that_returned_it() {
    let carried: Shared<Option<Call>> = Arc::default();
    let out = Arc::clone(&carried);
    let first = run(RunConfig::new(16), move |world| async move {
        let history = world.history::<&'static str, u32>();
        let call = history.invoke(ClientId(1), "a").map_err(fail)?;
        set(&out, Some(call));
        Ok(())
    });
    assert_eq!(first.result, RunResult::Pass);
    let entries: Shared<Vec<Entry<&'static str, u32>>> = Arc::default();
    let out = Arc::clone(&entries);
    let second = run(RunConfig::new(16), move |world| async move {
        let history = world.history::<&'static str, u32>();
        let own = history.invoke(ClientId(1), "b").map_err(fail)?;
        let foreign = carried
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("the first world's call");
        assert_ne!(own, foreign);
        history.complete(foreign, 99);
        set(&out, history.entries());
        Ok(())
    });
    assert_eq!(second.result, RunResult::Pass);
    assert_eq!(get(&entries)[0].completed, None);
}

/// Finding F1 of the T-0076 review (D-0043): two replays of one start that
/// each build more `rng` instances than it did draw distinct streams in
/// those extra instances; only the instances the replayed start reached
/// repeat its streams (clause 3.1.5).
#[test]
fn extra_instances_of_two_replays_of_one_start_draw_distinct_streams() {
    let draws: Shared<Vec<(u32, usize, u64)>> = Arc::default();
    let kept = Arc::clone(&draws);
    observed(17, move |world| async move {
        let config = HostConfig::new(boot(move |b: HostBoot| {
            let draws = Arc::clone(&kept);
            async move {
                let mut rngs = vec![Arc::clone(b.runtime.rng())];
                if b.start > 1 {
                    let mem = alloy_seam::ImplName::new("mem").expect("name");
                    let config = alloy_seam::ConfigValue::defaults(1).expect("v1");
                    let registry = b.registry.build();
                    let built = registry.instantiate::<alloy_rt::RngSeam>(&(), &mem, &config);
                    rngs.push(built.expect("mem builds"));
                }
                for (index, rng) in rngs.iter().enumerate() {
                    let mut bytes = [0u8; 8];
                    rng.fill(&mut bytes).expect("mem never fails");
                    let draw = (b.start, index, u64::from_le_bytes(bytes));
                    draws
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(draw);
                }
            }
        }));
        let host = world.add_host(config).map_err(fail)?;
        for _ in 0..2 {
            world.sleep(Duration::from_millis(1)).await;
            let crash = Fault::Crash {
                host,
                after_polls: 0,
                unsynced: Unsynced::Lost,
            };
            world.apply(crash).map_err(fail)?;
            let restart = Fault::Restart {
                host,
                rng_replay: Some(1),
            };
            world.apply(restart).map_err(fail)?;
        }
        world.sleep(Duration::from_millis(1)).await;
        Ok(())
    });
    let draws = get(&draws);
    let places: Vec<(u32, usize)> = draws.iter().map(|d| (d.0, d.1)).collect();
    assert_eq!(places, [(1, 0), (2, 0), (2, 1), (3, 0), (3, 1)]);
    // Instance 0 of each replay repeats start 1's; the extra instances
    // differ from each other and from start 1's.
    assert_eq!(draws[1].2, draws[0].2);
    assert_eq!(draws[3].2, draws[0].2);
    let mut distinct = vec![draws[0].2, draws[2].2, draws[4].2];
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "{draws:?}");
}
