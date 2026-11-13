//! Stop requests: [`Stop`] and [`StopCause`].

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::watch;

/// Why `alloyd` stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopCause {
    /// SIGTERM, on Unix platforms.
    Terminate,
    /// SIGINT, on Unix platforms.
    Interrupt,
    /// Ctrl-C or Ctrl-Break at a Windows console.
    CtrlC,
    /// A stop request from the Windows service control manager.
    ServiceStop,
    /// The Windows service control manager reports that the system shuts
    /// down.
    SystemShutdown,
}

impl fmt::Display for StopCause {
    /// Writes the signal's or the request's usual name, for the log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StopCause::Terminate => "SIGTERM",
            StopCause::Interrupt => "SIGINT",
            StopCause::CtrlC => "Ctrl-C",
            StopCause::ServiceStop => "service stop",
            StopCause::SystemShutdown => "system shutdown",
        })
    }
}

/// A stop request that any thread can make and any task can wait for. Clones
/// share it. The first [`Stop::trigger`] decides the cause; later ones change
/// nothing.
#[derive(Clone, Debug)]
pub struct Stop(Arc<watch::Sender<Option<StopCause>>>);

impl Default for Stop {
    fn default() -> Stop {
        Stop::new()
    }
}

impl Stop {
    /// A stop that has not been requested.
    pub fn new() -> Stop {
        Stop(Arc::new(watch::Sender::new(None)))
    }

    /// Requests the stop with `cause`, unless one was already requested.
    /// Never blocks; callable from any thread, also from a signal or service
    /// control handler thread.
    pub fn trigger(&self, cause: StopCause) {
        self.0.send_if_modified(|current| {
            if current.is_none() {
                *current = Some(cause);
                true
            } else {
                false
            }
        });
    }

    /// The cause of the requested stop, if one was requested.
    pub fn cause(&self) -> Option<StopCause> {
        *self.0.borrow()
    }

    /// Completes with the cause once a stop is requested, at once when one
    /// already was. Never completes when every clone of this `Stop` is
    /// dropped without a request.
    pub fn wait(&self) -> impl Future<Output = StopCause> + Send + 'static {
        let mut requests = self.0.subscribe();
        async move {
            match requests.wait_for(Option::is_some).await.map(|cause| *cause) {
                Ok(Some(cause)) => cause,
                _ => std::future::pending().await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::*;

    #[test]
    fn the_first_trigger_decides_the_cause() {
        let stop = Stop::new();
        assert_eq!(stop.cause(), None);
        let other = stop.clone();
        other.trigger(StopCause::ServiceStop);
        stop.trigger(StopCause::Terminate);
        assert_eq!(stop.cause(), Some(StopCause::ServiceStop));
    }

    #[test]
    fn wait_completes_after_a_trigger() {
        let stop = Stop::new();
        let mut cx = Context::from_waker(Waker::noop());
        let mut wait = pin!(stop.wait());
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
        stop.clone().trigger(StopCause::Interrupt);
        assert_eq!(
            wait.as_mut().poll(&mut cx),
            Poll::Ready(StopCause::Interrupt)
        );
        let mut late = pin!(stop.wait());
        assert_eq!(
            late.as_mut().poll(&mut cx),
            Poll::Ready(StopCause::Interrupt)
        );
    }

    #[test]
    fn wait_without_any_stop_left_never_completes() {
        let stop = Stop::new();
        let mut wait = pin!(stop.wait());
        drop(stop);
        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
    }

    #[test]
    fn causes_display_their_names() {
        let names: Vec<String> = [
            StopCause::Terminate,
            StopCause::Interrupt,
            StopCause::CtrlC,
            StopCause::ServiceStop,
            StopCause::SystemShutdown,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            names,
            [
                "SIGTERM",
                "SIGINT",
                "Ctrl-C",
                "service stop",
                "system shutdown"
            ]
        );
    }
}
