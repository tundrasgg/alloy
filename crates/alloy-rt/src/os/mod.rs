//! The operating system implementations of the runtime seams (ARFC 0005
//! section 2.6 and clause 3.1.2): [`OsHost`] and [`register_os`].
//!
//! Everything here runs on one `tokio` multi-thread runtime. Blocking file
//! system calls go to its blocking pool, sockets to its I/O driver, and sleeps
//! to its timer. Every future this module returns enters the runtime's context
//! on each poll, so a caller may poll it from any thread.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the `os` implementations are the code that calls the operating system (ARFC 0005 clause 3.1.1)"
)]

use std::fmt;
use std::future::{Future, poll_fn};

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::task::Poll;
use std::time::Duration;

use alloy_seam::{BuildError, ConfigValue, Detail, ImplName, RegistryBuilder, RegistryError};
use alloy_seam::{Factory, SeamConfig};
use tokio::runtime::{Handle, Runtime};

use crate::{BoxFuture, ClockSeam, DiskIoSeam, HostSeam, NetSeam, RngSeam, SpawnSeam};

mod clock;
mod config;
mod disk;
mod errors;
mod host;
mod net;
mod rng;
mod spawn;

/// The process's operating system runtime: one `tokio` multi-thread runtime
/// with time and I/O drivers (ARFC 0005 section 2.6). Clones share it.
///
/// The `os` seam instances that [`register_os`] builds run on it. Dropping the
/// last clone, and the last instance built on it, stops the runtime as
/// [`OsHost::shutdown`] does with no grace.
#[derive(Clone)]
pub struct OsHost(Arc<HostInner>);

/// Writes whether the host is shutting down, never its tasks.
impl fmt::Debug for OsHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsHost")
            .field("shutting_down", &self.0.is_shut())
            .finish_non_exhaustive()
    }
}

/// What every clone of an [`OsHost`] and every instance built on it share.
struct HostInner {
    handle: Handle,
    /// `None` once [`OsHost::shutdown`] has taken it.
    runtime: Mutex<Option<Runtime>>,
    /// Set by [`OsHost::shutdown`] before the runtime stops.
    shutting_down: AtomicBool,
    /// Whether the timer driver may be stopped. A poll of a `tokio` timer
    /// holds it shared and reads `true` only before shutdown; shutdown writes
    /// `false` holding it exclusively, so no timer poll is between its check
    /// and the `tokio` call while the driver stops. `tokio` panics on a timer
    /// polled after its driver stopped.
    timers: RwLock<bool>,
}

impl OsHost {
    /// Starts the runtime with `worker_threads` worker threads, or one per CPU
    /// when it is 0.
    ///
    /// # Errors
    ///
    /// [`BuildError::Failed`] when the operating system refuses the threads or
    /// the I/O driver.
    pub fn start(worker_threads: u16) -> Result<OsHost, BuildError> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_io().enable_time().thread_name("alloy-rt");
        if worker_threads > 0 {
            builder.worker_threads(usize::from(worker_threads));
        }
        let runtime = builder.build().map_err(|e| {
            BuildError::Failed(Detail::new(&format!(
                "the tokio runtime did not start: {e}"
            )))
        })?;
        Ok(OsHost(Arc::new(HostInner {
            handle: runtime.handle().clone(),
            runtime: Mutex::new(Some(runtime)),
            shutting_down: AtomicBool::new(false),
            timers: RwLock::new(true),
        })))
    }

    /// Runs `future` to completion on the calling thread, blocking it, with
    /// this host's timers and sockets available to the future.
    ///
    /// # Panics
    ///
    /// When called from a task of any `tokio` runtime, this host's included:
    /// `tokio` does not let a task block its worker. After
    /// [`OsHost::shutdown`] the future still runs, but this host's timers no
    /// longer fire and its sockets fail.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.0.handle.block_on(future)
    }

    /// Cancels every task, then stops the runtime, waiting at most `grace` for
    /// tasks and blocking file operations to be dropped. From then on `spawn`
    /// of every `os` instance built on this host returns
    /// [`SpawnError::ShuttingDown`](crate::SpawnError::ShuttingDown), sleeps
    /// never complete, and socket and file operations fail. Later calls do
    /// nothing.
    ///
    /// Called from a task, it cannot wait, and returns at once.
    pub fn shutdown(&self, grace: Duration) {
        self.0.shutting_down.store(true, Ordering::SeqCst);
        self.0.close_timers();
        let runtime = self
            .0
            .runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(runtime) = runtime {
            stop(runtime, grace);
        }
    }

    fn host(&self) -> Host {
        Host(Arc::clone(&self.0))
    }
}

/// Stops `runtime`. Inside a runtime's context `tokio` forbids waiting, so the
/// runtime is then stopped in the background.
fn stop(runtime: Runtime, grace: Duration) {
    if Handle::try_current().is_ok() {
        runtime.shutdown_background();
    } else {
        runtime.shutdown_timeout(grace);
    }
}

impl HostInner {
    fn is_shut(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// Waits for every timer poll in progress to end, and makes every later
    /// one see the host shut.
    fn close_timers(&self) {
        *self.timers.write().unwrap_or_else(PoisonError::into_inner) = false;
    }
}

impl Drop for HostInner {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        *self
            .timers
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = false;
        let runtime = self
            .runtime
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(runtime) = runtime {
            stop(runtime, Duration::ZERO);
        }
    }
}

/// A seam instance's reference to its host.
#[derive(Clone)]
struct Host(Arc<HostInner>);

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("shutting_down", &self.is_shut())
            .finish()
    }
}

impl Host {
    fn handle(&self) -> &Handle {
        &self.0.handle
    }

    fn is_shut(&self) -> bool {
        self.0.is_shut()
    }

    /// `future`, entering the runtime's context on every poll, so that
    /// `tokio` sockets and timers work whichever thread polls it.
    fn run<'a, T>(&self, future: impl Future<Output = T> + Send + 'a) -> BoxFuture<'a, T> {
        let handle = self.0.handle.clone();
        let mut future = Box::pin(future);
        Box::pin(poll_fn(move |cx| {
            let _context = handle.enter();
            future.as_mut().poll(cx)
        }))
    }

    /// Completes once `deadline` has passed on the runtime's timer. Never
    /// completes once the host is shutting down: `tokio` panics on a timer
    /// polled after its driver stopped, and this keeps every sleep from
    /// reaching that, from any thread, also while the host shuts down.
    fn sleep_until(&self, deadline: std::time::Instant) -> SleepUntil {
        SleepUntil {
            sleep: None,
            host: self.clone(),
            deadline: tokio::time::Instant::from_std(deadline),
        }
    }

    /// Runs `work` on the runtime's blocking pool.
    ///
    /// # Errors
    ///
    /// [`Stopped`] when the host stopped before `work` ran to its end.
    fn blocking<T, F>(&self, work: F) -> impl Future<Output = Result<T, Stopped>> + Send + use<T, F>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let task = self.0.handle.spawn_blocking(work);
        async move { task.await.map_err(|_| Stopped) }
    }
}

/// The host stopped before blocking work ran to its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stopped;

/// The future of [`Host::sleep_until`].
struct SleepUntil {
    /// Created on the first poll that needs it, inside the runtime's context.
    /// Declared first so that it is dropped before `host`, which may hold the
    /// last reference to the runtime.
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    host: Host,
    deadline: tokio::time::Instant,
}

impl fmt::Debug for SleepUntil {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SleepUntil")
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

impl Future for SleepUntil {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<()> {
        let SleepUntil {
            sleep,
            host,
            deadline,
        } = &mut *self;
        // Held until the `tokio` poll returns, so the driver cannot stop in
        // between (see `HostInner::timers`).
        let open = host.0.timers.read().unwrap_or_else(PoisonError::into_inner);
        if !*open {
            return Poll::Pending;
        }
        let _context = host.handle().enter();
        let deadline = *deadline;
        let sleep = sleep.get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));
        sleep.as_mut().poll(cx)
    }
}

/// One place in a per-instance count of open resources: UDP sockets, streams
/// and listeners, or disk handles. Dropping it frees the place.
#[derive(Debug)]
struct Permit(Arc<AtomicUsize>);

impl Permit {
    /// Takes a place when fewer than `max` are taken.
    fn take(count: &Arc<AtomicUsize>, max: usize) -> Option<Permit> {
        count
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| Permit(Arc::clone(count)))
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Registers `os` for the five runtime kinds, `clock`, `net`, `disk-io`,
/// `rng`, and `spawn`, and for `host` (ARFC 0005 section 2.8), with
/// factories that build instances on `host` (ARFC 0005 clause 3.1.14).
/// [`Runtime::from_registry`](crate::Runtime::from_registry) builds no
/// `host` instance.
///
/// Each factory accepts configuration version 1 only (ARFC 0005 section 4.2):
/// `message Empty {}` for `clock`, `net`, `rng`, `spawn`, and `host`, and
/// `message OsDiskIoConfig { string root = 1; }` for `disk-io`, ignoring
/// unknown fields. `root` must be an absolute native path of an existing
/// directory; an empty `root` means `/` on Unix platforms and is rejected
/// elsewhere.
///
/// # Errors
///
/// As [`RegistryBuilder::register`], for the first registration it refuses.
/// The registrations made before it stay in `builder`.
pub fn register_os(builder: &mut RegistryBuilder, host: &OsHost) -> Result<(), RegistryError> {
    let name = || {
        // "os" is three lowercase ASCII letters, inside every rule of an
        // implementation name (ARFC 0001 section 2.1).
        ImplName::new("os").expect("`os` is a valid implementation name")
    };

    let h = host.host();
    let clock: Factory<ClockSeam> = Arc::new(move |(): &(), value: &ConfigValue| {
        decode::<config::Empty>(value)?;
        Ok(Arc::new(clock::OsClock::new(h.clone())))
    });
    builder.register::<ClockSeam>(name(), clock)?;

    let rng: Factory<RngSeam> = Arc::new(|(): &(), value: &ConfigValue| {
        decode::<config::Empty>(value)?;
        Ok(Arc::new(rng::OsRng))
    });
    builder.register::<RngSeam>(name(), rng)?;

    let h = host.host();
    let spawn: Factory<SpawnSeam> = Arc::new(move |(): &(), value: &ConfigValue| {
        decode::<config::Empty>(value)?;
        Ok(Arc::new(spawn::OsSpawn::new(h.clone())))
    });
    builder.register::<SpawnSeam>(name(), spawn)?;

    let h = host.host();
    let net: Factory<NetSeam> = Arc::new(move |(): &(), value: &ConfigValue| {
        decode::<config::Empty>(value)?;
        Ok(Arc::new(net::OsNet::new(h.clone())))
    });
    builder.register::<NetSeam>(name(), net)?;

    let h = host.host();
    let disk_io: Factory<DiskIoSeam> = Arc::new(move |(): &(), value: &ConfigValue| {
        let config = decode::<config::OsDiskIoConfig>(value)?;
        let root = config::resolve_root(&config.root).map_err(BuildError::Config)?;
        Ok(Arc::new(disk::OsDiskIo::new(h.clone(), root)))
    });
    builder.register::<DiskIoSeam>(name(), disk_io)?;

    let h = host.host();
    let machine: Factory<HostSeam> = Arc::new(move |(): &(), value: &ConfigValue| {
        decode::<config::Empty>(value)?;
        Ok(Arc::new(host::OsMachine::new(h.clone())))
    });
    builder.register::<HostSeam>(name(), machine)?;
    Ok(())
}

fn decode<C: SeamConfig>(value: &ConfigValue) -> Result<C, BuildError> {
    C::from_value(value).map_err(BuildError::Config)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use alloy_seam::Registry;

    use super::*;
    use crate::{Runtime as RtRuntime, RuntimeConfig, SpawnError, TaskName};

    pub(super) fn host() -> OsHost {
        OsHost::start(2).unwrap()
    }

    pub(super) fn registry(host: &OsHost) -> Registry {
        let mut builder = RegistryBuilder::new();
        register_os(&mut builder, host).unwrap();
        builder.build()
    }

    #[test]
    fn register_os_registers_the_five_kinds_and_host() {
        let host = host();
        let entries: Vec<String> = registry(&host)
            .entries()
            .into_iter()
            .map(|(kind, name, _)| format!("{kind}/{name}"))
            .collect();
        assert_eq!(
            entries,
            [
                "clock/os",
                "disk-io/os",
                "host/os",
                "net/os",
                "rng/os",
                "spawn/os"
            ]
        );
        let mut again = RegistryBuilder::new();
        register_os(&mut again, &host).unwrap();
        assert!(matches!(
            register_os(&mut again, &host),
            Err(RegistryError::Duplicate { .. })
        ));
        host.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn from_registry_builds_an_os_runtime() {
        let host = host();
        let config = RuntimeConfig::all(ImplName::new("os").unwrap());
        let runtime = RtRuntime::from_registry(&registry(&host), &config);
        if cfg!(unix) {
            // An empty `root` means `/` on Unix platforms.
            let runtime = runtime.unwrap();
            let name = TaskName::new("answer").unwrap();
            let out = host.block_on(runtime.spawn(name, async { 42 }).unwrap().join());
            assert_eq!(out, Ok(42));
        } else {
            assert!(matches!(
                runtime.map(|_| ()),
                Err(BuildError::Config(alloy_seam::ConfigError::Invalid { .. }))
            ));
        }
        host.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn shutdown_stops_spawning_and_sleeping() {
        let host = host();
        let registry = registry(&host);
        let empty = ConfigValue::defaults(1).unwrap();
        let os = ImplName::new("os").unwrap();
        let spawn = registry.instantiate::<SpawnSeam>(&(), &os, &empty).unwrap();
        let clock = registry.instantiate::<ClockSeam>(&(), &os, &empty).unwrap();
        let polls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&polls);
        let name = TaskName::new("forever").unwrap();
        let handle = spawn
            .spawn(
                name.clone(),
                Box::pin(poll_fn(move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Poll::<()>::Pending
                })),
            )
            .unwrap();
        let mut sleep = clock.sleep_until(clock.now().saturating_add(Duration::from_secs(3600)));
        host.shutdown(Duration::from_secs(1));
        host.shutdown(Duration::from_secs(1));
        assert_eq!(
            spawn.spawn(name, Box::pin(async {})).map(|_| ()),
            Err(SpawnError::ShuttingDown)
        );
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        assert!(sleep.as_mut().poll(&mut cx).is_pending());
        let mut join = handle.join();
        assert_eq!(
            join.as_mut().poll(&mut cx),
            Poll::Ready(Err(crate::JoinError::Cancelled))
        );
        assert!(format!("{host:?}").contains("shutting_down: true"));
    }

    #[test]
    fn sleeps_polled_while_the_host_shuts_down_never_panic() {
        // Each round races four pollers against the shutdown. Without the
        // gate in `SleepUntil::poll`, a poll panics inside `tokio` within the
        // first 50 rounds or so, so 500 rounds find it.
        for _ in 0..500 {
            let host = host();
            let registry = registry(&host);
            let empty = ConfigValue::defaults(1).unwrap();
            let os = ImplName::new("os").unwrap();
            let clock = registry.instantiate::<ClockSeam>(&(), &os, &empty).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let started = Arc::new(AtomicU32::new(0));
            let pollers: Vec<_> = (0..4)
                .map(|_| {
                    let clock = Arc::clone(&clock);
                    let stop = Arc::clone(&stop);
                    let started = Arc::clone(&started);
                    std::thread::spawn(move || {
                        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
                        let mut rounds = 0_u32;
                        while !stop.load(Ordering::SeqCst) || rounds == 0 {
                            let deadline = clock.now().saturating_add(Duration::from_secs(3600));
                            let mut sleep = clock.sleep_until(deadline);
                            assert!(sleep.as_mut().poll(&mut cx).is_pending());
                            assert!(sleep.as_mut().poll(&mut cx).is_pending());
                            drop(sleep);
                            if rounds == 0 {
                                started.fetch_add(1, Ordering::SeqCst);
                            }
                            rounds += 1;
                        }
                    })
                })
                .collect();
            while started.load(Ordering::SeqCst) < 4 {
                std::thread::yield_now();
            }
            host.shutdown(Duration::from_millis(100));
            stop.store(true, Ordering::SeqCst);
            for poller in pollers {
                assert!(poller.join().is_ok(), "a poll panicked");
            }
        }
    }

    #[test]
    fn dropping_the_last_clone_inside_a_task_does_not_panic() {
        let outer = host();
        let inner = host();
        let moved = inner.clone();
        drop(inner);
        outer.block_on(async move {
            drop(moved);
        });
        let inner = host();
        let in_task = inner.clone();
        drop(inner);
        let registry = registry(&outer);
        let empty = ConfigValue::defaults(1).unwrap();
        let os = ImplName::new("os").unwrap();
        let spawn = registry.instantiate::<SpawnSeam>(&(), &os, &empty).unwrap();
        let name = TaskName::new("drop-host").unwrap();
        let handle = spawn
            .spawn(name, Box::pin(async move { drop(in_task) }))
            .unwrap();
        assert_eq!(outer.block_on(handle.join()), Ok(()));
        outer.shutdown(Duration::from_secs(1));
    }

    #[test]
    fn factories_reject_other_versions_and_malformed_payloads() {
        let host = host();
        let registry = registry(&host);
        let os = ImplName::new("os").unwrap();
        let v2 = ConfigValue::defaults(2).unwrap();
        let unsupported = alloy_seam::ConfigError::UnsupportedVersion {
            version: 2,
            min: 1,
            max: 1,
        };
        assert_eq!(
            registry.instantiate::<RngSeam>(&(), &os, &v2).map(|_| ()),
            Err(BuildError::Config(unsupported.clone()))
        );
        assert_eq!(
            registry
                .instantiate::<DiskIoSeam>(&(), &os, &v2)
                .map(|_| ()),
            Err(BuildError::Config(unsupported))
        );
        let truncated = ConfigValue::new(1, vec![0x08]).unwrap();
        assert!(matches!(
            registry
                .instantiate::<ClockSeam>(&(), &os, &truncated)
                .map(|_| ()),
            Err(BuildError::Config(
                alloy_seam::ConfigError::Malformed { .. }
            ))
        ));
        host.shutdown(Duration::from_secs(1));
    }
}
