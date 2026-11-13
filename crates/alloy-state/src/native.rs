//! The `native` implementations (ARFC 0006 sections 4.2 and 5.2, ADR 0064):
//! one `redb` file in the configured directory. The log commits with two-phase
//! commit and immediate durability, the state store with immediate durability
//! and the default commit.
//!
//! `redb` does its file access synchronously, so each call runs its
//! transaction inline on the task that awaits it. ADR 0064 confines these
//! stores to `os` runtimes, where a call holds one runtime thread for the
//! length of its commit.

use std::fmt;
use std::io::ErrorKind;
use std::ops::Bound;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use alloy_seam::{BoxFuture, BuildError, ConfigError, ConfigValue, Detail, SeamConfig};
use redb::{
    Database, DatabaseError, Durability, ReadTransaction, ReadableDatabase, ReadableTable,
    TableDefinition, WriteTransaction,
};

use crate::config::NativeStoreConfig;
use crate::export::{Export, Reader, Source};
use crate::rules::{
    LOG_ID_LEN, check_append, check_apply, check_scan_limit, corrupt, decode_log_id, decode_vote,
    encode_log_id, encode_vote, invalid, read_full, scan_full,
};
use crate::store::{
    ApplyBatch, Entry, LogId, LogState, LogStore, Op, Pairs, StateInstall, StateSnapshot,
    StateStore, StoreError, Vote,
};

/// Log entries by index: the entry's position, then its payload.
const ENTRIES: TableDefinition<'_, u64, &[u8]> = TableDefinition::new("entries");
/// `vote`, `last_purged`, and `applied`.
const META: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("meta");
/// The state store's pairs.
const PAIRS: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("pairs");
/// The pairs of an install in progress.
const STAGED: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("staged");

fn store_error(error: impl Into<redb::Error>) -> StoreError {
    match error.into() {
        redb::Error::Corrupted(detail) => corrupt(&detail),
        other => StoreError::Io(Detail::new(&other.to_string())),
    }
}

/// An open `redb` file, with the sticky `Io` failure of clauses 5.3.8 and
/// 5.4.7.
struct Db {
    db: Database,
    path: PathBuf,
    failed: AtomicBool,
}

impl Db {
    /// Opens or creates `file` in the configured directory, then runs `init`,
    /// which creates the tables so that reads never meet a missing one.
    fn open(
        config: &ConfigValue,
        file: &str,
        init: fn(&WriteTransaction) -> Result<(), StoreError>,
    ) -> Result<Db, BuildError> {
        let config = NativeStoreConfig::from_value(config).map_err(BuildError::Config)?;
        let path = config.checked_dir().map_err(BuildError::Config)?.join(file);
        let db = Database::create(&path).map_err(|error| match error {
            DatabaseError::Storage(redb::StorageError::Io(io))
                if matches!(io.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                BuildError::Config(ConfigError::Invalid {
                    field: Detail::new("dir"),
                    detail: Detail::new("is not an existing directory"),
                })
            }
            other => BuildError::Failed(Detail::new(&format!("{}: {other}", path.display()))),
        })?;
        let db = Db {
            db,
            path,
            failed: AtomicBool::new(false),
        };
        db.write(false, init)
            .map_err(|error| BuildError::Failed(Detail::new(&error.to_string())))?;
        Ok(db)
    }

    /// Runs `f` and makes an `Io` failure sticky.
    fn run<R>(&self, f: impl FnOnce(&Database) -> Result<R, StoreError>) -> Result<R, StoreError> {
        if self.failed.load(Ordering::SeqCst) {
            return Err(StoreError::Io(Detail::new("an earlier call failed")));
        }
        let result = f(&self.db);
        if let Err(StoreError::Io(_)) = &result {
            self.failed.store(true, Ordering::SeqCst);
        }
        result
    }

    /// Runs `f` in a write transaction committed with immediate durability.
    /// The transaction is aborted when `f` fails.
    fn write<R>(
        &self,
        two_phase: bool,
        f: impl FnOnce(&WriteTransaction) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        self.run(|db| {
            let mut txn = db.begin_write().map_err(store_error)?;
            txn.set_durability(Durability::Immediate)
                .map_err(store_error)?;
            txn.set_two_phase_commit(two_phase);
            let result = f(&txn)?;
            txn.commit().map_err(store_error)?;
            Ok(result)
        })
    }

    fn read<R>(
        &self,
        f: impl FnOnce(&ReadTransaction) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        self.run(|db| f(&db.begin_read().map_err(store_error)?))
    }
}

fn meta_log_id(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    key: &str,
) -> Result<Option<LogId>, StoreError> {
    match table.get(key).map_err(store_error)? {
        None => Ok(None),
        Some(bytes) => decode_log_id(bytes.value())
            .map(Some)
            .ok_or_else(|| corrupt(&format!("{key} does not decode"))),
    }
}

fn decode_entry(index: u64, bytes: &[u8]) -> Result<Entry, StoreError> {
    let (id, payload) = bytes
        .split_at_checked(LOG_ID_LEN)
        .ok_or_else(|| corrupt("a short entry"))?;
    let log_id = decode_log_id(id)
        .filter(|id| id.index == index)
        .ok_or_else(|| corrupt("an entry's position"))?;
    Ok(Entry {
        log_id,
        payload: payload.to_vec(),
    })
}

/// The `native` log store.
pub(crate) struct NativeLog(Db);

impl fmt::Debug for NativeLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeLog")
            .field("path", &self.0.path)
            .finish()
    }
}

impl NativeLog {
    fn log_state(
        txn_meta: &impl ReadableTable<&'static str, &'static [u8]>,
        entries: &impl ReadableTable<u64, &'static [u8]>,
    ) -> Result<LogState, StoreError> {
        let last_purged = meta_log_id(txn_meta, "last_purged")?;
        let last = match entries.last().map_err(store_error)? {
            Some((index, bytes)) => Some(decode_entry(index.value(), bytes.value())?.log_id),
            None => last_purged,
        };
        Ok(LogState { last_purged, last })
    }
}

impl LogStore for NativeLog {
    fn read_vote(&self) -> BoxFuture<'_, Result<Option<Vote>, StoreError>> {
        Box::pin(async move {
            self.0.read(|txn| {
                let meta = txn.open_table(META).map_err(store_error)?;
                match meta.get("vote").map_err(store_error)? {
                    None => Ok(None),
                    Some(bytes) => decode_vote(bytes.value())
                        .map(Some)
                        .ok_or_else(|| corrupt("the vote")),
                }
            })
        })
    }

    fn save_vote(&self, vote: Vote) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            self.0.write(true, |txn| {
                let mut meta = txn.open_table(META).map_err(store_error)?;
                meta.insert("vote", &encode_vote(vote)[..])
                    .map_err(store_error)?;
                Ok(())
            })
        })
    }

    fn state(&self) -> BoxFuture<'_, Result<LogState, StoreError>> {
        Box::pin(async move {
            self.0.read(|txn| {
                let meta = txn.open_table(META).map_err(store_error)?;
                let entries = txn.open_table(ENTRIES).map_err(store_error)?;
                NativeLog::log_state(&meta, &entries)
            })
        })
    }

    fn read(&self, first: u64, last: u64) -> BoxFuture<'_, Result<Vec<Entry>, StoreError>> {
        Box::pin(async move {
            if first > last {
                return Err(invalid("first is above last"));
            }
            self.0.read(|txn| {
                let entries = txn.open_table(ENTRIES).map_err(store_error)?;
                let mut out = Vec::new();
                let mut total = 0usize;
                for item in entries.range(first..=last).map_err(store_error)? {
                    let (index, bytes) = item.map_err(store_error)?;
                    let entry = decode_entry(index.value(), bytes.value())?;
                    total = total.saturating_add(entry.payload.len());
                    out.push(entry);
                    if read_full(total) {
                        break;
                    }
                }
                Ok(out)
            })
        })
    }

    fn append(&self, entries: Vec<Entry>) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            self.0.write(true, |txn| {
                let meta = txn.open_table(META).map_err(store_error)?;
                let mut table = txn.open_table(ENTRIES).map_err(store_error)?;
                check_append(&entries, NativeLog::log_state(&meta, &table)?.last)?;
                let mut value = Vec::new();
                for entry in &entries {
                    value.clear();
                    value.extend_from_slice(&encode_log_id(entry.log_id));
                    value.extend_from_slice(&entry.payload);
                    table
                        .insert(entry.log_id.index, &value[..])
                        .map_err(store_error)?;
                }
                Ok(())
            })
        })
    }

    fn truncate_from(&self, index: u64) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            self.0.write(true, |txn| {
                let meta = txn.open_table(META).map_err(store_error)?;
                if meta_log_id(&meta, "last_purged")?.is_some_and(|p| index <= p.index) {
                    return Err(invalid("index is at or below the last purged index"));
                }
                let mut entries = txn.open_table(ENTRIES).map_err(store_error)?;
                entries
                    .retain_in(index.., |_, _| false)
                    .map_err(store_error)?;
                Ok(())
            })
        })
    }

    fn purge_through(&self, log_id: LogId) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            self.0.write(true, |txn| {
                let mut meta = txn.open_table(META).map_err(store_error)?;
                if meta_log_id(&meta, "last_purged")?.is_some_and(|p| log_id.index <= p.index) {
                    return Ok(());
                }
                let mut entries = txn.open_table(ENTRIES).map_err(store_error)?;
                entries
                    .retain_in(..=log_id.index, |_, _| false)
                    .map_err(store_error)?;
                meta.insert("last_purged", &encode_log_id(log_id)[..])
                    .map_err(store_error)?;
                Ok(())
            })
        })
    }
}

/// The `native` state store.
pub(crate) struct NativeState {
    db: Arc<Db>,
    installing: Arc<AtomicBool>,
}

impl fmt::Debug for NativeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeState")
            .field("path", &self.db.path)
            .finish()
    }
}

/// A snapshot's source: a read transaction, which sees the store as of its
/// start.
struct TxnSource(ReadTransaction);

impl Source for TxnSource {
    fn after(&mut self, after: Option<&[u8]>, max: usize) -> Result<Pairs, StoreError> {
        let pairs = self.0.open_table(PAIRS).map_err(store_error)?;
        let start = after.map_or(Bound::Unbounded, Bound::Excluded);
        let mut out = Vec::new();
        for item in pairs
            .range::<&[u8]>((start, Bound::Unbounded))
            .map_err(store_error)?
            .take(max)
        {
            let (key, value) = item.map_err(store_error)?;
            out.push((key.value().to_vec(), value.value().to_vec()));
        }
        Ok(out)
    }
}

impl StateStore for NativeState {
    fn applied(&self) -> BoxFuture<'_, Result<Option<LogId>, StoreError>> {
        Box::pin(async move {
            self.db
                .read(|txn| meta_log_id(&txn.open_table(META).map_err(store_error)?, "applied"))
        })
    }

    fn apply(&self, batch: ApplyBatch) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            if self.installing.load(Ordering::SeqCst) {
                self.db.run(|_| Ok(()))?;
                return Err(StoreError::Busy);
            }
            self.db.write(false, |txn| {
                let mut meta = txn.open_table(META).map_err(store_error)?;
                check_apply(&batch, meta_log_id(&meta, "applied")?)?;
                let mut pairs = txn.open_table(PAIRS).map_err(store_error)?;
                for op in &batch.ops {
                    match op {
                        Op::Put { key, value } => {
                            drop(pairs.insert(&key[..], &value[..]).map_err(store_error)?)
                        }
                        Op::Delete { key } => drop(pairs.remove(&key[..]).map_err(store_error)?),
                    }
                }
                meta.insert("applied", &encode_log_id(batch.log_id)[..])
                    .map_err(store_error)?;
                Ok(())
            })
        })
    }

    fn get<'a>(&'a self, key: &'a [u8]) -> BoxFuture<'a, Result<Option<Vec<u8>>, StoreError>> {
        Box::pin(async move {
            self.db.read(|txn| {
                let pairs = txn.open_table(PAIRS).map_err(store_error)?;
                Ok(pairs
                    .get(key)
                    .map_err(store_error)?
                    .map(|value| value.value().to_vec()))
            })
        })
    }

    fn scan<'a>(
        &'a self,
        start: &'a [u8],
        end: Option<&'a [u8]>,
        limit: usize,
    ) -> BoxFuture<'a, Result<Pairs, StoreError>> {
        Box::pin(async move {
            check_scan_limit(limit)?;
            self.db.read(|txn| {
                let mut out = Vec::new();
                if end.is_some_and(|end| end <= start) {
                    return Ok(out);
                }
                let pairs = txn.open_table(PAIRS).map_err(store_error)?;
                let upper = end.map_or(Bound::Unbounded, Bound::Excluded);
                let mut total = 0usize;
                for item in pairs
                    .range::<&[u8]>((Bound::Included(start), upper))
                    .map_err(store_error)?
                {
                    let (key, value) = item.map_err(store_error)?;
                    total = total.saturating_add(key.value().len() + value.value().len());
                    out.push((key.value().to_vec(), value.value().to_vec()));
                    if scan_full(out.len(), limit, total) {
                        break;
                    }
                }
                Ok(out)
            })
        })
    }

    fn snapshot(&self) -> BoxFuture<'_, Result<Box<dyn StateSnapshot>, StoreError>> {
        Box::pin(async move {
            self.db.run(|db| {
                let txn = db.begin_read().map_err(store_error)?;
                let applied = meta_log_id(&txn.open_table(META).map_err(store_error)?, "applied")?;
                Ok(Box::new(Export::new(applied, TxnSource(txn))) as Box<dyn StateSnapshot>)
            })
        })
    }

    fn begin_install(&self) -> BoxFuture<'_, Result<Box<dyn StateInstall>, StoreError>> {
        Box::pin(async move {
            self.db.run(|_| Ok(()))?;
            if self.installing.swap(true, Ordering::SeqCst) {
                return Err(StoreError::Busy);
            }
            let install = NativeInstall {
                db: Arc::clone(&self.db),
                installing: Arc::clone(&self.installing),
                reader: Mutex::new(Reader::default()),
            };
            // Drops what an install cut short by a stop left behind.
            self.db.write(false, |txn| {
                txn.delete_table(STAGED).map_err(store_error)?;
                drop(txn.open_table(STAGED).map_err(store_error)?);
                Ok(())
            })?;
            Ok(Box::new(install) as Box<dyn StateInstall>)
        })
    }
}

/// An install into a `native` state store, staged in its own table until
/// `finish` moves it in one transaction.
struct NativeInstall {
    db: Arc<Db>,
    installing: Arc<AtomicBool>,
    reader: Mutex<Reader>,
}

impl fmt::Debug for NativeInstall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeInstall")
            .field("path", &self.db.path)
            .finish_non_exhaustive()
    }
}

impl Drop for NativeInstall {
    fn drop(&mut self) {
        self.installing.store(false, Ordering::SeqCst);
    }
}

impl StateInstall for NativeInstall {
    fn write(&self, chunk: Vec<u8>) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            let mut pairs = Vec::new();
            self.reader
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(&chunk, &mut |key, value| {
                    pairs.push((key, value));
                    Ok(())
                })?;
            if pairs.is_empty() {
                return self.db.run(|_| Ok(()));
            }
            self.db.write(false, |txn| {
                let mut staged = txn.open_table(STAGED).map_err(store_error)?;
                for (key, value) in &pairs {
                    staged.insert(&key[..], &value[..]).map_err(store_error)?;
                }
                Ok(())
            })
        })
    }

    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<Option<LogId>, StoreError>> {
        Box::pin(async move {
            let applied = self
                .reader
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .finish()?;
            self.db.write(false, |txn| {
                txn.delete_table(PAIRS).map_err(store_error)?;
                let mut pairs = txn.open_table(PAIRS).map_err(store_error)?;
                let staged = txn.open_table(STAGED).map_err(store_error)?;
                for item in staged.iter().map_err(store_error)? {
                    let (key, value) = item.map_err(store_error)?;
                    pairs
                        .insert(key.value(), value.value())
                        .map_err(store_error)?;
                }
                drop(staged);
                txn.delete_table(STAGED).map_err(store_error)?;
                let mut meta = txn.open_table(META).map_err(store_error)?;
                match applied {
                    Some(id) => drop(
                        meta.insert("applied", &encode_log_id(id)[..])
                            .map_err(store_error)?,
                    ),
                    None => drop(meta.remove("applied").map_err(store_error)?),
                }
                Ok(applied)
            })
        })
    }
}

pub(crate) fn log_factory(config: &ConfigValue) -> Result<Arc<dyn LogStore>, BuildError> {
    let db = Db::open(config, "log.redb", |txn| {
        drop(txn.open_table(ENTRIES).map_err(store_error)?);
        drop(txn.open_table(META).map_err(store_error)?);
        Ok(())
    })?;
    Ok(Arc::new(NativeLog(db)))
}

pub(crate) fn state_factory(config: &ConfigValue) -> Result<Arc<dyn StateStore>, BuildError> {
    let db = Db::open(config, "state.redb", |txn| {
        drop(txn.open_table(PAIRS).map_err(store_error)?);
        drop(txn.open_table(META).map_err(store_error)?);
        Ok(())
    })?;
    Ok(Arc::new(NativeState {
        db: Arc::new(db),
        installing: Arc::new(AtomicBool::new(false)),
    }))
}
