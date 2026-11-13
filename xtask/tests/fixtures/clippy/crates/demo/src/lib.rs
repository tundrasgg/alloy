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

/// Clock reads other than `now`: from the epoch, and since an instant.
pub fn elapsed(start: std::time::Instant) -> bool {
    let epoch = std::time::UNIX_EPOCH.elapsed().is_ok();
    let through_type = std::time::SystemTime::UNIX_EPOCH.elapsed().is_ok();
    epoch && through_type && start.elapsed().as_secs() > 0
}

/// The deprecated sleep and park, and the current thread.
#[allow(deprecated, reason = "the deprecated forms reach the clock too")]
pub fn waits() -> std::thread::ThreadId {
    std::thread::sleep_ms(1);
    std::thread::park_timeout_ms(1);
    std::thread::current().id()
}

/// The tokio clock beyond `now`, and its test controls.
pub async fn tokio_clock(start: tokio::time::Instant) -> bool {
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    tokio::time::resume();
    start.elapsed().as_secs() > 0
}

/// A runtime on the current thread that is not `Send`.
pub fn local_runtime(runtime: Option<tokio::runtime::LocalRuntime>) -> bool {
    runtime.is_some()
}

/// Windows links and named pipes.
#[cfg(windows)]
pub fn windows_only() -> bool {
    use tokio::net::windows::named_pipe;
    let file = std::os::windows::fs::symlink_file("a", "b").is_ok();
    let dir = std::os::windows::fs::symlink_dir("a", "c").is_ok();
    let client: Option<named_pipe::NamedPipeClient> =
        named_pipe::ClientOptions::new().open(r"\\.\pipe\x").ok();
    let server: Option<named_pipe::NamedPipeServer> =
        named_pipe::ServerOptions::new().create(r"\\.\pipe\x").ok();
    file && dir && client.is_some() && server.is_some()
}

/// Unix links, owners, roots, and pipes.
#[cfg(unix)]
pub fn unix_only() -> bool {
    use std::os::unix::fs;
    use tokio::net::unix::pipe;
    let link = fs::symlink("a", "b").is_ok();
    let owner = fs::chown("a", None, None).is_ok();
    let link_owner = fs::lchown("b", None, None).is_ok();
    let fd_owner = fs::fchown(std::io::stdin(), None, None).is_ok();
    let root = fs::chroot("/").is_ok();
    let pair = pipe::pipe().is_ok();
    let receiver: Option<pipe::Receiver> = pipe::OpenOptions::new().open_receiver("x").ok();
    let sender: Option<pipe::Sender> = None;
    link && owner && link_owner && fd_owner && root && pair && receiver.is_some() && sender.is_none()
}

/// Waits that end on the clock: on a channel and on a condition variable.
#[allow(deprecated, reason = "the deprecated form waits on the clock too")]
pub fn timed_waits(
    rx: &std::sync::mpsc::Receiver<u8>,
    lock: &std::sync::Mutex<bool>,
    cv: &std::sync::Condvar,
) -> Option<bool> {
    let tick = std::time::Duration::from_millis(1);
    let received = rx.recv_timeout(tick).is_ok();
    let guard = lock.lock().ok()?;
    let (guard, _) = cv.wait_timeout(guard, tick).ok()?;
    let (guard, _) = cv.wait_timeout_ms(guard, 1).ok()?;
    let (guard, _) = cv.wait_timeout_while(guard, tick, |ready| !*ready).ok()?;
    Some(received && *guard)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn single_threaded() {}

    #[tokio::test(flavor = "multi_thread")]
    async fn on_a_pool() {}
}
