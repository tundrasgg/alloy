//! Runs test bodies on a simulated host (ARFC 0005), whose `mem` runtime
//! serves the randomness trait and the disk.

#![allow(dead_code, reason = "each test file uses part of this module")]

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use alloy_rt::{BoxFuture, Runtime};
use alloy_seam::Detail;
use alloy_sim::{
    Boot, Failure, Fault, HostBoot, HostConfig, RunConfig, RunResult, Unsynced, WorldError, run,
};

/// What one start of the host does. Its result is reported to the control
/// task; a start that never finishes reports nothing.
pub(crate) type Start =
    Arc<dyn Fn(Runtime) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

pub(crate) fn start<F, Fut>(f: F) -> Start
where
    F: Fn(Runtime) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    Arc::new(move |runtime| Box::pin(f(runtime)))
}

/// What the control task does after a start: wait for it to finish, crash
/// the host at once with `unsynced`, restart it, and before the next start
/// is polled bound the file namespace to `capacity` bytes, which makes every
/// write that would pass the bound fail with `NoSpace`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Then {
    pub(crate) unsynced: Unsynced,
    pub(crate) capacity: u64,
}

impl Then {
    /// Every unsynced change lost, and a capacity no test reaches.
    pub(crate) const FINISH: Then = Then {
        unsynced: Unsynced::Lost,
        capacity: 1 << 30,
    };
}

/// What a start returned, `None` until it finishes.
type Outcome = Option<Result<(), String>>;

/// Runs one host through `starts`, in order. Before each start but the
/// first, the control task applies the `Then` paired with the start before.
/// Fails the test if any start that finished returned an error.
pub(crate) fn run_starts(seed: u64, starts: Vec<(Start, Then)>) {
    let results: Arc<Mutex<Vec<Outcome>>> = Arc::new(Mutex::new(vec![None; starts.len()]));
    let kept = Arc::clone(&results);
    let summary = run(RunConfig::new(seed), move |world| async move {
        let fail = |error: WorldError| Failure(Detail::new(&error.to_string()));
        let bodies: Vec<Start> = starts.iter().map(|(s, _)| Arc::clone(s)).collect();
        let reports = Arc::clone(&kept);
        let boot: Boot = Arc::new(move |b: HostBoot| {
            let index = (b.start - 1) as usize;
            let body = bodies.get(index).cloned();
            let reports = Arc::clone(&reports);
            Box::pin(async move {
                if let Some(body) = body {
                    let result = body(b.runtime).await;
                    reports.lock().unwrap_or_else(PoisonError::into_inner)[index] = Some(result);
                }
            })
        });
        let host = world.add_host(HostConfig::new(boot)).map_err(fail)?;
        let finished = |i: usize| kept.lock().unwrap_or_else(PoisonError::into_inner)[i].is_some();
        for (i, (_, then)) in starts.iter().enumerate() {
            if i + 1 == starts.len() {
                while !finished(i) {
                    world.sleep(Duration::from_millis(1)).await;
                }
                break;
            }
            while !finished(i) {
                world.sleep(Duration::from_millis(1)).await;
            }
            world
                .apply(Fault::Crash {
                    host,
                    after_polls: 0,
                    unsynced: then.unsynced,
                })
                .map_err(fail)?;
            world
                .apply(Fault::Restart {
                    host,
                    rng_replay: None,
                })
                .map_err(fail)?;
            world
                .apply(Fault::Capacity {
                    host,
                    bytes: then.capacity,
                })
                .map_err(fail)?;
        }
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
    for (i, result) in results
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .enumerate()
    {
        if let Some(Err(error)) = result {
            panic!("seed {seed}, start {}: {error}", i + 1);
        }
    }
}

/// Runs `body` once on a fresh host.
pub(crate) fn on_host<F, Fut>(seed: u64, body: F)
where
    F: Fn(Runtime) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    run_starts(seed, vec![(start(body), Then::FINISH)]);
}

/// Turns any error into the test's error string.
pub(crate) fn err<E: std::fmt::Display>(context: &'static str) -> impl Fn(E) -> String {
    move |e| format!("{context}: {e}")
}
