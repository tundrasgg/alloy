//! The `spawn` seam (ARFC 0005 section 2.2 and clause 3.1.6).

use std::fmt;

use crate::BoxFuture;

/// A task's name, for traces and diagnostics: 1 to 64 bytes, each from 0x21 to
/// 0x7E (printable ASCII without space).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskName(String);

impl TaskName {
    /// The longest name, in bytes.
    const MAX_LEN: usize = 64;

    /// Accepts exactly the strings of 1 to 64 bytes, each from 0x21 to 0x7E.
    ///
    /// # Errors
    ///
    /// [`SpawnError::InvalidName`] for any other string.
    pub fn new(name: &str) -> Result<TaskName, SpawnError> {
        let valid = (1..=Self::MAX_LEN).contains(&name.len())
            && name.bytes().all(|b| (0x21..=0x7E).contains(&b));
        if valid {
            Ok(TaskName(name.to_owned()))
        } else {
            Err(SpawnError::InvalidName)
        }
    }

    /// The name as given to [`TaskName::new`].
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifies one task of one [`Spawn`] instance. Never reused by that
/// instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(pub u64);

/// Seam `spawn`: starts, joins, and cancels tasks (ARFC 0005 clause 3.1.6).
///
/// An instance holds at most 1,048,576 live tasks. A task that panics ends
/// without affecting other tasks or the process.
pub trait Spawn: Send + Sync + fmt::Debug + 'static {
    /// Starts `task` and returns without polling it on the caller's stack. The
    /// task is polled until it completes or is cancelled.
    ///
    /// # Errors
    ///
    /// [`SpawnError::Limit`] when 1,048,576 tasks are live, and
    /// [`SpawnError::ShuttingDown`] once the runtime has begun shutting down
    /// (`os`) or the host has crashed (`mem`).
    fn spawn(
        &self,
        name: TaskName,
        task: BoxFuture<'static, ()>,
    ) -> Result<Box<dyn TaskHandle>, SpawnError>;

    /// A future that completes on its second poll, having woken its task on
    /// the first, so that other tasks can run.
    fn yield_now(&self) -> BoxFuture<'static, ()>;
}

/// Control of one spawned task. Dropping it detaches the task, which keeps
/// running.
pub trait TaskHandle: Send + Sync + fmt::Debug {
    /// The task's id.
    fn id(&self) -> TaskId;

    /// Stops the task: after `cancel` returns it is not polled again, except
    /// that a poll already running on another thread finishes. Idempotent, and
    /// has no effect on a task that has ended.
    fn cancel(&self);

    /// Completes after the task has ended and its future has been dropped.
    /// Dropping the returned future detaches the task.
    ///
    /// # Errors
    ///
    /// [`JoinError::Cancelled`] when the task's future was dropped unfinished,
    /// and [`JoinError::Panicked`] when it panicked. A task whose future
    /// completed returns `Ok(())`, also when `cancel` raced with its last poll.
    fn join(self: Box<Self>) -> BoxFuture<'static, Result<(), JoinError>>;
}

/// Why a task was not started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpawnError {
    /// A task name broke the rules of [`TaskName::new`].
    #[error("task name must be 1 to 64 bytes from 0x21 to 0x7E")]
    InvalidName,
    /// The instance holds 1,048,576 live tasks.
    #[error("1048576 tasks are live")]
    Limit,
    /// The runtime is shutting down, or the simulated host has crashed.
    #[error("the runtime is shutting down")]
    ShuttingDown,
}

/// Why a task ended without completing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoinError {
    /// The task was cancelled and its future dropped unfinished.
    #[error("task was cancelled")]
    Cancelled,
    /// The task panicked.
    #[error("task panicked")]
    Panicked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_names_follow_the_rules() {
        for ok in ["a", "~", "!", "net/recv-0", &"x".repeat(64)] {
            assert_eq!(
                TaskName::new(ok).map(|n| n.as_str().to_owned()),
                Ok(ok.to_owned())
            );
        }
        for bad in ["", " a", "a b", "tab\t", "é", "\u{7f}", &"x".repeat(65)] {
            assert_eq!(TaskName::new(bad), Err(SpawnError::InvalidName), "{bad:?}");
        }
    }
}
