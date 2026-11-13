//! Runs through the public interface: results, world time, the trace and its
//! file, observers, and world calls from hooks and after the end.

// The trace file tests read the file back, and one test runs two worlds on
// two threads at once.
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "tests read the trace file and run worlds on threads"
)]

use std::future::poll_fn;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex, PoisonError, mpsc};
use std::task::Poll;
use std::thread::{self, ThreadId};
use std::time::Duration;

use alloy_seam::Detail;
use alloy_sim::{
    Failure, HostId, Observer, Record, RunConfig, RunResult, RunSummary, World, WorldError, run,
};

const NOTE: u16 = 0x00F0;
const RUN_END: u16 = 0x00FF;

/// A record as an observer saw it: time in ns, host, kind, body.
type Seen = (u64, Option<HostId>, u16, Vec<u8>);

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

    fn kinds(&self) -> Vec<u16> {
        self.records().iter().map(|r| r.2).collect()
    }
}

impl Observer for Collect {
    fn record(&mut self, record: &Record<'_>) {
        assert!(record.data.is_empty());
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push((
            record.time.as_nanos() as u64,
            record.host,
            record.kind,
            record.body.to_vec(),
        ));
    }
}

/// The body of a `note` record with `text`.
fn note_body(text: &str) -> Vec<u8> {
    let mut body = (text.len() as u16).to_le_bytes().to_vec();
    body.extend_from_slice(text.as_bytes());
    body
}

/// The bytes of the trace of a run that wrote only `run-end`.
fn only_run_end(seed: u64, time: u64, result: u8) -> Vec<u8> {
    let mut bytes = b"ALTR\x01\x00\x00\x00".to_vec();
    bytes.extend_from_slice(&seed.to_le_bytes());
    bytes.extend_from_slice(&time.to_le_bytes());
    bytes.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 1, 0, result]);
    bytes
}

fn detail(summary: &RunSummary) -> Option<&str> {
    summary.failure.as_ref().map(Detail::as_str)
}

#[test]
fn world_and_configs_cross_threads() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<World>();
    send_sync::<RunConfig>();
    send_sync::<RunSummary>();
    send_sync::<WorldError>();
}

/// Polls `a` and `b` until both complete.
async fn join<A, B>(a: A, b: B)
where
    A: Future<Output = ()>,
    B: Future<Output = ()>,
{
    let mut a = Some(Box::pin(a));
    let mut b = Some(Box::pin(b));
    poll_fn(|cx| {
        if let Some(f) = a.as_mut()
            && f.as_mut().poll(cx).is_ready()
        {
            a = None;
        }
        if let Some(f) = b.as_mut()
            && f.as_mut().poll(cx).is_ready()
        {
            b = None;
        }
        if a.is_none() && b.is_none() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
}

/// A scenario that writes notes from two concurrent branches and from the
/// scenario's random stream, so the trace depends on the seed.
async fn busy(world: World) -> Result<(), Failure> {
    let w1 = world.clone();
    let w2 = world.clone();
    join(
        async move {
            for i in 0..20 {
                w1.sleep(Duration::from_millis(w1.random_u64() % 5)).await;
                w1.note(&format!("a{i}")).ok();
            }
        },
        async move {
            for i in 0..20 {
                w2.sleep(Duration::from_millis(w2.random_u64() % 5)).await;
                w2.note(&format!("b{i}")).ok();
            }
        },
    )
    .await;
    Ok(())
}

#[test]
fn a_passing_run_ends_when_the_control_task_completes() {
    let observer = Collect::default();
    let watch = observer.clone();
    let summary = run(RunConfig::new(3), |world| async move {
        world
            .observe(Box::new(watch))
            .expect("room for an observer");
        assert_eq!(world.now(), Duration::ZERO);
        world.sleep(Duration::from_secs(2)).await;
        world.note("two").expect("note while running");
        world.sleep_until(Duration::from_secs(5)).await;
        // A deadline in the past completes at once.
        world.sleep_until(Duration::from_secs(1)).await;
        assert_eq!(world.now(), Duration::from_secs(5));
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass);
    assert_eq!(summary.failure, None);
    assert_eq!(summary.seed, 3);
    assert_eq!(summary.events, 2);
    assert_eq!(summary.virtual_time, Duration::from_secs(5));
    assert_eq!(
        observer.records(),
        [
            (2_000_000_000, None, NOTE, note_body("two")),
            (5_000_000_000, None, RUN_END, vec![0]),
        ]
    );
}

#[test]
fn the_same_seed_gives_the_same_trace_and_seeds_differ() {
    let first = run(RunConfig::new(11), busy);
    let second = run(RunConfig::new(11), busy);
    let other = run(RunConfig::new(12), busy);
    assert_eq!(first.result, RunResult::Pass);
    assert_eq!(first.events, 41);
    assert_eq!(first, second);
    assert_eq!(first.line(), second.line());
    assert_ne!(first.trace_hash, other.trace_hash);
}

#[test]
fn runs_on_two_threads_at_once_match_runs_alone() {
    let alone = [run(RunConfig::new(21), busy), run(RunConfig::new(22), busy)];
    let together = thread::scope(|scope| {
        let a = scope.spawn(|| run(RunConfig::new(21), busy));
        let b = scope.spawn(|| run(RunConfig::new(22), busy));
        [
            a.join().expect("the first run returns"),
            b.join().expect("the second run returns"),
        ]
    });
    assert_eq!(alone, together);
}

#[test]
fn an_err_from_the_control_task_fails_the_run() {
    let summary = run(RunConfig::new(1), |world| async move {
        world.sleep(Duration::from_millis(7)).await;
        Err(Failure(Detail::new("checker found a violation")))
    });
    assert_eq!(summary.result, RunResult::Fail);
    assert_eq!(detail(&summary), Some("checker found a violation"));
    assert_eq!(summary.virtual_time, Duration::from_millis(7));
    assert_eq!(summary.events, 1);
}

#[test]
fn a_panicking_control_task_fails_the_run() {
    let summary = run(RunConfig::new(1), |world| async move {
        world.sleep(Duration::from_millis(1)).await;
        panic!("control broke");
    });
    assert_eq!(summary.result, RunResult::Fail);
    assert_eq!(
        detail(&summary),
        Some("the control task panicked: control broke")
    );
    assert_eq!(summary.virtual_time, Duration::from_millis(1));
}

#[test]
fn a_panicking_scenario_function_fails_the_run_at_time_zero() {
    let summary = run(
        RunConfig::new(4),
        |_world| -> std::future::Ready<Result<(), Failure>> { panic!("no world for me") },
    );
    assert_eq!(summary.result, RunResult::Fail);
    assert_eq!(
        detail(&summary),
        Some("the control task panicked: no world for me")
    );
    assert_eq!(
        summary.trace_hash,
        alloy_crypto::hash(&only_run_end(4, 0, 1))
    );
}

#[test]
fn the_budget_ends_a_run_with_liveness_at_the_budget() {
    let mut config = RunConfig::new(1);
    config.budget = Duration::from_secs(10);
    let summary = run(config, |world| async move {
        world.sleep(Duration::from_secs(11)).await;
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Liveness);
    assert_eq!(summary.virtual_time, Duration::from_secs(10));
    assert_eq!(
        detail(&summary),
        Some("the run reached its budget of 10000000000 ns")
    );
}

#[test]
fn an_event_at_the_budget_still_happens() {
    let mut config = RunConfig::new(1);
    config.budget = Duration::from_secs(10);
    let summary = run(config, |world| async move {
        world.sleep(Duration::from_secs(10)).await;
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass);
    assert_eq!(summary.virtual_time, Duration::from_secs(10));
}

#[test]
fn a_world_with_nothing_to_do_ends_with_liveness_where_it_is() {
    let summary = run(RunConfig::new(1), |world| async move {
        world.sleep(Duration::from_secs(3)).await;
        std::future::pending::<()>().await;
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Liveness);
    assert_eq!(summary.virtual_time, Duration::from_secs(3));
}

#[test]
fn an_invalid_config_ends_the_run_before_the_scenario() {
    let mut configs = Vec::new();
    for budget in [Duration::ZERO, Duration::from_nanos(1 << 63)] {
        let mut config = RunConfig::new(9);
        config.budget = budget;
        configs.push(config);
    }
    let mut config = RunConfig::new(9);
    config.trace_cap = 15;
    configs.push(config);
    for config in configs {
        let mut called = false;
        let summary = run(config, |_| {
            called = true;
            async { Ok(()) }
        });
        assert!(!called);
        assert_eq!(summary.result, RunResult::Fail);
        assert_eq!(summary.events, 1);
        assert_eq!(summary.virtual_time, Duration::ZERO);
        assert!(detail(&summary).is_some_and(|d| d.starts_with("invalid run config")));
        assert_eq!(
            summary.trace_hash,
            alloy_crypto::hash(&only_run_end(9, 0, 1))
        );
    }
    let mut edge = RunConfig::new(9);
    edge.budget = Duration::from_nanos((1 << 63) - 1);
    edge.trace_cap = 16;
    assert_eq!(run(edge, |_| async { Ok(()) }).result, RunResult::Pass);
}

#[test]
fn an_observer_sees_only_records_after_it_was_added() {
    let observer = Collect::default();
    let watch = observer.clone();
    let summary = run(RunConfig::new(1), |world| async move {
        world.note("before").expect("note");
        world.observe(Box::new(watch)).expect("room");
        world.note("after").expect("note");
        Ok(())
    });
    assert_eq!(summary.events, 3);
    assert_eq!(observer.kinds(), [NOTE, RUN_END]);
    assert_eq!(observer.records()[0].3, note_body("after"));
}

/// Calls the world from inside `record` and keeps what each call returned.
struct Meddler {
    world: World,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Observer for Meddler {
    fn record(&mut self, _: &Record<'_>) {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let sleeps = self
            .world
            .sleep(Duration::ZERO)
            .as_mut()
            .poll(&mut cx)
            .is_pending();
        let calls = format!(
            "{:?} {:?} {} {:?} {}",
            self.world.note("inside"),
            self.world.observe(Box::new(Collect::default())),
            self.world.random_u64(),
            self.world.now(),
            sleeps,
        );
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(calls);
    }
}

#[test]
fn world_calls_from_an_observer_change_nothing() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let draws = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&draws);
    let summary = run(RunConfig::new(5), |world| async move {
        let meddler = Meddler {
            world: world.clone(),
            calls: seen,
        };
        world.observe(Box::new(meddler)).expect("room");
        world.sleep(Duration::from_nanos(4)).await;
        world.note("outside").expect("note");
        kept.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(world.random_u64());
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass);
    assert_eq!(summary.events, 2);
    let busy = "Err(Busy) Err(Busy) 0 4ns true".to_owned();
    assert_eq!(
        *calls.lock().unwrap_or_else(PoisonError::into_inner),
        [busy.clone(), busy]
    );
    // The meddler's draws did not advance the scenario's stream: the first
    // draw of this run equals the first draw of a run without it.
    let plain = Arc::new(Mutex::new(Vec::new()));
    let plain_kept = Arc::clone(&plain);
    run(RunConfig::new(5), |world| async move {
        plain_kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(world.random_u64());
        Ok(())
    });
    assert_eq!(
        *draws.lock().unwrap_or_else(PoisonError::into_inner),
        *plain.lock().unwrap_or_else(PoisonError::into_inner)
    );
}

/// Panics on the `n`-th record it sees, counting from 1.
struct PanicOn(u32);

impl Observer for PanicOn {
    fn record(&mut self, _: &Record<'_>) {
        self.0 -= 1;
        assert!(self.0 != 0, "observer gave up");
    }
}

#[test]
fn an_observer_panic_fails_the_run_and_run_end_follows() {
    let observer = Collect::default();
    let watch = observer.clone();
    let after = Arc::new(Mutex::new(None));
    let after_kept = Arc::clone(&after);
    let summary = run(RunConfig::new(1), |world| async move {
        world.observe(Box::new(PanicOn(2))).expect("room");
        world.observe(Box::new(watch)).expect("room");
        world.note("one").expect("note");
        world.sleep(Duration::from_secs(1)).await;
        // The second record makes the observer panic and ends the run.
        world.note("two").expect("note");
        *after_kept.lock().unwrap_or_else(PoisonError::into_inner) = Some(world.note("three"));
        world.sleep(Duration::from_secs(1)).await;
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Fail);
    assert_eq!(
        detail(&summary),
        Some("an observer panicked: observer gave up")
    );
    assert_eq!(summary.events, 3);
    assert_eq!(summary.virtual_time, Duration::from_secs(1));
    assert_eq!(observer.kinds(), [NOTE, NOTE, RUN_END]);
    assert_eq!(
        *after.lock().unwrap_or_else(PoisonError::into_inner),
        Some(Err(WorldError::Ended))
    );
}

#[test]
fn an_observer_panic_on_run_end_changes_nothing() {
    let summary = run(RunConfig::new(1), |world| async move {
        world.observe(Box::new(PanicOn(1))).expect("room");
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass);
    assert_eq!(summary.failure, None);
    assert_eq!(summary.events, 1);
}

#[test]
fn the_world_returns_ended_after_the_run() {
    let stash: Arc<Mutex<Option<World>>> = Arc::default();
    let keep = Arc::clone(&stash);
    let summary = run(RunConfig::new(1), |world| async move {
        world.sleep(Duration::from_millis(3)).await;
        *keep.lock().unwrap_or_else(PoisonError::into_inner) = Some(world);
        Ok(())
    });
    let world = stash
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .expect("the scenario kept its world");
    assert_eq!(world.note("late"), Err(WorldError::Ended));
    assert_eq!(
        world.observe(Box::new(Collect::default())),
        Err(WorldError::Ended)
    );
    assert_eq!(world.now(), summary.virtual_time);
    assert_eq!(format!("{world:?}"), "World { now: 3ms, hosts: 0 }");
}

#[test]
fn the_note_text_is_cut_to_1024_bytes() {
    let observer = Collect::default();
    let watch = observer.clone();
    run(RunConfig::new(1), |world| async move {
        world.observe(Box::new(watch)).expect("room");
        world.note(&"é".repeat(600)).expect("note");
        Ok(())
    });
    let body = &observer.records()[0].3;
    assert_eq!(body[..2], 1_024u16.to_le_bytes());
    assert_eq!(body.len(), 2 + 1_024);
}

fn trace_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("alloy-sim-{name}.altr"))
}

#[test]
fn the_trace_file_holds_exactly_the_hashed_bytes() {
    let path = trace_path("whole");
    std::fs::write(&path, b"old contents that the run replaces").expect("write");
    let mut config = RunConfig::new(11);
    config.trace_file = Some(path.clone());
    let with_file = run(config, busy);
    let bytes = std::fs::read(&path).expect("the trace file exists");
    assert_eq!(&bytes[..4], b"ALTR");
    assert_eq!(alloy_crypto::hash(&bytes), with_file.trace_hash);
    assert_eq!(with_file, run(RunConfig::new(11), busy));
}

#[test]
fn the_trace_file_stops_before_the_record_that_would_pass_the_cap() {
    let path = trace_path("capped");
    let mut config = RunConfig::new(11);
    config.trace_file = Some(path.clone());
    // The header (16 bytes) and the first note record (16 + 4 bytes), and 9
    // bytes short of the second.
    config.trace_cap = 16 + 20 + 11;
    let capped = run(config, busy);
    let bytes = std::fs::read(&path).expect("the trace file exists");
    assert_eq!(bytes.len(), 36);
    assert_eq!(&bytes[28..30], &NOTE.to_le_bytes());
    assert_eq!(capped.result, RunResult::Pass);
    assert_eq!(capped, run(RunConfig::new(11), busy));
}

#[test]
fn a_trace_file_that_cannot_be_written_fails_the_run_but_does_not_end_it() {
    let mut config = RunConfig::new(11);
    config.trace_file = Some(trace_path("missing-dir").join("trace.altr"));
    let summary = run(config, busy);
    assert_eq!(summary.result, RunResult::Fail);
    assert!(
        detail(&summary).is_some_and(|d| d.starts_with("the trace file could not be written: "))
    );
    // The run went on to the end: only the result byte of `run-end` differs.
    let clean = run(RunConfig::new(11), busy);
    assert_eq!(summary.virtual_time, clean.virtual_time);
    assert_eq!(summary.events, clean.events);
}

/// Runs `scenario` with seed `seed` on a helper thread and waits at most 10 s
/// for it to end, so a deadlock fails the test instead of hanging it.
fn run_within_10s<S, F>(seed: u64, scenario: S) -> RunSummary
where
    S: FnOnce(World) -> F + Send + 'static,
    F: Future<Output = Result<(), Failure>> + Send + 'static,
{
    let (done, ended) = mpsc::channel();
    thread::spawn(move || {
        done.send(run(RunConfig::new(seed), scenario)).ok();
    });
    ended
        .recv_timeout(Duration::from_secs(10))
        .expect("the run ends within 10 s")
}

/// The first draw of the scenario's stream in a plain run of `seed`.
fn first_draw(seed: u64) -> u64 {
    let draw = Arc::new(Mutex::new(0));
    let kept = Arc::clone(&draw);
    run(RunConfig::new(seed), |world| async move {
        *kept.lock().unwrap_or_else(PoisonError::into_inner) = world.random_u64();
        Ok(())
    });
    *draw.lock().unwrap_or_else(PoisonError::into_inner)
}

/// On every `note` record, calls the world and keeps what the calls
/// returned.
struct Reenter {
    world: World,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Observer for Reenter {
    fn record(&mut self, record: &Record<'_>) {
        if record.kind == NOTE {
            let calls = format!("{:?} {}", self.world.note("inner"), self.world.random_u64());
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(calls);
        }
    }
}

#[test]
fn world_calls_from_an_observer_off_the_run_thread_are_busy() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let draw = Arc::new(Mutex::new(0));
    let kept = Arc::clone(&draw);
    let summary = run_within_10s(7, |world| async move {
        let reenter = Reenter {
            world: world.clone(),
            calls: seen,
        };
        world.observe(Box::new(reenter)).expect("room");
        let off = world.clone();
        thread::spawn(move || off.note("outer"))
            .join()
            .expect("the note thread ends")
            .expect("note");
        *kept.lock().unwrap_or_else(PoisonError::into_inner) = world.random_u64();
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass);
    assert_eq!(summary.events, 2);
    assert_eq!(
        *calls.lock().unwrap_or_else(PoisonError::into_inner),
        ["Err(Busy) 0"]
    );
    // The observer's draw did not advance the scenario's stream.
    assert_eq!(
        *draw.lock().unwrap_or_else(PoisonError::into_inner),
        first_draw(7)
    );
}

/// On the first record it sees off the run's thread, signals `entered` and
/// waits at `release`.
struct Block {
    run_thread: ThreadId,
    entered: mpsc::Sender<()>,
    release: Arc<Barrier>,
    blocked: bool,
}

impl Observer for Block {
    fn record(&mut self, _: &Record<'_>) {
        if !self.blocked && thread::current().id() != self.run_thread {
            self.blocked = true;
            self.entered.send(()).ok();
            self.release.wait();
        }
    }
}

#[test]
fn an_observer_on_another_thread_leaves_calls_on_the_run_thread_alone() {
    let plain = first_draw(11);
    let outside = Arc::new(Mutex::new(None));
    let kept = Arc::clone(&outside);
    let summary = run_within_10s(11, |world| async move {
        let (entered, wait_entered) = mpsc::channel();
        let release = Arc::new(Barrier::new(2));
        let block = Block {
            run_thread: thread::current().id(),
            entered,
            release: Arc::clone(&release),
            blocked: false,
        };
        world.observe(Box::new(block)).expect("room");
        let off = world.clone();
        let other = thread::spawn(move || off.note("off"));
        wait_entered
            .recv_timeout(Duration::from_secs(5))
            .expect("the observer runs on the other thread");
        // The other thread is inside the observer and this one is not.
        let draw = world.random_u64();
        let mut sleep = world.sleep(Duration::ZERO);
        let ready = poll_fn(|cx| Poll::Ready(sleep.as_mut().poll(cx).is_ready())).await;
        release.wait();
        let note = other.join().expect("the note thread ends");
        *kept.lock().unwrap_or_else(PoisonError::into_inner) = Some((draw, ready, note));
        world.note("after").expect("note");
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass);
    assert_eq!(summary.events, 3);
    assert_eq!(
        *outside.lock().unwrap_or_else(PoisonError::into_inner),
        Some((plain, true, Ok(())))
    );
}
