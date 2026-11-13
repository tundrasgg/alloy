//! Other crates leave the ban on. Mentions in comments do not count:
//! `#![allow(clippy::disallowed_methods)]` and `OsRng`.

#![deny(clippy::all)]
#![warn(clippy::disallowed_methods)]
#![cfg_attr(test, allow(dead_code))]

use std::net::{IpAddr, SocketAddr};

#[allow(clippy::needless_return, reason = "a lint that holds no ban")]
pub fn describe(addr: SocketAddr) -> (IpAddr, &'static str) {
    return (addr.ip(), "#[allow(clippy::all)] and OsRng in a string do not count");
}
