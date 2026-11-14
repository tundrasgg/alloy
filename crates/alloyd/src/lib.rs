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
//! `control`, it serves the API port, the local admin socket, and the join
//! service over the global group ([`ControlNode`]), and makes the node's own
//! heartbeats. On any other joined node, a Linux node or a client node, it
//! heartbeats to and renews with the control plane over AL sessions to its
//! homes ([`MemberNode`]). On a node in local join state `joining` it first
//! resumes the join ([`join::resume`]).
//!
//! [`init`](mod@init) is the interface `alloy init` calls to create a cluster
//! (ARFC 0002 section 3.1), and [`join`](mod@join) the one `alloy join`
//! calls to join one (section 3.3).
//!
//! [`channel`] holds the sans-I/O parts of the node channel (ARFC 0002
//! section 3.13), which the node's `init`, `join`, and heartbeats run on.

#![forbid(unsafe_code)]

mod api;
mod args;
pub mod channel;
mod control;
mod data_dir;
mod host_objects;
pub mod init;
pub mod join;
mod log;
mod member;
mod mesh;
mod nodes;
mod records;
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
    DiskError, DiskIo, EntryKind, ListenOptions, Listener, OsHost, Protection, RtPath, Runtime,
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
pub use member::MemberNode;
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
        StartCheck::Alloy => match report_join_state(&runtime, location).await {
            Some(JoinState::Joined) => {
                node = start_node(&runtime, options, location, transport).await;
            }
            Some(JoinState::Joining) => {
                let paths = node_paths(&runtime, options, location);
                match join::resume(&runtime, &paths).await {
                    Ok(joined) => {
                        info!(node = %joined.node, "the join was resumed and is complete");
                        node = start_node(&runtime, options, location, transport).await;
                    }
                    Err(e) => warn!(error = %e, "the join could not be resumed"),
                }
            }
            Some(JoinState::Unjoined) | None => {}
        },
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

/// A node `alloyd` serves.
#[derive(Debug)]
pub(crate) enum Serving {
    Control(ControlNode),
    Member(MemberNode),
}

/// Serves the joined node of the data directory at `paths`: as a control
/// node over the listeners `listen` opens when its record holds `control`,
/// else as a member node over the mesh sockets `bind` binds.
///
/// # Errors
///
/// The [`NodeError`] of the start that applies.
pub(crate) async fn serve_joined<L, B>(
    runtime: &Runtime,
    paths: &NodePaths,
    listen: L,
    bind: B,
    transport: alloy_api::Transport,
) -> Result<Serving, NodeError>
where
    L: FnOnce(&alloy_id::NodeRecord) -> Result<Listeners, NodeError>,
    B: FnOnce(&alloy_id::NodeRecord) -> Result<Vec<Box<dyn alloy_rt::UdpSocket>>, NodeError>,
{
    match ControlNode::start(runtime, paths, listen, transport).await {
        Ok(node) => {
            if node.serves_local_admin() {
                info!(
                    node = node.name(),
                    "serving the API, the local admin socket, and the join service"
                );
            } else {
                info!(node = node.name(), "serving the API and the join service");
            }
            Ok(Serving::Control(node))
        }
        Err(NodeError::NotControl) => Ok(Serving::Member(
            MemberNode::start(runtime, paths, bind).await?,
        )),
        Err(e) => Err(e),
    }
}

fn node_paths(runtime: &Runtime, options: &RunOptions, location: &data_dir::Location) -> NodePaths {
    NodePaths {
        data: alloy_id::DataDir::new(Arc::clone(runtime.disk_io()), location.dir().clone()),
        dir: location.dir().clone(),
        native: options.data_dir.clone(),
    }
}

/// Starts serving the joined node of the data directory: as a control node
/// when it holds `control`, else as a member node. `None`, logged, when it
/// cannot.
async fn start_node(
    runtime: &Runtime,
    options: &RunOptions,
    location: &data_dir::Location,
    transport: alloy_api::Transport,
) -> Option<Serving> {
    let paths = node_paths(runtime, options, location);
    let admin = admin_dir_ready(runtime, &paths.data).await;
    let listen = |record: &alloy_id::NodeRecord| listeners(runtime, record, admin);
    let bind = |record: &alloy_id::NodeRecord| {
        mesh::bind(runtime, record.mesh_port).map_err(NodeError::Listen)
    };
    match serve_joined(runtime, &paths, listen, bind, transport).await {
        Ok(node) => Some(node),
        Err(e) => {
            error!(error = %e, "this node cannot serve");
            None
        }
    }
}

impl Serving {
    /// Stops the node, logging a control node that does not stop cleanly.
    pub(crate) async fn stop(self) {
        match self {
            Serving::Control(node) => {
                if let Err(e) = node.stop().await {
                    warn!(error = %e, "the control node did not stop cleanly");
                }
            }
            Serving::Member(node) => node.stop().await,
        }
    }
}

async fn stop_node(node: Option<Serving>) {
    if let Some(node) = node {
        node.stop().await;
    }
}

/// The API port on every local address, IPv4 and IPv6 (ARFC 0008 clause
/// 3.8), the mesh port likewise, and the local admin socket when `admin`.
fn listeners(
    runtime: &Runtime,
    record: &alloy_id::NodeRecord,
    admin: bool,
) -> Result<Listeners, NodeError> {
    let api_port = record.api_port;
    let mesh = mesh::bind(runtime, record.mesh_port)?;
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
    Ok(Listeners { api, admin, mesh })
}

/// Makes `/run/alloy` ready for the local admin socket (ARFC 0007 clause
/// 3.10.2): creates it `Private` when missing, after adding its `run` line
/// to the host file of `data` (ARFC 0017 section 4.2), refuses it when it is
/// not a directory owned by user id 0 with mode `0700`, and removes an entry
/// an earlier start left at the socket path. `false`, logged, when no socket
/// may be created, as on Windows, which has no local admin socket.
async fn admin_dir_ready(runtime: &Runtime, data: &alloy_id::DataDir) -> bool {
    if !cfg!(unix) {
        return false;
    }
    prepare_admin_dir(runtime.disk_io().as_ref(), data).await
}

/// The work of [`admin_dir_ready`] on `disk`, a `disk-io` rooted at `/`, on
/// any platform. Every `false` is logged at warn.
async fn prepare_admin_dir(disk: &dyn DiskIo, data: &alloy_id::DataDir) -> bool {
    let refuse = |why: String| {
        warn!(reason = %why, "no local admin socket is created");
        false
    };
    let socket = match RtPath::new(ADMIN_SOCKET) {
        Ok(socket) => socket,
        Err(e) => return refuse(format!("{ADMIN_SOCKET} is not a disk-io path: {e}")),
    };
    let Some(dir) = socket.parent() else {
        return refuse(format!("{ADMIN_SOCKET} has no directory"));
    };
    match disk.security(&dir).await {
        Err(DiskError::NotFound) => {
            let line = match host_objects::read_boot_id(disk).await {
                Ok(boot_id) => host_objects::run_line(&boot_id, &dir),
                Err(e) => return refuse(format!("/run/alloy cannot be recorded: {e}")),
            };
            if let Err(e) = host_objects::add_line(data, &line).await {
                return refuse(format!("/run/alloy cannot be recorded: {e}"));
            }
            if let Err(e) = disk.create_dir_protected(&dir, Protection::Private).await {
                if let Err(undo) = host_objects::remove_line(data, &line).await {
                    warn!(line = %line, reason = %undo, "the host file line could not be removed");
                }
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
            &'a dyn DiskIo,
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

    /// The boot id the `mem` hosts of these tests report.
    const BOOT_ID: &str = "6a2b8f0e-1c3d-4e5f-8a9b-0c1d2e3f4a5b";

    /// The host file of a joined node before `alloyd` adds any line.
    const HOST_HEAD: &str = "alloy-host 1\njoin joined\nrelease none\n";

    /// The `run` line of `/run/alloy` in the boot [`BOOT_ID`].
    fn run_line() -> String {
        format!("run {BOOT_ID} /run/alloy")
    }

    /// Runs `body` on the `mem` `disk-io` of a fresh simulated host, rooted
    /// at `/`, after creating `/run` and the boot id file as a Linux host has
    /// them, and a data directory at `/var/lib/alloyd` whose host file is
    /// [`HOST_HEAD`].
    fn on_mem_disk<F>(body: F)
    where
        F: Fn(Arc<dyn DiskIo>, alloy_id::DataDir) -> alloy_rt::BoxFuture<'static, ()>
            + Send
            + Sync
            + 'static,
    {
        use alloy_sim::{HostBoot, HostConfig, RunConfig, RunResult};
        let done: Arc<Mutex<bool>> = Arc::default();
        let finished = Arc::clone(&done);
        let body = Arc::new(body);
        let summary = alloy_sim::run(RunConfig::new(1), move |world| async move {
            let set = Arc::clone(&finished);
            world
                .add_host(HostConfig::new(Arc::new(move |boot: HostBoot| {
                    let set = Arc::clone(&set);
                    let body = Arc::clone(&body);
                    Box::pin(async move {
                        let disk = Arc::clone(boot.runtime.disk_io());
                        for dir in [
                            "/run",
                            "/proc",
                            "/proc/sys",
                            "/proc/sys/kernel",
                            "/proc/sys/kernel/random",
                            "/var",
                            "/var/lib",
                        ] {
                            disk.create_dir(&RtPath::new(dir).unwrap()).await.unwrap();
                        }
                        let random = RtPath::new("/proc/sys/kernel/random").unwrap();
                        alloy_id::DataDir::new(Arc::clone(&disk), random)
                            .write("boot_id", format!("{BOOT_ID}\n").as_bytes())
                            .await
                            .unwrap();
                        let dir = RtPath::new("/var/lib/alloyd").unwrap();
                        let data = alloy_id::DataDir::create(Arc::clone(&disk), dir)
                            .await
                            .unwrap();
                        data.write(HOST_FILE, HOST_HEAD.as_bytes()).await.unwrap();
                        body(disk, data).await;
                        *set.lock().unwrap() = true;
                    })
                })))
                .unwrap();
            while !*finished.lock().unwrap() {
                world.sleep(Duration::from_millis(1)).await;
            }
            Ok(())
        });
        assert_eq!(summary.result, RunResult::Pass, "{}", summary.line());
        assert!(*done.lock().unwrap());
    }

    /// The host file of `data`, as text.
    async fn host_text(data: &alloy_id::DataDir) -> String {
        let bytes = data.read(HOST_FILE).await.unwrap().unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn admin_dir() -> RtPath {
        RtPath::new("/run/alloy").unwrap()
    }

    fn admin_socket() -> RtPath {
        admin_dir().join("admin.sock").unwrap()
    }

    /// A `disk-io` that, when asked to create `/run/alloy`, first records
    /// the host file and whether `/run/alloy` exists, then creates it.
    #[derive(Debug)]
    struct Watched {
        inner: Arc<dyn DiskIo>,
        data: alloy_id::DataDir,
        seen: Mutex<Option<(String, bool)>>,
    }

    impl DiskIo for Watched {
        fn open<'a>(
            &'a self,
            path: &'a RtPath,
            options: alloy_rt::OpenOptions,
        ) -> alloy_rt::BoxFuture<'a, Result<Box<dyn alloy_rt::DiskFile>, DiskError>> {
            self.inner.open(path, options)
        }
        fn metadata<'a>(
            &'a self,
            path: &'a RtPath,
        ) -> alloy_rt::BoxFuture<'a, Result<alloy_rt::Metadata, DiskError>> {
            self.inner.metadata(path)
        }
        fn list<'a>(
            &'a self,
            dir: &'a RtPath,
        ) -> alloy_rt::BoxFuture<'a, Result<Vec<alloy_rt::DirEntry>, DiskError>> {
            self.inner.list(dir)
        }
        fn create_dir<'a>(
            &'a self,
            path: &'a RtPath,
        ) -> alloy_rt::BoxFuture<'a, Result<(), DiskError>> {
            self.inner.create_dir(path)
        }
        fn remove<'a>(
            &'a self,
            path: &'a RtPath,
        ) -> alloy_rt::BoxFuture<'a, Result<(), DiskError>> {
            self.inner.remove(path)
        }
        fn rename<'a>(
            &'a self,
            from: &'a RtPath,
            to: &'a RtPath,
        ) -> alloy_rt::BoxFuture<'a, Result<(), DiskError>> {
            self.inner.rename(from, to)
        }
        fn replace<'a>(
            &'a self,
            from: &'a RtPath,
            to: &'a RtPath,
        ) -> alloy_rt::BoxFuture<'a, Result<(), DiskError>> {
            self.inner.replace(from, to)
        }
        fn sync_dir<'a>(
            &'a self,
            dir: &'a RtPath,
        ) -> alloy_rt::BoxFuture<'a, Result<(), DiskError>> {
            self.inner.sync_dir(dir)
        }
        fn create_file<'a>(
            &'a self,
            path: &'a RtPath,
            protection: Protection,
        ) -> alloy_rt::BoxFuture<'a, Result<Box<dyn alloy_rt::DiskFile>, DiskError>> {
            self.inner.create_file(path, protection)
        }
        fn create_dir_protected<'a>(
            &'a self,
            path: &'a RtPath,
            protection: Protection,
        ) -> alloy_rt::BoxFuture<'a, Result<(), DiskError>> {
            Box::pin(async move {
                if *path == admin_dir() {
                    let text = host_text(&self.data).await;
                    let exists = self.inner.security(path).await.is_ok();
                    *self.seen.lock().unwrap() = Some((text, exists));
                }
                self.inner.create_dir_protected(path, protection).await
            })
        }
        fn security<'a>(
            &'a self,
            path: &'a RtPath,
        ) -> alloy_rt::BoxFuture<'a, Result<Security, DiskError>> {
            self.inner.security(path)
        }
        fn set_mode<'a>(
            &'a self,
            path: &'a RtPath,
            mode: u32,
        ) -> alloy_rt::BoxFuture<'a, Result<(), DiskError>> {
            self.inner.set_mode(path, mode)
        }
        fn rt_path(&self, native: &str) -> Result<RtPath, DiskError> {
            self.inner.rt_path(native)
        }
    }

    /// T-0149: `/run/alloy` is created `Private`, and nothing holds the
    /// socket path, so `admin.sock` may be bound there.
    #[test]
    fn the_admin_directory_is_created_private_on_a_disk_rooted_at_the_root() {
        on_mem_disk(|disk, data| {
            Box::pin(async move {
                assert!(prepare_admin_dir(disk.as_ref(), &data).await);
                assert_eq!(
                    disk.security(&admin_dir()).await.unwrap(),
                    Security::Unix {
                        kind: EntryKind::Dir,
                        symlink: false,
                        uid: 0,
                        mode: 0o700,
                    }
                );
                assert!(matches!(
                    disk.security(&admin_socket()).await,
                    Err(DiskError::NotFound)
                ));
                assert_eq!(admin_socket().as_str(), ADMIN_SOCKET);
            })
        });
    }

    /// T-0153: the `run` line of `/run/alloy`, with the boot id read from
    /// the boot id file, is in the host file before `/run/alloy` is created
    /// (ARFC 0017 section 4.2), and stays once it is.
    #[test]
    fn the_run_line_is_written_before_the_admin_directory_is_created() {
        on_mem_disk(|disk, data| {
            Box::pin(async move {
                let watched = Watched {
                    inner: Arc::clone(&disk),
                    data: data.clone(),
                    seen: Mutex::new(None),
                };
                assert!(prepare_admin_dir(&watched, &data).await);
                let (before, existed) = watched.seen.lock().unwrap().take().unwrap();
                assert_eq!(before, format!("{HOST_HEAD}{}\n", run_line()));
                assert!(!existed);
                assert_eq!(host_text(&data).await, before);
                assert!(disk.security(&admin_dir()).await.is_ok());
            })
        });
    }

    /// T-0153: a second start adds no second `run` line, whether
    /// `/run/alloy` is still there or was removed since, in the same boot.
    #[test]
    fn a_second_start_adds_no_second_run_line() {
        on_mem_disk(|disk, data| {
            Box::pin(async move {
                let once = format!("{HOST_HEAD}{}\n", run_line());
                assert!(prepare_admin_dir(disk.as_ref(), &data).await);
                assert!(prepare_admin_dir(disk.as_ref(), &data).await);
                assert_eq!(host_text(&data).await, once);
                disk.remove(&admin_dir()).await.unwrap();
                assert!(prepare_admin_dir(disk.as_ref(), &data).await);
                assert_eq!(host_text(&data).await, once);
                assert!(disk.security(&admin_dir()).await.is_ok());
            })
        });
    }

    /// T-0153: when `/run/alloy` cannot be created, here because `/run` is
    /// missing, its `run` line is removed again and no socket is created.
    #[test]
    fn a_failed_creation_leaves_no_run_line() {
        let (_, log) = logged(|| {
            on_mem_disk(|disk, data| {
                Box::pin(async move {
                    disk.remove(&RtPath::new("/run").unwrap()).await.unwrap();
                    assert!(!prepare_admin_dir(disk.as_ref(), &data).await);
                    assert_eq!(host_text(&data).await, HOST_HEAD);
                    assert!(matches!(
                        disk.security(&admin_dir()).await,
                        Err(DiskError::NotFound)
                    ));
                })
            });
        });
        assert!(
            log.starts_with(
                "WARN no local admin socket is created reason=\"/run/alloy could not be created"
            ),
            "{log}"
        );
    }

    /// T-0153: without a boot id, `/run/alloy` cannot be recorded, so it is
    /// not created and the host file is unchanged.
    #[test]
    fn without_a_boot_id_the_admin_directory_is_not_created() {
        let (_, log) = logged(|| {
            on_mem_disk(|disk, data| {
                Box::pin(async move {
                    let boot_id = RtPath::new(host_objects::BOOT_ID_FILE).unwrap();
                    disk.remove(&boot_id).await.unwrap();
                    assert!(!prepare_admin_dir(disk.as_ref(), &data).await);
                    assert_eq!(host_text(&data).await, HOST_HEAD);
                    assert!(matches!(
                        disk.security(&admin_dir()).await,
                        Err(DiskError::NotFound)
                    ));
                })
            });
        });
        assert!(
            log.starts_with(
                "WARN no local admin socket is created reason=\"/run/alloy cannot be recorded"
            ),
            "{log}"
        );
    }

    /// An entry an earlier start left at the socket path is removed, in a
    /// `/run/alloy` that is already right.
    #[test]
    fn an_old_entry_at_the_socket_path_is_removed() {
        on_mem_disk(|disk, data| {
            Box::pin(async move {
                disk.create_dir_protected(&admin_dir(), Protection::Private)
                    .await
                    .unwrap();
                let options = alloy_rt::OpenOptions {
                    access: alloy_rt::Access::ReadWrite,
                    create: alloy_rt::Create::IfMissing,
                    direct: false,
                    exclusive: false,
                };
                drop(disk.open(&admin_socket(), options).await.unwrap());
                assert!(prepare_admin_dir(disk.as_ref(), &data).await);
                assert!(matches!(
                    disk.security(&admin_socket()).await,
                    Err(DiskError::NotFound)
                ));
            })
        });
    }

    /// A `/run/alloy` others may read is refused, and the refusal is logged
    /// at warn.
    #[test]
    fn a_wrong_admin_directory_is_refused_with_a_warning() {
        let (_, log) = logged(|| {
            on_mem_disk(|disk, data| {
                Box::pin(async move {
                    disk.create_dir(&admin_dir()).await.unwrap();
                    disk.set_mode(&admin_dir(), 0o755).await.unwrap();
                    assert!(!prepare_admin_dir(disk.as_ref(), &data).await);
                })
            });
        });
        assert!(
            log.starts_with(
                "WARN no local admin socket is created reason=\"/run/alloy is not a directory \
                 owned by user id 0 with mode 0700"
            ),
            "{log}"
        );
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
