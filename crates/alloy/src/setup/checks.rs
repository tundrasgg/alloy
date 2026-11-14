//! The machine checks of ARFC 0002 section 3.8.
//!
//! The `host` check runs first, and when it fails nothing else runs. Then
//! privilege (exit 4) and the local join state (exit 5 on `Conflict`), and
//! then every check of the table in order, each printing its line even
//! after an earlier `fail`; the exit code is that of the first `fail`.
//! Nothing here writes to the machine.

use std::io::Write;
use std::path::Path;

use alloy_id::datadir::{
    self, Ace, DataDirFacts, InitAction, Principal, UnixEntry, UnixKind, WindowsEntry,
};
use alloy_id::{ClusterId, Role, Roles};
use alloy_rt::{
    Access, Arch, Create, DiskError, EntryKind, HostError, HostFacts, Interface, IoBuf,
    OpenOptions, Platform, ReadOp, Security, Sid,
};
use alloyd::{JoinState, parse_host_file};

use super::probe::{self, Route};
use super::{Setup, Status, heading, step};
use crate::text::escape;
use crate::{EXIT_CONFLICT, EXIT_DENIED, EXIT_FAILURE, Failure, VERSION};

/// Free space at or above which the check is `ok` (ARFC 0002 section 3.8).
const GOOD_FREE: u64 = 10 * GIB;
/// Free space below which the check fails.
const LEAST_FREE: u64 = GIB;
const GIB: u64 = 1_073_741_824;

/// The oldest Linux kernel and Windows build a node runs on.
const LINUX_MIN: (u32, u32) = (6, 1);
const WINDOWS_MIN_BUILD: u32 = 17_763;

/// Which command checks, and what it knows before any machine is
/// contacted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `alloy init`: the node takes every role.
    Init,
    /// `alloy join`, with the roles the node will take and the token's
    /// cluster.
    Join {
        /// The roles of `--roles`, else the token's preset, else the
        /// default (ARFC 0002 section 3.8).
        roles: Roles,
        /// The cluster, for the `addresses` check.
        cluster_id: ClusterId,
    },
}

impl Kind {
    fn client(self) -> bool {
        matches!(self, Kind::Join { roles, .. } if roles.is_empty())
    }

    fn serves_api(self) -> bool {
        match self {
            Kind::Init => true,
            Kind::Join { roles, .. } => {
                roles.contains(Role::Anchor) || roles.contains(Role::Control)
            }
        }
    }

    fn command(self) -> &'static str {
        match self {
            Kind::Init => "init",
            Kind::Join { .. } => "join",
        }
    }
}

/// What the checks found, for the steps after them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Passed {
    /// The machine's facts.
    pub(crate) facts: HostFacts,
    /// What to do with the data directory.
    pub(crate) action: InitAction,
    /// The local join state. For `join`, `Joining` skipped the table.
    pub(crate) state: JoinState,
    /// The `cluster_id` whose prefix passed the `addresses` check: for
    /// `init` the one it drew, for `join` the token's. `None` when the
    /// table was skipped.
    pub(crate) cluster_id: Option<ClusterId>,
}

/// One line of a check: its status and text, and for a `fail` its exit
/// code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) status: Status,
    pub(crate) text: String,
    pub(crate) code: u8,
}

impl Line {
    fn ok(text: String) -> Line {
        Line {
            status: Status::Ok,
            text,
            code: 0,
        }
    }

    fn warn(text: String) -> Line {
        Line {
            status: Status::Warn,
            text,
            code: 0,
        }
    }

    fn fail(code: u8, text: String) -> Line {
        Line {
            status: Status::Fail,
            text,
            code,
        }
    }
}

/// The install command of ARFC 0017 clause 3.15 for this release.
pub(crate) fn install_command(windows: bool) -> String {
    let base = format!("https://github.com/tundrasgg/alloy/releases/download/v{VERSION}");
    if windows {
        format!("irm {base}/install.ps1 | iex")
    } else {
        format!("curl -fsSL {base}/install.sh | sh")
    }
}

/// Runs the checks for `kind`, printing a line for each under the heading
/// `Checking this machine`.
///
/// # Errors
///
/// A [`Failure`] with the exit code of the first `fail`, after every line
/// that runs was printed.
pub(crate) async fn run(
    setup: &Setup<'_>,
    kind: Kind,
    mesh_port: u16,
    api_port: u16,
    err: &mut dyn Write,
) -> Result<Passed, Failure> {
    let style = setup.ctx.styles.err;
    let windows = setup.ctx.env.windows;
    heading(err, "Checking this machine");
    let facts = match host_check(setup, kind).await {
        Ok((facts, line)) => {
            step(err, style, line.status, &line.text);
            facts
        }
        Err((line, failure)) => {
            step(err, style, line.status, &line.text);
            return Err(failure);
        }
    };
    if !facts.privileged {
        let (text, run_as) = if windows {
            (
                "an elevated administrator process",
                format!("alloy {}", kind.command()),
            )
        } else {
            ("root", format!("sudo alloy {}", kind.command()))
        };
        step(err, style, Status::Fail, &format!("not run as {text}"));
        return Err(Failure::new(
            EXIT_DENIED,
            format!(
                "alloy {} changes this machine, so it must run as {text}.",
                kind.command()
            ),
        )
        .with_try(run_as));
    }
    let data = data_dir_check(setup).await;
    let state = match &data {
        Ok(InitAction::Use) => read_state(setup).await,
        _ => Ok(JoinState::Unjoined),
    }
    .map_err(|reason| {
        Failure::new(
            EXIT_CONFLICT,
            format!(
                "The host file {} cannot be read: {reason}.",
                setup.data.host_file_native.display()
            ),
        )
    })?;
    let conflict = match kind {
        Kind::Init => state != JoinState::Unjoined,
        Kind::Join { .. } => state == JoinState::Joined,
    };
    if conflict {
        let text = format!("this machine is already a node: its local join state is {state}");
        step(err, style, Status::Fail, &text);
        return Err(Failure::new(
            EXIT_CONFLICT,
            format!(
                "This machine is already a node (its local join state is {state}), so \
                 alloy {} changed nothing.",
                kind.command()
            ),
        ));
    }
    if state == JoinState::Joining {
        // The same token resumes at step 7 and skips the rest (ARFC 0002
        // section 3.3 step 1); `join` refuses another token.
        step(
            err,
            style,
            Status::Ok,
            "a join of this machine is in progress, so it resumes",
        );
        return Ok(Passed {
            facts,
            action: InitAction::Use,
            state,
            cluster_id: None,
        });
    }
    let mut lines = Vec::new();
    if !kind.client() {
        lines.push(virtualization_line(&facts));
    }
    lines.push(ports_line(setup, kind, mesh_port, api_port));
    lines.push(match &data {
        Ok(action) => Line::ok(data_dir_text(setup, *action)),
        Err(line) => line.clone(),
    });
    lines.push(names_line(setup).await);
    let (addresses, cluster_id) = addresses_check(setup, kind).await;
    lines.push(addresses);
    lines.push(free_space_line(setup, kind).await);
    for line in &lines {
        step(err, style, line.status, &line.text);
    }
    if let Some(failed) = lines.iter().find(|line| line.status == Status::Fail) {
        return Err(Failure::new(
            failed.code,
            format!(
                "A check of this machine failed, so alloy {} changed nothing: {}.",
                kind.command(),
                failed.text
            ),
        ));
    }
    let action = data.unwrap_or(InitAction::Use);
    Ok(Passed {
        facts,
        action,
        state,
        cluster_id,
    })
}

/// The `host` check: the platform and architecture, then the service.
async fn host_check(setup: &Setup<'_>, kind: Kind) -> Result<(HostFacts, Line), (Line, Failure)> {
    let windows = setup.ctx.env.windows;
    let unsupported = |text: String| {
        let failure = Failure::new(
            EXIT_FAILURE,
            format!(
                "alloy {} cannot run on this machine: {text}.",
                kind.command()
            ),
        );
        (Line::fail(EXIT_FAILURE, text), failure)
    };
    let facts = match setup.host.facts().await {
        Ok(facts) => facts,
        Err(HostError::Unsupported) => {
            return Err(unsupported(
                "Alloy nodes run on Linux, and client nodes also on Windows".to_owned(),
            ));
        }
        Err(e) => {
            return Err(unsupported(format!(
                "its facts cannot be read: {}",
                escape(&e.to_string())
            )));
        }
    };
    let arch_ok = matches!(facts.arch, Arch::X86_64 | Arch::Aarch64);
    let arch = escape(facts.arch.as_str());
    let (platform_ok, text) = match &facts.platform {
        Platform::Linux {
            release,
            major,
            minor,
        } => (
            (*major, *minor) >= LINUX_MIN,
            format!("Linux {} on {arch}", escape(release)),
        ),
        Platform::Windows {
            major,
            minor,
            build,
        } => (
            kind.client() && *build >= WINDOWS_MIN_BUILD,
            format!("Windows {major}.{minor}.{build} on {arch}"),
        ),
    };
    if !platform_ok || !arch_ok {
        let need = if kind.client() {
            "Linux 6.1 or later, or Windows build 17763 or later, on x86_64 or aarch64"
        } else {
            "Linux 6.1 or later on x86_64 or aarch64"
        };
        return Err(unsupported(format!("{text}: a node needs {need}")));
    }
    match setup.host.service_status().await {
        Err(HostError::NotInstalled | HostError::Unsupported) => {
            let service = if windows {
                "the service alloyd"
            } else {
                "the unit alloyd.service"
            };
            let line = Line::fail(
                EXIT_FAILURE,
                format!("{text}, but {service} is not installed"),
            );
            let failure = Failure::new(
                EXIT_FAILURE,
                format!(
                    "{service} is not installed, so alloy {} has nothing to start. \
                     Install Alloy first.",
                    kind.command()
                ),
            )
            .with_try(install_command(windows));
            Err((line, failure))
        }
        // The privilege check, which comes next, reports this one.
        Ok(_) | Err(HostError::PermissionDenied) => Ok((facts, Line::ok(text))),
        Err(e) => {
            let reason = escape(&e.to_string());
            let line = Line::fail(
                EXIT_FAILURE,
                format!("{text}, but whether alloyd is installed cannot be read: {reason}"),
            );
            let failure = Failure::new(
                EXIT_FAILURE,
                format!(
                    "alloy {} could not read the service alloyd: {reason}.",
                    kind.command()
                ),
            );
            Err((line, failure))
        }
    }
}

fn virtualization_line(facts: &HostFacts) -> Line {
    let source = match facts.platform {
        Platform::Linux { .. } => " (/dev/kvm)",
        Platform::Windows { .. } => "",
    };
    if facts.virtualization {
        Line::ok(format!("hardware virtualization{source}"))
    } else {
        Line::warn(format!(
            "no hardware virtualization{source}, so this node takes no compute role"
        ))
    }
}

fn ports_line(setup: &Setup<'_>, kind: Kind, mesh: u16, api: u16) -> Line {
    let api = kind.serves_api().then_some(api);
    let busy = probe::busy_ports(setup, mesh, api);
    if busy.is_empty() {
        match api {
            Some(api) => Line::ok(format!("UDP {mesh} and TCP {api} are free")),
            None => Line::ok(format!("UDP {mesh} is free")),
        }
    } else {
        Line::fail(EXIT_CONFLICT, busy.join(", "))
    }
}

fn data_dir_text(setup: &Setup<'_>, action: InitAction) -> String {
    let path = setup.data.native.display();
    match action {
        InitAction::Use => format!("data directory {path}"),
        InitAction::Create => format!("data directory {path}, to be created"),
        InitAction::Adopt => format!("data directory {path}, empty, to be adopted"),
    }
}

/// The `names` check: no reserved object exists that is not Alloy's (ARFC
/// 0017 clause 3.17).
async fn names_line(setup: &Setup<'_>) -> Line {
    let mut held = Vec::new();
    for iface in probe::interfaces(setup).await {
        if probe::reserved_interface(&iface.name) {
            held.push(format!("the interface {}", escape(&iface.name)));
        }
    }
    if !setup.ctx.env.windows {
        held.extend(linux_names(setup).await);
    }
    if held.is_empty() {
        Line::ok("no name Alloy reserves is taken".to_owned())
    } else {
        Line::fail(
            EXIT_CONFLICT,
            format!("reserved names are taken: {}", held.join(", ")),
        )
    }
}

async fn linux_names(setup: &Setup<'_>) -> Vec<String> {
    let disk = setup.disk().as_ref();
    let mut held = Vec::new();
    for (name, _) in probe::list_names(disk, "/run/netns").await {
        if name.starts_with("alloy-") {
            held.push(format!("the network namespace {}", escape(&name)));
        }
    }
    for dir in [
        "/etc/systemd/system",
        "/run/systemd/system",
        "/usr/lib/systemd/system",
        "/lib/systemd/system",
    ] {
        for (name, _) in probe::list_names(disk, dir).await {
            if name.starts_with("alloyd-") {
                held.push(format!("the unit {dir}/{}", escape(&name)));
            }
        }
    }
    for (file, fields) in [("/etc/passwd", &[2_usize, 3][..]), ("/etc/group", &[2][..])] {
        if let Some(text) = probe::read_text(disk, file).await {
            for (name, id) in probe::database_ids(&text, fields) {
                held.push(format!("the id {id} of {} in {file}", escape(&name)));
            }
        }
    }
    for file in ["/etc/subuid", "/etc/subgid"] {
        if let Some(text) = probe::read_text(disk, file).await {
            for line in probe::subordinate_lines(&text) {
                held.push(format!("the line {} of {file}", escape(&line)));
            }
        }
    }
    for (name, kind) in probe::list_names(disk, "/proc").await {
        if kind != EntryKind::Dir || !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let status = probe::read_text(disk, &format!("/proc/{name}/status")).await;
        if status.is_some_and(|text| probe::status_holds_guest_id(&text)) {
            held.push(format!("a guest id, held by the process {name}"));
        }
    }
    held
}

/// The `addresses` check: the cluster's infrastructure prefix overlaps no
/// route and no address (ARFC 0017 clause 3.17.3). `join` checks the
/// token's `cluster_id`. `init` draws its `cluster_id` here, before any
/// later step, and draws again while the prefix overlaps, at most
/// [`MAX_DRAWS`] draws, failing only when every draw overlaps (ARFC 0002
/// section 3.8). The id that passed is returned with the line.
async fn addresses_check(setup: &Setup<'_>, kind: Kind) -> (Line, Option<ClusterId>) {
    let routes: Vec<Route> = probe::routes(setup).await;
    let interfaces = probe::interfaces(setup).await;
    match kind {
        Kind::Join { cluster_id, .. } => {
            let found = overlaps_of(&cluster_id, &routes, &interfaces);
            let line = addresses_line(&cluster_id, &found, 1);
            let passed = (line.status != Status::Fail).then_some(cluster_id);
            (line, passed)
        }
        Kind::Init => draw_cluster_id(setup.ctx.runtime.rng().as_ref(), &routes, &interfaces),
    }
}

/// `init`'s draws: a `cluster_id` whose prefix overlaps nothing in
/// `routes` and `interfaces`, drawn from `rng` at most [`MAX_DRAWS`]
/// times, with its `addresses` line; the `fail` line when every draw
/// overlaps.
pub(crate) fn draw_cluster_id(
    rng: &dyn alloy_rt::Rng,
    routes: &[Route],
    interfaces: &[Interface],
) -> (Line, Option<ClusterId>) {
    let mut line = Line::fail(EXIT_FAILURE, "no cluster id was drawn".to_owned());
    for draw in 1..=MAX_DRAWS {
        let id = match ClusterId::generate(rng) {
            Ok(id) => id,
            Err(e) => {
                let text = format!("a cluster id cannot be drawn: {}", escape(&e.to_string()));
                return (Line::fail(EXIT_FAILURE, text), None);
            }
        };
        let found = overlaps_of(&id, routes, interfaces);
        line = addresses_line(&id, &found, draw);
        if found.is_empty() {
            return (line, Some(id));
        }
    }
    (line, None)
}

/// The most `cluster_id` draws of `init`'s `addresses` check (ARFC 0002
/// section 3.8).
pub(crate) const MAX_DRAWS: u32 = 16;

/// The `addresses` line for the prefix of `cluster_id`, which overlaps
/// `found`, after `draws` draws.
fn addresses_line(cluster_id: &ClusterId, found: &[String], draws: u32) -> Line {
    let prefix = cluster_id.infra_prefix();
    if found.is_empty() {
        let after = if draws > 1 {
            format!(", the cluster id drawn {draws} times")
        } else {
            String::new()
        };
        Line::ok(format!("{prefix}/48 overlaps no route or address{after}"))
    } else if draws > 1 {
        Line::fail(
            EXIT_CONFLICT,
            format!(
                "the prefix of each of {draws} cluster ids drawn overlaps a route or an \
                 address; the last, {prefix}/48, overlaps {}",
                found.join(", ")
            ),
        )
    } else {
        Line::fail(
            EXIT_CONFLICT,
            format!("{prefix}/48 overlaps {}", found.join(", ")),
        )
    }
}

/// The routes of prefix length above 0, and the addresses of interfaces
/// that are not reserved, that the infrastructure prefix of `cluster_id`
/// overlaps, each named.
fn overlaps_of(cluster_id: &ClusterId, routes: &[Route], interfaces: &[Interface]) -> Vec<String> {
    let prefix = std::net::IpAddr::V6(cluster_id.infra_prefix());
    let mut overlaps = Vec::new();
    for route in routes.iter().filter(|r| r.len > 0) {
        if probe::overlaps(prefix, 48, route.dest, route.len) {
            overlaps.push(format!(
                "the route {}/{} on {}",
                route.dest,
                route.len,
                escape(&route.iface)
            ));
        }
    }
    for iface in interfaces {
        if probe::reserved_interface(&iface.name) {
            continue;
        }
        for addr in iface.addrs.iter().filter(|a| a.prefix_len > 0) {
            if probe::overlaps(prefix, 48, addr.ip, addr.prefix_len) {
                overlaps.push(format!(
                    "the address {}/{} on {}",
                    addr.ip,
                    addr.prefix_len,
                    escape(&iface.name)
                ));
            }
        }
    }
    overlaps
}

async fn free_space_line(setup: &Setup<'_>, kind: Kind) -> Line {
    let path = setup.data.native.display().to_string();
    match setup.host.free_space(&path).await {
        Ok(free) => {
            let text = format!("{} free in {path}", size(free));
            if free < LEAST_FREE {
                Line::fail(EXIT_FAILURE, format!("{text}, under 1 GiB"))
            } else if free < GOOD_FREE && !kind.client() {
                Line::warn(format!("{text}, under 10 GiB"))
            } else {
                Line::ok(text)
            }
        }
        Err(e) => Line::fail(
            EXIT_FAILURE,
            format!(
                "free space in {path} cannot be read: {}",
                escape(&e.to_string())
            ),
        ),
    }
}

/// A size in binary units, one decimal below 10 (principle T7).
pub(crate) fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["bytes", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1_024.0 && unit + 1 < UNITS.len() {
        value /= 1_024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else if value < 10.0 {
        format!("{:.1} {}", (value * 10.0).floor() / 10.0, UNITS[unit])
    } else {
        format!("{} {}", value.floor(), UNITS[unit])
    }
}

/// The `data directory` check: what `init` and `join` may do with the path
/// (ARFC 0002 section 3.12), or its `fail` line.
async fn data_dir_check(setup: &Setup<'_>) -> Result<InitAction, Line> {
    let facts = gather(setup)
        .await
        .map_err(|text| Line::fail(EXIT_CONFLICT, text))?;
    datadir::check_for_init(&facts)
        .map_err(|problem| Line::fail(EXIT_CONFLICT, problem.to_string()))
}

/// The local join state in the host file of an Alloy data directory.
pub(crate) async fn read_state(setup: &Setup<'_>) -> Result<JoinState, String> {
    let head = host_file_head(setup)
        .await
        .map_err(|e| escape(&e.to_string()))?;
    parse_host_file(&head).map_err(|e| escape(&e.to_string()))
}

async fn host_file_head(setup: &Setup<'_>) -> Result<Vec<u8>, DiskError> {
    let options = OpenOptions {
        access: Access::Read,
        create: Create::Never,
        direct: false,
        exclusive: false,
    };
    let file = setup.disk().open(&setup.data.host_file, options).await?;
    let buf = IoBuf::zeroed(4_096)?;
    let done = file
        .read_at(vec![ReadOp { offset: 0, buf }])
        .await
        .pop()
        .ok_or(DiskError::Io)?;
    let count = done.result?;
    Ok(done.buf.get(..count).unwrap_or_default().to_vec())
}

/// The facts of [`datadir::check_for_init`], from one `security` call per
/// entry: the data directory, each directory above it, and its host file.
/// An entry whose call fails fails the check, naming its owner `unknown`.
async fn gather(setup: &Setup<'_>) -> Result<DataDirFacts, String> {
    let disk = setup.disk();
    let unreadable = |path: &Path, e: &DiskError| {
        format!(
            "{}, owned by unknown, cannot be checked: reading its security facts failed: {e}",
            path.display()
        )
    };
    let mut ancestors = Vec::with_capacity(setup.data.ancestors.len());
    for (path, native) in &setup.data.ancestors {
        let security = disk
            .security(path)
            .await
            .map_err(|e| unreadable(native, &e))?;
        ancestors.push((security, native.as_path()));
    }
    let dir = match disk.security(&setup.data.dir).await {
        Ok(security) => Some(security),
        Err(DiskError::NotFound) => None,
        Err(e) => return Err(unreadable(&setup.data.native, &e)),
    };
    let host = match &dir {
        None => None,
        Some(_) => match disk.security(&setup.data.host_file).await {
            Ok(security) => Some(security),
            Err(DiskError::NotFound) => None,
            Err(e) => return Err(unreadable(&setup.data.host_file_native, &e)),
        },
    };
    let empty = match &dir {
        Some(_) => disk
            .list(&setup.data.dir)
            .await
            .map(|entries| entries.is_empty())
            .unwrap_or(false),
        None => false,
    };
    let first_line_ok = match &host {
        Some(_) => host_file_head(setup)
            .await
            .is_ok_and(|head| head.starts_with(b"alloy-host 1\n")),
        None => false,
    };
    let mixed = || {
        format!(
            "{} cannot be checked: its file system reports the security facts of two platforms",
            setup.data.native.display()
        )
    };
    let windows = match (&dir, ancestors.first()) {
        (Some(Security::Windows { .. }), _) | (None, Some((Security::Windows { .. }, _))) => true,
        (Some(Security::Unix { .. }), _) | (None, Some((Security::Unix { .. }, _))) => false,
        (None, None) => setup.ctx.env.windows,
    };
    let host = host.map(|h| (h, setup.data.host_file_native.as_path()));
    let dir = dir.map(|d| (d, setup.data.native.as_path()));
    if windows {
        let entry = |(s, p): (Security, &Path)| windows_entry(s, p);
        Ok(DataDirFacts::Windows {
            ancestors: ancestors
                .into_iter()
                .map(entry)
                .collect::<Option<_>>()
                .ok_or_else(mixed)?,
            data_dir: dir.map(entry).map(|e| e.ok_or_else(mixed)).transpose()?,
            host_file: host.map(entry).map(|e| e.ok_or_else(mixed)).transpose()?,
            host_file_first_line_ok: first_line_ok,
        })
    } else {
        let entry = |(s, p): (Security, &Path)| unix_entry(s, p);
        Ok(DataDirFacts::Linux {
            ancestors: ancestors
                .into_iter()
                .map(entry)
                .collect::<Option<_>>()
                .ok_or_else(mixed)?,
            data_dir: dir.map(entry).map(|e| e.ok_or_else(mixed)).transpose()?,
            data_dir_empty: empty,
            host_file: host.map(entry).map(|e| e.ok_or_else(mixed)).transpose()?,
            host_file_first_line_ok: first_line_ok,
        })
    }
}

fn unix_entry(security: Security, path: &Path) -> Option<UnixEntry> {
    let Security::Unix {
        kind,
        symlink,
        uid,
        mode,
    } = security
    else {
        return None;
    };
    let kind = match kind {
        _ if symlink => UnixKind::Symlink,
        EntryKind::Dir => UnixKind::Dir,
        EntryKind::File => UnixKind::File,
        EntryKind::Block | EntryKind::Other => UnixKind::Other,
    };
    Some(UnixEntry {
        path: path.display().to_string(),
        kind,
        uid,
        mode,
    })
}

fn windows_entry(security: Security, path: &Path) -> Option<WindowsEntry> {
    let Security::Windows {
        kind,
        reparse_point,
        owner,
        dacl_protected,
        dacl,
    } = security
    else {
        return None;
    };
    let principal = |sid: Sid| Principal {
        sid: sid.sid,
        name: sid.name,
    };
    Some(WindowsEntry {
        path: path.display().to_string(),
        is_dir: kind == EntryKind::Dir,
        reparse_point,
        owner: principal(owner),
        dacl_protected,
        dacl: dacl.map(|aces| {
            aces.into_iter()
                .map(|ace| Ace {
                    allow: ace.allow,
                    principal: principal(ace.trustee),
                    mask: ace.mask,
                    inherit_only: ace.inherit_only,
                    inherited: ace.inherited,
                })
                .collect()
        }),
    })
}
