//! The public interface of the seams `log-store` and `state-store` (ARFC 0006
//! section 5.1).

use std::fmt;

use alloy_seam::{BoxFuture, Detail, Seam, SwapClass};

/// The largest payload of one log entry, in bytes.
pub const MAX_ENTRY_PAYLOAD: usize = 67_174_400;
/// The largest total payload of one `append`, in bytes.
pub const MAX_APPEND_BYTES: usize = 134_217_728;
/// The payload total at which `read` stops, and the key and value total at
/// which `scan` stops, in bytes.
pub const MAX_READ_BYTES: usize = 67_108_864;
/// The longest state store key, in bytes.
pub const MAX_KEY: usize = 4_096;
/// The longest state store value, in bytes.
pub const MAX_VALUE: usize = 2_097_152;
/// The largest total of keys and values in one `apply`, in bytes.
pub const MAX_BATCH_BYTES: usize = 67_108_864;
/// The largest `limit` of one `scan`.
pub const MAX_SCAN: usize = 10_000;
/// The largest chunk a [`StateSnapshot`] returns, in bytes.
pub const MAX_CHUNK: usize = 1_048_576;

/// Key and value pairs, in key order.
pub(crate) type Pairs = Vec<(Vec<u8>, Vec<u8>)>;

/// A member's Raft id: its `node_id` (ARFC 0002 section 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub [u8; 16]);

/// A Raft vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vote {
    /// The term voted in.
    pub term: u64,
    /// The candidate or leader voted for.
    pub node: NodeId,
    /// Granted by a quorum.
    pub committed: bool,
}

/// The position of a log entry: the term and leader that wrote it, and its
/// index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LogId {
    /// The term of the leader that wrote the entry.
    pub term: u64,
    /// The leader that wrote the entry.
    pub leader: NodeId,
    /// The entry's index in the log.
    pub index: u64,
}

/// One log entry. The store never interprets `payload`.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    /// The entry's position.
    pub log_id: LogId,
    /// The entry's bytes, at most [`MAX_ENTRY_PAYLOAD`].
    pub payload: Vec<u8>,
}

/// Writes `Entry { log_id: <log_id as Debug>, len: <payload.len()> }`.
impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("log_id", &self.log_id)
            .field("len", &self.payload.len())
            .finish()
    }
}

/// What a log store holds, by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogState {
    /// The highest position purged, `None` before the first purge.
    pub last_purged: Option<LogId>,
    /// The position of the highest entry held, else `last_purged`.
    pub last: Option<LogId>,
}

/// Seam `log-store`: one group's Raft log and vote on one member.
///
/// Every mutating call is atomic and durable when it returns `Ok` (ARFC 0006
/// clause 5.3.2). After a call fails with [`StoreError::Io`], every later call
/// on the instance fails with `Io` (clause 5.3.8).
pub trait LogStore: Send + Sync + fmt::Debug + 'static {
    /// The last vote saved, `None` when none was.
    fn read_vote(&self) -> BoxFuture<'_, Result<Option<Vote>, StoreError>>;
    /// Replaces the vote.
    fn save_vote(&self, vote: Vote) -> BoxFuture<'_, Result<(), StoreError>>;
    /// The positions of the last purged and the last held entries.
    fn state(&self) -> BoxFuture<'_, Result<LogState, StoreError>>;
    /// The held entries with index from `first` to `last`, ascending, stopping
    /// after the entry that brings the payload total to [`MAX_READ_BYTES`] or
    /// more. [`StoreError::InvalidInput`] when `first > last`.
    fn read(&self, first: u64, last: u64) -> BoxFuture<'_, Result<Vec<Entry>, StoreError>>;
    /// Adds `entries` after the last held entry (clause 5.3.4).
    fn append(&self, entries: Vec<Entry>) -> BoxFuture<'_, Result<(), StoreError>>;
    /// Removes every entry at or above `index` (clause 5.3.6).
    fn truncate_from(&self, index: u64) -> BoxFuture<'_, Result<(), StoreError>>;
    /// Removes every entry at or below `log_id.index` (clause 5.3.7).
    fn purge_through(&self, log_id: LogId) -> BoxFuture<'_, Result<(), StoreError>>;
}

/// One change to a state store.
#[derive(Clone, PartialEq, Eq)]
pub enum Op {
    /// Sets `key` to `value`.
    Put {
        /// 1 to [`MAX_KEY`] bytes.
        key: Vec<u8>,
        /// At most [`MAX_VALUE`] bytes.
        value: Vec<u8>,
    },
    /// Removes `key` if present.
    Delete {
        /// 1 to [`MAX_KEY`] bytes.
        key: Vec<u8>,
    },
}

/// Writes `Put { key_len: <n>, value_len: <n> }` or `Delete { key_len: <n> }`.
impl fmt::Debug for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Op::Put { key, value } => f
                .debug_struct("Put")
                .field("key_len", &key.len())
                .field("value_len", &value.len())
                .finish(),
            Op::Delete { key } => f
                .debug_struct("Delete")
                .field("key_len", &key.len())
                .finish(),
        }
    }
}

/// The ops of one applied log entry, and that entry's position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyBatch {
    /// The position the store's applied position becomes.
    pub log_id: LogId,
    /// The changes, performed in order.
    pub ops: Vec<Op>,
}

/// Seam `state-store`: one group's applied state on one member, an ordered
/// map from byte keys to byte values with an applied position.
///
/// `apply` and [`StateInstall::finish`] are atomic and durable when they
/// return `Ok` (ARFC 0006 clause 5.4.2). After a call fails with
/// [`StoreError::Io`], every later call on the instance fails with `Io`
/// (clause 5.4.7).
pub trait StateStore: Send + Sync + fmt::Debug + 'static {
    /// The position of the last entry applied, `None` before the first.
    fn applied(&self) -> BoxFuture<'_, Result<Option<LogId>, StoreError>>;
    /// Performs the batch's ops in order and sets the applied position
    /// (clause 5.4.3).
    fn apply(&self, batch: ApplyBatch) -> BoxFuture<'_, Result<(), StoreError>>;
    /// The value of `key`, `None` when absent.
    fn get<'a>(&'a self, key: &'a [u8]) -> BoxFuture<'a, Result<Option<Vec<u8>>, StoreError>>;
    /// The pairs from `start` up to `end`, ascending (clause 5.4.4). `end`
    /// `None` means no upper bound.
    #[allow(
        clippy::type_complexity,
        reason = "the signature of ARFC 0006 section 5.1"
    )]
    fn scan<'a>(
        &'a self,
        start: &'a [u8],
        end: Option<&'a [u8]>,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError>>;
    /// A view of the whole store as of the call (clause 5.4.5).
    fn snapshot(&self) -> BoxFuture<'_, Result<Box<dyn StateSnapshot>, StoreError>>;
    /// Starts replacing the whole store with an export (clause 5.4.6).
    fn begin_install(&self) -> BoxFuture<'_, Result<Box<dyn StateInstall>, StoreError>>;
}

/// A read-only view of a whole state store at one applied position.
pub trait StateSnapshot: Send + Sync + fmt::Debug {
    /// The view's applied position.
    fn applied(&self) -> Option<LogId>;
    /// The next 1 to `MAX_CHUNK` bytes of the export (section 4.1), `None`
    /// after the last.
    fn next_chunk(&self) -> BoxFuture<'_, Result<Option<Vec<u8>>, StoreError>>;
}

/// Receives an export in chunks of any size. Dropped before `finish`, it
/// leaves the store unchanged.
pub trait StateInstall: Send + Sync + fmt::Debug {
    /// Takes the next bytes of the export.
    fn write(&self, chunk: Vec<u8>) -> BoxFuture<'_, Result<(), StoreError>>;
    /// Checks the export, replaces the store's contents and applied position
    /// with it, and returns the export's applied position.
    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<Option<LogId>, StoreError>>;
}

/// Why a store call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// An argument breaks the call's rules.
    #[error("invalid input: {0}")]
    InvalidInput(Detail),
    /// An argument exceeds a limit.
    #[error("{what} is {len} bytes, the limit is {limit}")]
    TooLarge {
        /// What is too large.
        what: Detail,
        /// Its size.
        len: u64,
        /// The limit.
        limit: u64,
    },
    /// Appended entries do not follow the last held entry.
    #[error("the entries do not continue the log")]
    NotContiguous,
    /// An apply at or below the applied index.
    #[error("index {index} is not above the applied index {applied}")]
    Stale {
        /// The batch's index.
        index: u64,
        /// The store's applied index.
        applied: u64,
    },
    /// An install is in progress.
    #[error("an install is in progress")]
    Busy,
    /// An export breaks section 4.1.
    #[error("the export is malformed: {0}")]
    Malformed(Detail),
    /// Stored data does not decode.
    #[error("stored data is corrupt: {0}")]
    Corrupt(Detail),
    /// The storage below the store failed.
    #[error("storage failed: {0}")]
    Io(Detail),
}

/// Seam marker of `log-store` (ARFC 0001 section 2.1).
#[derive(Debug)]
pub struct LogStoreSeam;

impl Seam for LogStoreSeam {
    type Object = dyn LogStore;
    type Context = alloy_rt::Runtime;
    const KIND: &'static str = "log-store";
    const SWAP: SwapClass = SwapClass::Restart;
}

/// Seam marker of `state-store` (ARFC 0001 section 2.1).
#[derive(Debug)]
pub struct StateStoreSeam;

impl Seam for StateStoreSeam {
    type Object = dyn StateStore;
    type Context = alloy_rt::Runtime;
    const KIND: &'static str = "state-store";
    const SWAP: SwapClass = SwapClass::Restart;
}
