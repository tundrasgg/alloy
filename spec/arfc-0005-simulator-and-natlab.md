# ARFC 0005: Runtime, simulator, and NAT lab

- Status: accepted
- Depends on: 0001
- Crates: alloy-rt, alloy-sim, alloy-natlab
- Decisions: 0008, 0036, 0039, 0045

## 1. Purpose

All of Alloy runs on a small runtime interface. In production the operating system
implements it. In tests a deterministic simulator does, which lets whole clusters,
hostile networks, and years of faults run in seconds from a seed. This is built
before anything else.

## 2. Model

`alloy-rt` defines five seams:

| Seam | Trait | Gives |
|---|---|---|
| `clock` | `Clock` | Monotonic now, wall now, sleep until, timers |
| `net` | `Net` | Datagram and stream sockets, interface list, batch send and receive |
| `disk-io` | `DiskIo` | Open, read at, write at, sync, truncate, list, direct block access |
| `rng` | `Rng` | Random bytes |
| `spawn` | `Spawn` | Start a task, join it, cancel it |

A `Runtime` value bundles one instance of each. Every component takes a `Runtime`
at construction and uses nothing else to reach the world.

`alloy-sim` implements all five in memory, in one thread, driven by a seeded
scheduler and a virtual clock. A **world** holds hosts, links, routers, and NAT
devices. A **scenario** is a function that builds a world, runs Alloy code on its
hosts, injects faults, and asserts properties. `alloy-sim` is written in this
repository and depends on no simulation library (ADR 0036).

The **event trace** is the byte record of everything the simulator did in one run
(section 4.1). Its BLAKE3 hash is the **trace hash**.

### 2.1 Interface of `alloy-rt`

Sections 2.1 to 2.6 are the whole public interface of `alloy-rt`. Items marked
private are the developer's choice. `alloy-rt` depends on `alloy-seam` (ARFC 0001
section 2.1) and on no other Alloy crate. Every error type derives `Debug`,
`Clone`, `PartialEq`, and `Eq`, and implements `std::error::Error` and `Display`
through `thiserror`. Types that carry packet or disk bytes (`Datagram`, `RecvSlot`,
`IoBuf`) implement `Debug` by hand and print lengths only, never contents (I-22).

All asynchronous methods return `BoxFuture`, which is object safe and `Send`.
Network buffers are borrowed, so a receive writes into the caller's memory. Disk
buffers are owned and moved through each call, so the `os` implementation can hand
them to a blocking thread or to the kernel and the `mem` implementation can tear
them at a crash.

Common items and the clock:

```rust
pub use alloy_seam::BoxFuture;

use std::fmt;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

/// A point on one `Clock` instance's monotonic timeline, in nanoseconds
/// from an origin fixed when the instance is built. Values from different
/// instances are not comparable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(/* private: u64 */);

impl Instant {
    pub const ZERO: Instant;
    /// 2^63 - 1 nanoseconds.
    pub const MAX: Instant;
    /// Values above `MAX` give `MAX`.
    pub const fn from_nanos(nanos: u64) -> Instant;
    pub const fn as_nanos(self) -> u64;
    /// `None` when the sum is above `MAX`.
    pub fn checked_add(self, duration: Duration) -> Option<Instant>;
    /// `MAX` when the sum is above `MAX`.
    pub fn saturating_add(self, duration: Duration) -> Instant;
    /// Zero when `earlier` is later than `self`.
    pub fn saturating_duration_since(self, earlier: Instant) -> Duration;
}

/// Wall-clock time in nanoseconds since the Unix epoch, leap seconds ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WallTime(pub i64);

/// Seam `clock`.
pub trait Clock: Send + Sync + fmt::Debug + 'static {
    fn now(&self) -> Instant;
    fn wall(&self) -> WallTime;
    /// Completes once `now() >= deadline`. Dropping it removes the timer.
    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()>;
}

/// Seam `rng`.
pub trait Rng: Send + Sync + fmt::Debug + 'static {
    /// Fills all of `dest`, or returns an error and leaves `dest` unspecified.
    fn fill(&self, dest: &mut [u8]) -> Result<(), RngError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RngError {
    #[error("the platform random generator is unavailable")]
    Unavailable,
}
```

### 2.2 Tasks

```rust
/// 1 to 64 bytes, each from 0x21 to 0x7E.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskName(/* private */);

impl TaskName {
    pub fn new(name: &str) -> Result<TaskName, SpawnError>;
    pub fn as_str(&self) -> &str;
}

/// Never reused by one `Spawn` instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(pub u64);

/// Seam `spawn`.
pub trait Spawn: Send + Sync + fmt::Debug + 'static {
    fn spawn(
        &self,
        name: TaskName,
        task: BoxFuture<'static, ()>,
    ) -> Result<Box<dyn TaskHandle>, SpawnError>;
    /// Completes on its second poll, having woken its task on the first.
    fn yield_now(&self) -> BoxFuture<'static, ()>;
}

/// Control of one spawned task. Dropping it detaches the task.
pub trait TaskHandle: Send + Sync + fmt::Debug {
    fn id(&self) -> TaskId;
    /// Idempotent. Has no effect on a task that has ended.
    fn cancel(&self);
    fn join(self: Box<Self>) -> BoxFuture<'static, Result<(), JoinError>>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpawnError {
    #[error("task name must be 1 to 64 bytes from 0x21 to 0x7E")]
    InvalidName,
    #[error("1048576 tasks are live")]
    Limit,
    #[error("the runtime is shutting down")]
    ShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoinError {
    #[error("task was cancelled")]
    Cancelled,
    #[error("task panicked")]
    Panicked,
}
```

### 2.3 Network

```rust
/// The most datagrams one `send` or `recv` handles.
pub const MAX_NET_BATCH: usize = 64;

/// Seam `net`.
pub trait Net: Send + Sync + fmt::Debug + 'static {
    /// The host's interfaces. With `Some(g)`, waits until the list's
    /// generation is greater than `g`.
    fn interfaces(&self, newer_than: Option<u64>)
        -> BoxFuture<'_, Result<InterfaceList, NetError>>;
    fn bind_udp(&self, local: SocketAddr, options: UdpOptions)
        -> Result<Box<dyn UdpSocket>, NetError>;
    fn connect(&self, remote: StreamAddr, options: StreamOptions)
        -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>>;
    fn listen(&self, local: StreamAddr, options: ListenOptions)
        -> Result<Box<dyn Listener>, NetError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceList {
    pub generation: u64,
    /// Sorted by `index`.
    pub interfaces: Vec<Interface>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub index: u32,
    /// 1 to 15 bytes.
    pub name: String,
    pub mtu: u32,
    pub up: bool,
    pub loopback: bool,
    /// IPv4 before IPv6, each ascending.
    pub addrs: Vec<InterfaceAddr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceAddr {
    pub ip: IpAddr,
    pub prefix_len: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpOptions {
    /// Bytes of payload queued for `recv`. Default 4,194,304.
    pub recv_buffer: u32,
    /// Bytes of payload queued for sending. Default 4,194,304.
    pub send_buffer: u32,
}

impl Default for UdpOptions { /* the defaults above */ }

pub trait UdpSocket: Send + Sync + fmt::Debug {
    fn local_addr(&self) -> SocketAddr;
    fn send<'a>(&'a self, batch: &'a [Datagram<'a>]) -> BoxFuture<'a, SendOutcome>;
    /// Returns how many slots, from the first, it filled.
    fn recv<'a>(&'a self, slots: &'a mut [RecvSlot])
        -> BoxFuture<'a, Result<usize, NetError>>;
}

#[derive(Clone, Copy)]
pub struct Datagram<'a> {
    pub dst: SocketAddr,
    /// The source address to send from. `None` lets the host choose.
    pub src_ip: Option<IpAddr>,
    pub payload: &'a [u8],
}

/// Datagrams `0..sent` were sent. When `sent` is less than the batch
/// length, `error` is the failure of datagram `sent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendOutcome {
    pub sent: usize,
    pub error: Option<NetError>,
}

pub struct RecvSlot {
    pub buf: Box<[u8]>,
    /// Bytes of `buf` holding the datagram.
    pub len: usize,
    /// The datagram was longer than `buf` and its tail was discarded.
    pub truncated: bool,
    pub src: SocketAddr,
    /// The local address the datagram was sent to.
    pub dst_ip: IpAddr,
}

impl RecvSlot {
    /// The largest UDP payload, in bytes.
    pub const MAX_CAPACITY: usize = 65_535;
    /// A slot with a zeroed buffer of `capacity` bytes, `len` 0, `truncated`
    /// false, `src` `0.0.0.0:0`, and `dst_ip` `0.0.0.0`. `InvalidInput` when
    /// `capacity` is above `MAX_CAPACITY`.
    pub fn new(capacity: usize) -> Result<RecvSlot, NetError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StreamAddr {
    /// TCP.
    Ip(SocketAddr),
    /// A Unix domain stream socket.
    Unix(UnixPath),
}

/// An absolute native path, 1 to 107 bytes, starting with `/`, no NUL.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnixPath(/* private */);

impl UnixPath {
    pub fn new(path: &str) -> Result<UnixPath, NetError>;
    pub fn as_str(&self) -> &str;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamOptions {
    /// Disables send coalescing on TCP. Default true.
    pub nodelay: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenOptions {
    /// Pending connections. 1 to 65,535, default 1,024.
    pub backlog: u32,
    /// Permission bits of a Unix socket file. Default 0o600.
    pub unix_mode: u32,
}

impl Default for StreamOptions { /* the defaults above */ }
impl Default for ListenOptions { /* the defaults above */ }

pub trait Stream: Send + Sync + fmt::Debug {
    /// `None` for an unnamed Unix socket.
    fn local_addr(&self) -> Option<StreamAddr>;
    fn peer_addr(&self) -> Option<StreamAddr>;
    /// Returns 0 only at end of stream or for an empty `buf`.
    fn read<'a>(&'a self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize, NetError>>;
    /// Writes from `bufs` in order. Returns the bytes written.
    fn write<'a>(&'a self, bufs: &'a [&'a [u8]]) -> BoxFuture<'a, Result<usize, NetError>>;
    fn shutdown_write(&self) -> BoxFuture<'_, Result<(), NetError>>;
}

pub trait Listener: Send + Sync + fmt::Debug {
    fn local_addr(&self) -> StreamAddr;
    fn accept(&self) -> BoxFuture<'_, Result<Box<dyn Stream>, NetError>>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    #[error("address in use")]
    AddrInUse,
    #[error("address not available on this host")]
    AddrNotAvailable,
    #[error("permission denied")]
    PermissionDenied,
    #[error("connection refused")]
    ConnectionRefused,
    #[error("connection reset")]
    ConnectionReset,
    #[error("timed out")]
    TimedOut,
    #[error("host unreachable")]
    HostUnreachable,
    #[error("network unreachable")]
    NetworkUnreachable,
    #[error("datagram too long, at most {max_payload} bytes of payload fit")]
    MessageTooLong { max_payload: u32 },
    #[error("batch longer than 64")]
    BatchTooLarge,
    #[error("another read or write is pending on this stream")]
    Busy,
    #[error("invalid input: {0}")]
    InvalidInput(Detail),
    #[error("too many open sockets")]
    TooManyOpen,
    #[error("not supported on this platform")]
    Unsupported,
    #[error("closed")]
    Closed,
    /// An operating system error with no variant above. `os` only.
    #[error("operating system error {code}")]
    Os { code: i32 },
}
```

`Detail` is `alloy_seam::Detail`.

### 2.4 Disk

```rust
/// The most operations in one `read_at` or `write_at` batch.
pub const MAX_DISK_BATCH: usize = 256;

/// A path below a `DiskIo` instance's root (clause 3.1.10).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RtPath(/* private */);

impl RtPath {
    /// The path `/`.
    pub fn root() -> RtPath;
    pub fn new(path: &str) -> Result<RtPath, DiskError>;
    pub fn join(&self, component: &str) -> Result<RtPath, DiskError>;
    /// `None` for the root.
    pub fn parent(&self) -> Option<RtPath>;
    /// `None` for the root.
    pub fn file_name(&self) -> Option<&str>;
    pub fn as_str(&self) -> &str;
}

/// Seam `disk-io`.
pub trait DiskIo: Send + Sync + fmt::Debug + 'static {
    fn open<'a>(&'a self, path: &'a RtPath, options: OpenOptions)
        -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>>;
    fn metadata<'a>(&'a self, path: &'a RtPath)
        -> BoxFuture<'a, Result<Metadata, DiskError>>;
    /// Sorted by name, ascending bytewise.
    fn list<'a>(&'a self, dir: &'a RtPath)
        -> BoxFuture<'a, Result<Vec<DirEntry>, DiskError>>;
    fn create_dir<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>>;
    /// Removes a file or an empty directory.
    fn remove<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>>;
    fn rename<'a>(&'a self, from: &'a RtPath, to: &'a RtPath)
        -> BoxFuture<'a, Result<(), DiskError>>;
    fn sync_dir<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Create {
    /// Fail with `NotFound` when missing.
    Never,
    IfMissing,
    /// Fail with `AlreadyExists` when present.
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenOptions {
    pub access: Access,
    pub create: Create,
    /// Bypass the host's cache. Offsets and lengths must be multiples of
    /// the file's `block_size`.
    pub direct: bool,
    /// Block devices only: fail with `Busy` while another holder has it.
    pub exclusive: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Block,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub kind: EntryKind,
    pub len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
}

/// An open file or block device. Dropping it closes it once its pending
/// operations have finished.
pub trait DiskFile: Send + Sync + fmt::Debug {
    fn info(&self) -> BoxFuture<'_, Result<FileInfo, DiskError>>;
    /// One result per operation, in the order given, each with its buffer.
    fn read_at(&self, ops: Vec<ReadOp>) -> BoxFuture<'_, Vec<ReadDone>>;
    /// One result per operation, in the order given, each with its buffer.
    fn write_at(&self, ops: Vec<WriteOp>) -> BoxFuture<'_, Vec<WriteDone>>;
    fn sync(&self, kind: SyncKind) -> BoxFuture<'_, Result<(), DiskError>>;
    fn truncate(&self, len: u64) -> BoxFuture<'_, Result<(), DiskError>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub kind: EntryKind,
    pub len: u64,
    /// 512 or 4,096.
    pub block_size: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncKind {
    /// Data and the length needed to read it back.
    Data,
    /// Data and all metadata.
    All,
}

/// Reads `buf.len()` bytes at `offset` into `buf`.
#[derive(Debug)]
pub struct ReadOp {
    pub offset: u64,
    pub buf: IoBuf,
}

#[derive(Debug)]
pub struct ReadDone {
    pub buf: IoBuf,
    /// Bytes read. Fewer than `buf.len()` only at end of file.
    pub result: Result<usize, DiskError>,
}

/// Writes all of `buf` at `offset`.
#[derive(Debug)]
pub struct WriteOp {
    pub offset: u64,
    pub buf: IoBuf,
}

#[derive(Debug)]
pub struct WriteDone {
    pub buf: IoBuf,
    pub result: Result<(), DiskError>,
}

/// A zero-initialized byte buffer whose first byte is aligned to 4,096.
#[derive(Clone)]
pub struct IoBuf(/* private */);

impl IoBuf {
    pub const ALIGN: usize = 4_096;
    pub const MAX_LEN: usize = 67_108_864;
    /// `InvalidInput` when `len` is above `MAX_LEN`.
    pub fn zeroed(len: usize) -> Result<IoBuf, DiskError>;
    pub fn from_slice(bytes: &[u8]) -> Result<IoBuf, DiskError>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

impl std::ops::Deref for IoBuf { type Target = [u8]; /* ... */ }
impl std::ops::DerefMut for IoBuf { /* ... */ }

/// Variants are numbered from 1 in this order for the event trace
/// (section 4.1); `Os` is 255.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiskError {
    #[error("not found")]
    NotFound,
    #[error("already exists")]
    AlreadyExists,
    #[error("permission denied")]
    PermissionDenied,
    #[error("no space left")]
    NoSpace,
    #[error("device I/O error")]
    Io,
    #[error("offset, length, or buffer not aligned for direct I/O")]
    Unaligned,
    #[error("invalid path")]
    InvalidPath,
    #[error("invalid input: {0}")]
    InvalidInput(Detail),
    #[error("batch longer than 256")]
    BatchTooLarge,
    #[error("device is held by another user")]
    Busy,
    #[error("handle is read only")]
    ReadOnly,
    #[error("is a directory")]
    IsDirectory,
    #[error("not a directory")]
    NotDirectory,
    #[error("directory not empty")]
    NotEmpty,
    #[error("too many open files")]
    TooManyOpen,
    #[error("directory has more than 1048576 entries")]
    TooManyEntries,
    #[error("not supported on this file or platform")]
    Unsupported,
    /// An operating system error with no variant above. `os` only.
    #[error("operating system error {code}")]
    Os { code: i32 },
}
```

### 2.5 Runtime

```rust
use alloy_seam::{Binding, BuildError, ImplName, Registry, RegistryBuilder,
                 RegistryError, Seam, SwapClass};

/// Seam markers (ARFC 0001 section 2.1). Each has `Context = ()` and
/// `SWAP = SwapClass::Restart`.
#[derive(Debug)] pub struct ClockSeam;   // Object = dyn Clock,  KIND = "clock"
#[derive(Debug)] pub struct NetSeam;     // Object = dyn Net,    KIND = "net"
#[derive(Debug)] pub struct DiskIoSeam;  // Object = dyn DiskIo, KIND = "disk-io"
#[derive(Debug)] pub struct RngSeam;     // Object = dyn Rng,    KIND = "rng"
#[derive(Debug)] pub struct SpawnSeam;   // Object = dyn Spawn,  KIND = "spawn"

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub clock: Binding,
    pub net: Binding,
    pub disk_io: Binding,
    pub rng: Binding,
    pub spawn: Binding,
}

impl RuntimeConfig {
    /// All five bound to `name` with `ConfigValue::defaults(1)`.
    pub fn all(name: ImplName) -> RuntimeConfig;
}

/// One instance of each runtime seam. Clones share the instances.
#[derive(Clone, Debug)]
pub struct Runtime(/* private */);

impl Runtime {
    pub fn new(
        clock: Arc<dyn Clock>,
        net: Arc<dyn Net>,
        disk_io: Arc<dyn DiskIo>,
        rng: Arc<dyn Rng>,
        spawn: Arc<dyn Spawn>,
    ) -> Runtime;
    pub fn from_registry(registry: &Registry, config: &RuntimeConfig)
        -> Result<Runtime, BuildError>;
    pub fn clock(&self) -> &Arc<dyn Clock>;
    pub fn net(&self) -> &Arc<dyn Net>;
    pub fn disk_io(&self) -> &Arc<dyn DiskIo>;
    pub fn rng(&self) -> &Arc<dyn Rng>;
    pub fn spawner(&self) -> &Arc<dyn Spawn>;
    pub fn spawn<F>(&self, name: TaskName, future: F)
        -> Result<JoinHandle<F::Output>, SpawnError>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static;
    pub fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
    pub fn timeout<'a, F>(&self, duration: Duration, future: F)
        -> BoxFuture<'a, Result<F::Output, Elapsed>>
    where
        F: Future + Send + 'a,
        F::Output: Send + 'a;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("deadline elapsed")]
pub struct Elapsed;

/// A typed handle to a task started by `Runtime::spawn`. Dropping it
/// detaches the task.
#[derive(Debug)]
pub struct JoinHandle<T>(/* private */);

impl<T: Send + 'static> JoinHandle<T> {
    pub fn id(&self) -> TaskId;
    pub fn cancel(&self);
    pub fn join(self) -> BoxFuture<'static, Result<T, JoinError>>;
}
```

### 2.6 Operating system implementations

```rust
/// The process's operating system runtime: one `tokio` multi-thread
/// runtime with time and I/O drivers. Clones share it.
#[derive(Clone, Debug)]
pub struct OsHost(/* private */);

impl OsHost {
    /// `worker_threads` 0 means one per CPU.
    pub fn start(worker_threads: u16) -> Result<OsHost, BuildError>;
    /// Runs `future` to completion on this host, blocking the caller.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output;
    /// Cancels every task, then stops the runtime, waiting at most `grace`
    /// for tasks to be dropped.
    pub fn shutdown(&self, grace: Duration);
}

/// Registers `os` for the five runtime kinds. The factories use `host`.
pub fn register_os(builder: &mut RegistryBuilder, host: &OsHost)
    -> Result<(), RegistryError>;
```

`alloy-sim` registers `mem` for the five kinds in the registry of each simulated
host, with factories bound to that host. Its world and scenario interface is not
part of this section.

## 3. Behavior

### 3.1 Runtime

3.1.1 Only implementations of these seams call the operating system for time,
network, storage, randomness, or concurrency. `cargo xtask check-repo` rejects
those calls elsewhere.

3.1.2 The `os` implementation uses `tokio`, batched UDP I/O with segmentation
offload where the kernel offers it, and direct I/O for block devices.

3.1.3 All seams. Methods take `&self` and may be called concurrently from any task
unless a clause below limits it. No method panics on any argument: invalid input
returns an error. Dropping a future returned by a seam method cancels it, with the
guarantee stated for that method below, and never panics. `mem` implementations
never return an `Os` error.

3.1.4 Clock.

- `now` never returns a value lower than any earlier call on the same instance
  returned, from any thread.
- `sleep_until(d)` completes only when `now() >= d`. With `d` at or before `now()`
  it completes on its first poll. `sleep_until(Instant::MAX)` never completes.
- `wall` may move backward or forward between calls.
- `os`: `now` reads the monotonic clock that `std::time::Instant` reads, with the
  origin at instance creation. A sleep should complete within 2 ms after its
  deadline in an otherwise idle process. `wall` reads the system real-time clock.
- `mem`: `now` starts at 0 when the host starts and advances with world time at
  the host's clock rate. The rate is `1 + p / 1,000,000`, where `p` is the host's
  rate offset in parts per million: 0 unless a clock skew fault sets it, and from
  -500,000 to 500,000 (section 7). Exactly, with integer arithmetic: when the rate
  offset becomes `p` at world time `w0`, with host time `h0` at that instant, then
  until the next change host time at world time `w` is
  `h0 + floor((w - w0) * (1,000,000 + p) / 1,000,000)`. A host starts with
  `h0 = 0` at its start instant and the offset in force for it then.
- `mem`: a timer of `sleep_until(d)` fires at the first world nanosecond at which
  the host's `now()` is at least `d`, under whatever offsets apply until then. A
  task woken by it reads, on that poll, the host time at that nanosecond, unless
  a CPU stall fault is active on its host. That is the smallest value at or above
  `d` that the host clock reaches. It is `d` itself whenever the offset has been
  at most 0 since the sleep began, because the clock then takes every integer
  value; with a positive offset the clock skips some values, and the task may
  read a value above `d`.
- `mem`: `wall` starts at the world's configured wall time, advances at the
  host's clock rate, and jumps by clock step faults.

3.1.5 Rng.

- `fill` with an empty `dest` returns `Ok(())`.
- `os`: bytes come from the platform's cryptographically secure generator. The
  only error is `Unavailable`, when the platform reports its generator cannot
  serve.
- `mem`: each instance draws from its own byte stream, fixed by the world seed,
  the host, the number of times the host has started, and the instance's place in
  creation order among the `rng` instances built on the host since it last
  started. No two instances in one run, on one host or on different hosts, before
  or after a restart, produce the same stream, unless the scenario configures a
  repeat explicitly to model a faulty generator. Successive `fill` calls on one
  instance take consecutive bytes of its stream, so no position of a stream is
  returned twice.
  It never fails. Its output is not secret (section 8).

3.1.6 Spawn.

- `spawn` returns without polling the task on the caller's stack. The task is
  polled until it completes, is cancelled, or, in `mem`, its host crashes.
- After `cancel` returns, the task is not polled again, except that a poll already
  running on another thread finishes. The task's future is dropped before `join`
  completes. `join` returns `Ok(())` when the task's future completed, also when
  `cancel` raced with its last poll, and `Cancelled` when it was dropped
  unfinished.
- A task that panics ends. `join` returns `Panicked`, every other task keeps
  running, and the process does not abort.
- `join` completes after the task has ended and its future has been dropped.
  Dropping a `join` future, a `TaskHandle`, or a `JoinHandle` detaches the task,
  which keeps running.
- An instance holds at most 1,048,576 live tasks. `spawn` beyond that returns
  `Limit`. `TaskName::new` returns `InvalidName` for a name outside its rules.
- `os`: `spawn` returns `ShuttingDown` once `OsHost::shutdown` has been called.
  `mem`: `spawn` on the instance of a host that has crashed returns
  `ShuttingDown`.
- `mem`: task ids are 1, 2, 3, and so on per host, in spawn order, continuing
  across restarts of the host. `yield_now`
  returns the task to the runnable set and the scheduler picks the next task from
  the seed, which may be the same one.

3.1.7 Interfaces.

- `interfaces(None)` completes with the current list. `interfaces(Some(g))`
  completes with the current list at once if its generation is greater than `g`,
  and otherwise when the list next changes.
- The generation increases each time any field of the list changes, and never
  otherwise. The list holds at most 1,024 interfaces, the lowest indexes, and at
  most 64 addresses per interface, the first in the order of section 2.3.
- `os` should report a change within 1 s. `mem` reports it at the virtual instant
  of the change.

3.1.8 UDP sockets.

- `bind_udp` with port 0 picks a free port; `mem` picks it from 49,152 to 65,535
  using the seed. A bind to the IPv6 unspecified address receives IPv6 only.
  `bind_udp` fails with `AddrInUse` when the address and port are bound,
  `AddrNotAvailable` when the address is on no interface of the host,
  `PermissionDenied` when the platform refuses the port, `TooManyOpen` when the
  instance has 4,096 open UDP sockets, and `InvalidInput` when a buffer size is
  outside 65,536 to 67,108,864.
- `send` with more than 64 datagrams returns `sent: 0` and `BatchTooLarge`.
  Otherwise it sends the datagrams in order and stops at the first that fails.
  It may wait while the send buffer is full and does not drop a datagram for that
  reason. Completion means handed to the network, not delivered.
- No datagram is ever fragmented by IP (I-10): IPv4 datagrams carry the don't
  fragment flag and IPv6 datagrams no fragment header. A datagram whose IP packet
  would exceed the host's path MTU to `dst` fails with `MessageTooLong`, where
  `max_payload` is that MTU minus the IP header (20 bytes for IPv4, 40 for IPv6)
  minus 8. The path MTU is the outgoing interface's MTU, or a lower value the host
  learned for `dst` from an ICMP fragmentation needed or packet too big message.
  `mem` forgets a learned value after 600 s of virtual time.
- `src_ip` must be `None`, or the socket's bound address, or, for a socket bound
  to an unspecified address, an address of the host in the same family.
  Otherwise that datagram fails with `InvalidInput`.
- A dropped `send` future has sent a prefix of its batch, possibly empty, and
  nothing else of it.
- `recv` with an empty `slots` returns `InvalidInput`. Otherwise it waits until a
  datagram is queued, then fills up to `min(slots.len(), 64)` slots, from the
  first, with queued datagrams in arrival order, and returns the count. Each
  datagram goes to exactly one `recv` call, also when several are pending. A
  dropped `recv` future has taken no datagram.
- The socket queues received payload up to `recv_buffer` bytes and drops a
  datagram that arrives when it does not fit (`mem` exactly, `os` within the
  platform's accounting).
- ICMP errors other than those of the path MTU rule are never reported to the
  caller.

3.1.9 Streams.

- `connect` to `Ip` opens TCP with `nodelay` applied, and to `Unix` a Unix
  stream socket. It fails with `ConnectionRefused`, `TimedOut`,
  `HostUnreachable`, or `NetworkUnreachable`. `mem` returns `TimedOut` after 30 s
  of virtual time without an answer. A dropped `connect` future leaves no
  connection open.
- `listen` returns `InvalidInput` for a backlog outside 1 to 65,535. On `Unix` it
  fails with `AddrInUse` when the path exists, creates the socket file with the
  permission bits `unix_mode & 0o777`, and removes the file when the listener is
  dropped. A TCP port whose earlier listener has closed can be listened on again
  at once.
- `accept` returns pending connections in arrival order. A dropped `accept`
  future has taken no connection.
- `read` waits for data and returns between 1 and `buf.len()` bytes, 0 at end of
  stream, and 0 at once for an empty `buf`. `write` waits for buffer space and
  writes at least 1 byte unless `bufs` hold none. Both fail with
  `ConnectionReset` after a reset. `write` after `shutdown_write` fails with
  `Closed`.
- Buffering. `os` uses the platform's socket buffers and does not set their size.
  In `mem` each direction of a connection, TCP or Unix, holds at most 262,144
  bytes that one end has written and the other has not yet read, counting bytes
  in flight and bytes queued for `read` together. A `write` writes
  `min(free space, total length of bufs)` bytes, waits while the free space is 0,
  and never fails because the buffer is full. Space is freed when `read` returns
  bytes to the peer, and bytes not yet read are discarded when the reading end is
  dropped or the connection is reset.
- At most one `read` and one `write` may be pending on a stream. Another one fails
  at once with `Busy`. A dropped `read` future has read nothing and a dropped
  `write` future has written nothing.
- After `shutdown_write` completes, the peer reads end of stream once it has read
  every byte written before. Dropping a stream closes both directions.
- An instance holds at most 65,536 open streams and listeners together. Beyond
  that, `connect`, `listen`, and `accept` fail with `TooManyOpen`. A platform
  without Unix sockets returns `Unsupported` for `Unix` addresses.

3.1.10 Disk paths and the namespace.

- `RtPath::new` accepts exactly the strings that start with `/`, are `/` alone or
  have components of 1 to 255 bytes separated by single `/` with no trailing `/`,
  have no component `.` or `..`, contain no NUL and no `\`, and are at most 4,096
  bytes. Anything else returns `InvalidPath`. `join` applies the same rules.
- Every instance has a root, and `RtPath` `/` names it. `os` maps paths below the
  root directory of its configuration (section 4.2) and follows symbolic links, so
  the root is not a security boundary. In `mem` each host has its own namespace,
  and block devices are the ones the scenario attaches, at the paths it gives.
- `open` with `Create::IfMissing` or `Create::New` and `Access::Read` returns
  `InvalidInput`. A missing parent returns `NotFound`, a directory
  `IsDirectory`. `exclusive` on anything but a block device returns
  `InvalidInput`. An instance holds at most 65,536 open handles, beyond which
  `open` returns `TooManyOpen`.
- `list` returns every entry except `.` and `..`, sorted by name, ascending
  bytewise, or `TooManyEntries` above 1,048,576. `create_dir` needs an existing
  parent and returns `AlreadyExists` for an existing path. `remove` returns
  `NotEmpty` for a directory with entries. `rename` replaces a file at `to`
  atomically, and returns `IsDirectory` when `to` is a directory.

3.1.11 Disk I/O.

- A batch longer than 256 operations, an operation not aligned on a direct handle
  (offset or length not a multiple of `block_size`), a write on an
  `Access::Read` handle, two writes of one batch with overlapping byte ranges, and
  an operation past the end of a block device each fail the whole batch: every
  result is the error (`BatchTooLarge`, `Unaligned`, `ReadOnly`, `InvalidInput`,
  `InvalidInput`) and no operation of the batch is performed. Every buffer is
  returned in its result.
- Operations of one batch run in any order and may run concurrently. Operations
  of batches pending at the same time have no defined order between them.
- A read returns `buf.len()` bytes, or fewer only when it reaches the end of the
  file. A write writes all its bytes or fails, and extends a regular file, filling
  any gap with zeros. A completed write is seen by every read that starts after it
  completes, on any handle of the same file in the instance.
- `sync(Data)` returns `Ok` once every write that completed on the handle before
  `sync` was called, and the file length those writes need, is durable.
  `sync(All)` also makes the file's metadata durable. `sync` first waits for every
  operation issued on the handle before it was called, including operations of
  dropped futures.
- `truncate` sets the length of a regular file, filling with zeros when it grows.
  It returns `Unsupported` on a block device and `ReadOnly` on a read handle.
- The operations of a dropped `read_at` or `write_at` future may still run, and
  their buffers are lost to the caller. Until a later `sync` on the handle
  completes, a dropped write has the outcome of an unsynced write at a crash
  (clause 3.1.12). After it, the outcome is fixed.
- `block_size` is the logical block size of the device that holds the file, 4,096
  when `os` cannot learn it. In `mem` it is the simulated disk's, 512 or 4,096,
  default 4,096. `IoBuf` alignment covers both.

3.1.12 Durability. After a host crash and restart, for each file:

- every write that completed before a `sync` on any handle of the file that then
  completed is intact,
- every other write holds, in each `block_size` unit it covers, either its old or
  its new bytes; in `mem` a write is split this way only when the scenario
  enables torn writes, and otherwise lands whole or not at all,
- a length change not followed by a completed `sync` may be undone,
- a create, remove, or rename not followed by a completed `sync_dir` of its parent
  directory, of both parents for a rename, may be undone, and a rename is never
  half done.

`mem` implements exactly this contract and loses, at a crash, everything it
allows to be lost, using the seed to choose among the allowed outcomes. `os`
relies on the platform for it.

3.1.13 `IoBuf`. The first byte of every `IoBuf` is at an address that is a
multiple of 4,096. `zeroed` and `from_slice` return `InvalidInput` above
67,108,864 bytes. Its `Debug` output holds its length only.

3.1.14 Runtime.

- `from_registry` builds the five instances in the order clock, rng, spawn, net,
  disk-io, each with context `()`. On the first failure it drops what it built and
  returns that error.
- `Runtime::spawn` spawns through the `spawn` instance, and `JoinHandle::join`
  returns the task's output or the `JoinError` of clause 3.1.6.
- `sleep(d)` is `clock.sleep_until(clock.now().saturating_add(d))`.
- `timeout(d, f)` polls `f` before the timer on every poll. It returns `f`'s output
  if `f` completes first, and otherwise `Elapsed`, dropping `f`.
- `register_os` registers `os` under the five kinds and fails as
  `RegistryBuilder::register` does. Each `os` factory accepts the configuration of
  section 4.2.

### 3.2 Determinism

3.2.1 A scenario run is a pure function of `(binary, seed)`. Two runs with the
same pair must produce byte-identical event traces (I-20). The trace hash is
printed at the end of every run.

3.2.2 The scheduler picks the next runnable task and the delivery order of
simultaneous events from the seed. Time advances only when no task is runnable.

3.2.3 Virtual time has nanosecond resolution. A scenario covering one virtual hour
of a 100-node cluster must finish in under 60 seconds on one core.

3.2.4 The simulator appends one record to the event trace (section 4.1) for each
of these, in the order it processes them: a host starting, crashing, or stalling;
a task spawned, polled, or ended; a timer firing; a clock fault; a UDP datagram
sent, delivered, dropped, or altered by a fault; a stream opened, carrying data,
or closed; each operation of a disk call; an `Rng::fill`; any other fault applied
or changed; a note from the scenario; and the end of the run. Each record carries
the world's virtual time when it happened. Section 4.1 states, for each kind,
which event writes it, on which host, and with which field values, so that one
sequence of simulator events has exactly one trace byte string.

3.2.5 The trace hash is the 32-byte BLAKE3 hash of the trace bytes: the header and
every record, in order, including records not written to a file because of the
cap in section 7. It is computed through `alloy-crypto`, which owns the BLAKE3
dependency (`agent/eng/crypto.md`). A record's `digest` field is the first 8
bytes of the BLAKE3 hash of the bytes it describes.

3.2.6 At the end of every run, passed or failed, the simulator writes exactly one
line to standard error:

```
alloy-sim seed=<seed> trace=<hash> events=<n> virtual_ns=<t> result=<result>
```

`<seed>` is the seed as 16 lowercase hexadecimal digits, `<hash>` the trace hash
as 64 lowercase hexadecimal digits, `<n>` the number of records and `<t>` the
virtual time of the last record, both in decimal, and `<result>` one of `pass`,
`fail`, `liveness`. Two runs of one binary with one seed must write identical
lines. CI runs each scenario twice per seed and compares them.

### 3.3 Faults

The world can inject, at any virtual time or at seeded random times: per-link
loss, duplication, reordering, delay, jitter, corruption, and MTU limits with or
without ICMP feedback. One-way and two-way partitions. Host crash at any await
point, with loss of all unsynced disk writes, optionally torn. Host restart. Disk
read errors, corrupt reads, and full disks. Clock skew and steps. CPU stalls.

### 3.4 NAT lab

`alloy-natlab` adds NAT devices to the world, configured along the axes of RFC
4787:

| Axis | Values |
|---|---|
| Mapping | endpoint-independent, address-dependent, address-and-port-dependent |
| Filtering | endpoint-independent, address-dependent, address-and-port-dependent |
| Port assignment | preserving, sequential, random |
| Hairpinning | yes, no |
| Mapping timeout | 10 s to 600 s, whole seconds |
| Layers | single, double (carrier grade in front of a home router) |
| Family | IPv4 only, dual stack, IPv6 only with a stateful firewall |
| UDP | allowed, blocked |

The mapping timeout range covers every published value in `research/R-0003.md`,
from 10 s (the shortest measured) to 350 s (a cloud NAT gateway). A device
configured with a value outside the range is rejected with an error when the world
is built; values are never clamped.

The lab models these rules for every device:

- A device has one external address per IP family (RFC 6888 REQ-2, paired
  pooling, as `research/R-0003.md` reports it).
- Filtering applies per mapping. Under endpoint-independent filtering a mapping
  admits every inbound packet. Under address-dependent filtering it admits a
  packet only from an address that mapping has sent to, and under
  address-and-port-dependent filtering only from an address and port it has sent
  to. A packet that no mapping admits is dropped.
- A new mapping never takes an external port that another mapping of the device
  holds.
- The mapping timeout applies to each mapping and, under address-dependent or
  address-and-port-dependent filtering, separately to each filter entry (the
  permission for one remote address, or one remote address and port). A mapping
  or entry is removed when no outbound packet has used it for the timeout.
  Inbound packets do not refresh it.

Named presets cover common devices. The **matrix** is every ordered pair of
presets, plus pairs on one local network and pairs behind one device, with the
expected path class (`direct` or `relayed`) and a time bound. The class follows
ARFC 0004 section 3.3 and the bound follows ARFC 0004 section 3.2. It lives in
`tests/natlab/matrix.toml` and is normative for ARFC 0004.

The presets must include `short-timeout`: endpoint-independent mapping,
address-and-port-dependent filtering, preserving port assignment, no hairpinning,
a mapping timeout of 20 s, single layer, IPv4 only, UDP allowed. Its timeout is
below the 25 s keepalive of ARFC 0004 section 3.4, so an idle direct path through
it loses its filter entry between keepalives. The relay route to a node behind it
stays open: the node sends on its home session at least once every 5 s (ARFC 0004
section 3.4), which refreshes its mapping and its filter entry for the anchor
within the 20 s timeout. Expected, for a node behind
`short-timeout` paired with a node behind any preset whose matrix class with it is
`direct`:

- The direct path becomes active within the bound of ARFC 0004 section 3.2, as
  for any other `direct` pair.
- While the path is idle, each node either receives an authenticated packet on
  the path at least once every 75 s (3 keepalive periods) or marks the path
  `failed` no later than 75 s after the last one it received.
- Every packet a node sends after marking the path `failed` is delivered, through
  the relay until a direct path is active again.
- Packets sent on the idle direct path before either of those may be lost, and
  the lab does not count that loss as a failure.

### 3.5 Real mode

Every NAT preset also has a script that builds it from Linux network namespaces
and nftables. `tests/system` runs the real `alloyd` binaries through a sample of
the matrix. A disagreement between the simulated and real result is a bug in the
model and blocks the milestone.

### 3.6 Checking

`alloy-sim` provides checkers used by scenarios: a linearizability checker for
histories of register and key-value operations, a packet isolation checker (no
packet crosses VNIs), and a durability checker (every acknowledged write is
readable after any allowed failure).

## 4. Formats

### 4.1 Event trace

A trace is a 16-byte header followed by records. All integers are little endian.

Header:

| Bytes | Field | Value |
|---|---|---|
| 0 to 3 | `magic` | `41 4C 54 52` (ASCII `ALTR`) |
| 4 to 5 | `version` | u16, 1 |
| 6 to 7 | `reserved` | u16, 0 |
| 8 to 15 | `seed` | u64, the run's seed |

Record:

| Bytes | Field | Value |
|---|---|---|
| 0 to 7 | `time` | u64, world virtual time in nanoseconds |
| 8 to 11 | `host` | u32, the host's id in creation order from 0, or `FFFFFFFF` for the world |
| 12 to 13 | `kind` | u16, from the table below |
| 14 to 15 | `len` | u16, length of `body` |
| 16 to 16 + `len` | `body` | Per `kind` |

Records appear in the order of clause 3.2.4, so `time` never decreases. A reader
skips a record of a kind it does not know by its `len`. New kinds may be added
without changing `version`. A change to the header or to the record layout above
increases it.

Body fields used below, in order of appearance in each body:

- `addr`: u8 family (4 or 6), then the IP address (4 or 16 bytes), then u16 port.
- `saddr`: an `addr`; or u8 0, then u8 path length, then the Unix path bytes; or
  u8 `FF` for an unnamed Unix socket.
- `digest`: 8 bytes (clause 3.2.5).
- `text`: u16 length, at most 1,024, then that many bytes of UTF-8. Longer text
  is cut at the last character boundary that fits.
- Socket, connection, handle, and task numbers are u64, assigned 1, 2, 3, and so
  on per host in creation order, continuing across restarts of the host. Task
  numbers are the `TaskId`s of clause 3.1.6.

| Kind | Name | Body |
|---|---|---|
| `0001` | `host-start` | empty |
| `0002` | `host-crash` | empty |
| `0003` | `host-stall` | u64 stall length in ns |
| `0010` | `task-spawn` | u64 task, u8 name length, name bytes |
| `0011` | `task-poll` | u64 task |
| `0012` | `task-end` | u64 task, u8 outcome: 0 completed, 1 cancelled, 2 panicked, 3 dropped by a host crash |
| `0020` | `timer-fire` | u64 task woken, u64 deadline on the host's clock |
| `0021` | `clock-fault` | i32 clock rate offset in parts per million in force after the fault, i64 wall step in ns (0 for none) |
| `0030` | `udp-send` | u64 socket, `addr` source, `addr` destination, u32 payload length, `digest` of the payload |
| `0031` | `udp-deliver` | u64 receiving socket, `addr` source, `addr` destination, u32 payload length, `digest` of the payload |
| `0032` | `packet-drop` | u8 reason, `addr` source, `addr` destination, u32 payload length |
| `0033` | `packet-fault` | u8 fault: 1 duplicated, 2 corrupted, 3 delayed; `addr` source, `addr` destination, u64 added delay in ns |
| `0040` | `stream-open` | u64 connection, `saddr` local, `saddr` peer |
| `0041` | `stream-data` | u64 connection, u8 direction: 0 written by this host, 1 read by this host; u32 length, `digest` of the bytes |
| `0042` | `stream-close` | u64 connection, u8 how: 0 write side shut down by this host, 1 reset, 2 dropped by this host, 3 dropped by a host crash |
| `0050` | `disk-op` | u64 handle (0 for a path operation), u8 operation, u64 offset, u64 length, u8 result, `digest` |
| `0060` | `rng` | u32 bytes filled |
| `0070` | `fault` | `text` describing the fault applied or changed |
| `00F0` | `note` | `text` from the scenario |
| `00FF` | `run-end` | u8 result: 0 pass, 1 fail, 2 liveness |

Packet drop reasons: 1 loss fault, 2 partition, 3 larger than a link MTU, 4 link
queue full, 5 no socket bound at the destination, 6 receive queue full, 7
filtered by a NAT device, 8 no route.

Disk operations: 1 open, 2 read, 3 write, 4 sync, 5 truncate, 6 metadata, 7 list,
8 create-dir, 9 remove, 10 rename, 11 sync-dir, 12 info. `result` is 0 for success
and otherwise the `DiskError` number of section 2.4.

Which event writes each record, and its field values:

- `host` is the host the event happened on. It is `FFFFFFFF` for events on links,
  routers, and NAT devices, which are `packet-drop` with reasons 1, 2, 3, 4, 7,
  and 8, every `packet-fault`, and a `fault` applied to a link, router, or NAT
  device, and for `note` and `run-end`. `packet-drop` with reasons 5 and 6
  carries the destination host. A `fault` or `clock-fault` applied to one host
  carries that host.
- The addresses of `udp-send`, `udp-deliver`, `packet-drop`, and `packet-fault`
  are those the packet carries at the point of the event, after every NAT
  translation before it.
- `udp-send`: one record per datagram that `send` hands to the network, in batch
  order. A datagram that fails in `send` writes none. `udp-deliver`: when the
  datagram is queued on the receiving socket.
- A host crash writes, at the time of its `host-crash` record and right after it,
  `task-end` with outcome 3 for each live task of the host in ascending task
  number, then `stream-close` with how 3 for each open connection of the host in
  ascending connection number.
- Streams. Each end of a connection writes its own records, on its own host, with
  its own connection number. A host numbers a connection when its `connect`
  completes or when its `accept` returns the connection, and writes
  `stream-open` then. A connection still in a listener's backlog has no number
  and no record on the listening host.
- `stream-data` is written once for each `write` that writes at least 1 byte,
  with direction 0, and once for each `read` that returns at least 1 byte, with
  direction 1. `length` is the number of bytes written or returned, and `digest`
  covers exactly those bytes. A `read` that returns 0 writes no record.
- `stream-close` is written with how 0 when the host's `shutdown_write`
  completes, how 1 at the instant the host's end becomes reset (its `read` and
  `write` fail with `ConnectionReset` from then on), how 2 when the host drops its
  `Stream`, and how 3 at a host crash, as above. One connection may write several
  on one host, one per event.
- `disk-op` records are written when the simulator completes the operation, also
  for a dropped `read_at` or `write_at` future. A batch's records are written
  together, in batch order, when its last operation completes. A batch that fails
  whole (clause 3.1.11) writes one record per operation, each with the error.
- `disk-op` `handle`: for `open`, the number of the new handle, or 0 when `open`
  fails; for read, write, sync, truncate, and info, the handle's number; 0 for the
  path operations metadata, list, create-dir, remove, rename, and sync-dir.
- `disk-op` `offset` and `length`: for a read or a write, the operation's offset
  and `buf.len()`, which is the requested length also when a read returns fewer
  bytes or fails; for truncate, 0 and the new length; 0 and 0 for every other
  operation.
- `disk-op` `digest`: for a successful write, of the bytes written; for a
  successful read, of the bytes it returned, which may be fewer than `length` or
  none; 8 zero bytes for every failed operation and every other operation.
- `rng`: one record for every `fill` call, also with an empty `dest`, with the
  number of bytes filled.

When a scenario asks for a trace file, the simulator streams the trace to it until
the next record would take the file past the cap (section 7). It writes no record
after that one, and the hash still covers them all. Independently, it keeps the
last 200 records in memory for section 6.

### 4.2 Configuration of the runtime implementations

Payloads are Protocol Buffers messages (ARFC 0001 section 4). Every runtime
implementation reads and writes version 1 only (`MIN_VERSION` and `VERSION` both
1) and ignores unknown fields.

| Kind | Implementation | Message |
|---|---|---|
| `clock`, `rng`, `spawn`, `net` | `os`, `mem` | `message Empty {}` |
| `disk-io` | `mem` | `message Empty {}` |
| `disk-io` | `os` | `message OsDiskIoConfig { string root = 1; }` |

`root` is an absolute native path of an existing directory. An empty `root` means
`/` on Linux and macOS, and is rejected on other platforms. A rejected or missing
root fails the build with `BuildError::Config(ConfigError::Invalid { .. })` naming
the field `root`.

## 5. Seams

| Kind | Trait | Marker | Implementations | Suite |
|---|---|---|---|---|
| `clock` | `Clock` | `ClockSeam` | `os`, `mem` | `tests/conformance/rt/clock/` |
| `net` | `Net` | `NetSeam` | `os`, `mem` | `tests/conformance/rt/net/` |
| `disk-io` | `DiskIo` | `DiskIoSeam` | `os`, `mem` | `tests/conformance/rt/disk-io/` |
| `rng` | `Rng` | `RngSeam` | `os`, `mem` | `tests/conformance/rt/rng/` |
| `spawn` | `Spawn` | `SpawnSeam` | `os`, `mem` | `tests/conformance/rt/spawn/` |

All five have swap class `restart` and context `()`. Each suite runs against
`mem` on the simulator and against `os` on the CI host. A clause that names one
implementation is tested on that implementation only. The trace format and the
summary line of clauses 3.2.4 to 3.2.6 are tested by the simulator's own suite,
`tests/sim/`.

## 6. Failure behavior

A scenario that fails prints its seed, the trace hash, and the last 200 events. A
scenario that exceeds its virtual time budget fails as a liveness violation.

A task that panics in `mem` fails the scenario, after the run ends, with result
`fail`. A trace file that cannot be written fails the scenario with result `fail`
and the summary line is still written.

## 7. Limits

10,000 hosts per world. 2^63 nanoseconds of virtual time. Event traces are
streamed to disk and capped at 1 GiB unless a scenario raises the cap.

| Bound | Value | At the bound |
|---|---|---|
| Datagrams per `send` or `recv` | 64 | `BatchTooLarge` / fills 64 slots |
| UDP sockets per `net` instance | 4,096 | `TooManyOpen` |
| Streams and listeners per `net` instance | 65,536 | `TooManyOpen` |
| `mem` stream buffer, per direction of a connection | 262,144 bytes | `write` waits for space |
| `RecvSlot` capacity | 65,535 bytes | `InvalidInput` |
| UDP receive and send buffer | 65,536 to 67,108,864 bytes, default 4,194,304 | `InvalidInput`; arrivals beyond the receive buffer dropped |
| Listen backlog | 1 to 65,535, default 1,024 | `InvalidInput` |
| Interfaces listed | 1,024, 64 addresses each | The rest are left out |
| Unix socket path | 107 bytes | `InvalidInput` |
| Operations per disk batch | 256 | `BatchTooLarge` |
| `IoBuf` length | 67,108,864 bytes | `InvalidInput` |
| Open disk handles per `disk-io` instance | 65,536 | `TooManyOpen` |
| Directory entries per `list` | 1,048,576 | `TooManyEntries` |
| `RtPath` | 4,096 bytes, 255 per component | `InvalidPath` |
| Live tasks per `spawn` instance | 1,048,576 | `SpawnError::Limit` |
| `TaskName` | 64 bytes | `SpawnError::InvalidName` |
| `Instant` | 2^63 - 1 ns | Saturates or `None` (section 2.1) |
| `mem` clock rate offset | -500,000 to 500,000 parts per million | The world rejects the fault when it is injected |
| Trace record body | 65,535 bytes | `text` fields are cut to 1,024 bytes |
| Records kept for failure output | 200 | Oldest dropped |

## 8. Security

The simulator is test infrastructure and is not built into release binaries.
Upholds I-20 and is the means of testing most other invariants.

`mem` randomness is predictable from the seed, so `mem` implementations must never
be registered in a release build (ARFC 0001 clause 3.5). Trace records carry
lengths and 8-byte digests of payloads, never the payloads themselves.

## 9. Open questions

- Parameters and real-world frequencies of NAT presets. Task T-0005.
