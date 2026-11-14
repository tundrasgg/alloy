//! The deterministic simulator of Alloy (ARFC 0005).
//!
//! A scenario is a function that builds a [`World`] and decides the result of
//! a run. [`run`] calls it once with a fresh world and drives the returned
//! future, the control task, on the calling thread: one thread, a seeded
//! scheduler, and a virtual clock, so that a run is a pure function of the
//! binary and the seed (ARFC 0005 clause 3.2.1).
//!
//! Everything the simulator does is recorded in the event trace (ARFC 0005
//! section 4.1). Its BLAKE3 hash, the trace hash, is printed in the summary
//! line that ends every run, and two runs with one seed produce the same
//! line. Observers ([`World::observe`]) see each record as it is written.
//!
//! The world time starts at 0 and advances only when nothing is runnable, to
//! the next pending event. A run ends when the control task completes or
//! panics, when the world runs out of events before the budget, or at a bound
//! of section 7 of the ARFC.
//!
//! ```
//! use std::time::Duration;
//!
//! use alloy_sim::{RunConfig, RunResult, run};
//!
//! let summary = run(RunConfig::new(7), |world| async move {
//!     world.sleep(Duration::from_secs(5)).await;
//!     world.note("five seconds in").ok();
//!     Ok(())
//! });
//! assert_eq!(summary.result, RunResult::Pass);
//! assert_eq!(summary.virtual_time, Duration::from_secs(5));
//! ```

#![forbid(unsafe_code)]
// The simulator is a runtime crate (ARFC 0005 clause 3.1.1, D-0017): it
// streams the event trace to a file and writes the summary line itself.
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "alloy-sim implements the runtime seams and writes the trace file"
)]

mod core;
mod disk;
mod exec;
mod fault;
mod history;
mod hook;
mod host;
mod machine;
mod run;
mod stream;
mod trace;
mod world;

pub use fault::{DiskTarget, Fault, Ppm, Unsynced};
pub use history::{Call, ClientId, Entry, History};
pub use host::{Boot, DeviceConfig, HostBoot, HostConfig, HostState};
pub use machine::MachineConfig;
pub use run::{Failure, RunConfig, RunResult, RunSummary, run};
pub use trace::{Observer, Record};
pub use world::{HostId, World, WorldError};
