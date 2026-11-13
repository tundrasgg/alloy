//! The `mem` implementations (ARFC 0006 section 5.2): contents in memory,
//! shared by name within one `register_mem` call, so that dropping an
//! instance and building a new one models a restart.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Bound;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use alloy_seam::{BoxFuture, BuildError, ConfigValue, Detail, SeamConfig};

use crate::config::MemStoreConfig;
use crate::export::{Export, Reader, Source};
use crate::rules::{check_append, check_apply, check_scan_limit, invalid, read_full, scan_full};
use crate::store::{
    ApplyBatch, Entry, LogId, LogState, LogStore, Op, Pairs, StateInstall, StateSnapshot,
    StateStore, StoreError, Vote,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn io(detail: &str) -> StoreError {
    StoreError::Io(Detail::new(detail))
}

/// Contents shared by every instance built with one name.
#[derive(Default)]
struct Shared<T> {
    data: Mutex<Counted<T>>,
    live: AtomicBool,
}

#[derive(Default)]
struct Counted<T> {
    data: T,
    /// Mutating calls that succeeded, for `fail_after`.
    succeeded: u64,
}

/// The tables of one `register_mem` call.
#[derive(Default)]
pub(crate) struct Tables {
    logs: Mutex<BTreeMap<String, Arc<Shared<LogData>>>>,
    states: Mutex<BTreeMap<String, Arc<Shared<StateData>>>>,
}

/// One instance's hold on its shared contents.
struct Inner<T> {
    shared: Arc<Shared<T>>,
    name: String,
    fail_after: u64,
    failed: AtomicBool,
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        self.shared.live.store(false, Ordering::SeqCst);
    }
}

impl<T: Default> Inner<T> {
    fn build(
        table: &Mutex<BTreeMap<String, Arc<Shared<T>>>>,
        config: &ConfigValue,
    ) -> Result<Inner<T>, BuildError> {
        let config = MemStoreConfig::from_value(config).map_err(BuildError::Config)?;
        config.check().map_err(BuildError::Config)?;
        let shared = Arc::clone(lock(table).entry(config.name.clone()).or_default());
        if shared.live.swap(true, Ordering::SeqCst) {
            return Err(BuildError::Failed(Detail::new(&format!(
                "a mem store named {} is live",
                config.name
            ))));
        }
        Ok(Inner {
            shared,
            name: config.name,
            fail_after: config.fail_after,
            failed: AtomicBool::new(false),
        })
    }
}

impl<T> Inner<T> {
    fn read<R>(&self, f: impl FnOnce(&T) -> Result<R, StoreError>) -> Result<R, StoreError> {
        if self.failed.load(Ordering::SeqCst) {
            return Err(io("an earlier call failed"));
        }
        f(&lock(&self.shared.data).data)
    }

    /// Runs `f`, which changes nothing when it fails, as one mutating call.
    fn mutate<R>(&self, f: impl FnOnce(&mut T) -> Result<R, StoreError>) -> Result<R, StoreError> {
        if self.failed.load(Ordering::SeqCst) {
            return Err(io("an earlier call failed"));
        }
        let mut guard = lock(&self.shared.data);
        if self.fail_after > 0 && guard.succeeded >= self.fail_after {
            self.failed.store(true, Ordering::SeqCst);
            return Err(io("fail_after reached"));
        }
        let result = f(&mut guard.data)?;
        guard.succeeded += 1;
        Ok(result)
    }
}

#[derive(Default)]
struct LogData {
    vote: Option<Vote>,
    entries: BTreeMap<u64, Entry>,
    last_purged: Option<LogId>,
}

impl LogData {
    fn last(&self) -> Option<LogId> {
        self.entries
            .last_key_value()
            .map(|(_, entry)| entry.log_id)
            .or(self.last_purged)
    }
}

/// The `mem` log store.
pub(crate) struct MemLog(Inner<LogData>);

impl fmt::Debug for MemLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemLog")
            .field("name", &self.0.name)
            .finish()
    }
}

impl LogStore for MemLog {
    fn read_vote(&self) -> BoxFuture<'_, Result<Option<Vote>, StoreError>> {
        Box::pin(async move { self.0.read(|log| Ok(log.vote)) })
    }

    fn save_vote(&self, vote: Vote) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            self.0.mutate(|log| {
                log.vote = Some(vote);
                Ok(())
            })
        })
    }

    fn state(&self) -> BoxFuture<'_, Result<LogState, StoreError>> {
        Box::pin(async move {
            self.0.read(|log| {
                Ok(LogState {
                    last_purged: log.last_purged,
                    last: log.last(),
                })
            })
        })
    }

    fn read(&self, first: u64, last: u64) -> BoxFuture<'_, Result<Vec<Entry>, StoreError>> {
        Box::pin(async move {
            if first > last {
                return Err(invalid("first is above last"));
            }
            self.0.read(|log| {
                let mut out = Vec::new();
                let mut total = 0usize;
                for entry in log.entries.range(first..=last).map(|(_, e)| e) {
                    total = total.saturating_add(entry.payload.len());
                    out.push(entry.clone());
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
            self.0.mutate(|log| {
                check_append(&entries, log.last())?;
                for entry in entries {
                    log.entries.insert(entry.log_id.index, entry);
                }
                Ok(())
            })
        })
    }

    fn truncate_from(&self, index: u64) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            self.0.mutate(|log| {
                if log.last_purged.is_some_and(|purged| index <= purged.index) {
                    return Err(invalid("index is at or below the last purged index"));
                }
                log.entries.split_off(&index);
                Ok(())
            })
        })
    }

    fn purge_through(&self, log_id: LogId) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            self.0.mutate(|log| {
                if log.last_purged.is_some_and(|p| log_id.index <= p.index) {
                    return Ok(());
                }
                log.entries = match log_id.index.checked_add(1) {
                    Some(next) => log.entries.split_off(&next),
                    None => BTreeMap::new(),
                };
                log.last_purged = Some(log_id);
                Ok(())
            })
        })
    }
}

type Map = BTreeMap<Vec<u8>, Vec<u8>>;

#[derive(Default)]
struct StateData {
    map: Arc<Map>,
    applied: Option<LogId>,
    installing: bool,
}

/// The `mem` state store.
pub(crate) struct MemState(Arc<Inner<StateData>>);

impl fmt::Debug for MemState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemState")
            .field("name", &self.0.name)
            .finish()
    }
}

/// A snapshot's source: the map as it was, kept alive by its `Arc`.
struct MapSource(Arc<Map>);

impl Source for MapSource {
    fn after(&mut self, after: Option<&[u8]>, max: usize) -> Result<Pairs, StoreError> {
        let start = after.map_or(Bound::Unbounded, |key| Bound::Excluded(key.to_vec()));
        Ok(self
            .0
            .range((start, Bound::Unbounded))
            .take(max)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }
}

impl StateStore for MemState {
    fn applied(&self) -> BoxFuture<'_, Result<Option<LogId>, StoreError>> {
        Box::pin(async move { self.0.read(|state| Ok(state.applied)) })
    }

    fn apply(&self, batch: ApplyBatch) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            self.0.mutate(|state| {
                if state.installing {
                    return Err(StoreError::Busy);
                }
                check_apply(&batch, state.applied)?;
                let map = Arc::make_mut(&mut state.map);
                for op in batch.ops {
                    match op {
                        Op::Put { key, value } => map.insert(key, value),
                        Op::Delete { key } => map.remove(&key),
                    };
                }
                state.applied = Some(batch.log_id);
                Ok(())
            })
        })
    }

    fn get<'a>(&'a self, key: &'a [u8]) -> BoxFuture<'a, Result<Option<Vec<u8>>, StoreError>> {
        Box::pin(async move { self.0.read(|state| Ok(state.map.get(key).cloned())) })
    }

    fn scan<'a>(
        &'a self,
        start: &'a [u8],
        end: Option<&'a [u8]>,
        limit: usize,
    ) -> BoxFuture<'a, Result<Pairs, StoreError>> {
        Box::pin(async move {
            check_scan_limit(limit)?;
            self.0.read(|state| {
                let mut out = Vec::new();
                if end.is_some_and(|end| end <= start) {
                    return Ok(out);
                }
                let upper = end.map_or(Bound::Unbounded, Bound::Excluded);
                let mut total = 0usize;
                for (key, value) in state.map.range::<[u8], _>((Bound::Included(start), upper)) {
                    total = total.saturating_add(key.len() + value.len());
                    out.push((key.clone(), value.clone()));
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
            self.0.read(|state| {
                let snapshot = Export::new(state.applied, MapSource(Arc::clone(&state.map)));
                Ok(Box::new(snapshot) as Box<dyn StateSnapshot>)
            })
        })
    }

    fn begin_install(&self) -> BoxFuture<'_, Result<Box<dyn StateInstall>, StoreError>> {
        Box::pin(async move {
            self.0.read(|_| Ok(()))?;
            let mut guard = lock(&self.0.shared.data);
            if guard.data.installing {
                return Err(StoreError::Busy);
            }
            guard.data.installing = true;
            Ok(Box::new(MemInstall {
                store: Arc::clone(&self.0),
                staged: Mutex::new((Reader::default(), Map::new())),
            }) as Box<dyn StateInstall>)
        })
    }
}

/// An install into a `mem` state store, staged in memory until `finish`.
struct MemInstall {
    store: Arc<Inner<StateData>>,
    staged: Mutex<(Reader, Map)>,
}

impl fmt::Debug for MemInstall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemInstall")
            .field("name", &self.store.name)
            .finish_non_exhaustive()
    }
}

impl Drop for MemInstall {
    fn drop(&mut self) {
        lock(&self.store.shared.data).data.installing = false;
    }
}

impl StateInstall for MemInstall {
    fn write(&self, chunk: Vec<u8>) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            self.store.read(|_| Ok(()))?;
            let mut guard = lock(&self.staged);
            let (reader, map) = &mut *guard;
            reader.push(&chunk, &mut |key, value| {
                map.insert(key, value);
                Ok(())
            })
        })
    }

    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<Option<LogId>, StoreError>> {
        Box::pin(async move {
            let (reader, map) = std::mem::take(&mut *lock(&self.staged));
            let applied = reader.finish()?;
            self.store.mutate(|state| {
                state.map = Arc::new(map);
                state.applied = applied;
                Ok(applied)
            })
        })
    }
}

/// Builds `mem` instances of both kinds from one set of tables.
pub(crate) fn log_factory(
    tables: &Arc<Tables>,
    config: &ConfigValue,
) -> Result<Arc<dyn LogStore>, BuildError> {
    Ok(Arc::new(MemLog(Inner::build(&tables.logs, config)?)))
}

pub(crate) fn state_factory(
    tables: &Arc<Tables>,
    config: &ConfigValue,
) -> Result<Arc<dyn StateStore>, BuildError> {
    Ok(Arc::new(MemState(Arc::new(Inner::build(
        &tables.states,
        config,
    )?))))
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;

    use super::*;
    use crate::store::NodeId;

    fn config(name: &str, fail_after: u64) -> ConfigValue {
        let config = MemStoreConfig {
            name: name.to_owned(),
            fail_after,
        };
        config.to_value(1).unwrap()
    }

    fn id(index: u64) -> LogId {
        LogId {
            term: 1,
            leader: NodeId([1; 16]),
            index,
        }
    }

    fn now<T>(future: BoxFuture<'_, T>) -> T {
        future.now_or_never().unwrap()
    }

    #[test]
    fn contents_outlive_an_instance_and_one_lives_at_a_time() {
        let tables = Arc::new(Tables::default());
        let log = log_factory(&tables, &config("a", 0)).unwrap();
        assert!(matches!(
            log_factory(&tables, &config("a", 0)),
            Err(BuildError::Failed(_))
        ));
        let entry = Entry {
            log_id: id(1),
            payload: vec![1],
        };
        now(log.append(vec![entry.clone()])).unwrap();
        drop(log);
        let log = log_factory(&tables, &config("a", 0)).unwrap();
        assert_eq!(now(log.read(1, 1)), Ok(vec![entry]));
        let other = log_factory(&tables, &config("b", 0)).unwrap();
        assert_eq!(now(other.state()).unwrap().last, None);
    }

    #[test]
    fn fail_after_fails_every_later_call_with_io() {
        let tables = Arc::new(Tables::default());
        let state = state_factory(&tables, &config("s", 2)).unwrap();
        let batch = |i| ApplyBatch {
            log_id: id(i),
            ops: vec![],
        };
        now(state.apply(batch(1))).unwrap();
        now(state.apply(batch(2))).unwrap();
        assert!(matches!(now(state.apply(batch(3))), Err(StoreError::Io(_))));
        assert!(matches!(now(state.applied()), Err(StoreError::Io(_))));
        drop(state);
        let state = state_factory(&tables, &config("s", 0)).unwrap();
        assert_eq!(now(state.applied()), Ok(Some(id(2))));
    }

    #[test]
    fn an_install_is_exclusive_and_dropping_it_changes_nothing() {
        let tables = Arc::new(Tables::default());
        let state = state_factory(&tables, &config("i", 0)).unwrap();
        let put = ApplyBatch {
            log_id: id(1),
            ops: vec![Op::Put {
                key: b"k".to_vec(),
                value: b"v".to_vec(),
            }],
        };
        now(state.apply(put)).unwrap();
        let install = now(state.begin_install()).unwrap();
        assert!(matches!(now(state.begin_install()), Err(StoreError::Busy)));
        let empty = ApplyBatch {
            log_id: id(2),
            ops: vec![],
        };
        assert_eq!(now(state.apply(empty.clone())), Err(StoreError::Busy));
        now(install.write(crate::export::tests::export_bytes(None, &Map::new()))).unwrap();
        drop(install);
        assert_eq!(now(state.get(b"k")), Ok(Some(b"v".to_vec())));
        now(state.apply(empty)).unwrap();
        let install = now(state.begin_install()).unwrap();
        now(install.write(b"ALSTATE1".to_vec())).unwrap();
        assert!(matches!(
            now(install.finish()),
            Err(StoreError::Malformed(_))
        ));
        assert_eq!(now(state.applied()), Ok(Some(id(2))));
    }
}
