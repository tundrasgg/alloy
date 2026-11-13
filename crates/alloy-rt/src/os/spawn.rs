//! The `os` spawner (ARFC 0005 clause 3.1.6): tasks on the host's `tokio`
//! runtime.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};

use tokio::task::{AbortHandle, JoinHandle};

use super::{Host, Permit};
use crate::{BoxFuture, JoinError, Spawn, SpawnError, TaskHandle, TaskId, TaskName};

/// The most live tasks of one instance (ARFC 0005 section 7).
const MAX_LIVE: usize = 1_048_576;

/// Seam `spawn`, implementation `os`.
pub(super) struct OsSpawn {
    host: Host,
    /// Tasks whose future has not been dropped yet.
    live: Arc<AtomicUsize>,
    /// The id of the last task spawned. Ids start at 1.
    last_id: AtomicU64,
}

/// Writes the count of live tasks.
impl fmt::Debug for OsSpawn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsSpawn")
            .field("live", &self.live.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl OsSpawn {
    pub(super) fn new(host: Host) -> OsSpawn {
        OsSpawn {
            host,
            live: Arc::new(AtomicUsize::new(0)),
            last_id: AtomicU64::new(0),
        }
    }
}

impl Spawn for OsSpawn {
    fn spawn(
        &self,
        name: TaskName,
        task: BoxFuture<'static, ()>,
    ) -> Result<Box<dyn TaskHandle>, SpawnError> {
        if self.host.is_shut() {
            return Err(SpawnError::ShuttingDown);
        }
        let place = Permit::take(&self.live, MAX_LIVE).ok_or(SpawnError::Limit)?;
        let id = TaskId(self.last_id.fetch_add(1, Ordering::SeqCst) + 1);
        // The place is freed when the future is dropped: on completion, on
        // cancellation, or when a stopping runtime drops the task.
        let join = self.host.handle().spawn(async move {
            let _place = place;
            task.await;
        });
        Ok(Box::new(OsTaskHandle {
            id,
            name,
            abort: join.abort_handle(),
            join,
        }))
    }

    fn yield_now(&self) -> BoxFuture<'static, ()> {
        Box::pin(YieldNow { yielded: false })
    }
}

/// The handle of one `os` task.
struct OsTaskHandle {
    id: TaskId,
    name: TaskName,
    abort: AbortHandle,
    join: JoinHandle<()>,
}

/// Writes the id and the name.
impl fmt::Debug for OsTaskHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsTaskHandle")
            .field("id", &self.id)
            .field("name", &self.name.as_str())
            .finish()
    }
}

impl TaskHandle for OsTaskHandle {
    fn id(&self) -> TaskId {
        self.id
    }

    fn cancel(&self) {
        self.abort.abort();
    }

    fn join(self: Box<Self>) -> BoxFuture<'static, Result<(), JoinError>> {
        let OsTaskHandle { join, .. } = *self;
        Box::pin(async move {
            // `tokio` reports a task as ended only after it dropped the
            // task's future.
            join.await.map_err(|e| {
                if e.is_panic() {
                    JoinError::Panicked
                } else {
                    JoinError::Cancelled
                }
            })
        })
    }
}

/// Completes on its second poll, having woken its task on the first.
#[derive(Debug)]
struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::os::tests::host;

    fn name() -> TaskName {
        TaskName::new("test-task").unwrap()
    }

    #[test]
    fn tasks_run_join_and_count_their_ids_from_1() {
        let host = host();
        let spawn = OsSpawn::new(host.host());
        let out = Arc::new(Mutex::new(0));
        let sink = Arc::clone(&out);
        let first = spawn
            .spawn(
                name(),
                Box::pin(async move {
                    *sink.lock().unwrap() = 7;
                }),
            )
            .unwrap();
        assert_eq!(first.id(), TaskId(1));
        assert!(format!("{first:?}").contains("test-task"));
        assert_eq!(host.block_on(first.join()), Ok(()));
        assert_eq!(*out.lock().unwrap(), 7);
        let second = spawn.spawn(name(), Box::pin(async {})).unwrap();
        assert_eq!(second.id(), TaskId(2));
        host.block_on(second.join()).unwrap();
        assert_eq!(spawn.live.load(Ordering::SeqCst), 0);
        host.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn cancel_drops_the_task_and_join_reports_it() {
        let host = host();
        let spawn = OsSpawn::new(host.host());
        let handle = spawn
            .spawn(name(), Box::pin(std::future::pending()))
            .unwrap();
        handle.cancel();
        handle.cancel();
        assert_eq!(host.block_on(handle.join()), Err(JoinError::Cancelled));
        assert_eq!(spawn.live.load(Ordering::SeqCst), 0);
        host.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn a_panicking_task_joins_panicked_and_others_run() {
        let host = host();
        let spawn = OsSpawn::new(host.host());
        let bad = spawn
            .spawn(name(), Box::pin(async { panic!("deliberate test panic") }))
            .unwrap();
        assert_eq!(host.block_on(bad.join()), Err(JoinError::Panicked));
        let good = spawn.spawn(name(), Box::pin(async {})).unwrap();
        assert_eq!(host.block_on(good.join()), Ok(()));
        host.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn the_limit_counts_live_tasks() {
        let host = host();
        let spawn = OsSpawn::new(host.host());
        spawn.live.store(MAX_LIVE, Ordering::SeqCst);
        assert_eq!(
            spawn.spawn(name(), Box::pin(async {})).map(|_| ()),
            Err(SpawnError::Limit)
        );
        spawn.live.store(MAX_LIVE - 1, Ordering::SeqCst);
        let handle = spawn
            .spawn(name(), Box::pin(std::future::pending()))
            .unwrap();
        assert_eq!(
            spawn.spawn(name(), Box::pin(async {})).map(|_| ()),
            Err(SpawnError::Limit)
        );
        handle.cancel();
        host.block_on(handle.join()).unwrap_err();
        assert_eq!(spawn.live.load(Ordering::SeqCst), MAX_LIVE - 1);
        spawn.live.store(0, Ordering::SeqCst);
        host.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn yield_now_completes_on_its_second_poll() {
        let host = host();
        let spawn = OsSpawn::new(host.host());
        let mut polls = 0;
        let mut yielding = spawn.yield_now();
        host.block_on(poll_fn(|cx| {
            polls += 1;
            yielding.as_mut().poll(cx)
        }));
        assert_eq!(polls, 2);
        host.shutdown(Duration::from_secs(1));
    }
}
