//! The `mem` instance of `host` (ARFC 0005 clauses 3.1.18.1 to 3.1.18.5):
//! one per simulated host, shared by every start, reporting the host's
//! [`MachineConfig`] and holding the state of its service `alloyd`.

use std::sync::{Mutex, Weak};

use alloy_rt::{
    Arch, BoxFuture, DiskFact, Host, HostError, HostFacts, Platform, PortHolder, PortProtocol,
    RtPath, RunState, ServiceAction, ServiceStatus,
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
/// The most `MachineConfig::ports` entries, and the most entries of a
/// `port_holders` result (section 7).
const MAX_HOLDERS: usize = 256;
/// The most bytes of `PortHolder::process` (section 7).
const MAX_PROCESS: usize = 255;

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
    /// Holders of ports that `port_holders` reports beside the host's own
    /// sockets, as other programs on the machine would hold them. They
    /// hold no port of the host's `net` (clause 3.1.18.5). At most 256.
    /// Default empty.
    pub ports: Vec<(PortProtocol, PortHolder)>,
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
            ports: Vec::new(),
        }
    }
}

impl MachineConfig {
    /// Why `add_host` refuses this configuration: `cores` 0, a Linux
    /// `release` over 64 bytes, an `Arch::Other` text outside 1 to 32
    /// bytes, more than 256 `ports` entries, or a `ports` entry whose
    /// `local` port is 0, whose `process` is outside 1 to 255 bytes, or
    /// whose `process` is `Some` with `pid` `None` (clause 3.1.18.5).
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
        if self.ports.len() > MAX_HOLDERS {
            return Err("a machine has at most 256 ports entries");
        }
        for (_, holder) in &self.ports {
            if holder.local.port() == 0 {
                return Err("a machine's ports entry has a local port from 1 to 65,535");
            }
            if let Some(process) = &holder.process {
                if !(1..=MAX_PROCESS).contains(&process.len()) {
                    return Err("a machine's ports entry has a process of 1 to 255 bytes");
                }
                if holder.pid.is_none() {
                    return Err("a machine's ports entry with a process has a pid");
                }
            }
        }
        Ok(())
    }
}

/// The second and third steps of clause 3.1.18.5: each control character
/// becomes U+FFFD, then the text is cut to 255 bytes at a character
/// boundary. `None` when that leaves nothing.
fn process_text(text: &str) -> Option<String> {
    let mut replaced: String = text
        .chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect();
    let mut end = replaced.len().min(MAX_PROCESS);
    while !replaced.is_char_boundary(end) {
        end -= 1;
    }
    replaced.truncate(end);
    (!replaced.is_empty()).then_some(replaced)
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

    /// The host's `net` opens no socket yet: it refuses every `bind_udp`
    /// and `listen` until T-0077 and T-0078 build them. So the result is
    /// the `ports` entries of `protocol` and `port` alone, which hold no
    /// port and outlive a crash. Writes no trace record.
    fn port_holders(
        &self,
        protocol: PortProtocol,
        port: u16,
    ) -> BoxFuture<'_, Result<Vec<PortHolder>, HostError>> {
        Box::pin(async move {
            if port == 0 {
                return Err(HostError::InvalidInput(Detail::new(
                    "port_holders takes a port from 1 to 65,535",
                )));
            }
            let mut holders: Vec<PortHolder> = self
                .config
                .ports
                .iter()
                .filter(|(p, holder)| *p == protocol && holder.local.port() == port)
                .map(|(_, holder)| PortHolder {
                    local: holder.local,
                    pid: holder.pid,
                    process: holder.process.as_deref().and_then(process_text),
                })
                .collect();
            holders.sort();
            holders.dedup();
            holders.truncate(MAX_HOLDERS);
            Ok(holders)
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
