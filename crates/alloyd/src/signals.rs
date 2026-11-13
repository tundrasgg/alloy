//! The stop signals of the `alloyd` process.
//!
//! No runtime seam of ARFC 0005 carries process signals, so the daemon's
//! entry watches them here, through `tokio`'s signal driver on the [`OsHost`]
//! runtime that [`run`] starts.
//!
//! [`OsHost`]: alloy_rt::OsHost
//! [`run`]: crate::run

use std::future::{Future, poll_fn};
use std::task::Poll;

use crate::{DaemonError, StopCause};

/// Watches the stop signals of this process and returns a future that
/// completes with the first one received: SIGTERM or SIGINT on Unix
/// platforms, Ctrl-C or Ctrl-Break on a Windows console.
///
/// Must be called inside the runtime's context, as [`run`](crate::run) calls
/// its `stop_when`. Once called, those signals no longer end the process by
/// default.
///
/// # Errors
///
/// [`DaemonError::Signals`] when the operating system refuses a handler.
#[cfg(unix)]
pub fn os_signals() -> Result<impl Future<Output = StopCause> + Send, DaemonError> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate()).map_err(DaemonError::Signals)?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(DaemonError::Signals)?;
    Ok(poll_fn(move |cx| {
        // `Ready(None)` means the signal driver stopped, which happens only
        // when the runtime shuts down: a stop either way.
        if terminate.poll_recv(cx).is_ready() {
            return Poll::Ready(StopCause::Terminate);
        }
        if interrupt.poll_recv(cx).is_ready() {
            return Poll::Ready(StopCause::Interrupt);
        }
        Poll::Pending
    }))
}

/// Watches the stop signals of this process and returns a future that
/// completes with the first one received: SIGTERM or SIGINT on Unix
/// platforms, Ctrl-C or Ctrl-Break on a Windows console.
///
/// Must be called inside the runtime's context, as [`run`](crate::run) calls
/// its `stop_when`. Once called, those signals no longer end the process by
/// default.
///
/// # Errors
///
/// [`DaemonError::Signals`] when the operating system refuses a handler.
#[cfg(windows)]
pub fn os_signals() -> Result<impl Future<Output = StopCause> + Send, DaemonError> {
    use tokio::signal::windows::{ctrl_break, ctrl_c};

    let mut c = ctrl_c().map_err(DaemonError::Signals)?;
    let mut brk = ctrl_break().map_err(DaemonError::Signals)?;
    Ok(poll_fn(move |cx| {
        // `Ready(None)` means the signal driver stopped, which happens only
        // when the runtime shuts down: a stop either way.
        if c.poll_recv(cx).is_ready() || brk.poll_recv(cx).is_ready() {
            return Poll::Ready(StopCause::CtrlC);
        }
        Poll::Pending
    }))
}
