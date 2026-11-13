//! Other crates use address values, and mention std::fs only in comments.

use std::net::{IpAddr, SocketAddr};

/// Takes time from the runtime, never from `Instant::now`.
pub fn describe(addr: SocketAddr) -> (IpAddr, &'static str) {
    (addr.ip(), "std::fs and SystemTime::now in a string do not count")
}
