//! The runtime crate, found by its package name, may switch the ban off.

// alloy-rt implements the runtime seams, so it calls the operating system.
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "alloy-rt implements the runtime seams"
)]

pub fn seed() -> u64 {
    use rand::TryRngCore;
    rand::rngs::OsRng.try_next_u64().unwrap_or(0)
}
