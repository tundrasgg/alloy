//! The runtime crate, found by its package name, may call the operating system.

use std::fs;
use std::time::Instant;

pub fn now() -> Instant {
    Instant::now()
}

pub fn read(path: &str) -> std::io::Result<Vec<u8>> {
    fs::read(path)
}
