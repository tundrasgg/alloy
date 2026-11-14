//! The `os` implementation of `host` (ARFC 0005 clause 3.1.18): the
//! machine's facts, free space, and the service `alloyd` through systemd on
//! Linux and the service control manager on Windows. On any other platform
//! every method returns `Unsupported`.
//!
//! Every call runs on the runtime's blocking pool. A `service` action runs
//! to its end there with a deadline 60 seconds after it started, so a
//! dropped future leaves the action to finish or time out on its own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{BoxFuture, HostError, HostFacts, RunState, ServiceAction, ServiceStatus};

use super::Host as OsRuntime;

mod parse;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as sys;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

#[cfg(not(any(target_os = "linux", windows)))]
mod other;
#[cfg(not(any(target_os = "linux", windows)))]
use other as sys;

#[cfg(test)]
mod tests;

/// The longest one `service` action takes (ARFC 0005 section 7).
pub(super) const ACTION_LIMIT: Duration = Duration::from_secs(60);

/// How often a service's state is read while an action waits for it.
const POLL: Duration = Duration::from_millis(100);

/// The `os` instance of `host`.
#[derive(Debug)]
pub(super) struct OsMachine {
    host: OsRuntime,
    /// Held by the `service` call that runs, so that calls run one at a
    /// time. `tokio`'s mutex grants it in the order it was asked for.
    serial: Arc<tokio::sync::Mutex<()>>,
}

impl OsMachine {
    pub(super) fn new(host: OsRuntime) -> OsMachine {
        OsMachine {
            host,
            serial: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Runs `work` on the blocking pool, entering the runtime's context.
    fn blocking<T, F>(&self, work: F) -> BoxFuture<'_, Result<T, HostError>>
    where
        F: FnOnce() -> Result<T, HostError> + Send + 'static,
        T: Send + 'static,
    {
        let task = self.host.blocking(work);
        self.host.run(async move {
            task.await
                .unwrap_or(Err(HostError::Os { code: sys::STOPPED }))
        })
    }
}

impl crate::Host for OsMachine {
    fn facts(&self) -> BoxFuture<'_, Result<HostFacts, HostError>> {
        self.blocking(sys::facts)
    }

    fn free_space<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<u64, HostError>> {
        let path = path.to_owned();
        self.blocking(move || sys::free_space(&path))
    }

    fn service_status(&self) -> BoxFuture<'_, Result<ServiceStatus, HostError>> {
        self.blocking(|| sys::status(Instant::now() + ACTION_LIMIT))
    }

    fn service(&self, action: ServiceAction) -> BoxFuture<'_, Result<(), HostError>> {
        let serial = Arc::clone(&self.serial);
        let host = self.host.clone();
        self.host.run(async move {
            let _turn = serial.lock().await;
            let task = host.blocking(move || act(action, Instant::now() + ACTION_LIMIT));
            task.await
                .unwrap_or(Err(HostError::Os { code: sys::STOPPED }))
        })
    }
}

/// What a platform gives `act`.
pub(super) trait Manager {
    /// Whether `Start` waits for the service to leave `Starting`. `systemctl
    /// start` returns once its job has finished, so on Linux the state is
    /// read once after it; the service control manager returns at once.
    const POLLS_START: bool;
    /// Whether the process is privileged as `facts` defines it.
    fn privileged() -> bool;
    /// The service's state, or `NotInstalled`.
    fn status(deadline: Instant) -> Result<ServiceStatus, HostError>;
    /// Asks the service manager for `action` and returns once it has
    /// answered. For `Start` and `Stop` the caller then waits for the state.
    fn request(action: ServiceAction, deadline: Instant) -> Result<(), HostError>;
    /// When the service manager stopped a started service, why.
    fn stop_reason() -> Option<String>;
}

/// Applies `action` by the rules of clause 3.1.18.3, through the platform's
/// service manager, finishing by `deadline`.
fn act(action: ServiceAction, deadline: Instant) -> Result<(), HostError> {
    act_with::<sys::Service>(action, deadline)
}

fn act_with<M: Manager>(action: ServiceAction, deadline: Instant) -> Result<(), HostError> {
    if !M::privileged() {
        return Err(HostError::PermissionDenied);
    }
    let status = M::status(deadline)?;
    match action {
        ServiceAction::Enable if status.enabled => Ok(()),
        ServiceAction::Disable if !status.enabled => Ok(()),
        ServiceAction::Enable | ServiceAction::Disable => M::request(action, deadline),
        ServiceAction::Start if !status.enabled => Err(HostError::Disabled),
        ServiceAction::Start if status.run == RunState::Running => Ok(()),
        ServiceAction::Stop if status.run == RunState::Stopped => Ok(()),
        ServiceAction::Start => {
            M::request(action, deadline)?;
            wait_for::<M>(RunState::Running, deadline)
        }
        ServiceAction::Stop => {
            M::request(action, deadline)?;
            wait_for::<M>(RunState::Stopped, deadline)
        }
    }
}

/// Reads the state every [`POLL`] until it is `want`. Waiting to run, a
/// service that is `Stopped` has stopped instead: `Refused`.
fn wait_for<M: Manager>(want: RunState, deadline: Instant) -> Result<(), HostError> {
    loop {
        let status = M::status(deadline)?;
        if status.run == want {
            return Ok(());
        }
        let gave_up =
            want == RunState::Running && (status.run == RunState::Stopped || !M::POLLS_START);
        if gave_up {
            let reason = M::stop_reason().unwrap_or_else(|| "alloyd stopped".to_owned());
            return Err(HostError::Refused(alloy_seam::Detail::new(&reason)));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(HostError::TimedOut);
        }
        std::thread::sleep(POLL.min(deadline - now));
    }
}
