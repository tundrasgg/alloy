//! A runtime crate: the same reaches that `demo` makes pass here.

// `alloy-rt` implements the runtime seams, so it is where Alloy calls the
// operating system (ARFC 0005 clause 3.1.1).
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "alloy-rt implements the runtime seams and calls the operating system"
)]

use rand::SeedableRng;
use std::{collections::BTreeMap, process::Command};

/// Starts a child process through a grouped import.
pub fn child() -> BTreeMap<u8, bool> {
    let started = Command::new("sh").spawn().is_ok();
    BTreeMap::from([(0, started)])
}

/// Seeds from the operating system.
pub fn seeded() -> Option<rand::rngs::StdRng> {
    rand::rngs::StdRng::try_from_os_rng().ok()
}

/// Builds a thread pool through the attribute.
#[tokio::main]
pub async fn entry() {
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
}
