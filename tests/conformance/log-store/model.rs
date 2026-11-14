//! A model of one log store, written from clauses 5.3.1 to 5.3.7. The checks
//! compare a store against it. It is a model, not a seam implementation: it
//! has no trait, no storage, and no instance in any registry.

use std::collections::BTreeMap;

use crate::state::{
    Entry, LogId, LogState, MAX_APPEND_BYTES, MAX_ENTRY_PAYLOAD, MAX_READ_BYTES, StoreError, Vote,
};

/// The error a call fails with, without its detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    InvalidInput,
    TooLarge,
    NotContiguous,
    Other,
}

/// The kind of `error`.
pub(crate) fn kind(error: &StoreError) -> Kind {
    match error {
        StoreError::InvalidInput(_) => Kind::InvalidInput,
        StoreError::TooLarge { .. } => Kind::TooLarge,
        StoreError::NotContiguous => Kind::NotContiguous,
        _ => Kind::Other,
    }
}

/// The kind of a result's error, or `None` for `Ok`.
pub(crate) fn outcome<T>(result: &Result<T, StoreError>) -> Option<Kind> {
    result.as_ref().err().map(kind)
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LogModel {
    pub(crate) vote: Option<Vote>,
    entries: BTreeMap<u64, Entry>,
    last_purged: Option<LogId>,
}

impl LogModel {
    /// `LogState` as section 5.1 defines it.
    pub(crate) fn state(&self) -> LogState {
        LogState {
            last_purged: self.last_purged,
            last: self
                .entries
                .values()
                .next_back()
                .map(|entry| entry.log_id)
                .or(self.last_purged),
        }
    }

    /// The index an append must start at, 1 when the log has no `last`.
    pub(crate) fn next_index(&self) -> u64 {
        self.state().last.map_or(1, |last| last.index + 1)
    }

    /// The held entry at `index`.
    pub(crate) fn held(&self, index: u64) -> Option<&Entry> {
        self.entries.get(&index)
    }

    /// Clause 5.3.4.
    pub(crate) fn append(&mut self, entries: &[Entry]) -> Result<(), Kind> {
        let mut expected = self.state().last.map(|last| last.index + 1);
        let mut total = 0usize;
        for entry in entries {
            if expected.is_some_and(|index| index != entry.log_id.index) {
                return Err(Kind::NotContiguous);
            }
            expected = Some(entry.log_id.index + 1);
        }
        for entry in entries {
            if entry.payload.len() > MAX_ENTRY_PAYLOAD {
                return Err(Kind::TooLarge);
            }
            total += entry.payload.len();
        }
        if total > MAX_APPEND_BYTES {
            return Err(Kind::TooLarge);
        }
        for entry in entries {
            self.entries.insert(entry.log_id.index, entry.clone());
        }
        Ok(())
    }

    /// Clause 5.3.5.
    pub(crate) fn read(&self, first: u64, last: u64) -> Result<Vec<Entry>, Kind> {
        if first > last {
            return Err(Kind::InvalidInput);
        }
        let mut out = Vec::new();
        let mut total = 0usize;
        for entry in self.entries.range(first..=last).map(|(_, entry)| entry) {
            out.push(entry.clone());
            total += entry.payload.len();
            if total >= MAX_READ_BYTES {
                break;
            }
        }
        Ok(out)
    }

    /// Clause 5.3.6.
    pub(crate) fn truncate_from(&mut self, index: u64) -> Result<(), Kind> {
        if self.last_purged.is_some_and(|purged| index <= purged.index) {
            return Err(Kind::InvalidInput);
        }
        self.entries.retain(|&held, _| held < index);
        Ok(())
    }

    /// Clause 5.3.7.
    pub(crate) fn purge_through(&mut self, log_id: LogId) {
        if self
            .last_purged
            .is_some_and(|purged| log_id.index <= purged.index)
        {
            return;
        }
        self.entries.retain(|&held, _| held > log_id.index);
        self.last_purged = Some(log_id);
    }
}
