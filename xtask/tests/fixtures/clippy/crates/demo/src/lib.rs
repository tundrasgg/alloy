//! Any other crate: each reach below is rejected, except the values at the end.

use rand::SeedableRng;
use std::{collections::BTreeMap, process::Command};

/// A grouped import.
pub fn grouped() -> BTreeMap<u8, bool> {
    let started = Command::new("sh").spawn().is_ok();
    BTreeMap::from([(0, started)])
}

/// A renamed import.
pub fn renamed() -> std::time::Instant {
    use std::time::Instant as Clock;
    Clock::now()
}

/// A glob import.
pub fn globbed() -> std::time::SystemTime {
    use std::time::*;
    SystemTime::now()
}

macro_rules! spawn_it {
    () => {
        std::thread::spawn(|| ())
    };
}

/// A call that a macro generates.
pub fn generated() -> bool {
    spawn_it!().join().is_ok()
}

/// The fallible and the infallible seeding from the operating system.
pub fn seeded() -> Option<rand::rngs::StdRng> {
    let _ = rand::rngs::StdRng::from_os_rng();
    rand::rngs::StdRng::try_from_os_rng().ok()
}

/// The thread-local generator.
pub fn per_thread() -> u8 {
    rand::random()
}

/// The operating system generator as a unit struct value. Clippy does not see
/// a type used as a value, so `check-repo` rejects `OsRng` by name.
pub fn os_value() -> u64 {
    use rand::TryRngCore;
    rand::rngs::OsRng.try_next_u64().unwrap_or(0)
}

/// A thread pool built by the attribute.
#[tokio::main]
pub async fn entry() {
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
}

/// A single-threaded runtime built by the attribute.
#[tokio::main(flavor = "current_thread")]
pub async fn single() {}

/// Tasks, files, sockets, and paths.
pub async fn reaches(path: &std::path::Path) -> bool {
    let task = tokio::spawn(async {});
    let read = tokio::fs::read("x").await.is_ok();
    let file = std::fs::File::open("x").is_ok();
    let socket = tokio::net::TcpStream::connect("127.0.0.1:1").await.is_ok();
    task.await.is_ok() && read && file && socket && path.exists()
}

/// Values that reach nothing: addresses, durations, exit codes.
pub fn values(addr: std::net::SocketAddr) -> (std::time::Duration, std::process::ExitCode) {
    let _ = addr.port();
    (std::time::Duration::ZERO, std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn single_threaded() {}

    #[tokio::test(flavor = "multi_thread")]
    async fn on_a_pool() {}
}
