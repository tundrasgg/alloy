//! Histories of operations for checkers (ARFC 0005 clause 3.6.2).

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_seam::Detail;

use crate::core::Shared;
use crate::exec::lock;
use crate::world::WorldError;

/// Entries per history (section 7).
const MAX_ENTRIES: usize = 16_777_216;

/// A client of a history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub u64);

/// One invoked operation. Not `Clone`, so it completes at most once.
#[derive(Debug)]
pub struct Call {
    /// The token of its history. Tokens are compared by identity: each is
    /// its own allocation, which holds the reference counts, so a call
    /// matches only the history that returned it, in any world.
    history: Arc<()>,
    index: usize,
}

impl PartialEq for Call {
    /// Calls are equal when they come from the same history and invocation.
    fn eq(&self, other: &Call) -> bool {
        Arc::ptr_eq(&self.history, &other.history) && self.index == other.index
    }
}

impl Eq for Call {}

/// One operation of a history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry<Op, Ret> {
    /// The client that invoked it.
    pub client: ClientId,
    /// The operation.
    pub op: Op,
    /// The world time of the invocation.
    pub invoked: Duration,
    /// `None` while the call has not completed.
    pub completed: Option<(Duration, Ret)>,
}

struct Log<Op, Ret> {
    token: Arc<()>,
    shared: Arc<Shared>,
    entries: Mutex<Vec<Entry<Op, Ret>>>,
}

/// A log of operations with their world times. Clones share the log.
/// Histories write no trace records.
pub struct History<Op, Ret>(Arc<Log<Op, Ret>>);

impl<Op, Ret> Clone for History<Op, Ret> {
    /// Shares the log.
    fn clone(&self) -> Self {
        History(Arc::clone(&self.0))
    }
}

impl<Op, Ret> fmt::Debug for History<Op, Ret> {
    /// The number of entries only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("History")
            .field("entries", &lock(&self.0.entries).len())
            .finish()
    }
}

impl<Op: Clone + Send + 'static, Ret: Clone + Send + 'static> History<Op, Ret> {
    /// A new, empty history of the world `shared`.
    pub(crate) fn new(shared: Arc<Shared>) -> History<Op, Ret> {
        History(Arc::new(Log {
            token: Arc::new(()),
            shared,
            entries: Mutex::new(Vec::new()),
        }))
    }

    /// Appends an entry with `client`, `op`, and the current world time, and
    /// returns its call.
    ///
    /// # Errors
    ///
    /// [`WorldError::Limit`] when the history holds 16,777,216 entries, and
    /// [`WorldError::Ended`] after the run.
    pub fn invoke(&self, client: ClientId, op: Op) -> Result<Call, WorldError> {
        if self.0.shared.has_ended() {
            return Err(WorldError::Ended);
        }
        let mut entries = lock(&self.0.entries);
        if entries.len() >= MAX_ENTRIES {
            return Err(WorldError::Limit(Detail::new("16777216 history entries")));
        }
        entries.push(Entry {
            client,
            op,
            invoked: Duration::from_nanos(self.0.shared.now_nanos()),
            completed: None,
        });
        Ok(Call {
            history: Arc::clone(&self.0.token),
            index: entries.len() - 1,
        })
    }

    /// Sets the entry of `call` completed at the current world time with
    /// `ret`. A call of another history changes nothing.
    pub fn complete(&self, call: Call, ret: Ret) {
        if !Arc::ptr_eq(&call.history, &self.0.token) {
            return;
        }
        let now = Duration::from_nanos(self.0.shared.now_nanos());
        if let Some(entry) = lock(&self.0.entries).get_mut(call.index) {
            entry.completed = Some((now, ret));
        }
    }

    /// Every entry, in invocation order.
    pub fn entries(&self) -> Vec<Entry<Op, Ret>> {
        lock(&self.0.entries).clone()
    }
}
