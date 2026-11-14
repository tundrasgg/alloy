//! The checks that both implementations of each seam share (ARFC 0006
//! sections 5.3 and 5.4), and the fixed encodings of positions and votes.

use alloy_seam::Detail;

use crate::store::{
    ApplyBatch, Entry, LogId, MAX_APPEND_BYTES, MAX_BATCH_BYTES, MAX_ENTRY_PAYLOAD, MAX_KEY,
    MAX_READ_BYTES, MAX_SCAN, MAX_VALUE, NodeId, Op, StoreError, Vote,
};

pub(crate) fn invalid(detail: &str) -> StoreError {
    StoreError::InvalidInput(Detail::new(detail))
}

pub(crate) fn too_large(what: &str, len: usize, limit: usize) -> StoreError {
    StoreError::TooLarge {
        what: Detail::new(what),
        len: len as u64,
        limit: limit as u64,
    }
}

pub(crate) fn corrupt(detail: &str) -> StoreError {
    StoreError::Corrupt(Detail::new(detail))
}

/// Clause 5.3.4: sizes, then contiguity after `last`.
pub(crate) fn check_append(entries: &[Entry], last: Option<LogId>) -> Result<(), StoreError> {
    let mut total = 0usize;
    for entry in entries {
        if entry.payload.len() > MAX_ENTRY_PAYLOAD {
            return Err(too_large(
                "an entry payload",
                entry.payload.len(),
                MAX_ENTRY_PAYLOAD,
            ));
        }
        total = total.saturating_add(entry.payload.len());
    }
    if total > MAX_APPEND_BYTES {
        return Err(too_large("the appended payloads", total, MAX_APPEND_BYTES));
    }
    let mut previous = last.map(|id| id.index);
    for entry in entries {
        let expected = previous.map_or(Some(entry.log_id.index), |index| index.checked_add(1));
        if expected != Some(entry.log_id.index) {
            return Err(StoreError::NotContiguous);
        }
        previous = Some(entry.log_id.index);
    }
    Ok(())
}

/// Clause 5.3.5: whether `read` stops after an entry, given the payload total
/// so far including it.
pub(crate) fn read_full(total: usize) -> bool {
    total >= MAX_READ_BYTES
}

/// Clause 5.4.3, all but `Busy`: position, keys, and sizes.
pub(crate) fn check_apply(batch: &ApplyBatch, applied: Option<LogId>) -> Result<(), StoreError> {
    if let Some(applied) = applied
        && batch.log_id.index <= applied.index
    {
        return Err(StoreError::Stale {
            index: batch.log_id.index,
            applied: applied.index,
        });
    }
    let mut total = 0usize;
    for op in &batch.ops {
        let (key, value_len) = match op {
            Op::Put { key, value } => (key, value.len()),
            Op::Delete { key } => (key, 0),
        };
        if key.is_empty() {
            return Err(invalid("a key is empty"));
        }
        if key.len() > MAX_KEY {
            return Err(too_large("a key", key.len(), MAX_KEY));
        }
        if value_len > MAX_VALUE {
            return Err(too_large("a value", value_len, MAX_VALUE));
        }
        total = total.saturating_add(key.len()).saturating_add(value_len);
    }
    if total > MAX_BATCH_BYTES {
        return Err(too_large(
            "the batch's keys and values",
            total,
            MAX_BATCH_BYTES,
        ));
    }
    Ok(())
}

/// Clause 5.4.4: the `limit` of a scan.
pub(crate) fn check_scan_limit(limit: usize) -> Result<(), StoreError> {
    if limit == 0 || limit > MAX_SCAN {
        return Err(invalid("a scan limit must be 1 to 10000"));
    }
    Ok(())
}

/// Whether a scan stops after a pair, given the key and value total so far
/// including it.
pub(crate) fn scan_full(count: usize, limit: usize, total: usize) -> bool {
    count >= limit || total >= MAX_READ_BYTES
}

/// The bytes of a position: term, leader, and index, big endian.
pub(crate) const LOG_ID_LEN: usize = 32;

pub(crate) fn encode_log_id(id: LogId) -> [u8; LOG_ID_LEN] {
    let mut out = [0u8; LOG_ID_LEN];
    out[..8].copy_from_slice(&id.term.to_be_bytes());
    out[8..24].copy_from_slice(&id.leader.0);
    out[24..].copy_from_slice(&id.index.to_be_bytes());
    out
}

pub(crate) fn decode_log_id(bytes: &[u8]) -> Option<LogId> {
    let bytes: &[u8; LOG_ID_LEN] = bytes.try_into().ok()?;
    let (term, rest) = bytes.split_first_chunk::<8>()?;
    let (leader, index) = rest.split_first_chunk::<16>()?;
    let index: &[u8; 8] = index.try_into().ok()?;
    Some(LogId {
        term: u64::from_be_bytes(*term),
        leader: NodeId(*leader),
        index: u64::from_be_bytes(*index),
    })
}

/// The bytes of a vote: term, node, and 1 or 0 for committed.
pub(crate) fn encode_vote(vote: Vote) -> [u8; 25] {
    let mut out = [0u8; 25];
    out[..8].copy_from_slice(&vote.term.to_be_bytes());
    out[8..24].copy_from_slice(&vote.node.0);
    out[24] = u8::from(vote.committed);
    out
}

pub(crate) fn decode_vote(bytes: &[u8]) -> Option<Vote> {
    let bytes: &[u8; 25] = bytes.try_into().ok()?;
    let (term, rest) = bytes.split_first_chunk::<8>()?;
    let (node, committed) = rest.split_first_chunk::<16>()?;
    let committed = match committed {
        [0] => false,
        [1] => true,
        _ => return None,
    };
    Some(Vote {
        term: u64::from_be_bytes(*term),
        node: NodeId(*node),
        committed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(index: u64) -> LogId {
        LogId {
            term: 2,
            leader: NodeId([7; 16]),
            index,
        }
    }

    #[test]
    fn append_must_continue_the_log() {
        let entry = |index| Entry {
            log_id: id(index),
            payload: vec![1],
        };
        assert_eq!(check_append(&[entry(1), entry(2)], None), Ok(()));
        assert_eq!(check_append(&[entry(5)], None), Ok(()));
        assert_eq!(check_append(&[entry(4)], Some(id(3))), Ok(()));
        assert_eq!(
            check_append(&[entry(5)], Some(id(3))),
            Err(StoreError::NotContiguous)
        );
        assert_eq!(
            check_append(&[entry(1), entry(3)], None),
            Err(StoreError::NotContiguous)
        );
        assert_eq!(check_append(&[], Some(id(3))), Ok(()));
    }

    #[test]
    fn apply_checks_position_keys_and_sizes() {
        let batch = |index, ops| ApplyBatch {
            log_id: id(index),
            ops,
        };
        assert_eq!(
            check_apply(&batch(3, vec![]), Some(id(3))),
            Err(StoreError::Stale {
                index: 3,
                applied: 3
            })
        );
        let empty_key = vec![Op::Delete { key: vec![] }];
        assert!(matches!(
            check_apply(&batch(1, empty_key), None),
            Err(StoreError::InvalidInput(_))
        ));
        let long_key = vec![Op::Delete {
            key: vec![0; MAX_KEY + 1],
        }];
        assert!(matches!(
            check_apply(&batch(1, long_key), None),
            Err(StoreError::TooLarge { .. })
        ));
        let ok = vec![Op::Put {
            key: vec![1],
            value: vec![0; MAX_VALUE],
        }];
        assert_eq!(check_apply(&batch(1, ok), None), Ok(()));
    }

    #[test]
    fn positions_and_votes_round_trip() {
        assert_eq!(decode_log_id(&encode_log_id(id(9))), Some(id(9)));
        let vote = Vote {
            term: 4,
            node: NodeId([3; 16]),
            committed: true,
        };
        assert_eq!(decode_vote(&encode_vote(vote)), Some(vote));
        let mut bad = encode_vote(vote);
        bad[24] = 2;
        assert_eq!(decode_vote(&bad), None);
        assert_eq!(decode_log_id(&[0; 31]), None);
    }
}
