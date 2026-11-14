//! The `host` seam: facts about the machine, free space at a path, and
//! control of the one service Alloy owns, `alloyd` (ARFC 0005 section 2.8
//! and clause 3.1.18, ADR 0071).
//!
//! It is not part of [`Runtime`](crate::Runtime): only `alloy init` and
//! `alloy join` build an instance, with `Registry::instantiate::<HostSeam>`
//! and context `()`, so no other component can stop or disable `alloyd`.

use std::fmt;

use alloy_seam::{Detail, Seam, SwapClass};

use crate::BoxFuture;

/// Seam marker of `host`: object `dyn Host`, context `()`, swap class
/// `restart`.
#[derive(Debug)]
pub struct HostSeam;

impl Seam for HostSeam {
    type Object = dyn Host;
    type Context = ();
    const KIND: &'static str = "host";
    const SWAP: SwapClass = SwapClass::Restart;
}

/// Seam `host`.
///
/// `facts`, `free_space`, and `service_status` change nothing and need no
/// privilege. `service` calls on one instance run one at a time, in the
/// order they are first polled; a dropped `service` future leaves its action
/// done or not done (ARFC 0005 clause 3.1.18).
pub trait Host: Send + Sync + fmt::Debug + 'static {
    /// Facts about the machine (clause 3.1.18.1).
    ///
    /// # Errors
    ///
    /// [`HostError::Unsupported`] on a platform the implementation does not
    /// cover, and [`HostError::Os`] for a failing operating system call.
    fn facts(&self) -> BoxFuture<'_, Result<HostFacts, HostError>>;

    /// Bytes free to this process on the file system that holds the
    /// absolute native path `path`, or its nearest existing ancestor
    /// (clause 3.1.18.2). It follows symbolic links and creates nothing.
    ///
    /// # Errors
    ///
    /// [`HostError::InvalidInput`] for a path that is not absolute in the
    /// platform's form or is over 4,096 bytes, and [`HostError::Os`].
    fn free_space<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<u64, HostError>>;

    /// The state of the service `alloyd` (clause 3.1.18.3).
    ///
    /// # Errors
    ///
    /// [`HostError::NotInstalled`] when the unit or service does not exist,
    /// [`HostError::Unsupported`] where there is no service manager the
    /// implementation drives, and the service manager's refusals.
    fn service_status(&self) -> BoxFuture<'_, Result<ServiceStatus, HostError>>;

    /// Applies `action` to the service `alloyd`. No method names any other
    /// service (clause 3.1.18.3).
    ///
    /// # Errors
    ///
    /// [`HostError::PermissionDenied`] first, calling no service manager,
    /// when the process is not privileged; then
    /// [`HostError::NotInstalled`]; [`HostError::Disabled`] for `Start` of a
    /// service that is not enabled; [`HostError::Refused`] when the service
    /// manager refuses or a started service stops; and
    /// [`HostError::TimedOut`] after 60 seconds.
    fn service(&self, action: ServiceAction) -> BoxFuture<'_, Result<(), HostError>>;
}

/// What [`Host::facts`] reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFacts {
    /// The operating system and its version.
    pub platform: Platform,
    /// The native architecture of the operating system.
    pub arch: Arch,
    /// Root on Linux, an elevated token on Windows.
    pub privileged: bool,
    /// Online logical processors, 1 or more.
    pub cores: u32,
    /// Bytes of physical memory.
    pub memory: u64,
    /// Hardware virtualization is usable.
    pub virtualization: bool,
    /// Whole-disk block devices, sorted by `path` ascending bytewise. At
    /// most 32.
    pub disks: Vec<DiskFact>,
}

/// An operating system and its version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Platform {
    /// `release` is the kernel release, at most 64 bytes. `major` and
    /// `minor` are its leading numbers, 0 and 0 when it has none.
    Linux {
        /// The kernel release, as `uname -r` prints it.
        release: String,
        /// Its first number.
        major: u32,
        /// Its second number.
        minor: u32,
    },
    /// The version Windows reports for itself, such as 10.0.26100.
    Windows {
        /// The major version.
        major: u32,
        /// The minor version.
        minor: u32,
        /// The build number.
        build: u32,
    },
}

/// The native architecture of the operating system, not of the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arch {
    /// `x86_64`.
    X86_64,
    /// `aarch64`.
    Aarch64,
    /// Any other, named as clause 3.1.18.1 says. 1 to 32 bytes.
    Other(String),
}

impl Arch {
    /// `x86_64`, `aarch64`, or the `Other` text.
    pub fn as_str(&self) -> &str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
            Arch::Other(name) => name,
        }
    }
}

/// One whole-disk block device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskFact {
    /// The native path of the device, such as `/dev/nvme0n1`. At most 255
    /// bytes.
    pub path: String,
    /// Bytes.
    pub size: u64,
}

/// The state of the service `alloyd`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceStatus {
    /// The service starts with the machine.
    pub enabled: bool,
    /// Whether it runs.
    pub run: RunState,
}

/// Whether the service runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Not running.
    Stopped,
    /// Started, and not yet running.
    Starting,
    /// Running.
    Running,
    /// Told to stop, and not yet stopped.
    Stopping,
}

/// What [`Host::service`] does. Numbered from 1 in this order for the event
/// trace (ARFC 0005 section 4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    /// Make the service start with the machine.
    Enable,
    /// Make the service not start with the machine.
    Disable,
    /// Start the service, which must be enabled.
    Start,
    /// Stop the service.
    Stop,
}

/// Errors of the `host` seam. Variants are numbered from 1 in this order
/// for the event trace (ARFC 0005 section 4.1); `Os` is 255.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// The process is not privileged.
    #[error("permission denied")]
    PermissionDenied,
    /// The unit `alloyd.service` or the service `alloyd` does not exist.
    #[error("the service alloyd is not installed")]
    NotInstalled,
    /// `Start` of a service that is not enabled.
    #[error("the service alloyd is disabled")]
    Disabled,
    /// The service manager refused, or a started service stopped.
    #[error("the service manager refused: {0}")]
    Refused(Detail),
    /// The action did not finish within 60 seconds.
    #[error("the service manager did not finish within 60 s")]
    TimedOut,
    /// An argument is not valid.
    #[error("invalid input: {0}")]
    InvalidInput(Detail),
    /// The platform, or its service manager, is not one the implementation
    /// covers.
    #[error("not supported on this platform")]
    Unsupported,
    /// An operating system error with no variant above. `os` only.
    #[error("operating system error {code}")]
    Os {
        /// The operating system's error code.
        code: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_names_follow_clause_3_1_18_1() {
        assert_eq!(Arch::X86_64.as_str(), "x86_64");
        assert_eq!(Arch::Aarch64.as_str(), "aarch64");
        assert_eq!(Arch::Other("riscv64".into()).as_str(), "riscv64");
    }

    #[test]
    fn the_seam_marker_is_host_with_restart() {
        assert_eq!(HostSeam::KIND, "host");
        assert_eq!(HostSeam::SWAP, SwapClass::Restart);
    }

    #[test]
    fn errors_print_their_reason() {
        assert_eq!(
            HostError::NotInstalled.to_string(),
            "the service alloyd is not installed"
        );
        assert_eq!(
            HostError::Os { code: 5 }.to_string(),
            "operating system error 5"
        );
    }
}
