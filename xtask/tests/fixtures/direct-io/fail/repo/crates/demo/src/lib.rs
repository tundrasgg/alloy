//! Each way to switch off the Clippy ban, and the values Clippy cannot see.

#![allow(clippy::disallowed_methods, reason = "no")]
#![cfg_attr(test, expect(
    clippy::disallowed_types,
))]

#[allow(clippy::style)]
pub fn styled() {}

#[expect(clippy::all)]
pub fn everything() {}

#[allow(clippy::disallowed_method)]
pub fn former_name() {}

pub fn os_value() -> u64 {
    use rand::TryRngCore;
    rand::rngs::OsRng.try_next_u64().unwrap_or(0)
}

pub fn sys_value() -> u64 {
    getrandom::SysRng.try_next_u64().unwrap_or(0)
}
