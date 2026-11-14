//! `alloy init` and `alloy join`: the commands that make this machine a node
//! (ARFC 0002 sections 3.1, 3.3, and 3.8).
//!
//! Both read the machine and control the service `alloyd` through the
//! `host` seam (ARFC 0005 section 2.8), which only they build, beside their
//! `os` runtime (ADR 0071). They run the checks of section 3.8
//! ([`checks`]), prepare the data directory, call `alloyd`'s
//! [`init`](alloyd::init::init) or [`join`](alloyd::join::join), and then
//! enable and start `alloyd` ([`start_alloyd`]).
//!
//! Step lines go to standard error under a heading for each group, each
//! starting with its status word padded to six columns (principle T2). The
//! join token and the login code are the result, on standard output.

pub(crate) mod checks;
pub(crate) mod init;
pub(crate) mod join;
pub(crate) mod probe;

use std::io::Write;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use alloy_id::datadir::InitAction;
use alloy_id::{DataDir, Role, Roles};
use alloy_rt::{DiskError, DiskIo, EntryKind, Host, HostError, RtPath, RunState, ServiceAction};
use alloyd::{HOST_FILE, JoinState, NodePaths};

use crate::text::escape;
use crate::{
    CommandSpec, Ctx, EXIT_DENIED, EXIT_FAILURE, Failure, Invocation, Style, UsageError, note,
};

/// The default mesh port (ARFC 0004 section 3.6).
pub(crate) const DEFAULT_MESH_PORT: u16 = 7476;

/// The default API port (ARFC 0008 section 3.8).
pub(crate) const DEFAULT_API_PORT: u16 = 443;

/// The longest node name (ARFC 0002 section 3.3).
const MAX_NODE_NAME: usize = 63;

/// What `init` and `join` run with besides the command line.
pub(crate) struct Setup<'a> {
    /// The runtime, the environment, and the styles of the streams.
    pub(crate) ctx: &'a Ctx<'a>,
    /// The machine, through the `host` seam.
    pub(crate) host: &'a dyn Host,
    /// Where the data directory is.
    pub(crate) data: DataPath,
}

impl Setup<'_> {
    fn disk(&self) -> &Arc<dyn DiskIo> {
        self.ctx.runtime.disk_io()
    }
}

/// Where the data directory is, as the `disk-io` seam names it and as the
/// operating system does (ARFC 0002 section 3.12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DataPath {
    /// Every directory above the data directory, from the root down to its
    /// parent, each with its native path for messages.
    pub(crate) ancestors: Vec<(RtPath, PathBuf)>,
    /// The data directory, below the runtime's `disk-io` root.
    pub(crate) dir: RtPath,
    /// Its native path.
    pub(crate) native: PathBuf,
    /// The host file.
    pub(crate) host_file: RtPath,
    /// The host file's native path.
    pub(crate) host_file_native: PathBuf,
}

impl DataPath {
    /// The data directory at the absolute native path `native`, below the
    /// root of its file system, which is the runtime's `disk-io` root.
    ///
    /// # Errors
    ///
    /// A [`Failure`] with [`EXIT_FAILURE`] when the path is not absolute or
    /// has a component that is not UTF-8 or is `..`.
    pub(crate) fn of(native: &Path) -> Result<DataPath, Failure> {
        let unusable = || {
            Failure::new(
                EXIT_FAILURE,
                format!(
                    "alloy cannot use the data directory {}: it must be an absolute path \
                     of UTF-8 names without `..`.",
                    escape(&native.display().to_string())
                ),
            )
        };
        if !native.is_absolute() {
            return Err(unusable());
        }
        let mut root = PathBuf::new();
        let mut names = Vec::new();
        for component in native.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => root.push(component.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => return Err(unusable()),
                Component::Normal(name) => names.push(name.to_str().ok_or_else(unusable)?),
            }
        }
        let mut ancestors = Vec::with_capacity(names.len());
        let mut dir = RtPath::root();
        let mut path = root;
        for name in names {
            ancestors.push((dir.clone(), path.clone()));
            dir = dir.join(name).map_err(|_| unusable())?;
            path.push(name);
        }
        let host_file = dir.join(HOST_FILE).map_err(|_| unusable())?;
        Ok(DataPath {
            ancestors,
            dir,
            host_file,
            host_file_native: path.join(HOST_FILE),
            native: path,
        })
    }
}

/// A status word of a step line (principle T2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// A check passed.
    Ok,
    /// A step finished.
    Done,
    /// A check found something to know about.
    Warn,
    /// A check or step failed.
    Fail,
}

impl Status {
    fn word(self, style: Style) -> String {
        let (word, padded) = match self {
            Status::Ok => ("ok", "ok    "),
            Status::Done => ("done", "done  "),
            Status::Warn => ("warn", "warn  "),
            Status::Fail => ("fail", "fail  "),
        };
        let painted = match self {
            Status::Ok | Status::Done => style.green(word),
            Status::Warn => style.yellow(word),
            Status::Fail => style.red(word),
        };
        format!("{painted}{}", &padded[word.len()..])
    }
}

/// A step line: two spaces, the status word padded to six columns, and the
/// text, its continuation lines indented to match (principles T2 and T3).
pub(crate) fn step_line(status: Status, text: &str, style: Style) -> String {
    let wrapped = crate::text::wrap(text, 72);
    let mut lines = wrapped.lines();
    let mut out = format!("  {}{}\n", status.word(style), lines.next().unwrap_or(""));
    for line in lines {
        out.push_str("        ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Writes a step line to standard error.
pub(crate) fn step(err: &mut dyn Write, style: Style, status: Status, text: &str) {
    note(err, &step_line(status, text, style));
}

/// Writes a heading line to standard error.
pub(crate) fn heading(err: &mut dyn Write, text: &str) {
    note(err, &format!("{text}\n"));
}

/// The `host` seam's refusal of a service action, as `init` and `join`
/// report it.
fn service_failure(action: ServiceAction, error: &HostError) -> Failure {
    let verb = match action {
        ServiceAction::Enable => "enable",
        ServiceAction::Disable => "disable",
        ServiceAction::Start => "start",
        ServiceAction::Stop => "stop",
    };
    let code = match error {
        HostError::PermissionDenied => EXIT_DENIED,
        _ => EXIT_FAILURE,
    };
    Failure::new(
        code,
        format!(
            "alloy could not {verb} the service alloyd: {}.",
            escape(&error.to_string())
        ),
    )
}

/// Enables and starts `alloyd` (ARFC 0002 section 3.1 step 4): `Enable`,
/// then `Stop` when it runs, so that it starts on the data directory just
/// written, then `Start`.
///
/// # Errors
///
/// A [`Failure`] naming the action the `host` seam refused.
pub(crate) async fn start_alloyd(host: &dyn Host) -> Result<(), Failure> {
    enable_alloyd(host).await?;
    restart_alloyd(host).await
}

/// `Enable` of `alloyd`, the first action of [`start_alloyd`].
///
/// # Errors
///
/// A [`Failure`] naming the action when the `host` seam refuses it.
pub(crate) async fn enable_alloyd(host: &dyn Host) -> Result<(), Failure> {
    act(host, ServiceAction::Enable).await
}

/// The rest of [`start_alloyd`] once `alloyd` is enabled: `Stop` when it
/// runs, then `Start`.
///
/// # Errors
///
/// A [`Failure`] naming the action the `host` seam refused.
pub(crate) async fn restart_alloyd(host: &dyn Host) -> Result<(), Failure> {
    let running = host
        .service_status()
        .await
        .is_ok_and(|status| status.run != RunState::Stopped);
    if running {
        act(host, ServiceAction::Stop).await?;
    }
    act(host, ServiceAction::Start).await
}

async fn act(host: &dyn Host, action: ServiceAction) -> Result<(), Failure> {
    host.service(action)
        .await
        .map_err(|e| service_failure(action, &e))
}

/// Stops and disables `alloyd`, which leaves it as a first install does
/// (ARFC 0005 clause 3.1.18.4). Both are tried; the first refusal is
/// returned.
pub(crate) async fn stop_alloyd(host: &dyn Host) -> Result<(), Failure> {
    let stop = host.service(ServiceAction::Stop).await;
    let disable = host.service(ServiceAction::Disable).await;
    stop.map_err(|e| service_failure(ServiceAction::Stop, &e))?;
    disable.map_err(|e| service_failure(ServiceAction::Disable, &e))
}

/// The data directory, made ready as the `data directory` check decided
/// (ARFC 0002 section 3.12): used, created, or adopted.
///
/// # Errors
///
/// A [`Failure`] naming the directory when it cannot be created or adopted.
pub(crate) async fn open_data_dir(
    setup: &Setup<'_>,
    action: InitAction,
) -> Result<NodePaths, Failure> {
    let disk = Arc::clone(setup.disk());
    let dir = setup.data.dir.clone();
    let data = match action {
        InitAction::Use => Ok(DataDir::new(disk, dir.clone())),
        InitAction::Create => DataDir::create(disk, dir.clone()).await,
        InitAction::Adopt => DataDir::adopt(disk, dir.clone()).await,
    }
    .map_err(|e| {
        Failure::new(
            EXIT_FAILURE,
            format!(
                "alloy could not prepare the data directory {}: {}.",
                setup.data.native.display(),
                escape(&e.to_string())
            ),
        )
    })?;
    Ok(NodePaths {
        data,
        dir,
        native: setup.data.native.clone(),
    })
}

/// Removes everything under the data directory but its host file, whose
/// join line becomes `unjoined`, so that a rerun starts clean (ARFC 0002
/// section 3.1).
///
/// # Errors
///
/// The first [`DiskError`].
pub(crate) async fn remove_written(paths: &NodePaths, disk: &dyn DiskIo) -> Result<(), DiskError> {
    for entry in disk.list(&paths.dir).await? {
        if entry.name == HOST_FILE {
            continue;
        }
        let path = paths.dir.join(&entry.name)?;
        if entry.kind == EntryKind::Dir {
            remove_tree(disk, &path).await?;
        } else {
            disk.remove(&path).await?;
        }
    }
    disk.sync_dir(&paths.dir).await?;
    let host_file = format!("alloy-host 1\njoin {}\nrelease none\n", JoinState::Unjoined);
    paths
        .data
        .write(HOST_FILE, host_file.as_bytes())
        .await
        .map_err(|_| DiskError::Io)
}

/// Removes the directory `dir` and everything below it, each directory once
/// it is empty.
async fn remove_tree(disk: &dyn DiskIo, dir: &RtPath) -> Result<(), DiskError> {
    let mut stack = vec![(dir.clone(), false)];
    while let Some((path, listed)) = stack.pop() {
        if listed {
            disk.remove(&path).await?;
            continue;
        }
        stack.push((path.clone(), true));
        for entry in disk.list(&path).await? {
            let child = path.join(&entry.name)?;
            if entry.kind == EntryKind::Dir {
                stack.push((child, false));
            } else {
                disk.remove(&child).await?;
            }
        }
    }
    Ok(())
}

/// The default node name: the hostname in lower case, cut at the first dot,
/// every character outside `[a-z0-9-]` replaced by `-`, prefixed `node-`
/// when it does not start with a letter, and cut to 63 characters (ARFC
/// 0002 section 3.3).
pub(crate) fn default_node_name(hostname: &str) -> String {
    let lower = hostname.trim().to_lowercase();
    let head = lower.split('.').next().unwrap_or_default();
    let mut name: String = head
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name.insert_str(0, "node-");
    }
    name.chars().take(MAX_NODE_NAME).collect()
}

/// A port flag's value, or `default` when it is not given.
///
/// # Errors
///
/// A [`UsageError`] naming the flag when the value is not 1 to 65535.
pub(crate) fn port(
    spec: &CommandSpec,
    invocation: &Invocation,
    flag: &str,
    default: u16,
) -> Result<u16, UsageError> {
    match invocation.value(flag) {
        None => Ok(default),
        Some(text) => text
            .parse::<u16>()
            .ok()
            .filter(|&port| port != 0)
            .ok_or_else(|| {
                UsageError::of(
                    spec,
                    format!(
                        "`{flag}` takes a port from 1 to 65535, and `{}` was given.",
                        escape(text)
                    ),
                )
            }),
    }
}

/// `--advertise <addr>[,<addr>...]`: 1 to 16 IP addresses, or `None` when
/// the flag is not given.
///
/// # Errors
///
/// A [`UsageError`] naming the first value that is not an IP address, or
/// more than 16.
pub(crate) fn advertise(
    spec: &CommandSpec,
    invocation: &Invocation,
) -> Result<Option<Vec<IpAddr>>, UsageError> {
    let Some(text) = invocation.value("--advertise") else {
        return Ok(None);
    };
    let mut addresses = Vec::new();
    for word in text.split(',') {
        let ip: IpAddr = word.trim().parse().map_err(|_| {
            UsageError::of(
                spec,
                format!(
                    "`--advertise` takes IP addresses separated by commas, and `{}` is \
                     not one.",
                    escape(word)
                ),
            )
        })?;
        if !addresses.contains(&ip) {
            addresses.push(ip);
        }
    }
    if addresses.len() > alloyd::init::MAX_ADVERTISE {
        return Err(UsageError::of(
            spec,
            format!(
                "`--advertise` takes at most {} addresses, and {} were given.",
                alloyd::init::MAX_ADVERTISE,
                addresses.len()
            ),
        ));
    }
    Ok(Some(addresses))
}

/// `--name`: a node name as given, checked against the rule of ARFC 0002
/// section 3.3, or `None` when it is not given.
///
/// # Errors
///
/// A [`UsageError`] when the name breaks the rule.
pub(crate) fn node_name(
    spec: &CommandSpec,
    invocation: &Invocation,
) -> Result<Option<String>, UsageError> {
    let Some(name) = invocation.value("--name") else {
        return Ok(None);
    };
    alloy_api::NodeName::new(name).map_err(|rule| {
        UsageError::of(spec, format!("The node name `{}` {rule}.", escape(name)))
    })?;
    Ok(Some(name.to_owned()))
}

/// The roles in the order and spelling of ARFC 0019 clause 3.1.15.
pub(crate) fn roles_text(roles: Roles) -> String {
    if roles.is_empty() {
        return "none".to_owned();
    }
    roles
        .iter()
        .map(|role| match role {
            Role::Anchor => "anchor",
            Role::Control => "control",
            Role::Compute => "compute",
            Role::Storage => "storage",
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether `ip` is private by the list of ARFC 0002 section 3.1 step 2.
pub(crate) fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 10
                || (a == 172 && (16..32).contains(&b))
                || (a == 192 && b == 168)
                || (a == 100 && (64..128).contains(&b))
                || (a == 169 && b == 254)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests;
