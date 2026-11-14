//! The runtime interface of Alloy (ARFC 0005 sections 2.1 to 2.5).
//!
//! All of Alloy reaches the world through five seams: [`Clock`], [`Net`],
//! [`DiskIo`], [`Rng`], and [`Spawn`]. A [`Runtime`] bundles one instance of
//! each, and every component takes a `Runtime` at construction and uses
//! nothing else for time, network, storage, randomness, or concurrency.
//! A sixth seam, [`Host`], gives the machine's facts and control of the
//! service `alloyd`; it is not in `Runtime`, and only `alloy init` and
//! `alloy join` build one (ARFC 0005 section 2.8, ADR 0071). In
//! production the `os` implementations serve the seams; under test the
//! deterministic simulator's `mem` implementations do.
//!
//! Every asynchronous method returns [`BoxFuture`], which keeps the traits
//! object safe. Network buffers are borrowed, so a receive writes into the
//! caller's memory; disk buffers ([`IoBuf`]) are owned and moved through each
//! call. Types that carry packet or disk bytes never print them in their
//! `Debug` output (I-22).
//!
//! [`OsHost`] and [`register_os`] provide the `os` implementations (ARFC 0005
//! section 2.6): the operating system's clock, secure generator, sockets, and
//! files, on one `tokio` runtime. They are the only code in this crate that
//! calls the operating system, and the only code that uses `unsafe`, where a
//! platform call has no safe binding.
//!
//! Methods of every seam take `&self`, may be called concurrently from any
//! task, and never panic on any argument: invalid input returns an error.
//! Dropping a future returned by a seam method cancels it with the guarantee
//! stated for that method (ARFC 0005 clause 3.1.3).

#![deny(unsafe_code)]

pub use alloy_seam::BoxFuture;

mod disk;
mod host;
mod net;
mod os;
mod rng;
mod runtime;
mod task;
mod time;

pub use disk::{
    Access, Ace, Create, DirEntry, DiskError, DiskFile, DiskIo, EntryKind, FileInfo, IoBuf,
    MAX_DISK_BATCH, Metadata, OpenOptions, Protection, ReadDone, ReadOp, RtPath, Security, Sid,
    SyncKind, WriteDone, WriteOp,
};
pub use host::{
    Arch, DiskFact, Host, HostError, HostFacts, HostSeam, Platform, RunState, ServiceAction,
    ServiceStatus,
};
pub use net::{
    Datagram, Interface, InterfaceAddr, InterfaceList, ListenOptions, Listener, MAX_NET_BATCH, Net,
    NetError, RecvSlot, SendOutcome, Stream, StreamAddr, StreamOptions, UdpOptions, UdpSocket,
    UnixPath,
};
pub use os::{OsHost, register_os};
pub use rng::{Rng, RngError};
pub use runtime::{
    ClockSeam, DiskIoSeam, Elapsed, JoinHandle, NetSeam, RngSeam, Runtime, RuntimeConfig, SpawnSeam,
};
pub use task::{JoinError, Spawn, SpawnError, TaskHandle, TaskId, TaskName};
pub use time::{Clock, Instant, WallTime};
