//! A model of one state store, written from clauses 5.4.1 to 5.4.4, and the
//! export encoding of section 4.1. The checks compare a store against it. It
//! is a model, not a seam implementation: it has no trait, no storage, and no
//! instance in any registry.

use std::collections::BTreeMap;

use crate::state::{
    ApplyBatch, LogId, MAX_BATCH_BYTES, MAX_KEY, MAX_READ_BYTES, MAX_SCAN, MAX_VALUE, Op,
    StoreError,
};

/// The error a call fails with, without its detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    InvalidInput,
    TooLarge,
    Stale,
    Busy,
    Malformed,
    Other,
}

/// The kind of `error`.
pub(crate) fn kind(error: &StoreError) -> Kind {
    match error {
        StoreError::InvalidInput(_) => Kind::InvalidInput,
        StoreError::TooLarge { .. } => Kind::TooLarge,
        StoreError::Stale { .. } => Kind::Stale,
        StoreError::Busy => Kind::Busy,
        StoreError::Malformed(_) => Kind::Malformed,
        _ => Kind::Other,
    }
}

/// The kind of a result's error, or `None` for `Ok`.
pub(crate) fn outcome<T>(result: &Result<T, StoreError>) -> Option<Kind> {
    result.as_ref().err().map(kind)
}

/// Pairs as `scan` returns them.
pub(crate) type Pairs = Vec<(Vec<u8>, Vec<u8>)>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StateModel {
    pub(crate) applied: Option<LogId>,
    pub(crate) map: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl StateModel {
    /// A model at `applied` holding `pairs`.
    pub(crate) fn with(applied: Option<LogId>, pairs: &[(&[u8], &[u8])]) -> StateModel {
        StateModel {
            applied,
            map: pairs
                .iter()
                .map(|(key, value)| (key.to_vec(), value.to_vec()))
                .collect(),
        }
    }

    /// Clause 5.4.3, for a store with no install in progress.
    pub(crate) fn apply(&mut self, batch: &ApplyBatch) -> Result<(), Kind> {
        if self
            .applied
            .is_some_and(|applied| batch.log_id.index <= applied.index)
        {
            return Err(Kind::Stale);
        }
        let mut total = 0usize;
        for op in &batch.ops {
            let (key, value) = match op {
                Op::Put { key, value } => (key, value.len()),
                Op::Delete { key } => (key, 0),
            };
            if key.is_empty() {
                return Err(Kind::InvalidInput);
            }
            if key.len() > MAX_KEY || value > MAX_VALUE {
                return Err(Kind::TooLarge);
            }
            total += key.len() + value;
        }
        if total > MAX_BATCH_BYTES {
            return Err(Kind::TooLarge);
        }
        for op in &batch.ops {
            match op {
                Op::Put { key, value } => {
                    self.map.insert(key.clone(), value.clone());
                }
                Op::Delete { key } => {
                    self.map.remove(key);
                }
            }
        }
        self.applied = Some(batch.log_id);
        Ok(())
    }

    /// Clause 5.4.4.
    pub(crate) fn scan(
        &self,
        start: &[u8],
        end: Option<&[u8]>,
        limit: usize,
    ) -> Result<Pairs, Kind> {
        if limit == 0 || limit > MAX_SCAN {
            return Err(Kind::InvalidInput);
        }
        let mut out = Vec::new();
        let mut total = 0usize;
        for (key, value) in self.map.range(start.to_vec()..) {
            if end.is_some_and(|end| key.as_slice() >= end) || out.len() == limit {
                break;
            }
            out.push((key.clone(), value.clone()));
            total += key.len() + value.len();
            if total >= MAX_READ_BYTES {
                break;
            }
        }
        Ok(out)
    }

    /// The export of section 4.1.
    pub(crate) fn export(&self) -> Vec<u8> {
        let records: Vec<(Vec<u8>, Vec<u8>)> = self
            .map
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        encode(&Header::of(self.applied), &records, records.len() as u64)
    }
}

/// The 41-byte header of an export, field by field, so that a check can
/// write any value into each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) magic: [u8; 8],
    pub(crate) has_applied: u8,
    pub(crate) term: u64,
    pub(crate) leader: [u8; 16],
    pub(crate) index: u64,
}

impl Header {
    /// The header of a store at `applied`.
    pub(crate) fn of(applied: Option<LogId>) -> Header {
        match applied {
            Some(at) => Header {
                magic: *b"ALSTATE1",
                has_applied: 1,
                term: at.term,
                leader: at.leader.0,
                index: at.index,
            },
            None => Header {
                magic: *b"ALSTATE1",
                has_applied: 0,
                term: 0,
                leader: [0; 16],
                index: 0,
            },
        }
    }
}

/// Encodes an export from its header, its records in the order given, and a
/// `count`, with the lengths of the actual keys and values and a correct
/// digest. With a correct header, sorted records, and the true count it is
/// the export of section 4.1.
pub(crate) fn encode(header: &Header, records: &[(Vec<u8>, Vec<u8>)], count: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&header.magic);
    out.push(header.has_applied);
    out.extend_from_slice(&header.term.to_be_bytes());
    out.extend_from_slice(&header.leader);
    out.extend_from_slice(&header.index.to_be_bytes());
    for (key, value) in records {
        out.extend_from_slice(&(key.len() as u32).to_be_bytes());
        out.extend_from_slice(key);
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(value);
    }
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&count.to_be_bytes());
    let digest = alloy_crypto::hash(&out);
    out.extend_from_slice(&digest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::NodeId;

    #[test]
    fn an_empty_export_is_header_trailer_and_digest() {
        let export = StateModel::default().export();
        assert_eq!(export.len(), 41 + 4 + 8 + 32);
        assert_eq!(&export[..8], b"ALSTATE1");
        assert!(export[8..53].iter().all(|&b| b == 0));
        assert_eq!(&export[53..], &alloy_crypto::hash(&export[..53]));
    }

    #[test]
    fn an_export_lays_out_fields_big_endian() {
        let applied = LogId {
            term: 0x0102,
            leader: NodeId([7; 16]),
            index: 0x0304,
        };
        let model = StateModel::with(Some(applied), &[(b"b", b"xy"), (b"a", b"")]);
        let export = model.export();
        assert_eq!(export[8], 1);
        assert_eq!(&export[9..17], &[0, 0, 0, 0, 0, 0, 1, 2]);
        assert_eq!(&export[17..33], &[7; 16]);
        assert_eq!(&export[33..41], &[0, 0, 0, 0, 0, 0, 3, 4]);
        assert_eq!(&export[41..50], &[0, 0, 0, 1, b'a', 0, 0, 0, 0]);
        assert_eq!(&export[50..61], &[0, 0, 0, 1, b'b', 0, 0, 0, 2, b'x', b'y']);
        assert_eq!(&export[61..73], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        assert_eq!(&export[73..], &alloy_crypto::hash(&export[..73]));
    }

    #[test]
    fn the_model_scan_honours_range_limit_and_order() {
        let mut model = StateModel::default();
        for key in [&b"c"[..], b"a", b"b", b"ab"] {
            model.map.insert(key.to_vec(), Vec::new());
        }
        let keys = |pairs: Pairs| pairs.into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        assert_eq!(
            keys(model.scan(b"a", Some(b"c"), 10).unwrap()),
            [b"a".to_vec(), b"ab".to_vec(), b"b".to_vec()]
        );
        assert_eq!(keys(model.scan(b"ab", None, 2).unwrap()).len(), 2);
        assert_eq!(model.scan(b"", None, 0), Err(Kind::InvalidInput));
        assert_eq!(model.scan(b"", None, MAX_SCAN + 1), Err(Kind::InvalidInput));
    }
}
