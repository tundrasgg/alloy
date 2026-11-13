//! `alloyd` as the Windows service `alloyd` (ARFC 0017 clause 3.15 step 7,
//! ARFC 0002 section 3.11).

use std::time::Duration;

use tracing::error;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::service_dispatcher;

use crate::{DaemonError, RunOptions, SHUTDOWN_GRACE, Stop, StopCause, default_data_dir, run};

/// The name of the Windows service (ARFC 0017 clauses 3.15 and 3.17).
pub const SERVICE_NAME: &str = "alloyd";

/// The Win32 error `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`: the process was
/// not started by the service control manager.
const NOT_A_SERVICE: i32 = 1063;

/// The exit code reported to the service control manager when [`run`] fails.
const FAILED: u32 = 1;

/// Hands the calling thread to the service control manager, which runs the
/// service `alloyd` on a thread of its own and returns once the service has
/// stopped. Returns `false` at once, changing nothing, when this process was
/// not started by the service control manager, as at a console.
///
/// The service reads `%ProgramData%` from its environment to find its data
/// directory, runs [`run`] until the service control manager sends a stop or
/// shutdown control, and reports the outcome as its exit code: 0 or 1.
///
/// # Errors
///
/// [`DaemonError::Service`] when the service control manager refuses the
/// dispatcher for another reason.
pub fn dispatch() -> Result<bool, DaemonError> {
    match service_dispatcher::start(SERVICE_NAME, service_main) {
        Ok(()) => Ok(true),
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(NOT_A_SERVICE) => {
            Ok(false)
        }
        Err(e) => Err(DaemonError::Service(e)),
    }
}

/// The service's main function, called by the service control manager on a
/// thread of its own. The service takes no start parameters, so the raw
/// arguments are not read.
extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    serve();
}

fn serve() {
    let stop = Stop::new();
    let requests = stop.clone();
    let handler = move |control| match control {
        ServiceControl::Stop => {
            requests.trigger(StopCause::ServiceStop);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Shutdown => {
            requests.trigger(StopCause::SystemShutdown);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = match service_control_handler::register(SERVICE_NAME, handler) {
        Ok(status) => status,
        Err(e) => {
            error!(error = %e, "the service control handler was refused");
            return;
        }
    };
    report(
        status,
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ServiceExitCode::NO_ERROR,
    );
    let program_data = std::env::var_os("ProgramData");
    let outcome = default_data_dir(program_data.as_deref())
        .map_err(DaemonError::from)
        .and_then(|data_dir| {
            run(&RunOptions { data_dir }, || {
                let stopped = stop.wait();
                Ok(async move {
                    let cause = stopped.await;
                    report(
                        status,
                        ServiceState::StopPending,
                        ServiceControlAccept::empty(),
                        ServiceExitCode::NO_ERROR,
                    );
                    cause
                })
            })
        });
    let exit = match outcome {
        Ok(_) => ServiceExitCode::NO_ERROR,
        Err(_) => ServiceExitCode::ServiceSpecific(FAILED),
    };
    report(
        status,
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit,
    );
}

/// Reports the service's state. A refused report is logged: the service
/// control manager then keeps the previous state.
fn report(
    status: ServiceStatusHandle,
    state: ServiceState,
    accept: ServiceControlAccept,
    exit: ServiceExitCode,
) {
    let wait_hint = if state == ServiceState::StopPending {
        SHUTDOWN_GRACE + Duration::from_secs(5)
    } else {
        Duration::ZERO
    };
    let refused = status.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: exit,
        checkpoint: 0,
        wait_hint,
        process_id: None,
    });
    if let Err(e) = refused {
        error!(error = %e, state = ?state, "the service state was not reported");
    }
}
