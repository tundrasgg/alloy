//! Hosts and their `mem` runtime: the clock of clause 3.1.4, the random
//! streams of clause 3.1.5, the tasks of clause 3.1.6, a `net` instance that
//! lists `lo`, and a `disk-io` instance; the starts, crashes, and stalls of
//! clauses 3.3.4 to 3.3.6.

use std::fmt;
use std::future::{Future, poll_fn, ready};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use alloy_rt::{
    BoxFuture, Clock, ClockSeam, DirEntry, DiskError, DiskFile, DiskIo, DiskIoSeam, Instant,
    Interface, InterfaceAddr, InterfaceList, JoinError, ListenOptions, Listener, Metadata, Net,
    NetError, NetSeam, OpenOptions, Rng, RngError, RngSeam, RtPath, Runtime, Spawn, SpawnError,
    SpawnSeam, Stream as RtStream, StreamAddr, StreamOptions, TaskHandle, TaskId, TaskName,
    UdpOptions, UdpSocket, WallTime,
};
use alloy_seam::{BuildError, ConfigError, ConfigValue, Detail, ImplName, RegistryBuilder};

use crate::core::{After, BootJob, Core, End, Event, HostTimer, MAX_NANOS, Shared};
use crate::exec::{Joint, Owner, TaskKey, lock};
use crate::stream::Stream;
use crate::trace::{KIND_HOST_CRASH, KIND_HOST_STALL, KIND_HOST_START, KIND_RNG, KIND_TASK_SPAWN};
use crate::world::HostId;

/// Hosts per world (section 7).
pub(crate) const MAX_HOSTS: usize = 10_000;
/// The largest clock rate offset, in parts per million (section 7).
pub(crate) const MAX_PPM: i32 = 500_000;
/// Live tasks per `spawn` instance (section 7).
const MAX_LIVE_TASKS: usize = 1_048_576;
/// The generation of the interface list of a host with only `lo`, which
/// never changes. T-0077 gives hosts interfaces whose lists change.
const GENERATION: u64 = 1;

/// Builds the host's software at each start. Its future runs as the host's
/// first task of that start, named `boot`.
pub type Boot = Arc<dyn Fn(HostBoot) -> BoxFuture<'static, ()> + Send + Sync>;

/// What a host's `boot` closure gets at each start.
pub struct HostBoot {
    /// The host.
    pub host: HostId,
    /// 1 at the first start, plus 1 at each restart.
    pub start: u32,
    /// The five `mem` instances of this start.
    pub runtime: Runtime,
    /// Holds `mem` for the five runtime kinds, bound to this host and this
    /// start.
    pub registry: RegistryBuilder,
}

impl fmt::Debug for HostBoot {
    /// The host and the start only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostBoot")
            .field("host", &self.host)
            .field("start", &self.start)
            .finish_non_exhaustive()
    }
}

/// How a host is built.
pub struct HostConfig {
    /// Builds the host's software at each start.
    pub boot: Boot,
    /// The clock rate offset at creation, in parts per million. -500,000 to
    /// 500,000. Default 0.
    pub clock_ppm: i32,
    /// Nanoseconds added to `RunConfig::wall_start` for this host. Default 0.
    pub wall_offset: i64,
}

impl HostConfig {
    /// `boot`, with every other field at its default.
    pub fn new(boot: Boot) -> HostConfig {
        HostConfig {
            boot,
            clock_ppm: 0,
            wall_offset: 0,
        }
    }
}

impl fmt::Debug for HostConfig {
    /// Every field but `boot`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostConfig")
            .field("clock_ppm", &self.clock_ppm)
            .field("wall_offset", &self.wall_offset)
            .finish_non_exhaustive()
    }
}

/// A host's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostState {
    /// Its tasks are polled.
    Running,
    /// A stall fault is in force: its tasks are not polled (clause 3.3.6).
    Stalled,
    /// It has crashed and has not restarted.
    Crashed,
}

/// `dw` world nanoseconds at the rate offset `ppm`, rounded down, in 128-bit
/// arithmetic (clause 3.1.4).
fn scaled(dw: u64, ppm: i32) -> u128 {
    // The rate `1,000,000 + ppm` is 500,000 to 1,500,000, so it is positive.
    let rate = (1_000_000 + i64::from(ppm)).unsigned_abs();
    u128::from(dw) * u128::from(rate) / 1_000_000
}

/// The clock of one start: host time `h0` at world time `w0`.
#[derive(Clone, Copy, Debug)]
struct Line {
    w0: u64,
    h0: u64,
}

/// A host's clocks: the rate offset, one monotonic line per start, and the
/// wall clock of clause 3.3.5.
#[derive(Debug)]
pub(crate) struct HostClock {
    ppm: i32,
    lines: Vec<Line>,
    /// The rate clock `r` of clause 3.3.5: `r0` at world time `rw0`.
    rw0: u64,
    r0: u128,
    /// `RunConfig::wall_start` plus the host's `wall_offset`.
    base: i128,
    /// The sum of the wall steps applied.
    steps: i128,
}

impl HostClock {
    /// Host time of start `start` at world time `w`, saturating at
    /// `Instant::MAX`.
    fn time(&self, start: u32, w: u64) -> u64 {
        let Some(line) = self.lines.get(start.saturating_sub(1) as usize) else {
            return 0;
        };
        let time = u128::from(line.h0) + scaled(w.saturating_sub(line.w0), self.ppm);
        u64::try_from(time.min(u128::from(MAX_NANOS))).unwrap_or(MAX_NANOS)
    }

    /// The first world time at which start `start` reads `deadline` or
    /// more, under the current rate. `None` when that is never.
    pub(crate) fn fire_at(&self, start: u32, deadline: u64) -> Option<u64> {
        let line = self.lines.get(start.saturating_sub(1) as usize)?;
        if deadline >= MAX_NANOS {
            return None;
        }
        let Some(left) = deadline.checked_sub(line.h0).filter(|left| *left > 0) else {
            return Some(line.w0);
        };
        let rate = u128::from((1_000_000 + i64::from(self.ppm)).unsigned_abs());
        let dw = (u128::from(left) * 1_000_000).div_ceil(rate);
        let at = u128::from(line.w0) + dw;
        u64::try_from(at).ok().filter(|at| *at <= MAX_NANOS)
    }

    /// The wall clock at world time `w`, saturating at the bounds of `i64`.
    fn wall(&self, w: u64) -> i64 {
        let r = self.r0 + scaled(w.saturating_sub(self.rw0), self.ppm);
        let r = i128::try_from(r).unwrap_or(i128::MAX);
        let wall = self.base.saturating_add(self.steps).saturating_add(r);
        i64::try_from(wall.clamp(i128::from(i64::MIN), i128::from(i64::MAX))).unwrap_or(0)
    }

    /// Sets the rate offset from world time `w` on.
    fn set_rate(&mut self, w: u64, ppm: i32) {
        for start in 1..=self.lines.len() {
            let h0 = self.time(start as u32, w);
            self.lines[start - 1] = Line { w0: w, h0 };
        }
        self.r0 += scaled(w.saturating_sub(self.rw0), self.ppm);
        self.rw0 = w;
        self.ppm = ppm;
    }
}

/// One host of a world.
pub(crate) struct Host {
    pub(crate) boot: Boot,
    /// The last start, 0 before the first.
    pub(crate) start: u32,
    pub(crate) crashed: bool,
    /// A crash applied during a poll of one of the host's tasks, which takes
    /// effect when that poll returns (clause 3.3.4).
    pub(crate) pending_crash: bool,
    /// An armed crash: the polls left, and the number of the last poll that
    /// started before it was armed.
    pub(crate) armed: Option<(u32, u64)>,
    /// The world time the stalls in force end at.
    pub(crate) stall_until: u64,
    /// The key of the event at `stall_until`, while a stall is in force.
    pub(crate) stall_event: Option<(u64, u64)>,
    /// Woken tasks waiting for the stall to end, in wake order.
    pub(crate) parked: Vec<TaskKey>,
    /// The next task number.
    next_task: u64,
    /// The runtime of the current start, while it runs.
    pub(crate) runtime: Option<Runtime>,
    pub(crate) clock: Arc<Mutex<HostClock>>,
    /// Per start: the `rng` instances built, and the start whose streams
    /// they draw. That source is never itself a replay: a replay of a
    /// replay draws the streams of the first start of the chain.
    rng: Vec<(u64, u32)>,
}

impl Host {
    /// A host created at world time `now`, not yet started.
    pub(crate) fn new(config: HostConfig, now: u64, wall_start: i64) -> Host {
        Host {
            boot: config.boot,
            start: 0,
            crashed: true,
            pending_crash: false,
            armed: None,
            stall_until: 0,
            stall_event: None,
            parked: Vec::new(),
            next_task: 1,
            runtime: None,
            clock: Arc::new(Mutex::new(HostClock {
                ppm: config.clock_ppm,
                lines: Vec::new(),
                rw0: now,
                r0: u128::from(now),
                base: i128::from(wall_start) + i128::from(config.wall_offset),
                steps: 0,
            })),
            rng: Vec::new(),
        }
    }

    pub(crate) fn stalled(&self, now: u64) -> bool {
        !self.crashed && self.stall_until > now
    }

    pub(crate) fn state(&self, now: u64) -> HostState {
        if self.crashed {
            HostState::Crashed
        } else if self.stalled(now) {
            HostState::Stalled
        } else {
            HostState::Running
        }
    }

    /// Whether objects of start `start` still work.
    fn live(&self, start: u32) -> bool {
        self.start == start && !self.crashed
    }

    /// Sets the clock rate offset at world time `now`.
    pub(crate) fn set_rate(&mut self, now: u64, ppm: i32) {
        lock(&self.clock).set_rate(now, ppm);
    }

    /// Adds `step_ns` to the wall clock and returns the rate offset.
    pub(crate) fn step(&mut self, step_ns: i64) -> i32 {
        let mut clock = lock(&self.clock);
        clock.steps = clock.steps.saturating_add(i128::from(step_ns));
        clock.ppm
    }
}

impl Core {
    /// Starts the host `host` at the current world time (clauses 3.3.4 and
    /// 4.1): writes `host-start` and the `task-spawn` of `boot`, builds the
    /// five `mem` instances, and leaves the call of `boot` to `after`. With
    /// `replay`, the `rng` instances draw the streams of that start.
    pub(crate) fn start(
        &mut self,
        shared: &Arc<Shared>,
        host: HostId,
        replay: Option<u32>,
        after: &mut After,
    ) {
        let now = shared.now_nanos();
        let Ok(entry) = self.host_mut(host) else {
            return;
        };
        entry.start += 1;
        let start = entry.start;
        entry.crashed = false;
        // Start `s` drew the streams of its own source, so a replay of `s`
        // draws those too (clause 3.3.4).
        let source = replay
            .and_then(|s| entry.rng.get(s.saturating_sub(1) as usize))
            .map_or(start, |(_, source)| *source);
        entry.rng.push((0, source));
        lock(&entry.clock).lines.push(Line { w0: now, h0: 0 });
        let id = TaskId(entry.next_task);
        entry.next_task += 1;
        let boot = Arc::clone(&entry.boot);
        let clock = Arc::clone(&entry.clock);
        shared.push(Some(host), KIND_HOST_START, &[]);
        let key = self.exec.insert(Owner::Host { host, id }, None, None);
        shared.push(Some(host), KIND_TASK_SPAWN, &spawn_body(id, "boot"));
        let at = Place {
            shared: Arc::clone(shared),
            host,
            start,
        };
        let rng = self.new_rng(&at);
        let runtime = Runtime::new(
            Arc::new(MemClock {
                at: at.clone(),
                clock: Arc::clone(&clock),
            }),
            Arc::new(MemNet { at: at.clone() }),
            Arc::new(MemDiskIo { at: at.clone() }),
            Arc::new(rng),
            Arc::new(MemSpawn::new(at.clone())),
        );
        if let Ok(entry) = self.host_mut(host) {
            entry.runtime = Some(runtime.clone());
        }
        after.boots.push(BootJob {
            key,
            boot,
            arg: HostBoot {
                host,
                start,
                runtime,
                registry: registry(&at, &clock),
            },
        });
    }

    /// Crashes `host` at once (clause 3.3.4): writes `host-crash`, ends every
    /// task of the host in ascending task number, and ends its stalls and
    /// any armed or pending crash.
    pub(crate) fn crash(&mut self, shared: &Shared, host: HostId, after: &mut After) {
        shared.push(Some(host), KIND_HOST_CRASH, &[]);
        let mut doomed: Vec<(TaskId, TaskKey)> = self
            .exec
            .keys()
            .into_iter()
            .filter_map(|key| match self.exec.task(key)?.owner {
                Owner::Host { host: owner, id } if owner == host => Some((id, key)),
                _ => None,
            })
            .collect();
        doomed.sort_unstable();
        for (_, key) in doomed {
            self.end_task(shared, key, End::Crashed, after);
        }
        let mut stall_event = None;
        if let Ok(entry) = self.host_mut(host) {
            entry.crashed = true;
            entry.pending_crash = false;
            entry.armed = None;
            entry.stall_until = 0;
            stall_event = entry.stall_event.take();
            entry.parked.clear();
            after.runtimes.extend(entry.runtime.take());
        }
        // The stalls ended here, so their end is no longer pending
        // (clause 3.3.6).
        if let Some(key) = stall_event {
            self.remove_event(key);
        }
    }

    /// Stalls `host` for `length` from now, or until the end of a stall in
    /// force when that is later (clause 3.3.6).
    pub(crate) fn stall(&mut self, shared: &Shared, host: HostId, length: Duration) {
        let now = shared.now_nanos();
        let nanos = u64::try_from(length.as_nanos()).unwrap_or(u64::MAX);
        shared.push(Some(host), KIND_HOST_STALL, &nanos.to_le_bytes());
        let Ok(entry) = self.host(host) else {
            return;
        };
        let end = now.saturating_add(nanos).min(MAX_NANOS);
        if end <= now || end <= entry.stall_until {
            return;
        }
        let parked = self.exec.take_runnable(
            |task| matches!(task.owner, Owner::Host { host: owner, .. } if owner == host),
        );
        let event = self.add_event(end, Event::StallEnd(host));
        let mut replaced = None;
        if let Ok(entry) = self.host_mut(host) {
            entry.stall_until = end;
            entry.parked.extend(parked);
            replaced = entry.stall_event.replace(event);
        }
        // Only the latest end of the stalls in force stays pending.
        if let Some(key) = replaced {
            self.remove_event(key);
        }
    }

    /// After a poll of a task of `host` numbered `number`: a pending crash
    /// takes effect, and an armed crash counts the poll.
    pub(crate) fn after_host_poll(
        &mut self,
        shared: &Shared,
        host: HostId,
        number: u64,
        after: &mut After,
    ) {
        let Ok(entry) = self.host_mut(host) else {
            return;
        };
        let crash = if entry.pending_crash {
            true
        } else if let Some((left, since)) = entry.armed
            && number > since
        {
            entry.armed = (left > 1).then(|| (left - 1, since));
            left <= 1
        } else {
            false
        };
        if crash {
            self.crash(shared, host, after);
        }
    }

    /// A new `rng` instance of the start at `at`.
    fn new_rng(&mut self, at: &Place) -> MemRng {
        let (index, source) = match self
            .hosts
            .get_mut(at.host.0 as usize)
            .and_then(|host| host.rng.get_mut(at.start.saturating_sub(1) as usize))
        {
            Some((next, source)) => {
                *next += 1;
                (*next - 1, *source)
            }
            None => (0, at.start),
        };
        let stream = Stream::new(
            at.shared.seed(),
            b"rng",
            &[u64::from(at.host.0), u64::from(source), index],
        );
        MemRng {
            at: at.clone(),
            state: Mutex::new((stream, [0; 8], 0)),
        }
    }
}

/// The body of a `task-spawn` record.
fn spawn_body(id: TaskId, name: &str) -> Vec<u8> {
    let mut body = id.0.to_le_bytes().to_vec();
    // A task name is at most 64 bytes.
    body.push(name.len() as u8);
    body.extend_from_slice(name.as_bytes());
    body
}

/// The host and start a `mem` instance belongs to.
#[derive(Clone)]
struct Place {
    shared: Arc<Shared>,
    host: HostId,
    start: u32,
}

impl fmt::Debug for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Place")
            .field("host", &self.host.0)
            .field("start", &self.start)
            .finish()
    }
}

impl Place {
    /// Whether the start is still running and the run has not ended.
    fn live(&self) -> bool {
        !self.shared.has_ended()
            && lock(&self.shared.core)
                .host(self.host)
                .is_ok_and(|host| host.live(self.start))
    }
}

/// The `mem` clock of one start (clause 3.1.4).
#[derive(Debug)]
struct MemClock {
    at: Place,
    clock: Arc<Mutex<HostClock>>,
}

impl Clock for MemClock {
    fn now(&self) -> Instant {
        let now = self.at.shared.now_nanos();
        Instant::from_nanos(lock(&self.clock).time(self.at.start, now))
    }

    fn wall(&self) -> WallTime {
        WallTime(lock(&self.clock).wall(self.at.shared.now_nanos()))
    }

    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()> {
        Box::pin(HostSleep {
            at: self.at.clone(),
            clock: Arc::clone(&self.clock),
            deadline: deadline.as_nanos(),
            timer: None,
        })
    }
}

/// A sleep on a host's clock. It holds a timer from its first pending poll
/// until it completes or is dropped.
struct HostSleep {
    at: Place,
    clock: Arc<Mutex<HostClock>>,
    deadline: u64,
    timer: Option<u64>,
}

impl Future for HostSleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let shared = Arc::clone(&self.at.shared);
        // The waker is cloned, and what is left of it dropped, with no lock
        // held (module `core`).
        let waker = cx.waker().clone();
        let mut core = lock(&shared.core);
        let (now, fire) = {
            let clock = lock(&self.clock);
            let now = clock.time(self.at.start, shared.now_nanos());
            (now, clock.fire_at(self.at.start, self.deadline))
        };
        if now >= self.deadline {
            drop(core);
            return Poll::Ready(());
        }
        let waker = match self.timer {
            Some(id) => core.touch_timer(id, waker),
            None => Err(waker),
        };
        let left = match waker {
            Ok(left) => Some(left),
            Err(waker) if shared.has_ended() => Some(waker),
            Err(waker) => {
                let timer = HostTimer {
                    host: self.at.host,
                    start: self.at.start,
                    deadline: self.deadline,
                    task: core.polling,
                };
                self.timer = Some(core.add_timer(fire, waker, Some(timer)));
                None
            }
        };
        drop(core);
        drop(left);
        Poll::Pending
    }
}

impl Drop for HostSleep {
    fn drop(&mut self) {
        if let Some(id) = self.timer {
            let waker = lock(&self.at.shared.core).remove_timer(id);
            drop(waker);
        }
    }
}

/// The `mem` random generator of one instance (clause 3.1.5): its stream,
/// and the bytes of the last value it has not returned yet.
#[derive(Debug)]
struct MemRng {
    at: Place,
    state: Mutex<(Stream, [u8; 8], usize)>,
}

impl Rng for MemRng {
    fn fill(&self, dest: &mut [u8]) -> Result<(), RngError> {
        {
            let mut state = lock(&self.state);
            let (stream, spare, left) = &mut *state;
            for byte in dest.iter_mut() {
                if *left == 0 {
                    *spare = stream.next_u64().to_le_bytes();
                    *left = 8;
                }
                *byte = spare[8 - *left];
                *left -= 1;
            }
        }
        let len = u32::try_from(dest.len()).unwrap_or(u32::MAX);
        self.at
            .shared
            .push(Some(self.at.host), KIND_RNG, &len.to_le_bytes());
        self.at.shared.flush();
        Ok(())
    }
}

/// The `mem` task spawner of one instance (clause 3.1.6).
#[derive(Debug)]
struct MemSpawn {
    at: Place,
    live: Arc<AtomicUsize>,
}

impl MemSpawn {
    fn new(at: Place) -> MemSpawn {
        MemSpawn {
            at,
            live: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Spawn for MemSpawn {
    fn spawn(
        &self,
        name: TaskName,
        task: BoxFuture<'static, ()>,
    ) -> Result<Box<dyn TaskHandle>, SpawnError> {
        let shared = &self.at.shared;
        let spawned = {
            let mut core = lock(&shared.core);
            let id = match core.host_mut(self.at.host) {
                Ok(host) if host.live(self.at.start) && !shared.has_ended() => {
                    if self.live.load(Ordering::Acquire) >= MAX_LIVE_TASKS {
                        Err(SpawnError::Limit)
                    } else {
                        host.next_task += 1;
                        Ok(TaskId(host.next_task - 1))
                    }
                }
                _ => Err(SpawnError::ShuttingDown),
            };
            match id {
                Err(error) => Err((error, task)),
                Ok(id) => {
                    self.live.fetch_add(1, Ordering::AcqRel);
                    let joint = Arc::new(Joint::new(Arc::clone(&self.live)));
                    let owner = Owner::Host {
                        host: self.at.host,
                        id,
                    };
                    let key = core
                        .exec
                        .insert(owner, Some(task), Some(Arc::clone(&joint)));
                    shared.push(
                        Some(self.at.host),
                        KIND_TASK_SPAWN,
                        &spawn_body(id, name.as_str()),
                    );
                    Ok(MemTaskHandle {
                        shared: Arc::clone(shared),
                        key,
                        id,
                        joint,
                    })
                }
            }
        };
        shared.flush();
        match spawned {
            Ok(handle) => Ok(Box::new(handle)),
            Err((error, task)) => {
                crate::exec::drop_quietly(task);
                Err(error)
            }
        }
    }

    fn yield_now(&self) -> BoxFuture<'static, ()> {
        let mut yielded = false;
        Box::pin(poll_fn(move |cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }))
    }
}

/// Control of one `mem` task.
struct MemTaskHandle {
    shared: Arc<Shared>,
    key: TaskKey,
    id: TaskId,
    joint: Arc<Joint>,
}

impl fmt::Debug for MemTaskHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemTaskHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl TaskHandle for MemTaskHandle {
    fn id(&self) -> TaskId {
        self.id
    }

    fn cancel(&self) {
        let mut after = After::default();
        lock(&self.shared.core).cancel(&self.shared, self.key, &mut after);
        self.shared.settle(after);
    }

    fn join(self: Box<Self>) -> BoxFuture<'static, Result<(), JoinError>> {
        let joint = self.joint;
        Box::pin(poll_fn(move |cx| match joint.poll_end(cx.waker()) {
            Some(result) => Poll::Ready(result),
            None => Poll::Pending,
        }))
    }
}

/// The `mem` network of one start. Its host has only `lo` (clause 3.3.1),
/// and it opens no sockets: UDP and streams return `Unsupported`.
#[derive(Debug)]
struct MemNet {
    at: Place,
}

impl MemNet {
    /// `Closed` once the start has crashed, `Unsupported` otherwise.
    fn refuse(&self) -> NetError {
        if self.at.live() {
            NetError::Unsupported
        } else {
            NetError::Closed
        }
    }
}

/// The interface list of a host with only `lo` (clause 3.3.1).
fn loopback_only() -> InterfaceList {
    InterfaceList {
        generation: GENERATION,
        interfaces: vec![Interface {
            index: 1,
            name: "lo".to_owned(),
            mtu: 65_536,
            up: true,
            loopback: true,
            addrs: vec![
                InterfaceAddr {
                    ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                    prefix_len: 8,
                },
                InterfaceAddr {
                    ip: IpAddr::V6(Ipv6Addr::LOCALHOST),
                    prefix_len: 128,
                },
            ],
        }],
    }
}

impl Net for MemNet {
    /// The list of a host with only `lo` never changes, so a wait for a list
    /// newer than generation 1 does not complete.
    fn interfaces(
        &self,
        newer_than: Option<u64>,
    ) -> BoxFuture<'_, Result<InterfaceList, NetError>> {
        Box::pin(poll_fn(move |_| {
            if !self.at.live() {
                Poll::Ready(Err(NetError::Closed))
            } else if newer_than.is_some_and(|g| g >= GENERATION) {
                Poll::Pending
            } else {
                Poll::Ready(Ok(loopback_only()))
            }
        }))
    }

    fn bind_udp(
        &self,
        _local: SocketAddr,
        _options: UdpOptions,
    ) -> Result<Box<dyn UdpSocket>, NetError> {
        Err(self.refuse())
    }

    fn connect(
        &self,
        _remote: StreamAddr,
        _options: StreamOptions,
    ) -> BoxFuture<'_, Result<Box<dyn RtStream>, NetError>> {
        Box::pin(ready(Err(self.refuse())))
    }

    fn listen(
        &self,
        _local: StreamAddr,
        _options: ListenOptions,
    ) -> Result<Box<dyn Listener>, NetError> {
        Err(self.refuse())
    }
}

/// The `mem` disk of one start. It holds no namespace: every call returns
/// `Unsupported`, and `Io` once the start has crashed (clause 3.3.4).
/// T-0094 builds the namespace, files, and block devices.
#[derive(Debug)]
struct MemDiskIo {
    at: Place,
}

impl MemDiskIo {
    fn refuse<T: Send + 'static>(&self) -> BoxFuture<'static, Result<T, DiskError>> {
        let error = if self.at.live() {
            DiskError::Unsupported
        } else {
            DiskError::Io
        };
        Box::pin(ready(Err(error)))
    }
}

impl DiskIo for MemDiskIo {
    fn open<'a>(
        &'a self,
        _path: &'a RtPath,
        _options: OpenOptions,
    ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
        self.refuse()
    }

    fn metadata<'a>(&'a self, _path: &'a RtPath) -> BoxFuture<'a, Result<Metadata, DiskError>> {
        self.refuse()
    }

    fn list<'a>(&'a self, _dir: &'a RtPath) -> BoxFuture<'a, Result<Vec<DirEntry>, DiskError>> {
        self.refuse()
    }

    fn create_dir<'a>(&'a self, _path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.refuse()
    }

    fn remove<'a>(&'a self, _path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.refuse()
    }

    fn rename<'a>(
        &'a self,
        _from: &'a RtPath,
        _to: &'a RtPath,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        self.refuse()
    }

    fn sync_dir<'a>(&'a self, _dir: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.refuse()
    }
}

/// Checks a `mem` configuration: version 1 of `message Empty {}`, whose
/// fields are all unknown and ignored (section 4.2).
fn check_config(value: &ConfigValue) -> Result<(), BuildError> {
    if value.version() != 1 {
        return Err(BuildError::Config(ConfigError::UnsupportedVersion {
            version: value.version(),
            min: 1,
            max: 1,
        }));
    }
    skip_fields(value.payload()).map_err(|detail| {
        BuildError::Config(ConfigError::Malformed {
            detail: Detail::new(detail),
        })
    })
}

/// Walks a Protocol Buffers message whose every field is unknown.
fn skip_fields(mut bytes: &[u8]) -> Result<(), &'static str> {
    fn varint(bytes: &mut &[u8]) -> Result<u64, &'static str> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let (&byte, rest) = bytes.split_first().ok_or("truncated varint")?;
            *bytes = rest;
            value |= u64::from(byte & 0x7F).checked_shl(shift).unwrap_or(0);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("varint longer than 10 bytes")
    }
    fn take(bytes: &mut &[u8], len: u64) -> Result<(), &'static str> {
        let len = usize::try_from(len).map_err(|_| "field too long")?;
        *bytes = bytes.get(len..).ok_or("truncated field")?;
        Ok(())
    }
    while !bytes.is_empty() {
        let key = varint(&mut bytes)?;
        if key >> 3 == 0 {
            return Err("field number 0");
        }
        match key & 7 {
            0 => {
                varint(&mut bytes)?;
            }
            1 => take(&mut bytes, 8)?,
            2 => {
                let len = varint(&mut bytes)?;
                take(&mut bytes, len)?;
            }
            5 => take(&mut bytes, 4)?,
            _ => return Err("unsupported wire type"),
        }
    }
    Ok(())
}

/// A registry builder holding `mem` for the five runtime kinds, bound to
/// the start at `at`.
fn registry(at: &Place, clock: &Arc<Mutex<HostClock>>) -> RegistryBuilder {
    // "mem" is 3 lowercase letters, a valid implementation name.
    let mem = ImplName::new("mem").expect("mem is a valid implementation name");
    let mut builder = RegistryBuilder::new();
    let (place, clock) = (at.clone(), Arc::clone(clock));
    let clock_factory: alloy_seam::Factory<ClockSeam> = Arc::new(move |_, config| {
        check_config(config)?;
        Ok(Arc::new(MemClock {
            at: place.clone(),
            clock: Arc::clone(&clock),
        }))
    });
    let place = at.clone();
    let net_factory: alloy_seam::Factory<NetSeam> = Arc::new(move |_, config| {
        check_config(config)?;
        Ok(Arc::new(MemNet { at: place.clone() }))
    });
    let place = at.clone();
    let disk_factory: alloy_seam::Factory<DiskIoSeam> = Arc::new(move |_, config| {
        check_config(config)?;
        Ok(Arc::new(MemDiskIo { at: place.clone() }))
    });
    let place = at.clone();
    let rng_factory: alloy_seam::Factory<RngSeam> = Arc::new(move |_, config| {
        check_config(config)?;
        let rng = lock(&place.shared.core).new_rng(&place);
        Ok(Arc::new(rng))
    });
    let place = at.clone();
    let spawn_factory: alloy_seam::Factory<SpawnSeam> = Arc::new(move |_, config| {
        check_config(config)?;
        Ok(Arc::new(MemSpawn::new(place.clone())))
    });
    // A new builder holds no registration, and the five kinds are valid
    // and distinct, so none of these can be refused.
    let registered = builder
        .register::<ClockSeam>(mem.clone(), clock_factory)
        .and_then(|()| builder.register::<NetSeam>(mem.clone(), net_factory))
        .and_then(|()| builder.register::<DiskIoSeam>(mem.clone(), disk_factory))
        .and_then(|()| builder.register::<RngSeam>(mem.clone(), rng_factory))
        .and_then(|()| builder.register::<SpawnSeam>(mem, spawn_factory));
    debug_assert!(registered.is_ok(), "{registered:?}");
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(ppm: i32) -> HostClock {
        HostClock {
            ppm,
            lines: vec![Line { w0: 100, h0: 0 }],
            rw0: 100,
            r0: 100,
            base: 1_000,
            steps: 0,
        }
    }

    #[test]
    fn host_time_follows_the_rate_formula() {
        let c = clock(0);
        assert_eq!(c.time(1, 100), 0);
        assert_eq!(c.time(1, 350), 250);
        let fast = clock(500_000);
        assert_eq!(fast.time(1, 103), 4);
        let slow = clock(-500_000);
        assert_eq!(slow.time(1, 103), 1);
        assert_eq!(slow.time(1, u64::MAX), MAX_NANOS - 50);
        assert_eq!(fast.time(1, MAX_NANOS), MAX_NANOS);
        assert_eq!(c.time(2, 500), 0);
    }

    #[test]
    fn a_timer_fires_at_the_first_world_nanosecond_its_deadline_is_reached() {
        for ppm in [-500_000, -1, 0, 1, 333_333, 500_000] {
            let c = clock(ppm);
            for deadline in [1u64, 2, 3, 999, 1_000_000_007] {
                let at = c.fire_at(1, deadline).expect("reachable");
                assert!(c.time(1, at) >= deadline, "ppm {ppm} deadline {deadline}");
                assert!(
                    c.time(1, at - 1) < deadline,
                    "ppm {ppm} deadline {deadline}"
                );
            }
            assert_eq!(c.fire_at(1, 0), Some(100));
            assert_eq!(c.fire_at(1, MAX_NANOS), None);
        }
        assert_eq!(clock(-500_000).fire_at(1, MAX_NANOS - 1), None);
    }

    #[test]
    fn a_rate_change_keeps_every_clock_continuous() {
        let mut c = clock(0);
        c.lines.push(Line { w0: 200, h0: 0 });
        c.set_rate(300, 250_000);
        assert_eq!(c.time(1, 300), 200);
        assert_eq!(c.time(2, 300), 100);
        assert_eq!(c.time(1, 304), 205);
        assert_eq!(c.wall(300), 1_300);
        assert_eq!(c.wall(304), 1_305);
        c.steps = -2_000;
        assert_eq!(c.wall(304), -695);
        c.steps = i128::from(i64::MAX);
        assert_eq!(c.wall(304), i64::MAX);
    }

    #[test]
    fn config_payloads_are_walked_as_unknown_fields() {
        assert_eq!(skip_fields(&[]), Ok(()));
        // Field 1 varint 300, field 2 bytes "ab", field 3 fixed64, field 4
        // fixed32.
        let good = [
            0x08, 0xAC, 0x02, 0x12, 2, b'a', b'b', 0x19, 0, 0, 0, 0, 0, 0, 0, 0, 0x25, 0, 0, 0, 0,
        ];
        assert_eq!(skip_fields(&good), Ok(()));
        assert!(skip_fields(&[0x08]).is_err());
        assert!(skip_fields(&[0x12, 5, 1]).is_err());
        assert!(skip_fields(&[0x0B]).is_err());
        assert!(skip_fields(&[0x00, 0x00]).is_err());
        let two = ConfigValue::defaults(2).expect("version 2");
        assert_eq!(
            check_config(&two),
            Err(BuildError::Config(ConfigError::UnsupportedVersion {
                version: 2,
                min: 1,
                max: 1
            }))
        );
    }
}
