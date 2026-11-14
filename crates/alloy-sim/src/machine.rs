//! The `mem` instance of `host` (ARFC 0005 clauses 3.1.18.1 to 3.1.18.3):
//! one per simulated host, shared by every start, reporting the host's
//! [`MachineConfig`] and holding the state of its service `alloyd`.

use std::sync::{Mutex, Weak};

use alloy_rt::{
    Arch, BoxFuture, DiskFact, Host, HostError, HostFacts, Platform, RtPath, RunState,
    ServiceAction, ServiceStatus,
};
use alloy_seam::Detail;

use crate::core::Shared;
use crate::exec::lock;
use crate::host::DeviceConfig;
use crate::trace::KIND_SERVICE;
use crate::world::HostId;

/// The most disks `HostFacts` holds (section 7).
const MAX_DISKS: usize = 32;
/// The most bytes of a Linux release in `Platform` (section 7).
const MAX_RELEASE: usize = 64;
/// The most bytes of an `Arch::Other` text (section 7).
const MAX_ARCH: usize = 32;

/// The machine a `mem` `host` instance describes (clause 3.1.18). The disks
/// it reports are the host's devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineConfig {
    /// `None` makes `facts` return `Unsupported`. Default
    /// `Some(Platform::Linux { release: "6.1.0".into(), major: 6, minor: 1 })`.
    pub platform: Option<Platform>,
    /// Default `Arch::X86_64`.
    pub arch: Arch,
    /// Default true.
    pub privileged: bool,
    /// 1 or more. Default 4.
    pub cores: u32,
    /// Bytes. Default 8,589,934,592.
    pub memory: u64,
    /// Default true.
    pub virtualization: bool,
    /// Whether the service `alloyd` is installed. Default true.
    pub service: bool,
}

impl Default for MachineConfig {
    fn default() -> MachineConfig {
        MachineConfig {
            platform: Some(Platform::Linux {
                release: "6.1.0".into(),
                major: 6,
                minor: 1,
            }),
            arch: Arch::X86_64,
            privileged: true,
            cores: 4,
            memory: 8_589_934_592,
            virtualization: true,
            service: true,
        }
    }
}

impl MachineConfig {
    /// Why `add_host` refuses this configuration: `cores` 0, a Linux
    /// `release` over 64 bytes, or an `Arch::Other` text outside 1 to 32
    /// bytes.
    pub(crate) fn check(&self) -> Result<(), &'static str> {
        if self.cores == 0 {
            return Err("machine cores must be 1 or more");
        }
        if let Some(Platform::Linux { release, .. }) = &self.platform
            && release.len() > MAX_RELEASE
        {
            return Err("a machine's Linux release is at most 64 bytes");
        }
        if let Arch::Other(name) = &self.arch
            && !(1..=MAX_ARCH).contains(&name.len())
        {
            return Err("a machine's Arch::Other text is 1 to 32 bytes");
        }
        Ok(())
    }
}

/// The `mem` instance of `host` of one simulated host.
#[derive(Debug)]
pub(crate) struct MemMachine {
    /// Weak, since the world holds this instance through its host.
    shared: Weak<Shared>,
    host: HostId,
    config: MachineConfig,
    disks: Vec<DiskFact>,
    /// The service `alloyd`, created as a first install leaves it.
    service: Mutex<ServiceStatus>,
}

impl MemMachine {
    pub(crate) fn new(
        shared: Weak<Shared>,
        host: HostId,
        config: MachineConfig,
        devices: &[DeviceConfig],
    ) -> MemMachine {
        let mut disks: Vec<DiskFact> = devices
            .iter()
            .map(|device| DiskFact {
                path: device.path.as_str().to_owned(),
                size: device.len,
            })
            .collect();
        disks.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        disks.truncate(MAX_DISKS);
        MemMachine {
            shared,
            host,
            config,
            disks,
            service: Mutex::new(ServiceStatus {
                enabled: false,
                run: RunState::Stopped,
            }),
        }
    }

    /// A crash of the host: the service stops, and its enablement is kept.
    pub(crate) fn crashed(&self) {
        lock(&self.service).run = RunState::Stopped;
    }

    /// A start of the host, before its `boot` task runs: an enabled service
    /// runs, as a service manager starts it at boot.
    pub(crate) fn starting(&self) {
        let mut service = lock(&self.service);
        if self.config.service && service.enabled {
            service.run = RunState::Running;
        }
    }

    fn act(&self, action: ServiceAction) -> Result<(), HostError> {
        if !self.config.privileged {
            return Err(HostError::PermissionDenied);
        }
        if !self.config.service {
            return Err(HostError::NotInstalled);
        }
        let mut service = lock(&self.service);
        match action {
            ServiceAction::Enable => service.enabled = true,
            ServiceAction::Disable => service.enabled = false,
            ServiceAction::Start if !service.enabled => return Err(HostError::Disabled),
            ServiceAction::Start => service.run = RunState::Running,
            ServiceAction::Stop => service.run = RunState::Stopped,
        }
        Ok(())
    }

    /// The `service` record of section 4.1.
    fn record(&self, action: ServiceAction, result: &Result<(), HostError>) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let action = match action {
            ServiceAction::Enable => 1,
            ServiceAction::Disable => 2,
            ServiceAction::Start => 3,
            ServiceAction::Stop => 4,
        };
        let result = match result {
            Ok(()) => 0,
            Err(HostError::PermissionDenied) => 1,
            Err(HostError::NotInstalled) => 2,
            Err(HostError::Disabled) => 3,
            Err(HostError::Refused(_)) => 4,
            Err(HostError::TimedOut) => 5,
            Err(HostError::InvalidInput(_)) => 6,
            Err(HostError::Unsupported) => 7,
            Err(HostError::Os { .. }) => 255,
        };
        shared.push(Some(self.host), KIND_SERVICE, &[action, result]);
    }
}

impl Host for MemMachine {
    fn facts(&self) -> BoxFuture<'_, Result<HostFacts, HostError>> {
        Box::pin(async move {
            let platform = self.config.platform.clone().ok_or(HostError::Unsupported)?;
            Ok(HostFacts {
                platform,
                arch: self.config.arch.clone(),
                privileged: self.config.privileged,
                cores: self.config.cores,
                memory: self.config.memory,
                virtualization: self.config.virtualization,
                disks: self.disks.clone(),
            })
        })
    }

    fn free_space<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<u64, HostError>> {
        Box::pin(async move {
            RtPath::new(path).map_err(|_| {
                HostError::InvalidInput(Detail::new("free_space takes a valid RtPath text"))
            })?;
            let Some(shared) = self.shared.upgrade() else {
                return Ok(0);
            };
            let core = lock(&shared.core);
            Ok(core.host(self.host).map_or(0, |host| host.disk.free()))
        })
    }

    fn service_status(&self) -> BoxFuture<'_, Result<ServiceStatus, HostError>> {
        Box::pin(async move {
            if !self.config.service {
                return Err(HostError::NotInstalled);
            }
            Ok(*lock(&self.service))
        })
    }

    fn service(&self, action: ServiceAction) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            let result = self.act(action);
            self.record(action, &result);
            result
        })
    }
}
