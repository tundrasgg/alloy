//! The Alloy node daemon, `alloyd` (ARFC 0000 section 2.3).
//!
//! This release of `alloyd` starts the operating system runtime of `alloy-rt`
//! ([`OsHost`]), reports the release it is and the data directory it uses
//! (ARFC 0002 sections 3.3 and 3.12), checks that the data directory is an
//! Alloy data directory before it reads anything in it, logging why not every
//! 60 seconds when it is not (section 3.12), reads the node's local join
//! state from the host file of an Alloy data directory (ARFC 0017 section
//! 4.2), and runs until it is told to
//! stop: by SIGTERM or SIGINT on Linux, and by the Windows service control
//! manager on Windows. It then shuts the runtime down within
//! [`SHUTDOWN_GRACE`], well inside the 30 seconds an uninstall waits for it
//! (ARFC 0017 clause 3.16 step 5).
//!
//! [`run`] is the run loop. The binary calls it with the stop signals of its
//! process ([`os_signals`]) or, on Windows, of the service control manager
//! (the `service` module); tests call it with a [`Stop`] of their own.
//!
//! On a node whose local join state is `joined` and whose record holds
//! `control`, it serves the API port and the local admin socket over the
//! global group ([`ControlNode`]), and makes the node's own heartbeats.
//!
//! [`init`](mod@init) is the interface `alloy init` calls to create a cluster
//! (ARFC 0002 section 3.1).
//!
//! [`channel`] holds the sans-I/O parts of the node channel (ARFC 0002
//! section 3.13), which the node's `init`, `join`, and heartbeats run on.

#![forbid(unsafe_code)]

mod api;
mod args;
pub mod channel;
mod control;
mod data_dir;
pub mod init;
mod log;
#[cfg(windows)]
pub mod service;
mod signals;
mod stop;

use std::future::{Future, poll_fn};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use alloy_rt::{
    DiskError, EntryKind, ListenOptions, Listener, OsHost, Protection, RtPath, Runtime,
    RuntimeConfig, Security, StreamAddr, UnixPath, register_os,
};
use alloy_seam::{Binding, BuildError, ConfigValue, Detail, ImplName, RegistryBuilder};
use tracing::{error, info, warn};

pub use args::{Command, HELP, USAGE, UsageError, parse_args};
pub use control::{Both, ControlNode, Listeners, NodeError, NodePaths};
use data_dir::StartCheck;
pub use data_dir::{
    DataDirError, HOST_FILE, HostFileError, JoinState, LINUX_DATA_DIR, WINDOWS_DATA_DIR_NAME,
    default_data_dir, parse_host_file,
};
pub use log::LineLog;
pub use signals::os_signals;
pub use stop::{Stop, StopCause};

/// The release this `alloyd` is: the package version at build time, which the
/// release workflow sets from the release's tag (ARFC 0017 clause 3.13).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long [`run`] lets the runtime's tasks and blocking file operations end
/// once it is told to stop. An uninstall kills `alloyd` 30 seconds after its
/// termination request (ARFC 0017 clause 3.16 step 5); this leaves 10 of them
/// for the process to exit.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(20);

/// How often `alloyd` logs again why its data directory is not an Alloy data
/// directory, while it runs (ARFC 0002 section 3.12).
pub const NOT_ALLOY_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// What [`run`] needs to know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOptions {
    /// The data directory: an absolute path whose components are UTF-8
    /// ([`default_data_dir`] gives the default).
    pub data_dir: PathBuf,
}

/// Why `alloyd` could not run.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// The data directory cannot be found or used.
    #[error(transparent)]
    DataDir(#[from] DataDirError),
    /// The `tokio` runtime of [`OsHost`] did not start.
    #[error("the operating system runtime did not start: {0}")]
    Host(BuildError),
    /// The `os` runtime seams did not build.
    #[error("the runtime seams did not build: {0}")]
    Seams(BuildError),
    /// The process's stop signals cannot be watched.
    #[error("the stop signals of alloyd cannot be watched: {0}")]
    Signals(std::io::Error),
    /// The Windows service control manager refused the service.
    #[cfg(windows)]
    #[error("the Windows service control manager refused alloyd: {0}")]
    Service(windows_service::Error),
}

/// Runs `alloyd` until the future that `stop_when` returns completes, and
/// returns its [`StopCause`].
///
/// It starts an [`OsHost`], builds the `os` runtime seams on it, logs that it
/// started with its release and data directory, and logs the local join
/// state, naming `alloy init` and `alloy join` when the node is not
/// initialized. It calls `stop_when` inside the runtime's context, so that it
/// may register operating system signal handlers ([`os_signals`]), and waits.
/// It then shuts the host down, waiting at most [`SHUTDOWN_GRACE`], logs that
/// it stopped, and returns.
///
/// It logs through `tracing`, on the calling thread.
///
/// # Errors
///
/// [`DaemonError::DataDir`] when `options.data_dir` is not an absolute path
/// of UTF-8 components, [`DaemonError::Host`] and [`DaemonError::Seams`] when
/// the runtime does not start, and the error `stop_when` returns. Each is also
/// logged. The host is shut down before `run` returns in every case.
///
/// # Panics
///
/// When called from a task of a `tokio` runtime, as [`OsHost::block_on`].
pub fn run<F, W>(options: &RunOptions, stop_when: F) -> Result<StopCause, DaemonError>
where
    F: FnOnce() -> Result<W, DaemonError>,
    W: Future<Output = StopCause>,
{
    let outcome = start_and_serve(options, stop_when);
    match &outcome {
        Ok(_) => info!("alloyd stopped"),
        Err(e) => error!(error = %e, "alloyd stopped after a failure"),
    }
    outcome
}

fn start_and_serve<F, W>(options: &RunOptions, stop_when: F) -> Result<StopCause, DaemonError>
where
    F: FnOnce() -> Result<W, DaemonError>,
    W: Future<Output = StopCause>,
{
    let location = data_dir::Location::of(&options.data_dir)?;
    let host = OsHost::start(0).map_err(DaemonError::Host)?;
    let outcome = host.block_on(serve(&host, options, &location, stop_when));
    host.shutdown(SHUTDOWN_GRACE);
    outcome
}

async fn serve<F, W>(
    host: &OsHost,
    options: &RunOptions,
    location: &data_dir::Location,
    stop_when: F,
) -> Result<StopCause, DaemonError>
where
    F: FnOnce() -> Result<W, DaemonError>,
    W: Future<Output = StopCause>,
{
    let (runtime, transport) = os_runtime(host, location)?;
    info!(
        version = VERSION,
        data_dir = %options.data_dir.display(),
        "alloyd started"
    );
    let check = data_dir::check_at_start(runtime.disk_io().as_ref(), location).await;
    let mut node = None;
    match &check {
        StartCheck::Alloy => {
            if report_join_state(&runtime, location).await == Some(JoinState::Joined) {
                node = start_node(&runtime, options, location, transport).await;
            }
        }
        StartCheck::Missing => log_not_initialized(),
        StartCheck::NotAlloy(reason) => log_not_alloy(reason),
    }
    let stop = match stop_when() {
        Ok(stop) => stop,
        Err(e) => {
            stop_node(node).await;
            return Err(e);
        }
    };
    let cause = match check {
        StartCheck::NotAlloy(reason) => wait_logging(&runtime, stop, &reason).await,
        StartCheck::Alloy | StartCheck::Missing => stop.await,
    };
    info!(cause = %cause, "alloyd stopping");
    stop_node(node).await;
    Ok(cause)
}

/// The local admin socket (ARFC 0007 clause 3.10.2).
pub const ADMIN_SOCKET: &str = "/run/alloy/admin.sock";

/// Starts serving the node of the data directory, when it holds
/// `control`. `None`, logged, when it does not or cannot.
async fn start_node(
    runtime: &Runtime,
    options: &RunOptions,
    location: &data_dir::Location,
    transport: alloy_api::Transport,
) -> Option<ControlNode> {
    let paths = NodePaths {
        data: alloy_id::DataDir::new(Arc::clone(runtime.disk_io()), location.dir().clone()),
        dir: location.dir().clone(),
        native: options.data_dir.clone(),
    };
    let admin = admin_dir_ready(runtime).await;
    let listen = |record: &alloy_id::NodeRecord| listeners(runtime, record.api_port, admin);
    match ControlNode::start(runtime, &paths, listen, transport).await {
        Ok(node) => {
            info!(
                node = node.name(),
                "serving the API and the local admin socket"
            );
            Some(node)
        }
        Err(NodeError::NotControl) => None,
        Err(e) => {
            error!(error = %e, "this node cannot serve");
            None
        }
    }
}

async fn stop_node(node: Option<ControlNode>) {
    if let Some(node) = node
        && let Err(e) = node.stop().await
    {
        warn!(error = %e, "the control node did not stop cleanly");
    }
}

/// The API port on every local address, IPv4 and IPv6 (ARFC 0008 clause
/// 3.8), and the local admin socket when `admin`.
fn listeners(runtime: &Runtime, api_port: u16, admin: bool) -> Result<Listeners, NodeError> {
    let net = runtime.net();
    let options = ListenOptions::default();
    let v4 = net.listen(
        StreamAddr::Ip(SocketAddr::from((Ipv4Addr::UNSPECIFIED, api_port))),
        options,
    )?;
    let v6 = net.listen(
        StreamAddr::Ip(SocketAddr::from((Ipv6Addr::UNSPECIFIED, api_port))),
        options,
    );
    let api: Box<dyn Listener> = match v6 {
        Ok(v6) => Box::new(Both(v4, v6)),
        Err(e) => {
            warn!(reason = %e, "the API port is not served on IPv6");
            v4
        }
    };
    let admin = if admin {
        let path = UnixPath::new(ADMIN_SOCKET)?;
        match net.listen(StreamAddr::Unix(path), options) {
            Ok(listener) => Some(listener),
            Err(e) => {
                warn!(reason = %e, "the local admin socket could not be opened");
                None
            }
        }
    } else {
        None
    };
    Ok(Listeners { api, admin })
}

/// Makes `/run/alloy` ready for the local admin socket (ARFC 0007 clause
/// 3.10.2): creates it `Private` when missing, refuses it when it is not a
/// directory owned by user id 0 with mode `0700`, and removes an entry an
/// earlier start left at the socket path. `false`, logged, when no socket
/// may be created, as on Windows, which has no local admin socket.
async fn admin_dir_ready(runtime: &Runtime) -> bool {
    if !cfg!(unix) {
        return false;
    }
    let disk = runtime.disk_io();
    let Ok(dir) = RtPath::new("run/alloy") else {
        return false;
    };
    let refuse = |why: String| {
        warn!(reason = %why, "no local admin socket is created");
        false
    };
    match disk.security(&dir).await {
        Err(DiskError::NotFound) => {
            if let Err(e) = disk.create_dir_protected(&dir, Protection::Private).await {
                return refuse(format!("/run/alloy could not be created: {e}"));
            }
        }
        Ok(Security::Unix {
            kind: EntryKind::Dir,
            symlink: false,
            uid: 0,
            mode: 0o700,
        }) => {}
        Ok(other) => {
            return refuse(format!(
                "/run/alloy is not a directory owned by user id 0 with mode 0700: {other:?}"
            ));
        }
        Err(e) => return refuse(format!("/run/alloy cannot be checked: {e}")),
    }
    let Ok(socket) = dir.join("admin.sock") else {
        return false;
    };
    match disk.remove(&socket).await {
        Ok(()) | Err(DiskError::NotFound) => true,
        Err(e) => refuse(format!("the old {ADMIN_SOCKET} could not be removed: {e}")),
    }
}

/// Waits for `stop`, logging `reason` again every [`NOT_ALLOY_LOG_INTERVAL`]
/// (ARFC 0002 section 3.12).
async fn wait_logging<W: Future<Output = StopCause>>(
    runtime: &Runtime,
    stop: W,
    reason: &str,
) -> StopCause {
    let mut stop = pin!(stop);
    loop {
        let mut sleep = runtime.sleep(NOT_ALLOY_LOG_INTERVAL);
        let woke = poll_fn(|cx| {
            if let Poll::Ready(cause) = stop.as_mut().poll(cx) {
                return Poll::Ready(Some(cause));
            }
            sleep.as_mut().poll(cx).map(|()| None)
        })
        .await;
        match woke {
            Some(cause) => return cause,
            None => log_not_alloy(reason),
        }
    }
}

fn log_not_alloy(reason: &str) {
    warn!(
        reason = %reason,
        "the data directory is not an Alloy data directory, so alloyd reads no secret and serves nothing"
    );
}

fn log_not_initialized() {
    info!(
        "this node is not initialized: run alloy init to create a cluster on it, \
         or alloy join with a join token to join a cluster"
    );
}

/// Logs the local join state that the host file of an Alloy data directory
/// holds, and returns it, `None` when it cannot be read.
async fn report_join_state(runtime: &Runtime, location: &data_dir::Location) -> Option<JoinState> {
    match data_dir::read_join_state(runtime.disk_io().as_ref(), location).await {
        Ok(JoinState::Unjoined) => {
            log_not_initialized();
            Some(JoinState::Unjoined)
        }
        Ok(state) => {
            info!(join_state = %state, "local join state");
            Some(state)
        }
        Err(e) => {
            warn!(
                host_file = %location.host_file_display(),
                reason = %e,
                "the host file cannot be read, so the local join state is unknown"
            );
            None
        }
    }
}

/// The `os` runtime seams on `host`, with `disk-io` rooted at the root of the
/// data directory's file system, and the API's transport on them.
fn os_runtime(
    host: &OsHost,
    location: &data_dir::Location,
) -> Result<(Runtime, alloy_api::Transport), DaemonError> {
    let mut builder = RegistryBuilder::new();
    register_os(&mut builder, host)
        .map_err(|e| DaemonError::Seams(BuildError::Failed(Detail::new(&e.to_string()))))?;
    let registry = builder.build();
    let os = ImplName::new("os")
        .map_err(|e| DaemonError::Seams(BuildError::Failed(Detail::new(&e.to_string()))))?;
    let mut config = RuntimeConfig::all(os.clone());
    config.disk_io = Binding {
        name: os,
        config: disk_io_config(location.native_root())?,
    };
    let runtime = Runtime::from_registry(&registry, &config).map_err(DaemonError::Seams)?;
    Ok((runtime, alloy_api::Transport::of(&config)))
}

/// Version 1 of the `os` `disk-io` configuration, `message OsDiskIoConfig {
/// string root = 1; }` (ARFC 0005 section 4.2), holding `root`.
fn disk_io_config(root: &str) -> Result<ConfigValue, DaemonError> {
    // Field 1, wire type 2 (length-delimited).
    const ROOT_KEY: u8 = 0x0A;
    let mut payload = vec![ROOT_KEY];
    let mut len = root.len();
    while len >= 0x80 {
        payload.push((len & 0x7F) as u8 | 0x80);
        len >>= 7;
    }
    payload.push(len as u8);
    payload.extend_from_slice(root.as_bytes());
    ConfigValue::new(1, payload).map_err(|e| DaemonError::Seams(BuildError::Config(e)))
}

#[cfg(test)]
#[path = "tests/node.rs"]
mod node_tests;

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::task::Poll;

    use super::*;

    /// A log sink that tests read back.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn logged<T>(body: impl FnOnce() -> T) -> (T, String) {
        let sink = Captured::default();
        let out = tracing::subscriber::with_default(LineLog::new(sink.clone()), body);
        (out, sink.text())
    }

    /// A data directory that does not exist, below one that does.
    fn missing_data_dir() -> PathBuf {
        std::env::temp_dir().join("alloyd-test-no-such-data-directory")
    }

    #[test]
    fn run_starts_logs_and_stops_on_a_stop_request() {
        let stop = Stop::new();
        stop.trigger(StopCause::Interrupt);
        let options = RunOptions {
            data_dir: missing_data_dir(),
        };
        let (cause, log) = logged(|| run(&options, || Ok(stop.wait())));
        assert_eq!(cause.unwrap(), StopCause::Interrupt);
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 4, "{log}");
        assert!(
            lines[0].starts_with("INFO alloyd started version="),
            "{log}"
        );
        assert!(lines[0].contains(&format!("version={VERSION}")), "{log}");
        assert!(
            lines[0].contains(&options.data_dir.display().to_string()),
            "{log}"
        );
        assert!(
            lines[1].starts_with("INFO this node is not initialized"),
            "{log}"
        );
        assert!(lines[1].contains("alloy init"), "{log}");
        assert!(lines[1].contains("alloy join"), "{log}");
        assert_eq!(lines[2], "INFO alloyd stopping cause=SIGINT");
        assert_eq!(lines[3], "INFO alloyd stopped");
    }

    #[test]
    fn run_waits_for_a_stop_that_comes_later() {
        let options = RunOptions {
            data_dir: std::env::temp_dir(),
        };
        let mut polls = 0;
        let (cause, log) = logged(|| {
            run(&options, || {
                Ok(poll_fn(move |cx| {
                    polls += 1;
                    if polls < 3 {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    } else {
                        Poll::Ready(StopCause::Terminate)
                    }
                }))
            })
        });
        assert_eq!(cause.unwrap(), StopCause::Terminate);
        assert!(
            log.contains("INFO alloyd stopping cause=SIGTERM\n"),
            "{log}"
        );
        assert!(log.ends_with("INFO alloyd stopped\n"), "{log}");
    }

    /// Runs `body` on the `os` `disk-io` seam of a fresh host, rooted as
    /// `alloyd` roots it for the data directory `dir`.
    fn on_disk(
        dir: &std::path::Path,
        body: impl for<'a> FnOnce(
            &'a dyn alloy_rt::DiskIo,
            &'a data_dir::Location,
        ) -> std::pin::Pin<Box<dyn Future<Output = ()> + 'a>>,
    ) {
        let location = data_dir::Location::of(dir).unwrap();
        let host = OsHost::start(0).unwrap();
        let (runtime, _) = os_runtime(&host, &location).unwrap();
        host.block_on(body(runtime.disk_io().as_ref(), &location));
        host.shutdown(SHUTDOWN_GRACE);
    }

    /// T-0091 review F1: a host file planted in a directory that a user
    /// created gives no join state, and `alloyd` says why.
    #[test]
    fn a_host_file_outside_an_alloy_data_directory_is_not_read() {
        let dir = std::env::temp_dir().join("alloyd-test-planted-host-file");
        on_disk(&dir, |disk, location| {
            Box::pin(async move {
                match disk.create_dir(location.dir()).await {
                    Ok(()) | Err(DiskError::AlreadyExists) => {}
                    Err(e) => panic!("{e}"),
                }
                let options = alloy_rt::OpenOptions {
                    access: alloy_rt::Access::ReadWrite,
                    create: alloy_rt::Create::IfMissing,
                    direct: false,
                    exclusive: false,
                };
                let file = disk.open(location.host_file(), options).await.unwrap();
                let buf = alloy_rt::IoBuf::from_slice(b"alloy-host 1\njoin joined\n").unwrap();
                let done = file
                    .write_at(vec![alloy_rt::WriteOp { offset: 0, buf }])
                    .await;
                assert!(done.iter().all(|d| d.result.is_ok()));
            })
        });
        let stop = Stop::new();
        stop.trigger(StopCause::Terminate);
        let options = RunOptions {
            data_dir: dir.clone(),
        };
        let (cause, log) = logged(|| run(&options, || Ok(stop.wait())));
        on_disk(&dir, |disk, location| {
            Box::pin(async move {
                disk.remove(location.host_file()).await.unwrap();
                disk.remove(location.dir()).await.unwrap();
            })
        });
        assert_eq!(cause.unwrap(), StopCause::Terminate);
        assert!(!log.contains("join_state"), "{log}");
        let lines: Vec<&str> = log.lines().collect();
        assert!(
            lines[1].starts_with(
                "WARN the data directory is not an Alloy data directory, \
                 so alloyd reads no secret and serves nothing reason="
            ),
            "{log}"
        );
    }

    #[test]
    fn run_returns_and_logs_the_error_of_stop_when() {
        let options = RunOptions {
            data_dir: missing_data_dir(),
        };
        let (outcome, log) = logged(|| {
            run(&options, || {
                Err::<std::future::Ready<StopCause>, _>(DaemonError::Signals(
                    std::io::Error::other("refused"),
                ))
            })
        });
        assert!(matches!(outcome, Err(DaemonError::Signals(_))));
        assert!(log.contains("INFO alloyd started"), "{log}");
        assert!(
            log.ends_with(
                "ERROR alloyd stopped after a failure \
                 error=\"the stop signals of alloyd cannot be watched: refused\"\n"
            ),
            "{log}"
        );
    }

    #[test]
    fn run_refuses_a_relative_data_directory_before_starting() {
        let options = RunOptions {
            data_dir: PathBuf::from("relative/alloyd"),
        };
        let (outcome, log) = logged(|| run(&options, || Ok(std::future::pending())));
        assert!(matches!(
            outcome,
            Err(DaemonError::DataDir(DataDirError::NotAbsolute { .. }))
        ));
        assert!(!log.contains("alloyd started"), "{log}");
    }

    #[test]
    fn disk_io_config_encodes_the_root_field() {
        let value = disk_io_config("/").unwrap();
        assert_eq!(value.version(), 1);
        assert_eq!(value.payload(), [0x0A, 1, b'/']);
        let long = "r".repeat(200);
        let value = disk_io_config(&long).unwrap();
        assert_eq!(value.payload()[..3], [0x0A, 0xC8, 0x01]);
        assert_eq!(&value.payload()[3..], long.as_bytes());
    }
}
