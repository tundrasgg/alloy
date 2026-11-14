//! `host` under `os` on a platform other than Linux and Windows: every
//! method returns `Unsupported` (ARFC 0005 clause 3.1.18).

use std::time::Instant;

use super::{Manager, ServiceAction};
use crate::{HostError, HostFacts, ServiceStatus};

/// The `Os` code of a call cut short because the runtime stopped: `EIO`.
pub(super) const STOPPED: i32 = 5;

pub(super) fn facts() -> Result<HostFacts, HostError> {
    Err(HostError::Unsupported)
}

pub(super) fn free_space(_path: &str) -> Result<u64, HostError> {
    Err(HostError::Unsupported)
}

pub(super) fn status(_deadline: Instant) -> Result<ServiceStatus, HostError> {
    Err(HostError::Unsupported)
}

/// No service manager.
pub(super) struct Service;

impl Manager for Service {
    const POLLS_START: bool = false;

    fn privileged() -> bool {
        true
    }

    fn status(_deadline: Instant) -> Result<ServiceStatus, HostError> {
        Err(HostError::Unsupported)
    }

    fn request(_action: ServiceAction, _deadline: Instant) -> Result<(), HostError> {
        Err(HostError::Unsupported)
    }

    fn stop_reason() -> Option<String> {
        None
    }
}
