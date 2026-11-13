//! The simulator may switch the ban off too.

#![allow(clippy::disallowed_methods, reason = "alloy-sim implements the runtime seams")]

pub fn seed() -> u64 {
    getrandom::SysRng.try_next_u64().unwrap_or(0)
}
