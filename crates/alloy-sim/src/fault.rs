//! Host faults (ARFC 0005 section 3.3): crash and restart, clock rate and
//! step, and stalls, applied now or scheduled at a world time (clause
//! 3.3.8).

use std::sync::Arc;
use std::time::Duration;

use alloy_seam::Detail;

use crate::core::{After, Core, Event, MAX_NANOS, Shared};
use crate::exec::Owner;
use crate::host::MAX_PPM;
use crate::trace::{KIND_CLOCK_FAULT, KIND_FAULT, text_field};
use crate::world::{HostId, WorldError};

/// Scheduled faults pending at once (section 7).
const MAX_SCHEDULED: usize = 1_048_576;

/// A fault the world injects (clause 3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Crashes the host right after its `after_polls`-th task poll counted
    /// from now, or at once for 0.
    Crash {
        /// The host.
        host: HostId,
        /// Polls of the host's tasks before the crash; 0 crashes at once.
        after_polls: u32,
        /// The fate of unsynced disk changes.
        unsynced: Unsynced,
    },
    /// Starts a crashed host. With `Some(s)`, its `rng` instances draw the
    /// streams they drew at start `s`.
    Restart {
        /// The host.
        host: HostId,
        /// A start from 1 to the last, whose `rng` streams to draw again.
        rng_replay: Option<u32>,
    },
    /// Sets the clock rate offset.
    ClockRate {
        /// The host.
        host: HostId,
        /// Parts per million, -500,000 to 500,000.
        ppm: i32,
    },
    /// Moves the wall clock.
    ClockStep {
        /// The host.
        host: HostId,
        /// Nanoseconds added to the wall clock.
        step_ns: i64,
    },
    /// Stops polling the host's tasks for a time (clause 3.3.6).
    Stall {
        /// The host.
        host: HostId,
        /// How long, from the instant the fault applies.
        length: Duration,
    },
}

/// What a crash does to writes and namespace changes that no completed
/// sync covers (clause 3.3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsynced {
    /// Every unsynced change is undone.
    Lost,
    /// Each unsynced change is kept or dropped by a draw.
    Seeded,
    /// As `Seeded`, with each write split into its blocks.
    Torn,
}

impl Fault {
    /// The host the fault is aimed at.
    fn host(&self) -> HostId {
        match self {
            Fault::Crash { host, .. }
            | Fault::Restart { host, .. }
            | Fault::ClockRate { host, .. }
            | Fault::ClockStep { host, .. }
            | Fault::Stall { host, .. } => *host,
        }
    }
}

fn state(text: &str) -> WorldError {
    WorldError::State(Detail::new(text))
}

impl Core {
    /// The checks that do not depend on state (clause 3.3.8): the host
    /// exists and values are in range.
    fn check_fault(&self, fault: &Fault) -> Result<(), WorldError> {
        self.host(fault.host())?;
        match fault {
            Fault::ClockRate { ppm, .. } if !(-MAX_PPM..=MAX_PPM).contains(ppm) => Err(
                WorldError::Invalid(Detail::new("ppm must be -500,000 to 500,000")),
            ),
            Fault::Restart {
                rng_replay: Some(0),
                ..
            } => Err(WorldError::Invalid(Detail::new("rng_replay starts at 1"))),
            _ => Ok(()),
        }
    }

    /// Applies `fault` at the current world instant, or returns the error and
    /// changes nothing.
    pub(crate) fn apply_fault(
        &mut self,
        shared: &Arc<Shared>,
        fault: Fault,
        after: &mut After,
    ) -> Result<(), WorldError> {
        self.check_fault(&fault)?;
        let now = shared.now_nanos();
        let target = fault.host();
        let host = self.host_mut(target)?;
        match fault {
            Fault::Crash { after_polls, .. } => {
                if host.crashed || host.pending_crash {
                    return Err(state("the host has crashed"));
                }
                let polled_here = self
                    .polling
                    .and_then(|key| self.exec.task(key))
                    .is_some_and(
                        |task| matches!(task.owner, Owner::Host { host, .. } if host == target),
                    );
                let polls = self.polls;
                let host = self.host_mut(target)?;
                if after_polls > 0 {
                    host.armed = Some((after_polls, polls));
                    let text = format!("crash armed after {after_polls} polls");
                    shared.push(Some(target), KIND_FAULT, &text_field(&text));
                } else if polled_here {
                    host.pending_crash = true;
                } else {
                    self.crash(shared, target, after);
                }
            }
            Fault::Restart { rng_replay, .. } => {
                if !host.crashed {
                    return Err(state("the host is running"));
                }
                if rng_replay.is_some_and(|s| s > host.start) {
                    return Err(WorldError::Invalid(Detail::new(
                        "rng_replay is after the last start",
                    )));
                }
                self.start(shared, target, rng_replay, after);
            }
            Fault::ClockRate { ppm, .. } => {
                host.set_rate(now, ppm);
                self.retime(target);
                clock_record(shared, target, ppm, 0);
            }
            Fault::ClockStep { step_ns, .. } => {
                let ppm = host.step(step_ns);
                clock_record(shared, target, ppm, step_ns);
            }
            Fault::Stall { length, .. } => {
                if host.crashed {
                    return Err(state("the host has crashed"));
                }
                self.stall(shared, target, length);
            }
        }
        Ok(())
    }

    /// Applies a scheduled fault at its time. One whose state check fails is
    /// skipped, and a `fault` record says so.
    pub(crate) fn apply_scheduled(
        &mut self,
        shared: &Arc<Shared>,
        fault: Fault,
        after: &mut After,
    ) {
        let host = fault.host();
        let text = format!("{fault:?}");
        if let Err(error) = self.apply_fault(shared, fault, after) {
            let text = format!("skipped {text}: {error}");
            shared.push(Some(host), KIND_FAULT, &text_field(&text));
        }
    }

    /// Schedules `fault` at world time `at` (clause 3.3.8).
    pub(crate) fn schedule_fault(
        &mut self,
        now: u64,
        at: Duration,
        fault: Fault,
    ) -> Result<(), WorldError> {
        let at = u64::try_from(at.as_nanos())
            .ok()
            .filter(|at| *at >= now && *at <= MAX_NANOS)
            .ok_or_else(|| {
                WorldError::Invalid(Detail::new("at must be from now to 2^63 - 1 ns"))
            })?;
        self.check_fault(&fault)?;
        if self.scheduled >= MAX_SCHEDULED {
            return Err(WorldError::Limit(Detail::new("1048576 scheduled faults")));
        }
        self.scheduled += 1;
        self.add_event(at, Event::Fault(fault));
        Ok(())
    }
}

/// Writes a `clock-fault` record.
fn clock_record(shared: &Shared, host: HostId, ppm: i32, step_ns: i64) {
    let mut body = ppm.to_le_bytes().to_vec();
    body.extend_from_slice(&step_ns.to_le_bytes());
    shared.push(Some(host), KIND_CLOCK_FAULT, &body);
}
