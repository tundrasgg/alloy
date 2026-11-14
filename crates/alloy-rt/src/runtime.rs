//! The runtime bundle and the runtime seam markers (ARFC 0005 section 2.5 and
//! clause 3.1.14).

use std::fmt;
use std::future::{Future, poll_fn};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::time::Duration;

use alloy_seam::{Binding, BuildError, ConfigValue, ImplName, Registry, Seam, SwapClass};

use crate::{
    BoxFuture, Clock, DiskIo, JoinError, Net, Rng, Spawn, SpawnError, TaskHandle, TaskId, TaskName,
};

/// Seam marker of `clock`: object `dyn Clock`, context `()`, swap class
/// `restart`.
#[derive(Debug)]
pub struct ClockSeam;

impl Seam for ClockSeam {
    type Object = dyn Clock;
    type Context = ();
    const KIND: &'static str = "clock";
    const SWAP: SwapClass = SwapClass::Restart;
}

/// Seam marker of `net`: object `dyn Net`, context `()`, swap class `restart`.
#[derive(Debug)]
pub struct NetSeam;

impl Seam for NetSeam {
    type Object = dyn Net;
    type Context = ();
    const KIND: &'static str = "net";
    const SWAP: SwapClass = SwapClass::Restart;
}

/// Seam marker of `disk-io`: object `dyn DiskIo`, context `()`, swap class
/// `restart`.
#[derive(Debug)]
pub struct DiskIoSeam;

impl Seam for DiskIoSeam {
    type Object = dyn DiskIo;
    type Context = ();
    const KIND: &'static str = "disk-io";
    const SWAP: SwapClass = SwapClass::Restart;
}

/// Seam marker of `rng`: object `dyn Rng`, context `()`, swap class `restart`.
#[derive(Debug)]
pub struct RngSeam;

impl Seam for RngSeam {
    type Object = dyn Rng;
    type Context = ();
    const KIND: &'static str = "rng";
    const SWAP: SwapClass = SwapClass::Restart;
}

/// Seam marker of `spawn`: object `dyn Spawn`, context `()`, swap class
/// `restart`.
#[derive(Debug)]
pub struct SpawnSeam;

impl Seam for SpawnSeam {
    type Object = dyn Spawn;
    type Context = ();
    const KIND: &'static str = "spawn";
    const SWAP: SwapClass = SwapClass::Restart;
}

/// The bindings of the five runtime seams. Its `Debug` output holds each
/// configuration's version and length only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// The `clock` binding.
    pub clock: Binding,
    /// The `net` binding.
    pub net: Binding,
    /// The `disk-io` binding.
    pub disk_io: Binding,
    /// The `rng` binding.
    pub rng: Binding,
    /// The `spawn` binding.
    pub spawn: Binding,
}

impl RuntimeConfig {
    /// All five bound to `name` with `ConfigValue::defaults(1)`.
    pub fn all(name: ImplName) -> RuntimeConfig {
        // Version 1 is not 0, and an empty payload serializes to 2 bytes, far
        // below the limit, so `defaults(1)` cannot fail.
        let config = ConfigValue::defaults(1).expect("version 1 with no payload is valid");
        let binding = Binding { name, config };
        RuntimeConfig {
            clock: binding.clone(),
            net: binding.clone(),
            disk_io: binding.clone(),
            rng: binding.clone(),
            spawn: binding,
        }
    }
}

/// One instance of each runtime seam: everything a component uses to reach
/// the world (ARFC 0005 section 2). Clones share the instances.
#[derive(Clone, Debug)]
pub struct Runtime {
    clock: Arc<dyn Clock>,
    net: Arc<dyn Net>,
    disk_io: Arc<dyn DiskIo>,
    rng: Arc<dyn Rng>,
    spawn: Arc<dyn Spawn>,
}

impl Runtime {
    /// A runtime of the given instances.
    pub fn new(
        clock: Arc<dyn Clock>,
        net: Arc<dyn Net>,
        disk_io: Arc<dyn DiskIo>,
        rng: Arc<dyn Rng>,
        spawn: Arc<dyn Spawn>,
    ) -> Runtime {
        Runtime {
            clock,
            net,
            disk_io,
            rng,
            spawn,
        }
    }

    /// Builds the five instances from `registry`, in the order clock, rng,
    /// spawn, net, disk-io, each with context `()` (ARFC 0005 clause 3.1.14).
    ///
    /// # Errors
    ///
    /// The first [`BuildError`]. The instances built before it are dropped.
    pub fn from_registry(
        registry: &Registry,
        config: &RuntimeConfig,
    ) -> Result<Runtime, BuildError> {
        fn build<S: Seam<Context = ()>>(
            registry: &Registry,
            binding: &Binding,
        ) -> Result<Arc<S::Object>, BuildError> {
            registry.instantiate::<S>(&(), &binding.name, &binding.config)
        }
        let clock = build::<ClockSeam>(registry, &config.clock)?;
        let rng = build::<RngSeam>(registry, &config.rng)?;
        let spawn = build::<SpawnSeam>(registry, &config.spawn)?;
        let net = build::<NetSeam>(registry, &config.net)?;
        let disk_io = build::<DiskIoSeam>(registry, &config.disk_io)?;
        Ok(Runtime::new(clock, net, disk_io, rng, spawn))
    }

    /// The `clock` instance.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// The `net` instance.
    pub fn net(&self) -> &Arc<dyn Net> {
        &self.net
    }

    /// The `disk-io` instance.
    pub fn disk_io(&self) -> &Arc<dyn DiskIo> {
        &self.disk_io
    }

    /// The `rng` instance.
    pub fn rng(&self) -> &Arc<dyn Rng> {
        &self.rng
    }

    /// The `spawn` instance.
    pub fn spawner(&self) -> &Arc<dyn Spawn> {
        &self.spawn
    }

    /// Spawns `future` through the `spawn` instance and returns a typed handle
    /// whose [`JoinHandle::join`] gives the future's output.
    ///
    /// # Errors
    ///
    /// As [`Spawn::spawn`].
    pub fn spawn<F>(&self, name: TaskName, future: F) -> Result<JoinHandle<F::Output>, SpawnError>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let output = Arc::new(Mutex::new(None));
        let sender = Arc::clone(&output);
        let task = Box::pin(async move {
            let value = future.await;
            *sender.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
        });
        let handle = self.spawn.spawn(name, task)?;
        Ok(JoinHandle { handle, output })
    }

    /// Completes after `duration` on this runtime's clock:
    /// `clock.sleep_until(clock.now().saturating_add(duration))`, with the
    /// deadline fixed at the call.
    pub fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        self.clock
            .sleep_until(self.clock.now().saturating_add(duration))
    }

    /// Runs `future` with a time limit of `duration` on this runtime's clock,
    /// starting at the call. Each poll polls `future` before the timer.
    ///
    /// # Errors
    ///
    /// [`Elapsed`] when the timer completes first. `future` is then dropped.
    pub fn timeout<'a, F>(
        &self,
        duration: Duration,
        future: F,
    ) -> BoxFuture<'a, Result<F::Output, Elapsed>>
    where
        F: Future + Send + 'a,
        F::Output: Send + 'a,
    {
        let mut timer = self.sleep(duration);
        let mut future = Box::pin(future);
        Box::pin(async move {
            poll_fn(|cx| {
                if let Poll::Ready(value) = future.as_mut().poll(cx) {
                    return Poll::Ready(Ok(value));
                }
                timer.as_mut().poll(cx).map(|()| Err(Elapsed))
            })
            .await
        })
    }
}

/// The time limit of [`Runtime::timeout`] passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("deadline elapsed")]
pub struct Elapsed;

/// A typed handle to a task started by [`Runtime::spawn`]. Dropping it detaches
/// the task, which keeps running.
pub struct JoinHandle<T> {
    handle: Box<dyn TaskHandle>,
    /// Written by the task when its future completes.
    output: Arc<Mutex<Option<T>>>,
}

/// Writes the task handle, never the output.
impl<T> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinHandle")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl<T: Send + 'static> JoinHandle<T> {
    /// The task's id.
    pub fn id(&self) -> TaskId {
        self.handle.id()
    }

    /// Cancels the task, as [`TaskHandle::cancel`].
    pub fn cancel(&self) {
        self.handle.cancel();
    }

    /// Completes after the task has ended and its future has been dropped,
    /// with the future's output.
    ///
    /// # Errors
    ///
    /// The [`JoinError`] of [`TaskHandle::join`].
    pub fn join(self) -> BoxFuture<'static, Result<T, JoinError>> {
        let JoinHandle { handle, output } = self;
        Box::pin(async move {
            handle.join().await?;
            let value = output.lock().unwrap_or_else(PoisonError::into_inner).take();
            // `join` returned `Ok`, so the task's future completed (ARFC 0005
            // clause 3.1.6), and its last act was to store the output.
            Ok(value.expect("a task that completed stored its output"))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::task::{Context, Waker};

    use alloy_seam::{Detail, Factory, Kind, RegistryBuilder};

    use super::*;
    use crate::{
        DirEntry, DiskError, DiskFile, Instant, InterfaceList, ListenOptions, Listener, Metadata,
        NetError, OpenOptions, Protection, RngError, RtPath, Security, Stream, StreamAddr,
        StreamOptions, UdpOptions, UdpSocket, WallTime,
    };

    fn poll<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    /// A clock the test moves by hand.
    #[derive(Debug, Default)]
    struct ManualClock(Arc<AtomicU64>);

    impl ManualClock {
        fn set(&self, nanos: u64) {
            self.0.store(nanos, Ordering::SeqCst);
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            Instant::from_nanos(self.0.load(Ordering::SeqCst))
        }
        fn wall(&self) -> WallTime {
            WallTime(0)
        }
        fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()> {
            let now = Arc::clone(&self.0);
            Box::pin(poll_fn(move |_| {
                if now.load(Ordering::SeqCst) >= deadline.as_nanos() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }))
        }
    }

    /// How a queued task ended: `Some(true)` completed, `Some(false)` dropped
    /// unfinished.
    type Ended = Arc<Mutex<Option<bool>>>;

    struct Queued {
        cancelled: Arc<AtomicBool>,
        ended: Ended,
        task: BoxFuture<'static, ()>,
    }

    /// Queues spawned tasks; the test runs them with `run`.
    #[derive(Default)]
    struct QueueSpawn {
        next: AtomicU64,
        queue: Mutex<Vec<Queued>>,
    }

    impl fmt::Debug for QueueSpawn {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("QueueSpawn")
        }
    }

    impl QueueSpawn {
        /// Polls every queued task once, in spawn order, and drops the ones
        /// that are cancelled or finished.
        fn run(&self) {
            let tasks = std::mem::take(&mut *self.queue.lock().unwrap());
            let mut pending = Vec::new();
            for mut queued in tasks {
                if queued.cancelled.load(Ordering::SeqCst) {
                    drop(queued.task);
                    *queued.ended.lock().unwrap() = Some(false);
                } else if poll(queued.task.as_mut()).is_ready() {
                    drop(queued.task);
                    *queued.ended.lock().unwrap() = Some(true);
                } else {
                    pending.push(queued);
                }
            }
            self.queue.lock().unwrap().extend(pending);
        }
    }

    #[derive(Debug)]
    struct QueueHandle {
        id: TaskId,
        cancelled: Arc<AtomicBool>,
        ended: Ended,
    }

    impl TaskHandle for QueueHandle {
        fn id(&self) -> TaskId {
            self.id
        }
        fn cancel(&self) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        fn join(self: Box<Self>) -> BoxFuture<'static, Result<(), JoinError>> {
            Box::pin(poll_fn(move |_| match *self.ended.lock().unwrap() {
                Some(true) => Poll::Ready(Ok(())),
                Some(false) => Poll::Ready(Err(JoinError::Cancelled)),
                None => Poll::Pending,
            }))
        }
    }

    impl Spawn for QueueSpawn {
        fn spawn(
            &self,
            _name: TaskName,
            task: BoxFuture<'static, ()>,
        ) -> Result<Box<dyn TaskHandle>, SpawnError> {
            let id = TaskId(self.next.fetch_add(1, Ordering::SeqCst) + 1);
            let cancelled = Arc::new(AtomicBool::new(false));
            let ended = Ended::default();
            self.queue.lock().unwrap().push(Queued {
                cancelled: cancelled.clone(),
                ended: ended.clone(),
                task,
            });
            Ok(Box::new(QueueHandle {
                id,
                cancelled,
                ended,
            }))
        }
        fn yield_now(&self) -> BoxFuture<'static, ()> {
            Box::pin(std::future::ready(()))
        }
    }

    /// Reports every operation as unsupported.
    #[derive(Debug)]
    struct NoNet;

    impl Net for NoNet {
        fn interfaces(&self, _: Option<u64>) -> BoxFuture<'_, Result<InterfaceList, NetError>> {
            Box::pin(std::future::ready(Err(NetError::Unsupported)))
        }
        fn bind_udp(&self, _: SocketAddr, _: UdpOptions) -> Result<Box<dyn UdpSocket>, NetError> {
            Err(NetError::Unsupported)
        }
        fn connect(
            &self,
            _: StreamAddr,
            _: StreamOptions,
        ) -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>> {
            Box::pin(std::future::ready(Err(NetError::Unsupported)))
        }
        fn listen(&self, _: StreamAddr, _: ListenOptions) -> Result<Box<dyn Listener>, NetError> {
            Err(NetError::Unsupported)
        }
    }

    /// Reports every operation as unsupported.
    #[derive(Debug)]
    struct NoDisk;

    fn unsupported<'a, T: Send + 'a>() -> BoxFuture<'a, Result<T, DiskError>> {
        Box::pin(std::future::ready(Err(DiskError::Unsupported)))
    }

    impl DiskIo for NoDisk {
        fn open<'a>(
            &'a self,
            _: &'a RtPath,
            _: OpenOptions,
        ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
            unsupported()
        }
        fn metadata<'a>(&'a self, _: &'a RtPath) -> BoxFuture<'a, Result<Metadata, DiskError>> {
            unsupported()
        }
        fn list<'a>(&'a self, _: &'a RtPath) -> BoxFuture<'a, Result<Vec<DirEntry>, DiskError>> {
            unsupported()
        }
        fn create_dir<'a>(&'a self, _: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
            unsupported()
        }
        fn remove<'a>(&'a self, _: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
            unsupported()
        }
        fn rename<'a>(
            &'a self,
            _: &'a RtPath,
            _: &'a RtPath,
        ) -> BoxFuture<'a, Result<(), DiskError>> {
            unsupported()
        }
        fn replace<'a>(
            &'a self,
            _: &'a RtPath,
            _: &'a RtPath,
        ) -> BoxFuture<'a, Result<(), DiskError>> {
            unsupported()
        }
        fn sync_dir<'a>(&'a self, _: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
            unsupported()
        }
        fn create_file<'a>(
            &'a self,
            _: &'a RtPath,
            _: Protection,
        ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
            unsupported()
        }
        fn create_dir_protected<'a>(
            &'a self,
            _: &'a RtPath,
            _: Protection,
        ) -> BoxFuture<'a, Result<(), DiskError>> {
            unsupported()
        }
        fn security<'a>(&'a self, _: &'a RtPath) -> BoxFuture<'a, Result<Security, DiskError>> {
            unsupported()
        }
        fn set_mode<'a>(&'a self, _: &'a RtPath, _: u32) -> BoxFuture<'a, Result<(), DiskError>> {
            unsupported()
        }
        fn rt_path(&self, _: &str) -> Result<RtPath, DiskError> {
            Err(DiskError::Unsupported)
        }
    }

    /// Fills with a fixed byte.
    #[derive(Debug)]
    struct FixedRng;

    impl Rng for FixedRng {
        fn fill(&self, dest: &mut [u8]) -> Result<(), RngError> {
            dest.fill(7);
            Ok(())
        }
    }

    type Log = Arc<Mutex<Vec<&'static str>>>;

    /// A factory that logs `kind` when called, then fails or calls `make`.
    fn logged<S, F>(log: &Log, kind: &'static str, failing: Option<&str>, make: F) -> Factory<S>
    where
        S: Seam<Context = ()>,
        F: Fn() -> Arc<S::Object> + Send + Sync + 'static,
    {
        let log = log.clone();
        let fails = failing == Some(kind);
        Arc::new(move |(): &(), _: &ConfigValue| {
            log.lock().unwrap().push(kind);
            if fails {
                Err(BuildError::Failed(Detail::new(kind)))
            } else {
                Ok(make())
            }
        })
    }

    /// `test` registered for the five kinds, `failing` naming the one whose
    /// factory fails.
    fn registry(log: &Log, failing: Option<&str>) -> Registry {
        let name = || ImplName::new("test").unwrap();
        let mut b = RegistryBuilder::new();
        let disk_io = logged::<DiskIoSeam, _>(log, "disk-io", failing, || Arc::new(NoDisk));
        let net = logged::<NetSeam, _>(log, "net", failing, || Arc::new(NoNet));
        let spawn =
            logged::<SpawnSeam, _>(log, "spawn", failing, || Arc::new(QueueSpawn::default()));
        let rng = logged::<RngSeam, _>(log, "rng", failing, || Arc::new(FixedRng));
        let clock =
            logged::<ClockSeam, _>(log, "clock", failing, || Arc::new(ManualClock::default()));
        b.register::<DiskIoSeam>(name(), disk_io).unwrap();
        b.register::<NetSeam>(name(), net).unwrap();
        b.register::<SpawnSeam>(name(), spawn).unwrap();
        b.register::<RngSeam>(name(), rng).unwrap();
        b.register::<ClockSeam>(name(), clock).unwrap();
        b.build()
    }

    fn test_config() -> RuntimeConfig {
        RuntimeConfig::all(ImplName::new("test").unwrap())
    }

    #[test]
    fn markers_declare_the_runtime_kinds() {
        let kinds: Vec<_> = registry(&Log::default(), None)
            .entries()
            .into_iter()
            .map(|(kind, _, swap)| (kind, swap))
            .collect();
        let expected: Vec<_> = ["clock", "disk-io", "net", "rng", "spawn"]
            .into_iter()
            .map(|k| (Kind::new(k).unwrap(), SwapClass::Restart))
            .collect();
        assert_eq!(kinds, expected);
    }

    #[test]
    fn runtime_config_all_binds_every_seam_to_defaults() {
        let name = ImplName::new("mem").unwrap();
        let binding = Binding {
            name: name.clone(),
            config: ConfigValue::defaults(1).unwrap(),
        };
        let config = RuntimeConfig::all(name);
        for b in [
            &config.clock,
            &config.net,
            &config.disk_io,
            &config.rng,
            &config.spawn,
        ] {
            assert_eq!(*b, binding);
        }
    }

    #[test]
    fn from_registry_builds_in_order() {
        let log = Log::default();
        let runtime = Runtime::from_registry(&registry(&log, None), &test_config()).unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            ["clock", "rng", "spawn", "net", "disk-io"]
        );
        let mut bytes = [0; 3];
        runtime.rng().fill(&mut bytes).unwrap();
        assert_eq!(bytes, [7; 3]);
        assert_eq!(runtime.clock().now(), Instant::ZERO);
        let any = "0.0.0.0:0".parse().unwrap();
        let bound = runtime.net().bind_udp(any, UdpOptions::default());
        assert_eq!(bound.map(|_| ()), Err(NetError::Unsupported));
        let clone = runtime.clone();
        assert!(Arc::ptr_eq(clone.rng(), runtime.rng()));
        assert!(Arc::ptr_eq(clone.disk_io(), runtime.disk_io()));
        assert!(Arc::ptr_eq(clone.spawner(), runtime.spawner()));
    }

    #[test]
    fn from_registry_stops_at_the_first_failure() {
        for (failing, built) in [
            ("clock", &["clock"][..]),
            ("spawn", &["clock", "rng", "spawn"][..]),
            ("disk-io", &["clock", "rng", "spawn", "net", "disk-io"][..]),
        ] {
            let log = Log::default();
            let registry = registry(&log, Some(failing));
            assert_eq!(
                Runtime::from_registry(&registry, &test_config()).map(|_| ()),
                Err(BuildError::Failed(Detail::new(failing)))
            );
            assert_eq!(*log.lock().unwrap(), built);
        }
        let log = Log::default();
        let missing = RuntimeConfig::all(ImplName::new("os").unwrap());
        assert_eq!(
            Runtime::from_registry(&registry(&log, None), &missing).map(|_| ()),
            Err(BuildError::SeamUnavailable {
                kind: Kind::new("clock").unwrap(),
                name: ImplName::new("os").unwrap()
            })
        );
        assert!(log.lock().unwrap().is_empty());
    }

    struct Parts {
        runtime: Runtime,
        clock: Arc<ManualClock>,
        spawn: Arc<QueueSpawn>,
    }

    fn parts() -> Parts {
        let clock = Arc::new(ManualClock::default());
        let spawn = Arc::new(QueueSpawn::default());
        let runtime = Runtime::new(
            clock.clone(),
            Arc::new(NoNet),
            Arc::new(NoDisk),
            Arc::new(FixedRng),
            spawn.clone(),
        );
        Parts {
            runtime,
            clock,
            spawn,
        }
    }

    #[test]
    fn spawn_returns_the_output_through_join() {
        let p = parts();
        let name = TaskName::new("answer").unwrap();
        let handle = p.runtime.spawn(name, async { 42 }).unwrap();
        assert_eq!(handle.id(), TaskId(1));
        assert!(format!("{handle:?}").starts_with("JoinHandle { handle: QueueHandle"));
        let mut join = handle.join();
        assert!(poll(join.as_mut()).is_pending());
        p.spawn.run();
        assert_eq!(poll(join.as_mut()), Poll::Ready(Ok(42)));
    }

    #[test]
    fn a_cancelled_task_joins_cancelled() {
        let p = parts();
        let name = TaskName::new("forever").unwrap();
        let handle = p.runtime.spawn(name, std::future::pending::<u8>()).unwrap();
        p.spawn.run();
        handle.cancel();
        p.spawn.run();
        let mut join = handle.join();
        assert_eq!(poll(join.as_mut()), Poll::Ready(Err(JoinError::Cancelled)));
    }

    #[test]
    fn sleep_waits_for_now_plus_duration() {
        let p = parts();
        p.clock.set(100);
        let mut sleep = p.runtime.sleep(Duration::from_nanos(50));
        p.clock.set(149);
        assert!(poll(sleep.as_mut()).is_pending());
        p.clock.set(150);
        assert!(poll(sleep.as_mut()).is_ready());
        let mut forever = p.runtime.sleep(Duration::MAX);
        p.clock.set(u64::MAX >> 2);
        assert!(poll(forever.as_mut()).is_pending());
    }

    /// Sets its flag when dropped.
    struct Flag(Arc<AtomicBool>);

    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn timeout_polls_the_future_before_the_timer() {
        let p = parts();
        let mut ready = p.runtime.timeout(Duration::ZERO, async { "done" });
        assert_eq!(poll(ready.as_mut()), Poll::Ready(Ok("done")));

        let dropped = Arc::new(AtomicBool::new(false));
        let flag = Flag(dropped.clone());
        let mut slow = p.runtime.timeout(Duration::from_nanos(10), async move {
            let _flag = flag;
            std::future::pending::<()>().await;
        });
        assert!(poll(slow.as_mut()).is_pending());
        p.clock.set(9);
        assert!(poll(slow.as_mut()).is_pending());
        p.clock.set(10);
        assert_eq!(poll(slow.as_mut()), Poll::Ready(Err(Elapsed)));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(Elapsed.to_string(), "deadline elapsed");
    }
}
