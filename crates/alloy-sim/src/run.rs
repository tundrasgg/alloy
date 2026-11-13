//! Runs: the configuration, the run loop of clause 3.2.7, the result of
//! clause 3.2.8, the summary line of clause 3.2.6, and the failure output of
//! section 6.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::future::Future;
use std::io::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_rt::WallTime;
use alloy_seam::Detail;

use crate::exec::{Executor, Polled, drop_quietly, lock, panic_text};
use crate::stream::Stream;
use crate::trace::{Closed, Kept, kind_name};
use crate::world::{MAX_NANOS, Shared, World};

/// Task polls at one world instant before the run ends with `liveness`
/// (section 7).
const MAX_POLLS_AT_INSTANT: u64 = 100_000_000;

/// The smallest trace file cap: the header (section 7).
const MIN_TRACE_CAP: u64 = 16;

/// How one run is set up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunConfig {
    /// The seed. Every choice the simulator makes follows from it.
    pub seed: u64,
    /// The world time at which an unfinished run ends with `Liveness`.
    /// 1 ns to 2^63 - 1 ns. Default 3,600 s.
    pub budget: Duration,
    /// Every host's wall clock at world time 0, before the host's own
    /// offset. Default 1,800,000,000 s after the Unix epoch.
    pub wall_start: WallTime,
    /// Where the event trace is streamed. Default `None`: no file.
    pub trace_file: Option<PathBuf>,
    /// The trace file cap in bytes, 16 or more. Default 1,073,741,824.
    pub trace_cap: u64,
}

impl RunConfig {
    /// `seed`, with every other field at its default.
    pub fn new(seed: u64) -> RunConfig {
        RunConfig {
            seed,
            budget: Duration::from_secs(3_600),
            wall_start: WallTime(1_800_000_000 * 1_000_000_000),
            trace_file: None,
            trace_cap: 1 << 30,
        }
    }

    /// The first field outside its range.
    fn check(&self) -> Result<(), Detail> {
        let budget = self.budget.as_nanos();
        if budget == 0 || budget > u128::from(MAX_NANOS) {
            return Err(Detail::new(
                "invalid run config: budget must be 1 ns to 2^63 - 1 ns",
            ));
        }
        if self.trace_cap < MIN_TRACE_CAP {
            return Err(Detail::new(
                "invalid run config: trace_cap must be 16 or more",
            ));
        }
        Ok(())
    }
}

/// How a run ended (clause 3.2.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunResult {
    /// The control task returned `Ok` and nothing failed.
    Pass,
    /// The control task returned `Err` or panicked, a host task, an
    /// observer, or a middlebox panicked, or the trace file could not be
    /// written.
    Fail,
    /// The run reached its budget or its poll bound without failing.
    Liveness,
}

impl RunResult {
    /// The `result` byte of the `run-end` record.
    pub(crate) fn code(self) -> u8 {
        match self {
            RunResult::Pass => 0,
            RunResult::Fail => 1,
            RunResult::Liveness => 2,
        }
    }

    fn name(self) -> &'static str {
        match self {
            RunResult::Pass => "pass",
            RunResult::Fail => "fail",
            RunResult::Liveness => "liveness",
        }
    }
}

/// Why a scenario failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure(pub Detail);

/// What a run did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSummary {
    /// The run's seed.
    pub seed: u64,
    /// The BLAKE3 hash of the whole event trace (clause 3.2.5).
    pub trace_hash: [u8; 32],
    /// Records in the trace, `run-end` included.
    pub events: u64,
    /// The world time of the `run-end` record.
    pub virtual_time: Duration,
    /// The result the `run-end` record carries.
    pub result: RunResult,
    /// The first cause of `Fail` or `Liveness` (clause 3.2.8). `None` for
    /// `Pass`.
    pub failure: Option<Detail>,
}

impl RunSummary {
    /// The line of clause 3.2.6, without its newline:
    /// `alloy-sim seed=<16 hex> trace=<64 hex> events=<n> virtual_ns=<t>
    /// result=<pass|fail|liveness>`.
    pub fn line(&self) -> String {
        let mut hash = String::with_capacity(64);
        for byte in self.trace_hash {
            let _ = write!(hash, "{byte:02x}");
        }
        format!(
            "alloy-sim seed={:016x} trace={hash} events={} virtual_ns={} result={}",
            self.seed,
            self.events,
            self.virtual_time.as_nanos(),
            self.result.name(),
        )
    }
}

/// Runs one scenario on the calling thread until the run ends (clause
/// 3.2.7), writes the summary line to standard error, and returns it.
///
/// `scenario` is called once with the new world, at world time 0, and the
/// future it returns becomes the control task. The run ends at the first of:
/// the control task completes or panics; an observer panics; nothing is
/// runnable and the next event, if any, is after `budget`; or 100,000,000
/// task polls at one world instant. A config field outside its range ends the
/// run at once with `Fail`, without calling `scenario`.
///
/// When the result is `Fail` or `Liveness`, the last 200 records and the
/// failure are written to standard error before the summary line (section
/// 6). Nothing written to standard error holds a payload.
pub fn run<F, Fut>(config: RunConfig, scenario: F) -> RunSummary
where
    F: FnOnce(World) -> Fut,
    Fut: Future<Output = Result<(), Failure>> + Send + 'static,
{
    run_bounded(config, scenario, MAX_POLLS_AT_INSTANT)
}

/// [`run`] with the poll bound as a parameter.
fn run_bounded<F, Fut>(config: RunConfig, scenario: F, max_polls: u64) -> RunSummary
where
    F: FnOnce(World) -> Fut,
    Fut: Future<Output = Result<(), Failure>> + Send + 'static,
{
    let shared = Arc::new(Shared::new(config.seed));
    let mut exec = Executor::new();
    match config.check() {
        Err(cause) => lock(&shared.trace).outcome.fail(cause),
        Ok(()) => {
            if let Some(path) = &config.trace_file {
                lock(&shared.trace).open_file(path, config.trace_cap);
            }
            let world = World::new(Arc::clone(&shared));
            match catch_unwind(AssertUnwindSafe(|| scenario(world))) {
                Ok(control) => drive(&shared, &mut exec, control, &config, max_polls),
                Err(payload) => {
                    let text = panic_text(payload.as_ref());
                    drop_quietly(payload);
                    lock(&shared.trace)
                        .outcome
                        .fail(Detail::new(&format!("the control task panicked: {text}")));
                }
            }
        }
    }
    finish(&shared, exec)
}

/// Runs the world until the run ends (clause 3.2.7 steps 3 and 4).
fn drive<Fut>(
    shared: &Shared,
    exec: &mut Executor,
    control: Fut,
    config: &RunConfig,
    max_polls: u64,
) where
    Fut: Future<Output = Result<(), Failure>> + Send + 'static,
{
    let output: Arc<Mutex<Option<Result<(), Failure>>>> = Arc::default();
    let slot = Arc::clone(&output);
    let control = exec.spawn(Box::pin(async move {
        let result = control.await;
        *lock(&slot) = Some(result);
    }));
    let budget = u64::try_from(config.budget.as_nanos()).unwrap_or(MAX_NANOS);
    let mut schedule = Stream::new(shared.seed(), b"schedule", &[]);
    let mut polls_here: u64 = 0;
    let fail = |text: String| lock(&shared.trace).outcome.fail(Detail::new(&text));
    let liveness = |text: String| lock(&shared.trace).outcome.liveness(Detail::new(&text));
    loop {
        if shared.has_ended() {
            return;
        }
        if let Some(key) = exec.pick(&mut schedule) {
            let polled = exec.poll(key);
            polls_here += 1;
            if key == control {
                match polled {
                    Polled::Pending => {}
                    Polled::Completed => {
                        if let Some(Err(Failure(cause))) = lock(&output).take() {
                            lock(&shared.trace).outcome.fail(cause);
                        }
                        return;
                    }
                    Polled::Panicked(text) => {
                        fail(format!("the control task panicked: {text}"));
                        return;
                    }
                }
            }
            if polls_here >= max_polls {
                liveness(format!(
                    "{max_polls} task polls at world time {} ns",
                    shared.now_nanos()
                ));
                return;
            }
            continue;
        }
        match shared.next_deadline() {
            None => {
                liveness(format!(
                    "nothing is runnable and no event is pending at world time {} ns, before the budget of {budget} ns",
                    shared.now_nanos()
                ));
                return;
            }
            Some(deadline) if deadline > budget => {
                shared.set_now(budget);
                liveness(format!("the run reached its budget of {budget} ns"));
                return;
            }
            Some(deadline) => {
                if deadline > shared.now_nanos() {
                    shared.set_now(deadline);
                    polls_here = 0;
                }
                let mut due = shared.take_due(deadline);
                schedule.shuffle(&mut due);
                for waker in due {
                    waker.wake();
                }
            }
        }
    }
}

/// Ends the run (clause 3.2.7 step 5): writes `run-end`, drops every task
/// and object of the world, and writes the failure output and the summary
/// line.
fn finish(shared: &Shared, mut exec: Executor) -> RunSummary {
    shared.end();
    let time = shared.now_nanos();
    let closed = lock(&shared.trace).finish(shared.hook(), time);
    exec.clear();
    drop_quietly(shared.clear_timers());
    let Closed {
        hash,
        events,
        result,
        failure,
        kept,
        observers,
    } = closed;
    drop_quietly(observers);
    let summary = RunSummary {
        seed: shared.seed(),
        trace_hash: hash,
        events,
        virtual_time: Duration::from_nanos(time),
        result,
        failure,
    };
    let text = output(&summary, &kept);
    // Standard error is the only output of a run; a run whose standard
    // error is closed still returns its summary.
    let _ = std::io::stderr().lock().write_all(text.as_bytes());
    summary
}

/// What a run writes to standard error (section 6): for `Fail` and
/// `Liveness`, one line per kept record and the failure on one line; then the
/// summary line.
fn output(summary: &RunSummary, kept: &VecDeque<Kept>) -> String {
    let mut text = String::new();
    if let Some(failure) = &summary.failure {
        for record in kept {
            let host = match record.host {
                Some(host) => host.0.to_string(),
                None => "world".to_owned(),
            };
            let _ = writeln!(
                text,
                "alloy-sim event time={} host={host} kind={} len={}",
                record.time,
                kind_name(record.kind),
                record.len
            );
        }
        let one_line = failure.as_str().replace('\n', "\\n").replace('\r', "\\r");
        let _ = writeln!(text, "alloy-sim failure: {one_line}");
    }
    let _ = writeln!(text, "{}", summary.line());
    text
}

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    use std::task::Poll;

    use super::*;

    #[test]
    fn the_poll_bound_ends_the_run_with_liveness() {
        let summary = run_bounded(
            RunConfig::new(1),
            |_| {
                poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    Poll::<Result<(), Failure>>::Pending
                })
            },
            10,
        );
        assert_eq!(summary.result, RunResult::Liveness);
        assert_eq!(summary.events, 1);
        assert_eq!(
            summary.failure,
            Some(Detail::new("10 task polls at world time 0 ns"))
        );
    }

    #[test]
    fn the_poll_bound_restarts_when_time_advances() {
        let summary = run_bounded(
            RunConfig::new(1),
            |world| async move {
                for _ in 0..5 {
                    for _ in 0..8 {
                        yield_once().await;
                    }
                    world.sleep(Duration::from_nanos(1)).await;
                }
                Ok(())
            },
            10,
        );
        assert_eq!(summary.result, RunResult::Pass);
        assert_eq!(summary.virtual_time, Duration::from_nanos(5));
    }

    async fn yield_once() {
        let mut yielded = false;
        poll_fn(|cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }

    #[test]
    fn the_summary_line_has_the_fixed_layout() {
        let summary = RunSummary {
            seed: 0xab,
            trace_hash: [0x0f; 32],
            events: 12,
            virtual_time: Duration::from_nanos(3_600_000_000_000),
            result: RunResult::Liveness,
            failure: None,
        };
        assert_eq!(
            summary.line(),
            format!(
                "alloy-sim seed=00000000000000ab trace={} events=12 virtual_ns=3600000000000 result=liveness",
                "0f".repeat(32)
            )
        );
    }

    #[test]
    fn failure_output_lists_kept_records_then_the_failure_then_the_summary() {
        let mut summary = RunSummary {
            seed: 1,
            trace_hash: [0; 32],
            events: 2,
            virtual_time: Duration::from_nanos(40),
            result: RunResult::Fail,
            failure: Some(Detail::new("bad\nthing")),
        };
        let kept = VecDeque::from([
            Kept {
                time: 40,
                host: None,
                kind: 0x00F0,
                len: 3,
            },
            Kept {
                time: 40,
                host: Some(crate::HostId(7)),
                kind: 0x00FF,
                len: 1,
            },
        ]);
        let text = output(&summary, &kept);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "alloy-sim event time=40 host=world kind=note len=3",
                "alloy-sim event time=40 host=7 kind=run-end len=1",
                "alloy-sim failure: bad\\nthing",
                summary.line().as_str(),
            ]
        );
        assert!(text.ends_with('\n'));
        summary.result = RunResult::Pass;
        summary.failure = None;
        assert_eq!(output(&summary, &kept), format!("{}\n", summary.line()));
    }

    #[test]
    fn defaults_follow_the_spec() {
        let config = RunConfig::new(5);
        assert_eq!(config.seed, 5);
        assert_eq!(config.budget, Duration::from_secs(3_600));
        assert_eq!(config.wall_start, WallTime(1_800_000_000_000_000_000));
        assert_eq!(config.trace_file, None);
        assert_eq!(config.trace_cap, 1_073_741_824);
        assert_eq!(config.check(), Ok(()));
    }
}
