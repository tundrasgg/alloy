# ARFC 0005: Runtime, simulator, and NAT lab

- Status: accepted
- Depends on: 0001
- Crates: alloy-rt, alloy-sim, alloy-natlab
- Decisions: 0008, 0036, 0039, 0045, 0054, 0068, 0069, 0071

## 1. Purpose

All of Alloy runs on a small runtime interface. In production the operating system
implements it. In tests a deterministic simulator does, which lets whole clusters,
hostile networks, and years of faults run in seconds from a seed. This is built
before anything else.

## 2. Model

`alloy-rt` defines six seams:

| Seam | Trait | Gives |
|---|---|---|
| `clock` | `Clock` | Monotonic now, wall now, sleep until, timers |
| `net` | `Net` | Datagram and stream sockets, interface list, batch send and receive, the user id of a Unix socket's peer, layer 3 virtual interfaces (TUN on Linux, Wintun on Windows) |
| `disk-io` | `DiskIo` | Open, read at, write at, sync, truncate, list, direct block access, owner and permissions, durable replace |
| `rng` | `Rng` | Random bytes |
| `spawn` | `Spawn` | Start a task, join it, cancel it |
| `host` | `Host` | Facts about the machine, free space at a path, and enabling, starting, stopping, and disabling the service `alloyd` and no other |

A `Runtime` value bundles one instance of each of the first five, and the
**runtime kinds** in this ARFC are those five. Every component takes a
`Runtime` at construction and uses nothing else to reach the world, except
that `alloy init` and `alloy join` (ARFC 0002 sections 3.1, 3.3, and 3.8) also
take a `Host`. `host` is not in `Runtime`, so no other component can stop or
disable `alloyd` (section 2.8, ADR 0071). All six are runtime seams in the
sense of ARFC 0001 section 2.1, with context `()`.

`alloy-sim` implements all six in memory, in one thread, driven by a seeded
scheduler and a virtual clock. A **world** holds hosts, links, routers, and NAT
devices. A **scenario** is a function that builds a world, runs Alloy code on its
hosts, injects faults, and asserts properties. `alloy-sim` is written in this
repository and depends on no simulation library (ADR 0036).

The **event trace** is the byte record of everything the simulator did in one run
(section 4.1). Its BLAKE3 hash is the **trace hash**.

**World time** is the virtual time of a run, in nanoseconds from its start. Each
host's clock is derived from it (clause 3.1.4). A **network** is an IP segment
with one or two prefixes and the link properties every packet crossing it gets.
Networks form trees: a network other than a root has one **router** to its parent
network, and the router may carry a **middlebox**, the plug-in through which
`alloy-natlab` translates and filters (clause 3.3.9). A host has interfaces, each
attached to one network. The **control task** is the scenario's own future: it
runs on the world, not on a host, builds the world, applies faults, and decides
the run's result (clause 3.2.7). Section 2.7 is the interface, and ADR 0054
records the model.

### 2.1 Interface of `alloy-rt`

Sections 2.1 to 2.6 and section 2.8 are the whole public interface of
`alloy-rt`. Items marked private are the developer's choice. `alloy-rt` depends on `alloy-seam` (ARFC 0001
section 2.1) and on no other Alloy crate. Every error type derives `Debug`,
`Clone`, `PartialEq`, and `Eq`, and implements `std::error::Error` and `Display`
through `thiserror`. Types that carry packet or disk bytes (`Datagram`, `RecvSlot`,
`IoBuf`) implement `Debug` by hand and print lengths only, never contents (I-22).
Their `Debug` output with `{:?}` is exactly the text the doc comment on each impl
below gives, with every number in decimal: for example `Datagram { len: 64 }`.
With `{:#?}` it holds the same fields, in the same order, in Rust's standard
multi-line layout for a struct. Types that contain them, such as `ReadOp` and
`ReadDone`, inherit this.

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
    /// Creates a layer 3 virtual interface: a TUN device on Linux, a
    /// Wintun adapter on Windows (clause 3.1.17).
    fn create_tun(&self, name: &str, options: TunOptions)
        -> BoxFuture<'_, Result<Box<dyn Tun>, NetError>>;
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

/// Writes `Datagram { len: <payload.len()> }`, never the payload, `dst`,
/// or `src_ip` (section 2.1).
impl fmt::Debug for Datagram<'_> { /* ... */ }

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

/// Writes `RecvSlot { capacity: <buf.len()>, len: <len> }`, never the
/// bytes, `truncated`, `src`, or `dst_ip` (section 2.1).
impl fmt::Debug for RecvSlot { /* ... */ }

impl RecvSlot {
    /// The largest slot buffer, in bytes: the largest value of the UDP
    /// length field. Every UDP payload fits, since a payload is at most
    /// 65,527 bytes (65,507 over IPv4).
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
    /// The effective user id that the kernel recorded for the process at
    /// the other end of a Unix stream socket (clause 3.1.9.2).
    fn peer_uid(&self) -> Result<u32, NetError>;
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
    /// An interface name, address, or route conflicts with another object
    /// of the host, which the detail names (clause 3.1.17).
    #[error("conflict: {0}")]
    Conflict(Detail),
    /// An operating system error with no variant above. `os` only.
    #[error("operating system error {code}")]
    Os { code: i32 },
}

/// The longest IP packet a `Tun` reads or writes, in bytes.
pub const MAX_TUN_PACKET: usize = 65_535;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TunOptions {
    /// 1,280 to 65,535 bytes.
    pub mtu: u32,
    /// The interface GUID of the Wintun adapter, in the byte order of
    /// the Windows `GUID` structure. Required on Windows, ignored elsewhere.
    pub guid: Option<[u8; 16]>,
}

/// An IP prefix: `len` at most 32 for IPv4 and 128 for IPv6, and no bit
/// of `addr` set after the first `len`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IpPrefix {
    pub addr: IpAddr,
    pub len: u8,
}

/// A layer 3 virtual interface (clause 3.1.17). Dropping it removes the
/// interface without waiting.
pub trait Tun: Send + Sync + fmt::Debug {
    /// The interface's `index` in `interfaces`.
    fn index(&self) -> u32;
    fn name(&self) -> &str;
    fn set_mtu(&self, mtu: u32) -> BoxFuture<'_, Result<(), NetError>>;
    fn add_addr(&self, addr: InterfaceAddr) -> BoxFuture<'_, Result<(), NetError>>;
    fn add_route(&self, prefix: IpPrefix) -> BoxFuture<'_, Result<(), NetError>>;
    fn remove_route(&self, prefix: IpPrefix) -> BoxFuture<'_, Result<(), NetError>>;
    /// Waits for the next packet the host sends through the interface,
    /// copies it to the front of `buf`, and returns its length.
    fn read<'a>(&'a self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize, NetError>>;
    /// Hands one IP packet to the host as if it arrived on the interface.
    fn write<'a>(&'a self, packet: &'a [u8]) -> BoxFuture<'a, Result<(), NetError>>;
    /// Removes the interface, with its addresses and routes, and completes
    /// once `interfaces` no longer lists it.
    fn remove(self: Box<Self>) -> BoxFuture<'static, Result<(), NetError>>;
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
    /// Adds exactly one component, which holds no `/` (clause 3.1.10).
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
    /// `rename` of a file over another in one directory, durable when it
    /// returns (clause 3.1.16).
    fn replace<'a>(&'a self, from: &'a RtPath, to: &'a RtPath)
        -> BoxFuture<'a, Result<(), DiskError>>;
    fn sync_dir<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>>;
    /// Creates a file that does not exist, protected as `protection` says,
    /// and returns it open with `Access::ReadWrite` (clause 3.1.15).
    fn create_file<'a>(&'a self, path: &'a RtPath, protection: Protection)
        -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>>;
    /// `create_dir`, protected as `protection` says (clause 3.1.15).
    fn create_dir_protected<'a>(&'a self, path: &'a RtPath, protection: Protection)
        -> BoxFuture<'a, Result<(), DiskError>>;
    /// The owner and permissions of the entry at `path`. A symbolic link or
    /// reparse point at the last component is not followed (clause 3.1.15).
    fn security<'a>(&'a self, path: &'a RtPath)
        -> BoxFuture<'a, Result<Security, DiskError>>;
    /// Sets the permission bits, `chmod`. `Unsupported` on Windows
    /// (clause 3.1.15).
    fn set_mode<'a>(&'a self, path: &'a RtPath, mode: u32)
        -> BoxFuture<'a, Result<(), DiskError>>;
    /// The path that names the absolute native path `native` in this
    /// instance. `Unsupported` under `mem` (clause 3.1.15).
    fn rt_path(&self, native: &str) -> Result<RtPath, DiskError>;
}

/// How `create_file` and `create_dir_protected` protect what they create
/// (clause 3.1.15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protection {
    /// As `open` and `create_dir` create.
    Default,
    /// Linux and macOS: mode `0o600` for a file, `0o700` for a directory.
    /// Windows: owner `Administrators` and no list of its own, so its list
    /// is what the parent passes on.
    Private,
    /// Linux and macOS: as `Private`. Windows: owner `Administrators`, and a
    /// list protected from inheritance that grants full control to `SYSTEM`
    /// and `Administrators` only.
    PrivateRoot,
}

/// What `security` reports for one entry (clause 3.1.15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Security {
    /// Linux, macOS, and `mem`: what `lstat` reports.
    Unix {
        /// `Other` for a symbolic link.
        kind: EntryKind,
        symlink: bool,
        /// The owner's user id.
        uid: u32,
        /// `st_mode & 0o7777`.
        mode: u32,
    },
    /// Windows.
    Windows {
        /// `Dir` when the entry has the directory attribute, else `File`.
        kind: EntryKind,
        /// `FILE_ATTRIBUTE_REPARSE_POINT`: a symbolic link, junction, or
        /// mounted folder.
        reparse_point: bool,
        owner: Sid,
        /// `SE_DACL_PROTECTED`: the list does not inherit from the parent.
        dacl_protected: bool,
        /// The discretionary access control list in its order. `None` for a
        /// null list, which grants everyone everything.
        dacl: Option<Vec<Ace>>,
    },
}

/// A Windows security identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sid {
    /// String form, such as `S-1-5-18`.
    pub sid: String,
    /// `DOMAIN\name` as `LookupAccountSidW` gives it, for messages. Empty
    /// when it gives none.
    pub name: String,
}

/// One access control entry of a discretionary list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ace {
    /// `false` for an access-denied type, `true` for every other type.
    pub allow: bool,
    pub trustee: Sid,
    pub mask: u32,
    /// `INHERIT_ONLY_ACE`: applies only to what the entry holds.
    pub inherit_only: bool,
    /// `INHERITED_ACE`: came from the parent.
    pub inherited: bool,
    /// `OBJECT_INHERIT_ACE`: files created in the entry inherit it.
    pub object_inherit: bool,
    /// `CONTAINER_INHERIT_ACE`: directories created in the entry inherit it.
    pub container_inherit: bool,
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

/// Writes `IoBuf { len: <len()> }`, never the bytes (section 2.1).
impl fmt::Debug for IoBuf { /* ... */ }

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
    /// The deadline is fixed at the call (clause 3.1.14).
    pub fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
    /// The deadline is fixed at the call, not at the first poll
    /// (clause 3.1.14).
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

/// Registers `os` for the five runtime kinds and for `host` (section 2.8).
/// The factories use `host`.
pub fn register_os(builder: &mut RegistryBuilder, host: &OsHost)
    -> Result<(), RegistryError>;
```

`alloy-sim` registers `mem` for the five runtime kinds and for `host` in the
registry of each simulated host, with factories bound to that host. Its world and scenario interface is
section 2.7.

### 2.7 Interface of `alloy-sim`

The items below are the whole public interface of `alloy-sim` that scenarios,
tests, and `alloy-natlab` use. Items marked private are the developer's choice.
`alloy-sim` depends on `alloy-rt`, `alloy-seam`, and `alloy-crypto`. `World`,
`History`, and every config type are `Send`, and `World` and `History` are also
`Sync`. Error types follow the rules of section 2.1. Every world time is a
`Duration` since the run started.

Runs:

```rust
use alloy_rt::{Arch, BoxFuture, Host, InterfaceAddr, Platform, RtPath, Runtime, WallTime};
use alloy_seam::{Detail, RegistryBuilder};
use std::fmt;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunConfig {
    pub seed: u64,
    /// The world time at which an unfinished run ends with `Liveness`.
    /// 1 ns to 2^63 - 1 ns. Default 3,600 s.
    pub budget: Duration,
    /// Every host's wall clock at world time 0, before the host's own
    /// offset. Default 1,800,000,000 s after the Unix epoch.
    pub wall_start: WallTime,
    /// Where the event trace is streamed. Default `None`: no file.
    pub trace_file: Option<PathBuf>,
    /// The trace file cap in bytes, 16 or more. Default 1,073,741,824.
    pub trace_cap: u64,
}

impl RunConfig {
    /// `seed`, with every other field at its default.
    pub fn new(seed: u64) -> RunConfig;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunResult {
    Pass,
    Fail,
    Liveness,
}

/// Why a scenario failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure(pub Detail);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSummary {
    pub seed: u64,
    pub trace_hash: [u8; 32],
    /// Records in the trace, `run-end` included.
    pub events: u64,
    /// The world time of the `run-end` record.
    pub virtual_time: Duration,
    pub result: RunResult,
    /// The first cause of `Fail` or `Liveness` (clause 3.2.8). `None` for
    /// `Pass`.
    pub failure: Option<Detail>,
}

impl RunSummary {
    /// The line of clause 3.2.6, without its newline.
    pub fn line(&self) -> String;
}

/// Runs one scenario on the calling thread until the run ends (clause
/// 3.2.7), writes the summary line to standard error, and returns it.
pub fn run<F, Fut>(config: RunConfig, scenario: F) -> RunSummary
where
    F: FnOnce(World) -> Fut,
    Fut: Future<Output = Result<(), Failure>> + Send + 'static;
```

The world:

```rust
/// One run's world. Clones share it.
#[derive(Clone)]
pub struct World(/* private */);

impl fmt::Debug for World { /* the world time and the host count */ }

impl World {
    pub fn now(&self) -> Duration;
    /// Completes at the first world instant at or after `t`.
    pub fn sleep_until(&self, t: Duration) -> BoxFuture<'static, ()>;
    pub fn sleep(&self, d: Duration) -> BoxFuture<'static, ()>;
    /// The next value of the scenario's random stream (clause 3.2.7).
    pub fn random_u64(&self) -> u64;
    /// Writes a `note` record.
    pub fn note(&self, text: &str) -> Result<(), WorldError>;

    pub fn add_network(&self, config: NetworkConfig) -> Result<NetworkId, WorldError>;
    /// Creates the host, attaches its interfaces and devices, and starts it.
    pub fn add_host(&self, config: HostConfig) -> Result<HostId, WorldError>;
    /// Returns the new interface's index.
    pub fn attach(&self, host: HostId, config: InterfaceConfig) -> Result<u32, WorldError>;
    pub fn detach(&self, host: HostId, index: u32) -> Result<(), WorldError>;
    pub fn set_up(&self, host: HostId, index: u32, up: bool) -> Result<(), WorldError>;
    pub fn host_state(&self, host: HostId) -> Result<HostState, WorldError>;
    /// The runtime of the host's current start. `State` while it is crashed.
    pub fn runtime(&self, host: HostId) -> Result<Runtime, WorldError>;

    /// Applies `fault` now.
    pub fn apply(&self, fault: Fault) -> Result<(), WorldError>;
    /// Applies `fault` at world time `at`, at or after now.
    pub fn schedule(&self, at: Duration, fault: Fault) -> Result<(), WorldError>;

    pub fn observe(&self, observer: Box<dyn Observer>) -> Result<(), WorldError>;
    /// A new, empty history.
    pub fn history<Op, Ret>(&self) -> History<Op, Ret>
    where
        Op: Clone + Send + 'static,
        Ret: Clone + Send + 'static;
}

/// Ids in creation order from 0 within a world. A `HostId` is the `host`
/// field of trace records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NetworkId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostState {
    Running,
    Stalled,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorldError {
    #[error("no such {0}")]
    Unknown(Detail),
    #[error("invalid argument: {0}")]
    Invalid(Detail),
    #[error("not allowed in the current state: {0}")]
    State(Detail),
    #[error("limit reached: {0}")]
    Limit(Detail),
    #[error("called from an observer or a middlebox")]
    Busy,
    #[error("the run has ended")]
    Ended,
}
```

Networks and links:

```rust
#[derive(Debug)]
pub struct NetworkConfig {
    /// At most one IPv4 and one IPv6 prefix, each with its host bits zero.
    pub prefixes: Vec<NetPrefix>,
    pub link: LinkConfig,
    /// `None` for a root network.
    pub uplink: Option<Uplink>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetPrefix {
    pub prefix: InterfaceAddr,
    /// The parent network routes this prefix down through the router.
    pub routable: bool,
}

/// The router between a network and its parent.
#[derive(Debug)]
pub struct Uplink {
    pub parent: NetworkId,
    /// The router's addresses on the parent, at most one per family.
    pub outside: Vec<IpAddr>,
    /// The router's addresses on this network, at most one per family.
    pub inside: Vec<IpAddr>,
    /// `None` forwards packets unchanged.
    pub middlebox: Option<Box<dyn Middlebox>>,
}

/// Parts per million, 0 to 1,000,000.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ppm(/* private: u32 */);

impl Ppm {
    pub const ZERO: Ppm;
    pub const ALWAYS: Ppm;
    /// `Invalid` above 1,000,000.
    pub fn new(ppm: u32) -> Result<Ppm, WorldError>;
    pub fn get(self) -> u32;
}

/// What every packet crossing a network gets (clause 3.3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkConfig {
    /// 0 to 3,600 s. Default 1 ms.
    pub latency: Duration,
    /// 0 to 3,600 s. Default 0.
    pub jitter: Duration,
    pub loss: Ppm,
    pub duplicate: Ppm,
    pub corrupt: Ppm,
    pub reorder: Ppm,
    /// 0 to 3,600 s. Default 10 ms.
    pub reorder_delay: Duration,
    /// Bytes per second per sender, 0 for unlimited. Default 0.
    pub bandwidth: u64,
    /// Bytes per sender waiting for bandwidth. 1,500 to 1,073,741,824.
    /// Default 1,048,576.
    pub queue: u32,
    /// The largest IP packet that crosses. 576 to 65,535, and 1,280 or more
    /// on a network with an IPv6 prefix. Default 1,500.
    pub mtu: u32,
    /// Default true.
    pub icmp_too_big: bool,
}

impl Default for LinkConfig { /* the defaults above, every Ppm ZERO */ }
```

Hosts:

```rust
/// Builds the host's software at each start. Its future runs as the
/// host's first task of that start, named `boot`.
pub type Boot = Arc<dyn Fn(HostBoot) -> BoxFuture<'static, ()> + Send + Sync>;

pub struct HostBoot {
    pub host: HostId,
    /// 1 at the first start, plus 1 at each restart.
    pub start: u32,
    /// The five `mem` instances of this start.
    pub runtime: Runtime,
    /// The host's `mem` instance of `host` (clause 3.1.18). One instance
    /// per host, shared by every start, so its service state outlives a
    /// crash as clause 3.1.18.3 says. The registry's `host` factory returns
    /// this same instance.
    pub machine: Arc<dyn Host>,
    /// Holds `mem` for the five runtime kinds and for `host`, bound to this
    /// host.
    pub registry: RegistryBuilder,
}

impl fmt::Debug for HostBoot { /* host and start only */ }

pub struct HostConfig {
    pub boot: Boot,
    /// Attached in order before the first start. Default empty.
    pub interfaces: Vec<InterfaceConfig>,
    /// Default empty.
    pub devices: Vec<DeviceConfig>,
    /// The clock rate offset at creation. -500,000 to 500,000. Default 0.
    pub clock_ppm: i32,
    /// Nanoseconds added to `RunConfig::wall_start` for this host.
    /// Default 0.
    pub wall_offset: i64,
    /// Bytes of the file namespace (clause 3.3.7). Default 1,073,741,824.
    pub capacity: u64,
    /// `block_size` of the file namespace, 512 or 4,096. Default 4,096.
    pub block_size: u32,
    /// What the host's `host` instance reports (clause 3.1.18).
    pub machine: MachineConfig,
}

impl HostConfig {
    /// `boot`, with every other field at its default.
    pub fn new(boot: Boot) -> HostConfig;
}

impl fmt::Debug for HostConfig { /* every field but `boot` */ }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceConfig {
    pub network: NetworkId,
    /// 1 to 64 addresses, each inside a prefix of `network`.
    pub addrs: Vec<IpAddr>,
    /// 576 to 65,535. Default 1,500.
    pub mtu: u32,
    /// Default true.
    pub up: bool,
}

impl InterfaceConfig {
    /// `network` and `addrs`, with every other field at its default.
    pub fn new(network: NetworkId, addrs: Vec<IpAddr>) -> InterfaceConfig;
}

/// A block device, all zeros at creation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceConfig {
    pub path: RtPath,
    /// A multiple of `block_size`, from `block_size` to 2^40.
    pub len: u64,
    /// 512 or 4,096.
    pub block_size: u32,
}

/// The machine a `mem` `host` instance describes (clause 3.1.18). The
/// disks it reports are the host's devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineConfig {
    /// `None` makes `facts` return `Unsupported`. Default
    /// `Some(Platform::Linux { release: "6.1.0".into(), major: 6, minor: 1 })`.
    pub platform: Option<Platform>,
    /// Default `Arch::X86_64`.
    pub arch: Arch,
    /// Default true.
    pub privileged: bool,
    /// 1 or more. Default 4.
    pub cores: u32,
    /// Bytes. Default 8,589,934,592.
    pub memory: u64,
    /// Default true.
    pub virtualization: bool,
    /// Whether the service `alloyd` is installed. Default true.
    pub service: bool,
}

impl Default for MachineConfig { /* the defaults above */ }
```

Faults (clause 3.3):

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Replaces the network's link configuration.
    Link { network: NetworkId, config: LinkConfig },
    /// Blocks traffic from the hosts in `from` to the hosts in `to`, and
    /// from `to` to `from` when `both_ways`.
    Partition { id: u32, from: Vec<HostId>, to: Vec<HostId>, both_ways: bool },
    Heal { id: u32 },
    /// Crashes the host right after its `after_polls`-th task poll counted
    /// from now, or at once for 0.
    Crash { host: HostId, after_polls: u32, unsynced: Unsynced },
    /// Starts a crashed host. With `Some(s)`, its `rng` instances draw the
    /// streams they drew at start `s`.
    Restart { host: HostId, rng_replay: Option<u32> },
    /// Sets the clock rate offset in parts per million.
    ClockRate { host: HostId, ppm: i32 },
    /// Moves the wall clock by `step_ns`.
    ClockStep { host: HostId, step_ns: i64 },
    Stall { host: HostId, length: Duration },
    /// Replaces the read fault rates of `target`.
    Disk { host: HostId, target: DiskTarget, read_error: Ppm, corrupt_read: Ppm },
    /// Sets the capacity of the file namespace.
    Capacity { host: HostId, bytes: u64 },
}

/// What a crash does to writes and namespace changes that no completed
/// sync covers (clause 3.3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsynced {
    Lost,
    Seeded,
    Torn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiskTarget {
    /// Every regular file of the host.
    Files,
    Device(RtPath),
}
```

Hooks for checkers (clause 3.6) and middleboxes (clause 3.3.9):

```rust
/// Sees every trace record as it is written.
pub trait Observer: Send {
    fn record(&mut self, record: &Record<'_>);
}

#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    pub time: Duration,
    /// `None` for `FFFFFFFF`.
    pub host: Option<HostId>,
    pub kind: u16,
    /// The body of section 4.1.
    pub body: &'a [u8],
    /// The bytes the record's `digest` covers. Empty when it has none.
    pub data: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub u64);

/// One invoked operation. Not `Clone`, so it completes at most once.
#[derive(Debug, PartialEq, Eq)]
pub struct Call(/* private */);

/// A log of operations with their world times. Clones share the log.
pub struct History<Op, Ret>(/* private */);

impl<Op, Ret> Clone for History<Op, Ret> { /* shares the log */ }

impl<Op: Clone + Send + 'static, Ret: Clone + Send + 'static> History<Op, Ret> {
    pub fn invoke(&self, client: ClientId, op: Op) -> Result<Call, WorldError>;
    pub fn complete(&self, call: Call, ret: Ret);
    /// Every entry, in invocation order.
    pub fn entries(&self) -> Vec<Entry<Op, Ret>>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry<Op, Ret> {
    pub client: ClientId,
    pub op: Op,
    pub invoked: Duration,
    /// `None` while the call has not completed.
    pub completed: Option<(Duration, Ret)>,
}

pub trait Middlebox: Send + fmt::Debug {
    fn process(&mut self, ctx: &mut MiddleboxCtx, packet: &Packet) -> Verdict;
}

pub struct MiddleboxCtx(/* private */);

impl MiddleboxCtx {
    pub fn now(&self) -> Duration;
    /// The next value of this router's random stream.
    pub fn random_u64(&mut self) -> u64;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Udp,
    /// A stream connection request.
    Tcp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Inside,
    Outside,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub proto: Proto,
    /// The side the packet reached the router from.
    pub from: Side,
    pub src: SocketAddr,
    pub dst: SocketAddr,
    /// UDP payload bytes; 0 for `Tcp`.
    pub len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Forward { to: Side, src: SocketAddr, dst: SocketAddr },
    Drop,
}
```

`Observer` and `Middlebox` are plug-in traits of the simulator, not seams: they
are not registered and have no `mem` or `os` implementation.

### 2.8 Host

The `host` seam is the machine itself: the facts that ARFC 0002 section 3.8
checks, free space at a path, and control of the one service Alloy owns,
`alloyd` (ARFC 0017 clause 3.17). It is part of the interface of `alloy-rt`
but not of `Runtime`: `alloy init` and `alloy join` build an instance with
`Registry::instantiate::<HostSeam>`, context `()`, beside their `Runtime`.
Clause 3.1.18 gives its behavior, and ADR 0071 records it.

```rust
use alloy_seam::Detail;

/// Seam marker (ARFC 0001 section 2.1). `Object = dyn Host`,
/// `KIND = "host"`, `Context = ()`, `SWAP = SwapClass::Restart`.
#[derive(Debug)]
pub struct HostSeam;

/// Seam `host`.
pub trait Host: Send + Sync + fmt::Debug + 'static {
    /// Facts about the machine (clause 3.1.18.1).
    fn facts(&self) -> BoxFuture<'_, Result<HostFacts, HostError>>;
    /// Bytes free to this process on the file system that holds the
    /// absolute native path `path`, or its nearest existing ancestor
    /// (clause 3.1.18.2).
    fn free_space<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<u64, HostError>>;
    /// The state of the service `alloyd` (clause 3.1.18.3).
    fn service_status(&self) -> BoxFuture<'_, Result<ServiceStatus, HostError>>;
    /// Applies `action` to the service `alloyd`. No method names any other
    /// service (clause 3.1.18.3).
    fn service(&self, action: ServiceAction) -> BoxFuture<'_, Result<(), HostError>>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFacts {
    pub platform: Platform,
    pub arch: Arch,
    /// Root on Linux, an elevated token on Windows.
    pub privileged: bool,
    /// Online logical processors, 1 or more.
    pub cores: u32,
    /// Bytes of physical memory.
    pub memory: u64,
    /// Hardware virtualization is usable.
    pub virtualization: bool,
    /// Whole-disk block devices, sorted by `path` ascending bytewise. At
    /// most 32.
    pub disks: Vec<DiskFact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Platform {
    /// `release` is the kernel release, at most 64 bytes. `major` and
    /// `minor` are its leading numbers, 0 and 0 when it has none.
    Linux { release: String, major: u32, minor: u32 },
    /// The version Windows reports for itself, such as 10.0.26100.
    Windows { major: u32, minor: u32, build: u32 },
}

/// The native architecture of the operating system, not of the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
    /// Any other, named as clause 3.1.18.1 says. 1 to 32 bytes.
    Other(String),
}

impl Arch {
    /// `x86_64`, `aarch64`, or the `Other` text.
    pub fn as_str(&self) -> &str;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskFact {
    /// The native path of the device, such as `/dev/nvme0n1`. At most
    /// 255 bytes.
    pub path: String,
    /// Bytes.
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceStatus {
    /// The service starts with the machine.
    pub enabled: bool,
    pub run: RunState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Stopped,
    Starting,
    Running,
    Stopping,
}

/// Numbered from 1 in this order for the event trace (section 4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Enable,
    Disable,
    Start,
    Stop,
}

/// Variants are numbered from 1 in this order for the event trace
/// (section 4.1); `Os` is 255.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error("permission denied")]
    PermissionDenied,
    #[error("the service alloyd is not installed")]
    NotInstalled,
    #[error("the service alloyd is disabled")]
    Disabled,
    #[error("the service manager refused: {0}")]
    Refused(Detail),
    #[error("the service manager did not finish within 60 s")]
    TimedOut,
    #[error("invalid input: {0}")]
    InvalidInput(Detail),
    #[error("not supported on this platform")]
    Unsupported,
    /// An operating system error with no variant above. `os` only.
    #[error("operating system error {code}")]
    Os { code: i32 },
}
```

## 3. Behavior

### 3.1 Runtime

3.1.1 Only implementations of these seams call the operating system for time,
network, storage, randomness, or concurrency, or to read machine facts or
control a service (clause 3.1.18). `cargo xtask check-repo` rejects those
calls elsewhere.

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
  `h0 = 0` at its start instant and the offset in force for it then. The
  formula is evaluated in 128-bit integer arithmetic, so it never overflows over
  the whole range of world time, and a result above `Instant::MAX` gives
  `Instant::MAX`.
- `mem`: a timer of `sleep_until(d)` fires at the first world nanosecond at which
  the host's `now()` is at least `d`, under whatever offsets apply until then. A
  task woken by it reads, on that poll, the host time at that nanosecond, unless
  a CPU stall fault is active on its host. That is the smallest value at or above
  `d` that the host clock reaches. It is `d` itself whenever the offset has been
  at most 0 since the sleep began, because the clock then takes every integer
  value; with a positive offset the clock skips some values, and the task may
  read a value above `d`.
- `mem`: `wall` starts at the world's configured wall time, advances at the
  host's clock rate, and jumps by clock step faults, exactly as clause 3.3.5
  states.

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
  repeat explicitly to model a faulty generator (`rng_replay`, clause 3.3.4).
  Successive `fill` calls on one
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
- `mem` send buffer. A datagram sent through an interface other than `lo` and
  tun interfaces (clause 3.1.17.3) is **in flight** from the instant `send` hands it to the network until it and
  every copy made of it (clause 3.3.2) have been delivered or dropped. Each
  packet in flight, original or copy, counts its packet size (clause 3.3.2)
  against two bounds: the `send_buffer` of the socket that sent it, while that
  socket is open, and 67,108,864 bytes for its origin host, across socket drops,
  crashes, and restarts. A packet fits a bound when nothing counts against the
  bound, or when the bytes counted plus its size are at most the bound. `send`
  waits until its next datagram fits both bounds, and a copy that does not fit
  both is not made. A datagram sent over `lo` is delivered at once, and one
  sent through a tun interface queued at once, and neither waits. A datagram in flight when its socket is dropped or its host crashes
  still travels.
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
  of virtual time without an answer, and `AddrNotAvailable` when no source port
  is free (clause 3.3.1). A dropped `connect` future leaves no connection open.
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

3.1.9.1 `mem` connections. These rules fix when a `mem` connection end becomes
reset, so that no `write` waits forever on a connection whose peer can no longer
read.

- Route and delay. At the instant `t` it is sent, a TCP connection request is
  routed like a datagram (clause 3.3.1), and each middlebox on its route sees it
  once, as a `Tcp` packet, at `t`. No link property and no `Link` fault applies
  to it. Its reply goes back over the same networks. The route, and the
  addresses as the middleboxes translated them, are fixed for the life of the
  connection. The **delay** of a direction at an instant is the sum of the
  `latency` of every network on the route at that instant. A Unix connection, and
  a TCP connection over loopback, has delay 0.
- Setup. Let `d` be the delay at `t`. When `d` is below 15 s, a request that
  reaches a listener with room in its backlog is queued there at `t + d`, and
  `connect` completes at `t + 2d`; a request that reaches a running host with no
  listener on its address and port makes `connect` fail with `ConnectionRefused`
  at `t + 2d`. A request whose `d` is 15 s or more, that a partition blocks in
  either direction at `t + d`, that a middlebox drops, that reaches a crashed
  host or a full backlog, or that has no route, gets no answer: nothing is
  queued, and `connect` fails with `TimedOut` 30 s after `t`. Until its answer
  arrives or 30 s pass, a request counts as an open stream of the requesting
  instance (clause 3.1.9), also after its `connect` future is dropped.
- Pending ends. A request that is queued at a listener has a **pending end** on
  the requesting host from `t + d` until `t + 2d`, also after its `connect`
  future is dropped. A crash of the requesting host ends it and discards the
  segments it holds, which frees their space. A segment that arrives at a pending end is **held**: kept, in order,
  and still counted against the buffer of its direction. When `connect`
  completes at `t + 2d`, the pending end becomes the live end of the returned
  `Stream`, and every held segment arrives at it at that instant, in order,
  under the rules below. A segment that arrives at `t + 2d` has the same outcome
  whether the scheduler processes its arrival before or after the completion
  (clause 3.2.2). The accepting end exists only from `t + d`, so each byte it
  sends is due at `t + d` or later, and a held byte is delivered less than 15 s
  after it was due.
- Abandoned setup. When a request was queued but its `connect` future has been
  dropped by `t + 2d`, by the caller or by a crash, the requesting host, if it is
  running at `t + 2d`, sends a reset then, as the only segment of its direction,
  and discards every segment its pending end holds, which frees their space. This
  reset is the one reset of that direction: a segment that arrives at `t + 2d` is
  held and discarded with the others, or arrives after the pending end has ended
  and draws no second reset (Arrival), whichever the scheduler processes first.
  A crashed host sends nothing.
- Segments. Each direction carries, in order, **segments**: the bytes of each
  `write`, then at most one end of stream, then at most one reset. Nothing
  follows a reset. A segment sent at `t` is due at the later of `t + d` and the
  due time of the segment before it in that direction, where `d` is the delay at
  `t`. It arrives when it is due, unless it is **blocked** then: a partition
  blocks the direction, the receiving host is crashed, or the sending end's local
  address is no longer on an up interface of its host. A blocked segment arrives
  at the first instant it is no longer blocked, still in order. Jitter, loss,
  duplication, reordering, corruption, bandwidth, and MTU do not apply to
  segments.
- Live ends. An end is **live** from the instant it is queued in a backlog, or
  its `connect` completes, until it becomes reset, its host drops its `Stream`,
  the listener holding it in a backlog is dropped, or its host crashes.
- Arrival. Bytes or an end of stream that arrive at a live end are
  **delivered**: queued for `read` in order. A segment that arrives at a pending
  end is held (Pending ends). A segment that arrives at an end that is neither
  live nor pending, or at a host that holds no end for the connection (because
  the host has restarted since the connection was set up, or the setup was
  abandoned), is discarded, and its bytes free their space at once. For
  a discarded byte or end of stream, the receiving host sends a reset back,
  unless a reset was already sent in the direction back. A reset that arrives
  where no live end is has no effect.
- Drop. When a host drops the `Stream` of a live end, the end sends, after every
  byte it wrote, a reset when bytes are queued unread at it or in flight toward
  it, and those bytes are discarded; otherwise it sends an end of stream unless
  it already sent one. When a listener is dropped, each end in its backlog sends
  a reset. Dropping an end that is not live sends nothing.
- Reset. A live end becomes reset at the instant a reset segment arrives at it,
  and at the instant 60 s after the due time of a byte it sent that has not been
  delivered by then. A reset end discards its unread bytes and every segment it
  sent that has not arrived, and its pending and later `read` and `write` fail
  with `ConnectionReset`. A reset end in a backlog is removed from the backlog.
  A held reset makes the new live end reset at the instant `connect` completes:
  `connect` still returns the `Stream`, and its `read` and `write` fail with
  `ConnectionReset`. A byte is due only after the route's delay, a held byte is
  delivered less than 15 s after it was due, and the accepting end's segments
  cannot arrive before its peer's pending end exists. So latency alone never
  fires this rule and never makes a segment arrive where no live or pending end
  is: blocking, crashes, and dropped, reset, or abandoned ends can.
- Crash. The ends of a crashed host vanish without sending anything. Segments
  they sent before the crash still arrive.
- After the peer's end of stream arrives, `read` returns every byte before it and
  then 0, and `write` still succeeds until a reset arrives.

3.1.9.2 Peer credentials. `peer_uid` tells the local admin socket of ARFC 0007
clause 3.10.2 who is calling. It reads and writes nothing on the stream and
writes no trace record.

- Under `os` on Linux, on a stream to or from a `Unix` address, it returns the
  `uid` field of the `struct ucred` that `getsockopt` with `SOL_SOCKET` and
  `SO_PEERCRED` gives for the stream's socket. That is the effective user id
  the peer process had when it called `connect`, for a stream that `accept`
  returned, or `listen`, for a stream that `connect` returned
  (<https://man7.org/linux/man-pages/man7/unix.7.html>, `SO_PEERCRED`). It is
  fixed when the connection is made: the peer changing its user id later, or
  passing the descriptor to another process, does not change it. A failing
  `getsockopt` returns `Os` with its error number.
- On a TCP stream, under every implementation, it returns `InvalidInput`.
- Under `os` on every other platform it returns `Unsupported`. None needs it: a
  local admin exists only on a node that holds `control` (ARFC 0007 clause
  3.10.3), every Windows node is a client node with no roles (ARFC 0002 section
  3.3), so a Windows `alloyd` never serves a local admin socket, and macOS runs
  no `alloyd` (ARFC 0019 section 2).
- Under `mem` it returns 0 on both ends of every Unix stream, the user id that
  owns every `mem` entry (clause 3.1.15). A scenario cannot make a peer of
  another user, so a refusal of a user id other than 0 is tested under `os`.
- The `os` suite on Linux opens a `Unix` listener, connects to it, and checks
  that `peer_uid` on both ends equals the effective user id of the test
  process, and that a TCP stream returns `InvalidInput`.

3.1.10 Disk paths and the namespace.

- `RtPath::new` accepts exactly the strings that start with `/`, are `/` alone or
  have components of 1 to 255 bytes separated by single `/` with no trailing `/`,
  have no component `.` or `..`, contain no NUL and no `\`, and are at most 4,096
  bytes. Anything else returns `InvalidPath`.
- `join(c)` adds exactly one component. It accepts `c` when `c` is 1 to 255 bytes,
  is not `.` or `..`, and contains no `/`, no NUL, and no `\`, and returns the
  path followed by `/` and `c`, or `/` and `c` for the root. It returns
  `InvalidPath` when `c` breaks one of these rules, `/` included, and when the
  result would be longer than 4,096 bytes. So for every path `p` and every `c`
  it accepts, `p.join(c)` has `parent()` equal to `Some(p)` and `file_name()`
  equal to `Some(c)`. A path of several components is built with `new` or with
  one `join` per component.
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

`mem` implements exactly this contract. Which of the allowed outcomes a crash
produces is set by the crash fault's `unsynced` mode (clause 3.3.4). `os` relies
on the platform for it.

3.1.13 `IoBuf`. The first byte of every `IoBuf` is at an address that is a
multiple of 4,096. `zeroed` and `from_slice` return `InvalidInput` above
67,108,864 bytes. Its `Debug` output is exactly `IoBuf { len: <len()> }`
(section 2.1).

3.1.14 Runtime.

- `from_registry` builds the five instances in the order clock, rng, spawn, net,
  disk-io, each with context `()`. On the first failure it drops what it built and
  returns that error.
- `Runtime::spawn` spawns through the `spawn` instance, and `JoinHandle::join`
  returns the task's output or the `JoinError` of clause 3.1.6.
- `sleep(d)` reads `clock.now()` once, when it is called, and is
  `clock.sleep_until(now.saturating_add(d))` with that `now`. Its deadline is fixed
  at the call.
- `timeout(d, f)` starts its timer at the call, not at its first poll: it reads
  `clock.now()` once, when it is called, and its deadline is
  `now.saturating_add(d)`. On every poll it polls `f` first and returns `Ok` with
  `f`'s output when `f` is ready. Only otherwise does it poll the timer, and when
  the timer has completed it drops `f` and returns `Err(Elapsed)`. So a `timeout`
  first polled at or after its deadline returns `Ok` when `f` is ready on that
  poll and `Elapsed` otherwise, and `timeout(Duration::ZERO, f)` returns `Elapsed`
  on its first poll unless `f` is ready then. The timer starts at the call
  because the caller measures the limit from the moment it asks: a future
  that is stored before it is awaited, or polled late by a busy scheduler, must
  not gain time, and `sleep`, whose deadline is fixed at the call, behaves the
  same way.
- `register_os` registers `os` under the five runtime kinds and `host`, and
  fails as `RegistryBuilder::register` does. Each `os` factory accepts the
  configuration of section 4.2. `from_registry` builds no `host` instance.

3.1.15 Disk ownership and permissions. These operations give ARFC 0002 section
3.12 and ARFC 0006 clause 5.2 what they need: creating an entry already
protected, reading an entry's owner and permissions, setting a mode, and
naming a native path.

- `create_file(path, p)` is `open` with `Access::ReadWrite`, `Create::New`, and
  `direct` and `exclusive` false, and `create_dir_protected(path, p)` is
  `create_dir`, with the same results and errors, except for the protection
  they give the new entry. With `Protection::Default` they are exactly those
  calls. With `Private` or `PrivateRoot`, the operating system call that
  creates the entry carries its protection, so the entry never exists with
  more access than this clause gives it. A call that cannot give the
  protection returns the error and leaves nothing at `path`.
- Linux and macOS, under `os`. `Private` and `PrivateRoot` give a file
  `st_mode & 0o7777` of exactly `0o600`, and a directory exactly `0o700`, when
  the call returns, whatever bits the umask removes or the parent adds, such as
  the set-group-ID bit that Linux `mkdir` copies from a set-group-ID parent
  (<https://man7.org/linux/man-pages/man2/mkdir.2.html>, NOTES). `os` passes
  that mode to the call that creates the entry, so the entry never has more
  access bits than it, then opens the new entry (a directory with
  `O_DIRECTORY` and `O_NOFOLLOW`), reads its mode through that descriptor, and
  sets the mode through the same descriptor when it differs. When that fails,
  `os` removes the entry and returns the error. The owner is the process's
  effective user id. `Default` gives what `open` and `create_dir` give, mode
  `0o666` or `0o777` less the umask. The conformance suite covers a parent
  with the set-group-ID bit and a umask of `0o077` and of `0o000`.
- Windows, under `os`. `Private` and `PrivateRoot` name the owner
  `Administrators` (`S-1-5-32-544`) in the security descriptor passed to the
  creating call. A process may name as owner a group its token holds with
  `SE_GROUP_OWNER`
  (<https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-token_groups>);
  when its token does not, the call returns `PermissionDenied` and leaves
  nothing at `path`. `alloy` and `alloyd` run from an elevated administrator
  or as `LocalSystem` (ARFC 0002 section 3.8, ARFC 0017 clause 3.15).
  - `Private`: the descriptor names the owner and holds no list
    (`SE_DACL_PRESENT` clear). So the entry's list is exactly the entries the
    parent passes on to an entry of its kind, in the parent's order, each
    `inherited`, and `dacl_protected` is false. An entry with
    `object_inherit` and `container_inherit` passes itself on again from a
    `Private` directory, so every `Private` entry at any depth below a
    `PrivateRoot` directory holds that directory's two entries. In a parent
    that passes on nothing, the list is the creating token's default list;
    ARFC 0002 section 3.12 creates no `Private` entry in such a parent. A
    descriptor that holds an empty list must not be used: Windows stores it as
    an empty, protected list, which grants no one any access. Source:
    `CreateFileW` and `CreateDirectoryW` on NTFS, Windows 10.0.26300, with
    descriptors from `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
    In a parent `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;U)`, the
    descriptor `O:U` gives a file `D:(A;ID;FA;;;SY)(A;ID;FA;;;BA)(A;ID;FA;;;U)`
    and a directory the same entries with `OICIID`, a file in that directory
    the same as the first, and `O:UD:` gives `D:P`. In a parent whose entries
    pass on nothing, `O:U` gives the token's default list. The `os`
    conformance suite repeats these cases on the Windows CI host.
  - `PrivateRoot`: the descriptor sets `SE_DACL_PROTECTED` and holds exactly
    two entries, in this order: allow `SYSTEM` (`S-1-5-18`) and allow
    `Administrators`, each with mask `0x001F01FF` (`FILE_ALL_ACCESS`), neither
    `inherited` nor `inherit_only`, and on a directory each with
    `object_inherit` and `container_inherit`, on a file with neither. So
    `icacls` lists only `SYSTEM` and `Administrators`, each with full control
    and neither inherited, and a `Private` entry created in that directory
    holds both, inherited.
  - `Default` passes no descriptor, as `open` and `create_dir` do.
- `security(path)` under `os` on Linux and macOS is `Security::Unix`, from
  `lstat` of the path: for a symbolic link at the last component, `symlink`
  true, `kind` `Other`, and the link's own `uid` and `mode`; otherwise
  `symlink` false and `kind` as `metadata` gives it. Components above the last
  are followed (clause 3.1.10).
- `security(path)` under `os` on Windows is `Security::Windows`, read from the
  entry opened without following a reparse point at the last component. An
  entry's `allow` is false for the types `ACCESS_DENIED_ACE_TYPE`,
  `ACCESS_DENIED_OBJECT_ACE_TYPE`, `ACCESS_DENIED_CALLBACK_ACE_TYPE`, and
  `ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE`, and true for every other type, so
  a type the reader does not know counts as a grant. `mask` is as stored, with
  no generic right mapped. The four flags are the `ACE_HEADER` flags of the
  same names
  (<https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-ace_header>).
  When the process may not read the entry's security descriptor, `security`
  returns `PermissionDenied`.
- `security` returns `NotFound` for a missing entry, and `InvalidInput` for a
  list of more than 4,096 entries.
- `set_mode(path, mode)` on Linux and macOS sets the permission bits of the
  entry at `path` to `mode`, following a symbolic link, and leaves its owner.
  A `mode` above `0o7777` returns `InvalidInput`. On Windows it returns
  `Unsupported`, since ARFC 0002 section 3.12 adopts no existing directory
  there.
- Durability of a mode. After a crash, a `set_mode` on a file is kept only
  when a `sync(All)` on a handle of that file completed after `set_mode`
  returned, and a `set_mode` on a directory only when a `sync_dir` of that
  directory itself completed after it. Nothing else makes it durable: not
  `sync(Data)`, and not a `sync_dir` of the parent. Under `os`, `sync_dir(d)`
  is `fsync` of a descriptor of `d`, which flushes `d`'s own metadata, its
  mode included (<https://man7.org/linux/man-pages/man2/fsync.2.html>), and
  durability rests on the platform as clause 3.1.12 says. The mode that
  `create_file` and `create_dir_protected` give is part of the create, which
  clause 3.1.12 makes durable by a `sync_dir` of the parent.
- `mem` follows the Linux rules above, with these values. Every entry is owned
  by user id 0 and none is a symbolic link. The root and every directory that
  `create_dir` creates have mode `0o755`, a file that `open` creates `0o644`,
  and a block device the scenario attaches `0o660`. `mem` refuses no
  operation because of a mode, as the operating system does not for user
  id 0. An entry's mode at creation is kept or undone by a crash with the
  create itself. A `set_mode` is kept or undone by a crash as clause 3.3.4
  says.
- `rt_path(native)` under `os` returns the `RtPath` that names the entry at
  the native path `native`: `/` followed by the components of `native` that
  come after the components of the configured `root` (section 4.2), joined by
  `/`, or `/` alone when none come after. Components are separated by `/` on
  Linux and macOS, and by `\` or `/` on Windows, and empty components are
  skipped. On Linux and macOS, `native` must start with `/`, and components
  are compared with `root`'s byte for byte. On Windows, `native` must start
  with a drive letter, `:`, and a separator, the drive is its first
  component, and components are compared with `root`'s ignoring ASCII case.
  It returns `InvalidPath` when `native` breaks these rules, holds a component
  `.` or `..`, does not begin with every component of `root`, or gives a
  path that `RtPath::new` refuses. It reads nothing from the disk and writes
  no trace record. Under `mem`, which has no native paths, it returns
  `Unsupported`.
- A library that creates and opens its own files outside `disk-io`, as the
  `native` stores open theirs through `redb` (ADR 0064, ARFC 0006 clause 5.2),
  gets the same protection this way. The caller uses the `disk-io` instance
  of the `Runtime` it is built with, which for a `native` store is its build
  context (ARFC 0006 section 5), and names the file `rt_path(dir)` joined
  with the file's name, where `dir` is the native path it was given. It calls
  `create_file` with `Protection::Private`. On `Ok` it completes `sync(All)`
  on the handle and `sync_dir` of the file's directory, then drops the
  handle. `AlreadyExists` means the file is there, and the caller goes on.
  Any other error is the caller's failure, and the library is not called.
  Only then does it let the library open the file. The library must open the
  existing file without truncating, replacing, or recreating it. `redb` 4.5.0
  `Database::create` qualifies: it opens with `create(true)` and
  `truncate(false)`, and initializes a database in an empty file
  (`redb-4.5.0/src/db.rs`, lines 2353 to 2361). The directory that holds the
  file is itself created `Private` (for the `native` stores of `alloyd`, by
  ARFC 0002 section 3.12), so on Linux and macOS a file that the library
  creates by itself is out of reach of other users whatever its mode, since
  its directory cannot be entered by them.

3.1.16 Durable replace. `replace(from, to)` writes a whole file durably for
ARFC 0002 section 3.14: the caller writes and syncs a new file, then replaces
the old one with it in one call that returns once the replacement is durable.

- It is `rename(from, to)`, with the same results and errors, except: `from`
  and `to` must be entries of the same directory, else it returns
  `InvalidInput` and changes nothing; `from` must be a regular file, and a
  directory there returns `IsDirectory` and any other kind `InvalidInput`; a
  directory at `to` returns `IsDirectory`. A file at `to` is replaced
  atomically.
- When it returns `Ok`, the rename is durable: after any later crash and
  restart, `to` names the file that `from` named and `from` names nothing.
  When it returns an error, a crash leaves either the state before the call or
  the renamed one, and a rename is never half done (clause 3.1.12).
- It makes no write to the file durable. A caller that needs the content
  durable completes a `sync` on a handle of `from` first.
- Under `os` on Linux and macOS it is `rename(2)` followed by `fsync` of a
  descriptor of the directory, as `sync_dir` does, and returns once both
  complete. When the rename succeeds and the `fsync` fails, it returns the
  `fsync` error, and the rename may or may not be durable.
- Under `os` on Windows it is one `MoveFileExW` call with the flags
  `MOVEFILE_REPLACE_EXISTING` and `MOVEFILE_WRITE_THROUGH`, which returns only
  once the file is moved on the disk
  (<https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw>).
- Under `mem` it is the rename and its durability as one operation that
  completes at one instant. At a crash after it completed, whatever the crash
  fault's `unsynced` mode (clause 3.3.4), the rename is in the durable state,
  together with the create of the file at `from` when that create was not, and
  no other operation is made durable by it: no write, and no other create,
  remove, or rename in the directory. A crash before it completed leaves the
  directory as if it had not been called.
- Its trace record is `disk-op` with operation 17 and handle 0 (section 4.1).

3.1.17 Layer 3 virtual interfaces. A **tun interface** is an interface made by
`create_tun`. Each IP packet the host sends through it is read by the holder of
its `Tun`, and each packet the holder writes the host receives as if it had
arrived on it. It carries bare IPv4 and IPv6 packets, with no link layer
header. ARFC 0002 section 3.4 uses one, `al0`, and ADR 0069 records the
choice.

- `create_tun(name, options)` creates the interface `name`, up, with MTU
  `options.mtu`, no address, and no route, and completes once `interfaces`
  lists it so. It fails with `InvalidInput` for a name that is not 1 to 15
  bytes of ASCII letters, digits, `-`, and `_`, for an MTU outside 1,280 to
  65,535, and on Windows for a `guid` of `None`; with `Conflict` when the host
  has an interface of that name, other than a Windows adapter that clause
  3.1.17.2 takes over; with `PermissionDenied` when the process
  lacks the privilege, `CAP_NET_ADMIN` on Linux and an elevated member of
  `Administrators` on Windows; with `TooManyOpen` when the instance holds 16
  tun interfaces; and with `Unsupported` on a platform other than Linux and
  Windows. A dropped `create_tun` future leaves behind no interface it created.
- The interface lasts until `remove` completes, its `Tun` is dropped, or, under
  `os`, the process ends, except a Windows adapter taken over, which clause
  3.1.17.2 says how long lasts. While it lasts, `interfaces` lists it with its
  `index`, `name`, and MTU, `up` true, `loopback` false, and the addresses
  `add_addr` gave it and no other, and each change to those is a change of the
  list (clause 3.1.7). Once it is gone, by these means or by another
  program's, pending and later calls on its `Tun` fail with `Closed`.
- `set_mtu` sets the MTU, and fails with `InvalidInput` outside 1,280 to
  65,535.
- A prefix **conflicts** with the host when it contains, or is contained by,
  the prefix of a route of prefix length 1 or more, in any routing table of the
  host, that does not go through this interface, or an address of another
  interface, taken as a prefix of length 32 or 128. `add_addr` and `add_route`
  check this at the call, which is the check of ARFC 0017 clause 3.17.3, and
  fail with `Conflict` naming one such route or address. The check and the
  change are not atomic against other programs.
- `add_addr(a)` adds the unicast address `a.ip` with prefix length
  `a.prefix_len`, and with it a route of that prefix through the interface,
  without duplicate address detection. It fails with `InvalidInput` for a
  prefix length above 32 for IPv4 or 128 for IPv6, for an unspecified,
  loopback, multicast, or link-local address, and when the interface holds 16
  addresses, and with `Conflict` when the prefix of `a` conflicts. Adding an
  address the interface holds, with the same prefix length, changes nothing
  and returns `Ok`.
- `add_route(p)` adds a route of `p` through the interface with no gateway.
  It fails with `InvalidInput` for a prefix that breaks the rules of
  `IpPrefix`, for a prefix length of 0, since Alloy never takes the default
  route, and when the interface holds 1,024 routes from `add_route`, and with
  `Conflict` when `p` conflicts. A route the interface holds already changes
  nothing and returns `Ok`. `remove_route(p)` removes a route that
  `add_route(p)` added, and fails with `InvalidInput` when the interface holds
  none. The route of an address is not removed this way.
- `read(buf)` fails with `InvalidInput` when `buf` is shorter than
  `MAX_TUN_PACKET`. Otherwise it waits for a packet the host sent through the
  interface, copies the whole packet, from its IP header on, to the front of
  `buf`, and returns its length. Packets are read in the order the host sent
  them. The host queues them up to a bound and drops a packet that arrives
  beyond it: 1,024 packets under `mem`, the platform's queue under `os`.
- `write(packet)` fails with `InvalidInput` when `packet` is empty, is longer
  than `MAX_TUN_PACKET`, or has first 4 bits other than 4 and 6, and with
  `MessageTooLong`, whose `max_payload` is the interface's MTU, when it is
  longer than that MTU. Otherwise it hands the packet to the host, which treats
  it as any packet arriving on an interface and may drop it, and completes
  without waiting for the host. Under `os` a packet that finds the platform's
  queue full is dropped and the call still returns `Ok`.
- At most one `read` and one `write` may be pending on a `Tun`. Another fails
  at once with `Busy`. A dropped `read` future has taken no packet, and a
  dropped `write` future has handed its packet or nothing.
- A dropped `set_mtu`, `add_addr`, `add_route`, or `remove_route` future has
  made its whole change or none of it.
- `remove` removes the interface and every address and route on it, and
  completes once `interfaces` no longer lists it, except as clause 3.1.17.2
  says for a Windows adapter taken over. A dropped `remove` future
  still removes the interface, without waiting, as dropping the `Tun` does.

3.1.17.1 Tun interfaces under `os` on Linux.

- `create_tun` opens `/dev/net/tun` with `O_RDWR | O_CLOEXEC`, so no program
  that the process runs inherits the descriptor
  (<https://man7.org/linux/man-pages/man2/open.2.html>), and registers the
  device with the `TUNSETIFF` ioctl and the flags
  `IFF_TUN | IFF_NO_PI | IFF_TUN_EXCL`, so each read and each write is one IP
  packet with no packet information header. It uses one queue and does not
  make the device persistent. When the descriptor is closed the kernel removes
  the device and all its routes
  (<https://docs.kernel.org/networking/tuntap.html>), so the interface never
  outlives its process.
- Without `IFF_TUN_EXCL`, `TUNSETIFF` attaches to an existing TUN device of
  that name. With it, the kernel refuses any existing device of that name with
  `EBUSY`, and it checks the name and registers the device under one lock,
  `rtnl_lock`, which every registration of a network device holds, so no other
  program creates a device of that name in between (`__tun_chr_ioctl` and
  `tun_set_iff` in
  <https://github.com/torvalds/linux/blob/master/drivers/net/tun.c>, and the
  `ASSERT_RTNL` of `register_netdevice` in
  <https://github.com/torvalds/linux/blob/master/net/core/dev.c>).
  `create_tun` returns `EBUSY` as `Conflict`, naming the interface, and so
  never takes over, changes, or removes a device it did not create.
- Before the device is up, it sets `net.ipv6.conf.<name>.addr_gen_mode` to 1,
  so the kernel generates no link-local address for it
  (<https://docs.kernel.org/networking/ip-sysctl.html>). It changes no other
  kernel parameter.
- Addresses are added with the `nodad` flag, which skips duplicate address
  detection (<https://man7.org/linux/man-pages/man8/ip-address.8.html>).
  Routes go in the table `main` (ARFC 0017 clause 3.17). The conflict check
  reads every routing table of both families.

3.1.17.2 Tun interfaces under `os` on Windows.

- The implementation loads `wintun.dll` with `LoadLibraryExW` by the full path
  of that file in the directory of the running executable, from nowhere else,
  and resolves only functions that `wintun.h` declares
  (<https://git.zx2c4.com/wintun/about/>, ADR 0062). When the file is missing
  or does not load, `create_tun` fails with `Unsupported`.
- `create_tun` first calls `WintunOpenAdapter` with `name`. When that opens an
  adapter, it reads the adapter's interface GUID by `WintunGetAdapterLuid` and
  `ConvertInterfaceLuidToGuid`
  (<https://learn.microsoft.com/en-us/windows/win32/api/netioapi/nf-netioapi-convertinterfaceluidtoguid>).
  When that GUID is `options.guid`, the adapter is one that a `create_tun` with
  that GUID made and that outlived its holder (section 9), and `create_tun`
  **takes it over**: it goes on as below from `WintunStartSession`, and once
  the session has started it removes every unicast address and every route on
  the adapter. When the GUID
  differs, it calls `WintunCloseAdapter` on the handle, which leaves an opened
  adapter in place (same source), and fails with `Conflict`, naming the
  adapter.
- Otherwise, when the host has an interface of that name, it fails with
  `Conflict`, naming it. Else it calls `WintunCreateAdapter` with `name`, the
  tunnel type `Alloy`, and `options.guid`. These checks and the creation are
  not atomic against other programs. Then it calls `WintunStartSession` with a
  ring capacity of 4,194,304 bytes, a power of two within Wintun's bounds of
  131,072 and 67,108,864, and fails with `Conflict`, closing what it opened,
  when that call fails on an adapter taken over.
- `WintunCloseAdapter` removes an adapter that `WintunCreateAdapter` made and
  leaves one that `WintunOpenAdapter` opened (same source). `remove` and the
  drop of the `Tun` end the session and call it. For an adapter taken over,
  they first remove every address and route on it, and the adapter then stays
  on the host, with no address and no route, until a later `create_tun` with
  its GUID takes it over again or something other than the `net` seam removes
  it (ARFC 0017 clause 3.16 step 6). `remove` of such an adapter completes once
  `interfaces` lists it with no address, and later calls on its `Tun` fail
  with `Closed`.
- Before the adapter carries a packet, it sets on the adapter's IPv6 and IPv4
  interface entries, by `SetIpInterfaceEntry`: `NlMtu` to the MTU,
  `RouterDiscoveryBehavior` to `RouterDiscoveryDisabled`, `DadTransmits` to 0,
  and `LinkLocalAddressBehavior` to `LinkLocalAlwaysOff`, so it holds no
  link-local address
  (<https://learn.microsoft.com/en-us/windows/win32/api/netioapi/ns-netioapi-mib_ipinterface_row>).
  `set_mtu` sets `NlMtu` on both.
- Addresses are added by `CreateUnicastIpAddressEntry` with
  `OnLinkPrefixLength` set to the prefix length, and routes by
  `CreateIpForwardEntry2` on the adapter with an unspecified next hop. The
  conflict check reads `GetIpForwardTable2` and `GetUnicastIpAddressTable` for
  both families.
- `read` waits on the event of `WintunGetReadWaitEvent` while
  `WintunReceivePacket` has no packet. `write` copies the packet into a buffer
  from `WintunAllocateSendPacket` and sends it with `WintunSendPacket`. A full
  ring is the platform's full queue of `write`.

3.1.17.3 Tun interfaces under `mem`.

- A tun interface takes the host's next interface index (clause 3.3.1) and the
  given name, and attaches to no network. A host crash removes every tun
  interface of the host with the packets queued on it, and a restart does not
  bring them back.
- The host's routing table holds the prefix of each address of each of its
  interfaces, through that interface, and the routes its tun interfaces hold.
  The conflict check reads it, and clause 3.3.1 chooses among its routes.
- A datagram the host sends through a tun interface is queued for `read` at
  the instant of `send` as one IP packet, and is never in flight (clause
  3.1.8). Over IPv6 the packet is a 40-byte header with version 6, traffic
  class 0, flow label 0, next header 17, hop limit 64, and no extension header
  (RFC 8200); over IPv4 a 20-byte header with IHL 5, type of service 0,
  identification 0, the don't fragment flag, fragment offset 0, time to live
  64, protocol 17, and its header checksum (RFC 791). Then come the 8-byte UDP
  header with its checksum (RFC 768, RFC 8200 section 8.1) and the payload. A
  packet that finds 1,024 packets queued is dropped with reason 6. A
  connection request through a tun interface fails with `Unsupported`: `mem`
  carries no stream over one.
- A packet written to a tun interface is handled at the instant of `write`.
  One that is not a single IPv4 or IPv6 packet whose length fields and IPv4
  header checksum agree with its bytes, or that carries UDP whose checksum does
  not verify (a zero checksum over IPv4 verifies), is dropped with reason 9.
  One addressed to no address of an up interface of the host is dropped with
  reason 8, since a `mem` host forwards nothing, and one addressed to the host
  that is not UDP with reason 5. Any other is delivered as a datagram, by
  clause 3.3.1's delivery rule, with `src` and `dst_ip` taken from its headers.

3.1.18 Host. The `host` seam (section 2.8) gives `alloy init` and `alloy join`
what ARFC 0002 sections 3.1, 3.3, and 3.8 need from the machine. Every rule
here holds for `os` on Linux and on Windows and for `mem`. On any other
platform every method under `os` returns `Unsupported`.

- The service is `alloyd` alone: on Linux the systemd unit `alloyd.service`,
  on Windows the service `alloyd` (ARFC 0017 clauses 3.15 and 3.17). No method
  takes a service name, and no implementation reads or changes any other unit
  or service, the units named `alloyd-*` included.
- `facts`, `free_space`, and `service_status` change nothing on the machine and
  need no privilege. Only `service` changes anything, and only what clause
  3.1.18.4 allows.
- `service` calls on one instance run one at a time: a call starts its action
  only once every earlier `service` call on the instance has returned or been
  dropped. The other methods may run at any time.
- A dropped `service` future leaves its action done or not done. Under `os`
  the service manager may finish an action after its future is dropped.

3.1.18.1 Facts. `facts` returns the fields of `HostFacts` as below. Under
`os`, a failing operating system call returns `Os` with its error code, except
where a bullet says what a failure gives.

- `os` on Linux:
  - `platform` is `Linux`, with `release` the `release` field of `uname(2)`
    (<https://man7.org/linux/man-pages/man2/uname.2.html>), cut to 64 bytes
    at a character boundary. `major` and `minor` are the decimal numbers
    `release` starts with, in the form `<digits>.<digits>`, each at most
    4,294,967,295; a release not of that form, or with a larger number, gives
    0 and 0.
  - `arch` is `X86_64` when the `machine` field of `uname(2)` is `x86_64`,
    `Aarch64` when it is `aarch64`, and otherwise `Other` holding that field
    cut to 32 bytes.
  - `privileged` is true when the effective user id is 0.
  - `cores` is `sysconf(_SC_NPROCESSORS_ONLN)`
    (<https://man7.org/linux/man-pages/man3/sysconf.3.html>), and 1 when that
    returns less than 1.
  - `memory` is the `MemTotal` value of `/proc/meminfo`, in kibibytes, times
    1,024 (<https://man7.org/linux/man-pages/man5/proc_meminfo.5.html>).
  - `virtualization` is true when `/dev/kvm` opens for reading and writing
    with `O_CLOEXEC`, and false on any error. The descriptor is closed at
    once. This is the `hardware virtualization` check of ARFC 0002 section
    3.8.
  - `disks` holds one entry for each entry `<name>` of `/sys/block` whose name
    does not start with `loop`, `ram`, `zram`, `nbd`, `dm-`, or `md`, whose
    file `removable` holds `0`, and whose file `size` holds a number above 0:
    `path` is `/dev/<name>` and `size` is that number times 512, since `size`
    counts 512-byte sectors
    (<https://www.kernel.org/doc/Documentation/ABI/stable/sysfs-block>). An
    entry whose files cannot be read is left out, and an unreadable
    `/sys/block` gives no entries. The first 32 by `path` are kept.
- `os` on Windows:
  - `platform` is `Windows`, with `dwMajorVersion`, `dwMinorVersion`, and
    `dwBuildNumber` from `RtlGetVersion`, which, unlike `GetVersionExW`, does
    not depend on the program's manifest
    (<https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-rtlgetversion>).
  - `arch` is the native machine that `IsWow64Process2` reports for the
    current process
    (<https://learn.microsoft.com/en-us/windows/win32/api/wow64apiset/nf-wow64apiset-iswow64process2>):
    `X86_64` for `IMAGE_FILE_MACHINE_AMD64` (`0x8664`), `Aarch64` for
    `IMAGE_FILE_MACHINE_ARM64` (`0xAA64`), and otherwise `Other` holding `0x`
    and the value as four lower-case hexadecimal digits.
  - `privileged` is true when `TokenIsElevated` of the process token's
    `TokenElevation` is not zero
    (<https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-token_elevation>).
  - `cores` is `GetActiveProcessorCount(ALL_PROCESSOR_GROUPS)`
    (<https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getactiveprocessorcount>).
  - `memory` is `ullTotalPhys` from `GlobalMemoryStatusEx`
    (<https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-globalmemorystatusex>).
  - `virtualization` is `IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED)`
    (<https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-isprocessorfeaturepresent>).
  - `disks` is empty. Windows joins only as a client node, which holds no
    disk (ARFC 0002 sections 3.3 and 3.11).
- `mem`: the host's `MachineConfig` (section 2.7) gives `platform`, `arch`,
  `privileged`, `cores`, `memory`, and `virtualization`, and a `platform` of
  `None` makes `facts` return `Unsupported`. `disks` holds one entry per block
  device of the host, `path` its `RtPath` text and `size` its `len`, the first
  32 by `path`. `add_host` returns `Invalid` for `cores` 0, a Linux `release`
  over 64 bytes, or an `Arch::Other` text outside 1 to 32 bytes. The facts
  never change during a run.

3.1.18.2 Free space. `free_space(path)` returns the bytes the calling process
may still write on the file system that holds the entry at `path`, or, when
nothing is at `path`, at its nearest ancestor that exists. It follows symbolic
links and creates nothing.

- `os` on Linux: `path` starts with `/`, holds no NUL byte, and is at most
  4,096 bytes, else `InvalidInput`. The result is `f_bavail` times `f_frsize`
  from `statvfs` (<https://man7.org/linux/man-pages/man3/statvfs.3.html>).
- `os` on Windows: `path` is a letter, `:`, and `\` followed by components
  separated by `\`, at most 4,096 bytes, else `InvalidInput`. The result is
  `FreeBytesAvailableToCaller` from `GetDiskFreeSpaceExW`, which counts the
  caller's disk quota
  (<https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getdiskfreespaceexw>).
- `mem`: `path` is a valid `RtPath` text, else `InvalidInput`. The result is
  the host's `capacity` less the total of clause 3.3.7, and 0 when the total
  is at or above it, whatever entry `path` names, since a `mem` host has one
  file namespace.
- The conformance suite checks, under `os`, that a missing path gives the
  result of its nearest existing ancestor, within 16,777,216 bytes since other
  programs write meanwhile, and the `InvalidInput` cases; and under `mem`, the
  exact result before and after a write and a `Capacity` fault.

3.1.18.3 Service control.

- `service_status` returns `NotInstalled` when the unit or service does not
  exist, and otherwise whether it is `enabled` and its `run` state.
- `service(action)` first returns `PermissionDenied`, calling no service
  manager, when the process is not privileged as `facts` defines it, then
  `NotInstalled` as `service_status` would. Otherwise:
  - `Enable`: when it returns `Ok`, the service is `enabled`. Its `run` state
    is unchanged. Enabling an enabled service changes nothing.
  - `Disable`: when it returns `Ok`, the service is not `enabled`. Its `run`
    state is unchanged. Disabling a disabled service changes nothing.
  - `Start`: on a service that is not `enabled`, returns `Disabled` and
    changes nothing. On a `Running` service, returns `Ok` and changes
    nothing. Otherwise starts the service and returns `Ok` once it is
    `Running`, or `Refused` when it stops instead.
  - `Stop`: on a `Stopped` service, returns `Ok` and changes nothing.
    Otherwise stops the service and returns `Ok` once it is `Stopped`.
  - An action that has not finished 60 seconds after it started returns
    `TimedOut`. The service is then in whatever state the service manager
    has reached.
- `os` on Linux. When `/run/systemd/system` is not a directory, systemd is
  not the running service manager
  (<https://www.freedesktop.org/software/systemd/man/latest/sd_booted.html>),
  and `service_status` and `service` return `Unsupported`. Otherwise `os`
  runs the first of `/usr/bin/systemctl` and `/bin/systemctl` that exists,
  never one found through `PATH`, with an environment holding only
  `LC_ALL=C`, standard input closed, and the arguments below
  (<https://www.freedesktop.org/software/systemd/man/latest/systemctl.html>).
  When neither exists, both methods return `Unsupported`. A run that exits
  with a status other than 0 gives `Refused` holding the first line of its
  standard error, cut to 1,024 bytes. At the 60-second limit `os` kills the
  `systemctl` process.
  - `service_status` runs `systemctl show --property=LoadState
    --property=UnitFileState --property=ActiveState alloyd.service` and reads
    its `<name>=<value>` lines. `LoadState` other than `loaded` gives
    `NotInstalled`. `enabled` is true when `UnitFileState` is `enabled`.
    `run` is `Running` for `ActiveState` `active` and `reloading`, `Starting`
    for `activating`, `Stopping` for `deactivating`, and `Stopped` for any
    other value, `inactive` and `failed` included
    (<https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html>).
  - `Enable`, `Disable`, `Start`, and `Stop` run `systemctl
    --no-ask-password --no-pager --quiet <verb> alloyd.service` with `<verb>`
    `enable`, `disable`, `start`, or `stop`. `systemctl start` and `stop`
    return once their job has finished, and for a unit of `Type=exec`, which
    the install writes (ARFC 0017 clause 3.15 step 7), the start job finishes
    once the program has been executed
    (<https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html>,
    `Type=`). `os` then reads `service_status`, and `Start` returns `Refused`
    unless the service is `Running`.
- `os` on Windows. `os` opens the service control manager with
  `SC_MANAGER_CONNECT` and the service `alloyd` with `OpenServiceW`, asking
  for `SERVICE_QUERY_CONFIG` and `SERVICE_QUERY_STATUS`, and also
  `SERVICE_CHANGE_CONFIG`, `SERVICE_START`, or `SERVICE_STOP` as the action
  needs. `ERROR_SERVICE_DOES_NOT_EXIST` (1060) gives `NotInstalled`,
  `ERROR_ACCESS_DENIED` (5) gives `PermissionDenied`, and any other error
  `Os` (<https://learn.microsoft.com/en-us/windows/win32/debug/system-error-codes--1000-1299->).
  - `service_status`: `enabled` is true when `dwStartType` from
    `QueryServiceConfigW` is `SERVICE_AUTO_START` (2), delayed or not. `run`
    from `dwCurrentState` of `QueryServiceStatusEx` is `Stopped` for
    `SERVICE_STOPPED`, `Starting` for `SERVICE_START_PENDING`, `Stopping`
    for `SERVICE_STOP_PENDING`, and `Running` for any other state
    (<https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_status_process>).
  - `Enable` and `Disable` call `ChangeServiceConfigW` with `dwStartType`
    `SERVICE_AUTO_START` (2) or `SERVICE_DISABLED` (4), and `SERVICE_NO_CHANGE`
    or a null pointer for every other argument
    (<https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-changeserviceconfigw>).
  - `Start` calls `StartServiceW` with no arguments, treating
    `ERROR_SERVICE_ALREADY_RUNNING` (1056) as success, then reads
    `QueryServiceStatusEx` every 100 ms: `SERVICE_RUNNING` returns `Ok`, and
    `SERVICE_STOPPED` returns `Refused` naming `dwWin32ExitCode`.
  - `Stop` calls `ControlService` with `SERVICE_CONTROL_STOP`, treating
    `ERROR_SERVICE_NOT_ACTIVE` (1062) as success, then reads
    `QueryServiceStatusEx` every 100 ms until `SERVICE_STOPPED`
    (<https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-controlservice>).
- `mem`. Each host has one service state, created with the host: installed
  when `MachineConfig::service` is true, not `enabled`, and `Stopped`, as a
  first install leaves it (ARFC 0017 clause 3.15 step 7). `privileged` false
  gives `PermissionDenied` and `service` false gives `NotInstalled`, as above.
  Every action finishes at the instant of its first poll, so `run` is never
  `Starting` or `Stopping` and `TimedOut` never happens. `mem` runs no
  program: `run` is a value that only these calls and the host's lifecycle
  change. A crash of the host makes it `Stopped`, and `enabled` is kept. A
  start of the host, the first included, makes an `enabled` service `Running`
  before the host's `boot` task runs, as a service manager starts an enabled
  service at boot. Each `service` call writes a `service` record (section
  4.1) when it returns.

3.1.18.4 Coexistence with the install. The install owns the definition of the
unit and the service: its file or service control manager entry, program,
account, restart policy, and ordering (ARFC 0017 clause 3.15 steps 6 and 7).
The uninstall stops, disables, and removes it (ARFC 0017 clause 3.16 steps 5
and 7). The `host` seam owns only whether the service is `enabled` and
whether it runs, so `init`, `join`, and the install never contend for one
property:

- Under `os` on Linux, the only change to the file system an action makes is
  the links that `systemctl enable` and `systemctl disable` add and remove
  from the unit's `[Install]` section. No action runs `daemon-reload`,
  `mask`, `unmask`, `edit`, `set-property`, `revert`, `link`, or `preset`,
  or writes or removes a unit file.
- Under `os` on Windows, the only change an action makes is the start type
  and the run state. No action creates or deletes a service or changes its
  program path, account, dependencies, description, or recovery actions.
- `enabled` means what the install's step 7 sets when a node resumes: unit
  file state `enabled`, start type automatic. `Disable` gives what a first
  install leaves: unit file state `disabled`, start type `Disabled`. So after
  `init` cleans up a failed run (ARFC 0002 section 3.1), the service is as
  the install left it, and a later install keeps the enablement `init` or
  `join` set (ARFC 0017 clause 3.15 step 7).
- The seam does not read the install manifest. `init` and `join` check with
  the `names` check of ARFC 0002 section 3.8 that the unit or service is the
  install's before they act on it.
- The conformance suite checks, under `os` with privilege, that across every
  action the unit file's bytes, or on Windows the configuration that
  `QueryServiceConfigW` returns other than `dwStartType`, are unchanged.

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
a task spawned, polled, or ended; a timer of a host's clock firing; a clock
fault; a UDP datagram sent, delivered, dropped, or altered by a fault; a stream
opened, carrying data, or closed; each operation of a disk call; an
`Rng::fill`; any other fault applied or changed; a note from the scenario; and
the end of the run. Each record carries the world's virtual time when it
happened. Section 4.1 states, for each kind,
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

3.2.7 A run. `run` does these steps on the calling thread:

1. It checks `config`. A field outside its range ends the run at world time 0
   with `fail`, a `run-end` record as the only record, and `scenario` not called.
2. It creates the world at world time 0, writes the trace header, creates the
   trace file when `trace_file` is set (replacing any file at that path), calls
   `scenario` once with the world, and makes the returned future the control
   task.
3. It runs the world. The control task is scheduled with the host tasks under
   clause 3.2.2 but belongs to no host: it writes no `task-*` records, and no
   fault affects it. It may call every `World` method, and so may any host task
   that holds a `World`.
4. The run ends at the first of: the control task completes or panics; an
   observer or a middlebox panics; the world has no runnable task and its next
   event, if any, is after `budget`; or the world has polled 100,000,000 tasks,
   the control task included, at one world instant. In the budget case world
   time becomes `budget` when an event is pending and stays where it is when
   none is. An observer that panics in `record` is removed and receives nothing
   more; the record still goes to every other observer. A middlebox that panics
   in `process` leaves its packet undelivered and unrecorded. After either panic
   the next record is `run-end`. An observer that panics in `record` of the
   `run-end` record itself is removed like any other, and no record follows: the
   run has already ended, and step 5 writes no second `run-end`. A trace file that cannot be written does not
   end the run: the simulator writes nothing more to the file and the run goes
   on.
5. At the end it writes the `run-end` record at the current world time, then
   polls nothing again and writes no more records. It drops every task and
   object of the world, closes the trace file, and writes the summary line.
   After that, every `World` method that returns a `Result` returns `Ended`.

`scenario` and the control task may build the world at any time: a host added at
world time `t` starts at `t`. `random_u64` draws from a stream of the seed that no
host, router, or link shares. Runs share no state: several runs, one after the
other or at once on different threads of one process, each produce what they
would produce alone.

3.2.8 The result is `fail` when the control task returned `Err` or panicked, a
host task panicked (section 6), an observer or a middlebox panicked, or the trace
file could not be written. Otherwise it is `liveness` when the run ended by step
4's budget case or poll case. Otherwise it is `pass`. `failure` holds the cause
seen first: the `Failure` detail, or a text naming the panic, the budget, the
poll bound, or the file error. The result is fixed when the `run-end` record is
written, and `RunSummary::result` and the summary line always give the result
that record carries: an observer that panics in `record` of the `run-end` record
does not change the result or `failure`. Checkers report through the control
task (clause 3.6.3), so no verdict depends on an observer's panic.

3.2.9 A world call made from inside `Observer::record` or `Middlebox::process`
changes nothing. Every `World` method that returns a `Result` returns `Busy`.
`now` returns the current world time, `random_u64` returns 0 without advancing
the scenario's stream, `sleep` and `sleep_until` return a future that never
completes, and `history` returns a new, empty history.

3.2.10 The echo scenario. Milestone M0's first two exit criteria use this scenario.
Its code lives with the simulator, as an example of `alloy-sim` that takes the
seed as an argument, and the verifier's suite runs it through `run`.

- One root network with prefix `10.0.0.0/16` and the default `LinkConfig`.
- 100 hosts. Host `i`, for `i` from 0 to 99, has one interface on that network
  with address `10.0.0.<i + 1>` and the defaults of `HostConfig`.
- Each host's boot binds UDP on `0.0.0.0:7` and `0.0.0.0:0`. One task, the
  server, receives on port 7 in batches of up to 64 slots of 2,048 bytes and
  sends each datagram's payload back to its source. Another, the client, loops:
  it sleeps 1 s on the host's clock, takes 8 bytes from the host's `rng` as a
  little endian `u64` `n` and picks host `n mod 100`, sends a 64-byte payload from
  the `rng` to that host's port 7 from the port 0 socket, and waits up to 500 ms
  for the echo. A missing or different echo counts one miss.
- The control task sleeps until world time 3,600 s, then returns `Ok` when no
  client counted a miss and `Err` otherwise.

With the default budget, the run must end with `pass` at world time 3,600 s. Two
runs with one seed must write identical summary lines, and runs with seeds 1 and
2 must have different trace hashes. A release build must complete the run in
under 60 s of wall time on one core of the CI runner (clause 3.2.3).

### 3.3 Faults

The world can inject, at any virtual time or at seeded random times: per-link
loss, duplication, reordering, delay, jitter, corruption, and MTU limits with or
without ICMP feedback. One-way and two-way partitions. Host crash at any await
point, with loss of all unsynced disk writes, optionally torn. Host restart. Disk
read errors, corrupt reads, and full disks. Clock skew and steps. CPU stalls.

`World::apply` and `World::schedule` inject them as `Fault` values (section 2.7).
Seeded random times are world times the scenario draws from `random_u64`. Every
random choice below is an independent draw from a stream fixed by the seed.

3.3.1 Topology and routing.

- `add_network` returns `Invalid` for a config outside the ranges of section 2.7,
  for two prefixes of one family, for a prefix with host bits set, for an
  `uplink` whose addresses are outside the parent's or this network's prefixes or
  already held on that network, and for a routable prefix that overlaps a
  routable prefix of another child of the same parent. An unknown parent is
  `Unknown`.
- `add_host` returns `Invalid` for a config outside the ranges of section 2.7 and
  for an interface or device that `attach` or clause 3.3.7 would reject, and
  `Limit` when the world holds 10,000 hosts. Every method taking a `HostId`,
  `NetworkId`, or interface index that does not exist returns `Unknown`.
  `detach` and `set_up` on index 1 return `Invalid`.
- Every host has interface index 1, named `lo`, loopback, up, MTU 65,536, with
  `127.0.0.1/8` and `::1/128`. Attached interfaces get indexes 2, 3, and so on in
  attach order, never reused on the host, and are named `eth<index - 2>`. An
  interface's addresses carry the prefix length of their network's prefix.
  `attach` returns `Invalid` for an address outside the network's prefixes or
  already held by an interface or router on that network. Interfaces, their
  addresses, and their up state persist across crashes, except tun interfaces
  (clause 3.1.17.3). `attach`, `detach`, and
  `set_up` change the host's interface list at that instant (clause 3.1.7).
- A host sends a datagram, or a connection request, through: the interface that
  holds the source address when one is given or bound; else its lowest-indexed
  up interface with an address of the destination's family whose prefix
  contains the destination, or that is a tun interface holding a route that
  contains it (clause 3.1.17); else its lowest-indexed up interface other than
  `lo` and tun interfaces with an address of the destination's family. With none, the datagram
  fails with `NetworkUnreachable`.
  A source address on no interface of the host fails with `AddrNotAvailable`, and
  one on a down interface with `NetworkUnreachable`. A destination in `127.0.0.0/8`,
  `::1`, or an address of an up interface of the sending host is delivered over
  `lo`: at the same world instant, with no link property, fault, or partition.
- Source address. When neither `src_ip` nor the socket's bound address gives a
  source, `mem` picks one. For delivery over `lo`, it is the destination itself
  when an interface of the host holds that address, and otherwise `127.0.0.1`.
  Through any other interface, it is the lowest address of the destination's
  family that the chosen interface holds. A connection request has no bound
  address and picks its source address the same way. Its source port is picked
  from 49,152 to 65,535 using the seed, among the ports that no stream or
  listener of the host holds on that address. With none free, `connect` fails
  with `AddrNotAvailable`.
- A packet that enters network `N` crosses it (clause 3.3.2) and then goes to the
  first of: the up host interface on `N` that holds the destination; the router
  of a child of `N` whose routable prefix contains the destination, entering
  from outside; the router that holds the destination as an address on `N`,
  entering from the side of that address; and, when the packet did not come down
  from `N`'s parent, `N`'s own router, entering from inside. When none applies it
  is dropped with reason 8.
- A router without a middlebox forwards a packet unchanged to its other side. A
  packet addressed to one of its own addresses is dropped with reason 5. A router
  with a middlebox passes every packet to it (clause 3.3.9). A packet crosses
  only when the router has an address of the packet's family on the side it
  leaves by, and is otherwise dropped with reason 8. A packet that would enter
  its 65th network is dropped with reason 8.
- A packet's **origin host** is the host whose `send` sent it, also after
  translation and for its duplicates. Its **destination host** is the host
  whose interface it is delivered to.
- A datagram delivered to a host that is crashed, or to a port no socket is bound
  to for that address, is dropped with reason 5, and one that does not fit the
  socket's receive buffer with reason 6.

3.3.2 Link properties. A packet crossing a network gets, from the network's
`LinkConfig` in force when it enters:

- MTU. A packet larger than `mtu` is dropped with reason 3. With `icmp_too_big`,
  the origin host learns `mtu` as the path MTU to the destination it sent to, at
  the instant of the drop (clause 3.1.8). The packet size is the UDP payload plus
  28 bytes over IPv4 and 48 over IPv6.
- Loss. With probability `loss`, the packet is dropped with reason 1.
- Bandwidth. When `bandwidth` is not 0, each sender into the network (a host
  interface or a router side) has its own FIFO queue, and a packet takes
  `ceil(size * 1,000,000,000 / bandwidth)` ns to leave it, computed in 128-bit
  arithmetic. A packet that would take the queue's waiting bytes above `queue`
  is dropped with reason 4.
- Corruption. With probability `corrupt`, one bit of the payload, at a position
  drawn from the seed, is flipped, and a `packet-fault` with fault 2 is written.
  An empty payload is never corrupted.
- Duplication. With probability `duplicate`, a copy is made and a `packet-fault`
  with fault 1 is written. When the draw asks for a copy that does not fit the
  bounds of clause 3.1.8, the draw is still made, but no copy and no record. The
  copy crosses this network with its own draws of jitter and reordering, but is
  not duplicated again here, and goes on alone.
- Delay. The packet leaves the network `latency` after it leaves the bandwidth
  queue, plus a jitter drawn uniformly from 0 to `jitter` ns, plus, with
  probability `reorder`, `reorder_delay`, in which case a `packet-fault` with
  fault 3 and that added delay is written. Packets may overtake one another
  whenever their delays differ.

A `Link` fault replaces the network's `LinkConfig` from that instant. Packets
already crossing keep what they drew.

3.3.3 Partitions. While partition `id` is in force, every packet and every stream
segment whose origin host is in `from` and whose destination host is in `to`, and
when `both_ways` also from `to` to `from`, is blocked. A blocked datagram is
dropped with reason 2 at the instant it would be delivered. Stream segments and
connection requests follow clause 3.1.9.1. `from` and `to` must be non-empty,
disjoint sets of existing hosts, and `id` must not be in force: otherwise
`Partition` returns `Invalid` or `State`. `Heal` ends the partition and returns
`State` for an `id` not in force. Partitions never affect `lo`.

3.3.4 Crash and restart.

- `Crash` with `after_polls` 0 crashes the host at once. With `n` above 0 it
  arms the crash: the host crashes right after the `n`-th poll of one of its
  tasks that starts after the call returns, before any other event. A crash that
  happens first, by any fault, disarms it. A second armed crash on the host
  replaces the first.
- A `Crash` with `after_polls` 0 applied during a poll of one of the host's own
  tasks, through a `World` that task holds, takes effect when that poll returns,
  before any other event. `apply` returns `Ok`. Until then the host runs as if
  no crash were pending: `host_state` returns `Running`, calls behave and write
  records as they would without the crash, and a further `Crash` of the host
  returns `State`.
- At a crash the host writes the records of section 4.1, drops every task without
  polling it again (their `join` returns `Cancelled`), loses its sockets,
  connections, listeners, open files, and learned path MTUs, and handles the disk
  as below. Objects of the crashed start that something outside the host still
  holds fail every call from then on: network objects with `Closed`, disk objects
  with `Io`, `spawn` with `ShuttingDown`. Its `Clock` and `Rng` instances keep
  working.
- At a crash, the durable state of each file is every operation that completed
  before the call of the last completed `sync` on any handle of it, and of each
  directory every create, remove, and rename in it that completed before the call
  of the last completed `sync_dir` of it (both parents for a rename). A
  `set_mode` is in the durable state when it completed before the call of the
  last completed `sync(All)` on a handle of its file, or of the last completed
  `sync_dir` of its directory itself (clause 3.1.15); `sync(Data)` and a
  `sync_dir` of the parent do not count. A completed `replace` is in the
  durable state as clause 3.1.16 says. The rest is unsynced, and `unsynced`
  sets its fate:
  - `Lost`: every unsynced operation is undone.
  - `Seeded`: the unsynced operations are replayed on the durable state in
    completion order, each kept or dropped by a draw. An operation that depends
    on a dropped one (a write to, a rename of, or a `set_mode` on an entry
    whose create is dropped) is dropped too. A rename is kept or dropped whole.
  - `Torn`: as `Seeded`, but each unsynced write is split into its `block_size`
    units, each kept or dropped by its own draw.
  Block devices follow the same rules for their writes.
- `Restart` starts a crashed host at that instant: `host-start` is written, the
  five `mem` instances are built anew, the host's one `host` instance is kept
  (clause 3.1.18.3), and `boot` runs with `start` one above the last. With `rng_replay` `Some(s)`, `s` from 1 to the last start, the host's
  `rng` instances of this start draw, in creation order, the streams that the
  instances of start `s` drew in the same order (clause 3.1.5).
- `Crash` on a crashed host and `Restart` on a running host return `State`.

3.3.5 Clocks. `ClockRate` sets the host's rate offset from that instant, with the
formula of clause 3.1.4, and returns `Invalid` outside -500,000 to 500,000.
`ClockStep` adds `step_ns` to the host's wall clock. Both write a `clock-fault`
record and apply to crashed hosts too. A host's wall clock at world time `w` is
`W + S + r(w)`, where `W` is `wall_start` plus `wall_offset`, `S` is the sum of
the steps applied up to `w`, and `r` is the host's rate clock. For a host
created at world time `c`, `r(w) = w` for `w` up to `c`, so its wall clock at
creation is `W + c`, and the offset is `clock_ppm` from `c`. When the offset
becomes `p` at `w0`, `r(w) = r(w0) + floor((w - w0) * (1,000,000 + p) /
1,000,000)` until the next change, in 128-bit arithmetic. The rate clock runs
through crashes, so the wall clock persists across restarts. The result
saturates at the bounds of `i64`.

3.3.6 Stalls. `Stall` stops polling the host's tasks for `length`: a stall that
starts at `t` ends at `t + length`, or at the end of a stall already in force if
that is later. Timers still fire, and datagrams and segments still arrive into
the host's sockets, but woken tasks are polled only after the stall ends. Each
`Stall` writes a `host-stall` record with `length`, and the host's state is
`Stalled` until it ends. A crash ends every stall in force on the host, so a
restarted host is not stalled. `Stall` on a crashed host returns `State`.

3.3.7 Disks.

- The host's file namespace holds at most `capacity` bytes, counting each
  regular file's length rounded up to a multiple of `block_size`. A write or a
  `truncate` that would take the total above `capacity` fails with `NoSpace` and
  changes nothing. `Capacity` sets the bound from that instant; a value at or
  below the current total makes the disk full. Block devices do not count.
- `Disk` sets, for `target`, the probability that each read operation fails with
  `Io`, and the probability that a read that returns at least one byte has one
  bit flipped, at a position drawn from the seed, in the bytes it returns. The
  stored bytes are not changed. Both start at 0, persist across crashes, and
  apply to every handle of the target. A `Device` path that names no device of
  the host is `Unknown`.
- Devices are created with the host, at their paths, with missing parent
  directories created durable. A device path in use, or two devices on one path,
  is `Invalid`.

3.3.8 When faults apply.

- `apply` checks the fault against the world's current state and applies it at
  the current world instant, or returns the error and changes nothing.
- `schedule` returns `Invalid` for `at` before now, and checks at once what does
  not depend on state: hosts, networks, and devices exist, values are in range,
  and partition sets are well formed. The
  fault then applies at `at`, among the events of that instant in the order of
  clause 3.2.2. A scheduled fault whose state check fails at `at` (a `Restart` of
  a running host, a `Heal` of a partition not in force) is skipped, and a `fault`
  record says so.
- Every fault that applies writes its record: `host-crash`, `host-start`,
  `host-stall`, or `clock-fault` for the faults that have one, and a `fault`
  record for every other fault, for an armed crash, for every `add_network`,
  `attach`, `detach`, and `set_up`, and for each interface `add_host` attaches
  (section 4.1).

3.3.9 Middleboxes. A router with a middlebox calls `process` for every packet that
reaches the router, from either side, and once for each stream connection request
(clause 3.1.9.1), with `ctx.now()` the world time. `Forward` sends the packet out
on `to` with the given addresses, which may be the side it came from (hairpinning).
`Drop`, and a `Forward` whose addresses are not of the packet's family, drop it
with reason 7. Replies to a stream connection request, and its later segments,
are translated as the request was and do not reach `process`. ICMP feedback of
clause 3.3.2 reaches the origin host directly and does not reach `process`.

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
readable after any allowed failure). The checkers' algorithms are not specified
here. They attach to three hook points:

3.6.1 Observers. `observe` adds an observer, which from then on receives every
record, in trace order, synchronously as the record is written and before the
simulator does anything else. `data` holds the bytes the record's `digest` covers:
the payload for `udp-send` and `udp-deliver`, the bytes for `stream-data`, and the
bytes written or returned for `disk-op`; it is empty for every other record and
for a failed disk operation. Observers see payloads that the trace leaves out
(section 8). A world holds at most 64 observers; beyond that `observe` returns
`Limit`.

3.6.2 Histories. `invoke` appends an entry with the client, the operation, and
the current world time as `invoked`, and returns its `Call`. `complete` sets the
entry's `completed` to the current world time and `ret`. `entries` returns a copy
of every entry in invocation order. An entry whose call is dropped without
`complete`, for example by a host crash, stays incomplete. A history holds at most
16,777,216 entries; beyond that `invoke` returns `Limit`, and after the run ends it
returns `Ended`. Histories write no records.

3.6.3 Verdicts. A checker reports a violation through the control task, which
returns `Err(Failure)`, so the run ends with `fail` (clause 3.2.8).

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
| `0034` | `tun-read` | u32 interface index, u32 packet length, `digest` of the packet |
| `0035` | `tun-write` | u32 interface index, u32 packet length, `digest` of the packet |
| `0036` | `tun-change` | u32 interface index, u8 change: 1 created, 2 MTU set, 3 address added, 4 route added, 5 route removed, 6 removed |
| `0040` | `stream-open` | u64 connection, `saddr` local, `saddr` peer |
| `0041` | `stream-data` | u64 connection, u8 direction: 0 written by this host, 1 read by this host; u32 length, `digest` of the bytes |
| `0042` | `stream-close` | u64 connection, u8 how: 0 write side shut down by this host, 1 reset, 2 dropped by this host, 3 dropped by a host crash |
| `0050` | `disk-op` | u64 handle (0 for a path operation), u8 operation, u64 offset, u64 length, u8 result, `digest` |
| `0060` | `rng` | u32 bytes filled |
| `0070` | `fault` | `text` describing the fault applied or changed |
| `0080` | `service` | u8 action, the `ServiceAction` number; u8 result: 0 for `Ok`, else the `HostError` number (clause 3.1.18.3) |
| `00F0` | `note` | `text` from the scenario |
| `00FF` | `run-end` | u8 result: 0 pass, 1 fail, 2 liveness |

Packet drop reasons: 1 loss fault, 2 partition, 3 larger than a link MTU, 4 link
queue full, 5 no socket bound at the destination, 6 receive queue full, 7
filtered by a NAT device, 8 no route, 9 malformed packet written to a tun
interface.

Disk operations: 1 open, 2 read, 3 write, 4 sync, 5 truncate, 6 metadata, 7 list,
8 create-dir, 9 remove, 10 rename, 11 sync-dir, 12 info, 13 create-file, 14
create-dir-protected, 15 security, 16 set-mode, 17 replace. `result` is 0 for success
and otherwise the `DiskError` number of section 2.4.

Which event writes each record, and its field values:

- `host` is the host the event happened on. It is `FFFFFFFF` for events on links,
  routers, and NAT devices, which are `packet-drop` with reasons 1, 2, 3, 4, 7,
  and 8, other than a drop of a packet written to a tun interface (the bullet
  Tun interfaces below), every `packet-fault`, and a `fault` applied to a link, router, or NAT
  device, and for `note` and `run-end`. `packet-drop` with reasons 5 and 6
  carries the destination host, or `FFFFFFFF` when a router drops a packet sent
  to its own address (clause 3.3.1). A `fault` or `clock-fault` applied to one
  host carries that host, and so does the `fault` record of `attach`, `detach`,
  `set_up`, and a skipped fault aimed at one host. Every other `fault` record,
  such as those of `add_network`, `Link`, `Partition`, and `Heal`, carries
  `FFFFFFFF`. The `fault` text describes the change; its wording is not
  specified, but it is a function of the change alone.
- The addresses of `udp-send`, `udp-deliver`, `packet-drop`, and `packet-fault`
  are those the packet carries at the point of the event, after every NAT
  translation before it.
- `udp-send`: one record per datagram that `send` hands to the network, in batch
  order. A datagram that fails in `send` writes none. `udp-deliver`: when the
  datagram is queued on the receiving socket.
- Starts. At each start of a host, by `add_host` or `Restart`, the host writes
  `host-start` and right after it `task-spawn` for its `boot` task. `add_host`
  first writes, at the same instant, one `fault` record for each interface of
  `interfaces`, in order, carrying the new host, as `attach` would. `lo` and
  the devices of `devices` write no record.
- `timer-fire` is written when a timer of a host's `Clock` fires, on that host,
  with the deadline and the task it wakes. `task woken` is 0 when that task is
  the control task or a task of another host. A timer of `World::sleep` or
  `World::sleep_until` writes no record, whichever task awaits it.
- A host crash writes, at the time of its `host-crash` record and right after it,
  `task-end` with outcome 3 for each live task of the host in ascending task
  number, then `stream-close` with how 3 for each open connection of the host in
  ascending connection number. A connection is open on a host from the instant
  the host numbers it until the host drops its `Stream`, whether or not its end
  has been shut down or reset since.
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
- `disk-op` `handle`: for `open` and `create_file`, the number of the new
  handle, or 0 when the call fails; for read, write, sync, truncate, and info,
  the handle's number; 0 for the path operations metadata, list, create-dir,
  remove, rename, sync-dir, create-dir-protected, security, set-mode, and
  replace.
- `disk-op` `offset` and `length`: for a read or a write, the operation's offset
  and `buf.len()`, which is the requested length also when a read returns fewer
  bytes or fails; for truncate, 0 and the new length; for create-file and
  create-dir-protected, 0 and the protection, 0 `Default`, 1 `Private`, 2
  `PrivateRoot`; for set-mode, 0 and the mode; 0 and 0 for every other
  operation.
- `disk-op` `digest`: for a successful write, of the bytes written; for a
  successful read, of the bytes it returned, which may be fewer than `length` or
  none; 8 zero bytes for every failed operation and every other operation.
- `rng`: one record for every `fill` call, also with an empty `dest`, with the
  number of bytes filled.
- Tun interfaces. `tun-read` is written when `read` returns a packet, and
  `tun-write` when `write` hands one to the host, before any record of its
  delivery or drop. A datagram sent through a tun interface writes `udp-send`
  as any other, and its drop for a full queue writes `packet-drop` with reason
  6, carrying the sending host. A packet that `write` hands to the host and
  that the host then drops writes `packet-drop` carrying the writing host,
  with reason 9, 8, or 5 as clause 3.1.17.3 says, or with the reason of clause
  3.3.1's delivery. In that record each `addr` has as family the packet's first
  4 bits and as address the source or destination field of its fixed IP
  header, bytes 12 to 15 and 16 to 19 for IPv4 and 8 to 23 and 24 to 39 for
  IPv6, or all zero bytes when the packet is shorter than 20 or 40 bytes. When
  the packet passed the checks of reason 9 and carries UDP, the ports and the
  payload length are those of its UDP header and payload; otherwise both ports
  are 0 and the payload length is the length of the whole packet. `tun-change` is written at the instant the
  interface list changes by a call that succeeds, and with change 6 for each
  tun interface a crash removes, after that crash's `stream-close` records, in
  ascending index.

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
| `clock`, `rng`, `spawn`, `net`, `host` | `os`, `mem` | `message Empty {}` |
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
| `host` | `Host` | `HostSeam` | `os`, `mem` | `tests/conformance/rt/host/` |

All six have swap class `restart` and context `()`. Each suite runs against
`mem` on the simulator and against `os` on the CI host. A clause that names one
implementation is tested on that implementation only. The `os` tests of clause
3.1.17 need the privilege that `create_tun` names. `cargo xtask ci` runs them
with it on the CI hosts, one Linux and one Windows: as root by the runner's
passwordless `sudo` on Linux, and on Windows in the runner's account, an
administrator with User Account Control off
(<https://docs.github.com/en/actions/reference/runners/github-hosted-runners#administrative-privileges>,
ADR 0037). There, where the environment variable `GITHUB_ACTIONS` is `true`
(<https://docs.github.com/en/actions/reference/workflows-and-actions/variables>), a
test that meets `PermissionDenied` fails, and so does a run that skips clause
3.1.17.1 on Linux or clause 3.1.17.2 on Windows. Elsewhere a harness that
lacks the privilege checks the `PermissionDenied` refusal and says in its
output that it did so.

In the `host` suite, `facts`, `free_space`, and `service_status` need no
privilege and run on every host. The `os` tests of `service` need root, or on
Windows elevation, and a unit or service `alloyd` that nothing else uses, so
they never act on an installed Alloy. On the CI hosts, where `GITHUB_ACTIONS`
is `true`, `cargo xtask ci` provides both, with the privilege above. Before
the `host` suite it checks that no unit `alloyd.service`, or on Windows no
service `alloyd`, exists, and fails when one does. It then installs one: on
Linux the unit `packaging/alloyd.service` with `ExecStart` naming the
workspace's own `alloyd` executable, at `/etc/systemd/system/alloyd.service`,
followed by `systemctl daemon-reload`; on Windows the service `alloyd` that
`packaging/install.ps1` creates, with start type `Disabled`, running that
executable. With no Alloy data directory at its path, that `alloyd` stays up,
serves nothing, and stops on request (ARFC 0002 section 3.12). It runs the
suite with the environment variable `ALLOY_TEST_SERVICE` set to `1`, then
stops, disables, and removes the unit or service, on Linux followed by
`systemctl daemon-reload`. A test of `service` under `os` runs only where
`ALLOY_TEST_SERVICE` is `1`. Where `GITHUB_ACTIONS` is `true`, a run in which
it is not `1`, or a test that meets `PermissionDenied`, fails. Elsewhere,
without that variable, the suite checks each action's `PermissionDenied`
refusal when it runs unprivileged, runs no action when it runs privileged,
and says in its output which it did. The `mem` tests of the suite run on the
simulator.

The simulator itself is not a seam. Its own suite, `tests/conformance/sim/`,
tests through section 2.7 the trace format and the summary line of clauses 3.2.4
to 3.2.6, the runs of clauses 3.2.7 to 3.2.10, the faults of section 3.3, the
hook points of section 3.6, and the `mem` clauses of section 3.1 that depend on
the world, such as clause 3.1.9.1. Whole-system scenarios live in `tests/sim/`.

## 6. Failure behavior

A scenario that fails prints its seed, the trace hash, and the last 200 events. A
scenario that exceeds its virtual time budget fails as a liveness violation.

When the result is `fail` or `liveness`, `run` writes to standard error, before
the summary line: one line per kept record, oldest first, each starting with
`alloy-sim event time=<ns> host=<id> kind=<name>`, where `<id>` is the host id in
decimal or `world` and `<name>` the kind's name from section 4.1; then one line
starting with `alloy-sim failure: ` followed by the `failure` text. What follows
those prefixes on a line is not specified. The summary line is always the last
line `run` writes.

A middlebox or observer that panics, and a control task that panics, end the run
with `fail` (clauses 3.2.7 and 3.2.8), except an observer that panics on the
`run-end` record, which comes after the result is fixed. A host task's panic
does not end the run.

A task that panics in `mem` fails the scenario, after the run ends, with result
`fail`. A trace file that cannot be written fails the scenario with result `fail`
when the run ends, which it does as it would otherwise, and the summary line is
still written.

## 7. Limits

10,000 hosts per world. 2^63 nanoseconds of virtual time. Event traces are
streamed to disk and capped at 1 GiB unless a scenario raises the cap.

| Bound | Value | At the bound |
|---|---|---|
| Datagrams per `send` or `recv` | 64 | `BatchTooLarge` / fills 64 slots |
| UDP sockets per `net` instance | 4,096 | `TooManyOpen` |
| Streams and listeners per `net` instance | 65,536 | `TooManyOpen` |
| `mem` stream buffer, per direction of a connection | 262,144 bytes | `write` waits for space |
| `mem` UDP packets in flight, per sending socket | `send_buffer` bytes of packet size | `send` waits; a duplicate is not made |
| `mem` UDP packets in flight, per origin host | 67,108,864 bytes of packet size | `send` waits; a duplicate is not made |
| `RecvSlot` capacity | 65,535 bytes | `InvalidInput` |
| UDP receive and send buffer | 65,536 to 67,108,864 bytes, default 4,194,304 | `InvalidInput`; arrivals beyond the receive buffer dropped |
| Listen backlog | 1 to 65,535, default 1,024 | `InvalidInput` |
| Interfaces listed | 1,024, 64 addresses each | The rest are left out |
| Tun interfaces per `net` instance | 16 | `TooManyOpen` |
| Tun interface MTU | 1,280 to 65,535 bytes | `InvalidInput` |
| Packet read from or written to a tun interface | 65,535 bytes | `InvalidInput` |
| Addresses per tun interface | 16 | `InvalidInput` |
| Routes per tun interface, from `add_route` | 1,024 | `InvalidInput` |
| `mem` packets queued for `read` per tun interface | 1,024 | Dropped with reason 6 |
| Unix socket path | 107 bytes | `InvalidInput` |
| Operations per disk batch | 256 | `BatchTooLarge` |
| `IoBuf` length | 67,108,864 bytes | `InvalidInput` |
| Open disk handles per `disk-io` instance | 65,536 | `TooManyOpen` |
| Directory entries per `list` | 1,048,576 | `TooManyEntries` |
| Access control entries per `security` result | 4,096 | `InvalidInput` |
| `set_mode` mode | `0o7777` | `InvalidInput` |
| `RtPath` | 4,096 bytes, 255 per component | `InvalidPath` |
| Live tasks per `spawn` instance | 1,048,576 | `SpawnError::Limit` |
| `TaskName` | 64 bytes | `SpawnError::InvalidName` |
| One `service` action | 60 s | `HostError::TimedOut` |
| Disks in `HostFacts` | 32 | The rest, by `path`, are left out |
| Linux `release` in `Platform` | 64 bytes | Cut at a character boundary |
| `Arch::Other` text | 32 bytes | Cut |
| `free_space` path under `os` | 4,096 bytes | `HostError::InvalidInput` |
| `Instant` | 2^63 - 1 ns | Saturates or `None` (section 2.1) |
| `mem` clock rate offset | -500,000 to 500,000 parts per million | The world rejects the fault when it is injected |
| Trace record body | 65,535 bytes | `text` fields are cut to 1,024 bytes |
| Records kept for failure output | 200 | Oldest dropped |
| Hosts per world | 10,000 | `add_host` returns `Limit` |
| Networks per world | 10,000 | `add_network` returns `Limit` |
| Interfaces per host, `lo` included | 1,024 | `attach` returns `Limit` |
| Addresses per interface | 1 to 64 | `Invalid` |
| Block devices per host | 64, each 2^40 bytes at most | `add_host` returns `Invalid` |
| Networks a packet crosses | 64 | Dropped with reason 8 |
| Partitions in force | 1,024 | `Partition` returns `Limit` |
| Scheduled faults pending | 1,048,576 | `schedule` returns `Limit` |
| Observers per world | 64 | `observe` returns `Limit` |
| Entries per history | 16,777,216 | `invoke` returns `Limit` |
| Link latency, jitter, reorder delay | 3,600 s each | `Invalid` |
| Link queue per sender | 1,500 to 1,073,741,824 bytes | `Invalid`; arrivals beyond it dropped with reason 4 |
| Link MTU | 576 to 65,535 bytes, 1,280 or more with IPv6 | `Invalid` |
| `mem` connection request without an answer | 30 s | `connect` fails with `TimedOut` |
| `mem` route delay at connection setup | Below 15 s | No answer: `connect` fails with `TimedOut` at 30 s |
| `mem` stream byte not delivered | 60 s after it was due | The sending end becomes reset |
| `mem` TCP source ports per local address | 49,152 to 65,535 | `connect` fails with `AddrNotAvailable` |
| Task polls at one world instant | 100,000,000 | The run ends with `liveness` |
| Run budget | 1 ns to 2^63 - 1 ns, default 3,600 s | The run ends with `liveness` |
| Trace file | 16 bytes or more, default 1 GiB | Later records are hashed, not written |

## 8. Security

The simulator is test infrastructure and is not built into release binaries.
Upholds I-20 and is the means of testing most other invariants.

`mem` randomness is predictable from the seed, so `mem` implementations must never
be registered in a release build (ARFC 0001 clause 3.5). Trace records carry
lengths and 8-byte digests of payloads, never the payloads themselves. Observers
(clause 3.6.1) receive payloads in memory, inside the test process only, so a
checker can inspect them. Nothing the simulator writes to a file or to standard
error holds them.

The `host` seam can change one thing on a machine: whether the service
`alloyd` is enabled and running (clause 3.1.18.4). It takes no service name,
runs `systemctl` only from a fixed path with a fixed environment, and is kept
out of `Runtime`, so only `init` and `join` hold it (ADR 0071).

## 9. Open questions

- Parameters and real-world frequencies of NAT presets. Task T-0005.
- Whether a Wintun adapter outlives a process that ends without calling
  `WintunCloseAdapter` is not documented (<https://git.zx2c4.com/wintun/about/>).
  Clause 3.1.17.2 holds either way: if it does, the next `create_tun` with the
  adapter's GUID takes it over, and the adapter stays on the host after that
  `Tun` is removed, until the uninstall removes it.
