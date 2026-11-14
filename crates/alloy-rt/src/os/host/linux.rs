//! `host` under `os` on Linux (ARFC 0005 clauses 3.1.18.1 to 3.1.18.4):
//! `uname(2)`, `sysconf`, `/proc/meminfo`, `/dev/kvm`, `/sys/block`,
//! `statvfs`, and `systemctl` from a fixed path.

use std::ffi::{CStr, CString};
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use alloy_seam::Detail;

use super::parse::{self, MAX_ARCH, MAX_DISKS, MAX_PATH, MAX_RELEASE};
use super::{Manager, ServiceAction};
use crate::{Arch, DiskFact, HostError, HostFacts, Platform, ServiceStatus};

/// The `Os` code of a call cut short because the runtime stopped: `EIO`.
pub(super) const STOPPED: i32 = libc::EIO;

/// Where `systemctl` is looked for, in order. Never through `PATH`.
const SYSTEMCTL: [&str; 2] = ["/usr/bin/systemctl", "/bin/systemctl"];

/// The unit, the only one any action names.
const UNIT: &str = "alloyd.service";

/// How often a running `systemctl` is checked for its exit.
const CHILD_POLL: Duration = Duration::from_millis(10);

fn os_error(error: &std::io::Error) -> HostError {
    HostError::Os {
        code: error.raw_os_error().unwrap_or(libc::EIO),
    }
}

fn last_os_error() -> HostError {
    os_error(&std::io::Error::last_os_error())
}

/// The `release` and `machine` fields of `uname(2)`.
#[allow(unsafe_code, reason = "uname has no safe binding")]
fn uname() -> Result<(String, String), HostError> {
    // SAFETY: `utsname` is plain old data, for which all zeros is a valid
    // value.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is a valid, writable `utsname` for the call.
    if unsafe { libc::uname(&raw mut name) } != 0 {
        return Err(last_os_error());
    }
    let field = |chars: &[libc::c_char]| {
        // SAFETY: `uname` writes NUL-terminated strings into each field,
        // and the struct was zeroed, so a NUL lies inside the array.
        let text = unsafe { CStr::from_ptr(chars.as_ptr()) };
        text.to_string_lossy().into_owned()
    };
    Ok((field(&name.release[..]), field(&name.machine[..])))
}

#[allow(unsafe_code, reason = "geteuid has no safe binding")]
fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory of ours, and
    // always succeeds.
    unsafe { libc::geteuid() }
}

#[allow(unsafe_code, reason = "sysconf has no safe binding")]
fn online_cores() -> u32 {
    // SAFETY: sysconf reads a configuration value and touches no memory of
    // ours.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    u32::try_from(n).ok().filter(|&n| n >= 1).unwrap_or(1)
}

fn read_text(path: &str) -> std::io::Result<String> {
    let mut text = String::new();
    std::fs::File::open(path)?.read_to_string(&mut text)?;
    Ok(text)
}

fn kvm_usable() -> bool {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open("/dev/kvm")
        .is_ok()
}

fn disks() -> Vec<DiskFact> {
    let Ok(entries) = std::fs::read_dir("/sys/block") else {
        return Vec::new();
    };
    let mut disks: Vec<DiskFact> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if parse::skipped_block_name(&name) {
                return None;
            }
            let base = format!("/sys/block/{name}");
            let removable = read_text(&format!("{base}/removable")).ok()?;
            if removable.trim() != "0" {
                return None;
            }
            let size = parse::sectors_to_bytes(&read_text(&format!("{base}/size")).ok()?)?;
            let path = format!("/dev/{name}");
            (path.len() <= 255).then_some(DiskFact { path, size })
        })
        .collect();
    disks.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    disks.truncate(MAX_DISKS);
    disks
}

pub(super) fn facts() -> Result<HostFacts, HostError> {
    let (release, machine) = uname()?;
    let release = parse::cut(&release, MAX_RELEASE).to_owned();
    let (major, minor) = parse::release_numbers(&release);
    let arch = match machine.as_str() {
        "x86_64" => Arch::X86_64,
        "aarch64" => Arch::Aarch64,
        other => Arch::Other(parse::cut(other, MAX_ARCH).to_owned()),
    };
    let meminfo = read_text("/proc/meminfo").map_err(|e| os_error(&e))?;
    let memory = parse::mem_total(&meminfo).ok_or(HostError::Os { code: libc::EIO })?;
    Ok(HostFacts {
        platform: Platform::Linux {
            release,
            major,
            minor,
        },
        arch,
        privileged: effective_uid() == 0,
        cores: online_cores(),
        memory,
        virtualization: kvm_usable(),
        disks: disks(),
    })
}

#[allow(unsafe_code, reason = "statvfs has no safe binding")]
fn statvfs(path: &CStr) -> Result<u64, std::io::Error> {
    // SAFETY: `statvfs` is plain old data, for which all zeros is a valid
    // value.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is NUL-terminated and `stat` is valid and writable for
    // the call.
    if unsafe { libc::statvfs(path.as_ptr(), &raw mut stat) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[allow(
        clippy::useless_conversion,
        reason = "the field types differ between targets"
    )]
    let (avail, frsize) = (u64::from(stat.f_bavail), u64::from(stat.f_frsize));
    Ok(avail.saturating_mul(frsize))
}

pub(super) fn free_space(path: &str) -> Result<u64, HostError> {
    if !path.starts_with('/') || path.contains('\0') || path.len() > MAX_PATH {
        return Err(HostError::InvalidInput(Detail::new(
            "free_space takes an absolute path of at most 4,096 bytes with no NUL",
        )));
    }
    let mut at = path;
    loop {
        let c_path = CString::new(at).map_err(|_| HostError::Os { code: libc::EINVAL })?;
        match statvfs(&c_path) {
            Ok(free) => return Ok(free),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                match parse::parent(at, '/', 1) {
                    Some(parent) => at = parent,
                    None => return Err(os_error(&e)),
                }
            }
            Err(e) => return Err(os_error(&e)),
        }
    }
}

/// The `systemctl` to run, or `Unsupported` when systemd is not the running
/// service manager or no `systemctl` is at a fixed path.
fn systemctl() -> Result<&'static str, HostError> {
    if !Path::new("/run/systemd/system").is_dir() {
        return Err(HostError::Unsupported);
    }
    SYSTEMCTL
        .into_iter()
        .find(|path| Path::new(path).exists())
        .ok_or(HostError::Unsupported)
}

/// What a `systemctl` run gave: its standard output, or `Refused`.
fn run_systemctl(args: &[&str], deadline: Instant) -> Result<String, HostError> {
    let program = systemctl()?;
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| os_error(&e))?;
    // `systemctl` writes a few lines at most, well under a pipe's buffer, so
    // it never blocks on a full pipe while it is waited for here.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(os_error(&e));
            }
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(HostError::TimedOut);
        }
        std::thread::sleep(CHILD_POLL.min(deadline - now));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut stderr);
    }
    if !status.success() {
        let line = parse::first_line(&stderr);
        let reason = if line.is_empty() {
            format!("systemctl exited with {status}")
        } else {
            line
        };
        return Err(HostError::Refused(Detail::new(&reason)));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

pub(super) fn status(deadline: Instant) -> Result<ServiceStatus, HostError> {
    Service::status(deadline)
}

/// systemd, through `systemctl`.
pub(super) struct Service;

impl Manager for Service {
    const POLLS_START: bool = false;

    fn privileged() -> bool {
        effective_uid() == 0
    }

    fn status(deadline: Instant) -> Result<ServiceStatus, HostError> {
        let output = run_systemctl(
            &[
                "show",
                "--property=LoadState",
                "--property=UnitFileState",
                "--property=ActiveState",
                UNIT,
            ],
            deadline,
        )?;
        parse::systemctl_show(&output).ok_or(HostError::NotInstalled)
    }

    fn request(action: ServiceAction, deadline: Instant) -> Result<(), HostError> {
        let verb = match action {
            ServiceAction::Enable => "enable",
            ServiceAction::Disable => "disable",
            ServiceAction::Start => "start",
            ServiceAction::Stop => "stop",
        };
        run_systemctl(
            &["--no-ask-password", "--no-pager", "--quiet", verb, UNIT],
            deadline,
        )
        .map(|_| ())
    }

    fn stop_reason() -> Option<String> {
        None
    }
}
