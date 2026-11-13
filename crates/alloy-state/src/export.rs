//! The state export format (ARFC 0006 section 4.1): a chunked writer over any
//! ordered source of pairs, and a streaming reader that checks every rule
//! before the caller replaces anything.

use std::fmt;
use std::sync::{Mutex, PoisonError};

use alloy_crypto::Hasher;
use alloy_seam::{BoxFuture, Detail};

use crate::rules::{LOG_ID_LEN, decode_log_id, encode_log_id};
use crate::store::{LogId, MAX_CHUNK, MAX_KEY, MAX_VALUE, Pairs, StateSnapshot, StoreError};

const MAGIC: &[u8; 8] = b"ALSTATE1";
/// `magic`, `has_applied`, and the position.
const HEADER_LEN: usize = 8 + 1 + LOG_ID_LEN;
/// The zero length, `count`, and `digest`.
const TRAILER_LEN: usize = 4 + 8 + 32;
/// How many pairs a snapshot reads from its source at a time.
const SOURCE_BATCH: usize = 256;

fn malformed(detail: &str) -> StoreError {
    StoreError::Malformed(Detail::new(detail))
}

/// An ordered source of a store's pairs, as of one moment.
pub(crate) trait Source: Send + 'static {
    /// Up to `max` pairs with keys above `after` (all keys when `None`), in
    /// ascending key order.
    fn after(&mut self, after: Option<&[u8]>, max: usize) -> Result<Pairs, StoreError>;
}

/// Writes an export into a buffer, hashing every byte it writes.
struct Writer {
    hasher: Hasher,
    count: u64,
    out: Vec<u8>,
}

impl Writer {
    fn new(applied: Option<LogId>) -> Writer {
        let mut writer = Writer {
            hasher: Hasher::new(),
            count: 0,
            out: Vec::new(),
        };
        writer.put(MAGIC);
        match applied {
            Some(id) => {
                writer.put(&[1]);
                writer.put(&encode_log_id(id));
            }
            None => writer.put(&[0; 1 + LOG_ID_LEN]),
        }
        writer
    }

    fn put(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.out.extend_from_slice(bytes);
    }

    fn record(&mut self, key: &[u8], value: &[u8]) {
        // Stored keys and values are within MAX_KEY and MAX_VALUE, so both
        // lengths fit in 4 bytes.
        self.put(&(key.len() as u32).to_be_bytes());
        self.put(key);
        self.put(&(value.len() as u32).to_be_bytes());
        self.put(value);
        self.count += 1;
    }

    fn trailer(&mut self) {
        self.put(&[0; 4]);
        self.put(&self.count.to_be_bytes());
        let digest = self.hasher.finalize();
        self.out.extend_from_slice(&digest);
    }
}

struct Progress<S> {
    source: S,
    writer: Writer,
    cursor: Option<Vec<u8>>,
    records_done: bool,
    sent: usize,
}

/// A [`StateSnapshot`] that exports a [`Source`] in chunks.
pub(crate) struct Export<S> {
    applied: Option<LogId>,
    progress: Mutex<Progress<S>>,
}

impl<S: Source> Export<S> {
    pub(crate) fn new(applied: Option<LogId>, source: S) -> Export<S> {
        Export {
            applied,
            progress: Mutex::new(Progress {
                source,
                writer: Writer::new(applied),
                cursor: None,
                records_done: false,
                sent: 0,
            }),
        }
    }

    fn next(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let mut guard = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
        let p = &mut *guard;
        while p.writer.out.len() - p.sent < MAX_CHUNK && !p.records_done {
            let batch = p.source.after(p.cursor.as_deref(), SOURCE_BATCH)?;
            match batch.last() {
                None => {
                    p.records_done = true;
                    p.writer.trailer();
                }
                Some((key, _)) => p.cursor = Some(key.clone()),
            }
            for (key, value) in &batch {
                p.writer.record(key, value);
            }
        }
        let available = p.writer.out.len() - p.sent;
        if available == 0 {
            return Ok(None);
        }
        let take = available.min(MAX_CHUNK);
        let chunk = p.writer.out[p.sent..p.sent + take].to_vec();
        p.sent += take;
        if p.sent == p.writer.out.len() {
            p.writer.out.clear();
            p.sent = 0;
        }
        Ok(Some(chunk))
    }
}

/// Writes `Export { applied: <applied> }`, never the contents.
impl<S> fmt::Debug for Export<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Export")
            .field("applied", &self.applied)
            .finish_non_exhaustive()
    }
}

impl<S: Source> StateSnapshot for Export<S> {
    fn applied(&self) -> Option<LogId> {
        self.applied
    }

    fn next_chunk(&self) -> BoxFuture<'_, Result<Option<Vec<u8>>, StoreError>> {
        Box::pin(async move { self.next() })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Header,
    Records,
    Done,
}

/// Reads an export in chunks of any size and hands out its pairs in order.
/// The first rule an export breaks is kept, and every later call reports it.
pub(crate) struct Reader {
    stage: Stage,
    pending: Vec<u8>,
    hasher: Hasher,
    count: u64,
    last_key: Option<Vec<u8>>,
    applied: Option<LogId>,
    error: Option<StoreError>,
}

impl Default for Reader {
    fn default() -> Reader {
        Reader {
            stage: Stage::Header,
            pending: Vec::new(),
            hasher: Hasher::new(),
            count: 0,
            last_key: None,
            applied: None,
            error: None,
        }
    }
}

/// Writes `Reader { count: <records read> }`, never the contents.
impl fmt::Debug for Reader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reader")
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl Reader {
    /// Takes `chunk` and calls `pair` for each record it completes. An error
    /// from `pair` is returned as is; a broken rule is kept for [`finish`].
    ///
    /// [`finish`]: Reader::finish
    pub(crate) fn push(
        &mut self,
        chunk: &[u8],
        pair: &mut dyn FnMut(Vec<u8>, Vec<u8>) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if self.error.is_some() {
            return Ok(());
        }
        let mut pending = std::mem::take(&mut self.pending);
        pending.extend_from_slice(chunk);
        let mut used = 0;
        let result = loop {
            match self.step(&pending[used..], pair) {
                Ok(0) => break Ok(()),
                Ok(n) => used += n,
                Err(error @ StoreError::Malformed(_)) => {
                    self.error = Some(error);
                    break Ok(());
                }
                Err(error) => break Err(error),
            }
        };
        if self.error.is_none() {
            pending.drain(..used);
            self.pending = pending;
        }
        result
    }

    /// Reads one item from the front of `bytes`. Returns how many bytes it
    /// used, 0 when `bytes` holds no whole item.
    fn step(
        &mut self,
        bytes: &[u8],
        pair: &mut dyn FnMut(Vec<u8>, Vec<u8>) -> Result<(), StoreError>,
    ) -> Result<usize, StoreError> {
        match self.stage {
            Stage::Header => {
                let Some(header) = bytes.get(..HEADER_LEN) else {
                    return Ok(0);
                };
                if &header[..8] != MAGIC {
                    return Err(malformed("magic is not ALSTATE1"));
                }
                let position = &header[9..];
                self.applied = match header[8] {
                    0 if position.iter().all(|b| *b == 0) => None,
                    0 => return Err(malformed("no applied position, yet bytes 9 to 40 are set")),
                    1 => decode_log_id(position),
                    _ => return Err(malformed("has_applied is neither 0 nor 1")),
                };
                self.hasher.update(header);
                self.stage = Stage::Records;
                Ok(HEADER_LEN)
            }
            Stage::Records => {
                let Some(len) = bytes.first_chunk::<4>() else {
                    return Ok(0);
                };
                let key_len = u32::from_be_bytes(*len) as usize;
                if key_len == 0 {
                    return self.trailer(bytes);
                }
                if key_len > MAX_KEY {
                    return Err(malformed("a key length is out of range"));
                }
                let Some(value_len) = bytes.get(4 + key_len..8 + key_len) else {
                    return Ok(0);
                };
                let value_len =
                    u32::from_be_bytes([value_len[0], value_len[1], value_len[2], value_len[3]])
                        as usize;
                if value_len > MAX_VALUE {
                    return Err(malformed("a value length is out of range"));
                }
                let end = 8 + key_len + value_len;
                let Some(record) = bytes.get(..end) else {
                    return Ok(0);
                };
                let key = &record[4..4 + key_len];
                if self.last_key.as_deref().is_some_and(|last| last >= key) {
                    return Err(malformed("keys are not in strictly ascending order"));
                }
                self.hasher.update(record);
                self.count += 1;
                self.last_key = Some(key.to_vec());
                pair(key.to_vec(), record[8 + key_len..].to_vec())?;
                Ok(end)
            }
            Stage::Done => {
                if bytes.is_empty() {
                    Ok(0)
                } else {
                    Err(malformed("bytes follow the digest"))
                }
            }
        }
    }

    fn trailer(&mut self, bytes: &[u8]) -> Result<usize, StoreError> {
        let Some(trailer) = bytes.get(..TRAILER_LEN) else {
            return Ok(0);
        };
        let (counted, digest) = trailer.split_at(12);
        self.hasher.update(counted);
        let count = u64::from_be_bytes([
            counted[4],
            counted[5],
            counted[6],
            counted[7],
            counted[8],
            counted[9],
            counted[10],
            counted[11],
        ]);
        if count != self.count {
            return Err(malformed("count differs from the records read"));
        }
        if digest != self.hasher.finalize() {
            return Err(malformed("digest does not match"));
        }
        self.stage = Stage::Done;
        Ok(TRAILER_LEN)
    }

    /// The export's applied position, once every byte has been pushed.
    ///
    /// # Errors
    ///
    /// [`StoreError::Malformed`] when the export broke a rule or ended early.
    pub(crate) fn finish(&self) -> Result<Option<LogId>, StoreError> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if self.stage != Stage::Done || !self.pending.is_empty() {
            return Err(malformed("the export ends early"));
        }
        Ok(self.applied)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::ops::Bound;

    use super::*;
    use crate::store::NodeId;

    /// A source over a map, for tests.
    pub(crate) struct MapSource(pub(crate) BTreeMap<Vec<u8>, Vec<u8>>);

    impl Source for MapSource {
        fn after(&mut self, after: Option<&[u8]>, max: usize) -> Result<Pairs, StoreError> {
            let start = after.map_or(Bound::Unbounded, |k| Bound::Excluded(k.to_vec()));
            Ok(self
                .0
                .range((start, Bound::Unbounded))
                .take(max)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        }
    }

    pub(crate) fn export_bytes(
        applied: Option<LogId>,
        map: &BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Vec<u8> {
        let export = Export::new(applied, MapSource(map.clone()));
        let mut out = Vec::new();
        while let Some(chunk) = export.next().unwrap() {
            assert!((1..=MAX_CHUNK).contains(&chunk.len()));
            out.extend(chunk);
        }
        out
    }

    fn read(bytes: &[u8], chunk: usize) -> Result<(Option<LogId>, Pairs), StoreError> {
        let mut reader = Reader::default();
        let mut pairs = Vec::new();
        for piece in bytes.chunks(chunk.max(1)) {
            reader.push(piece, &mut |k, v| {
                pairs.push((k, v));
                Ok(())
            })?;
        }
        reader.finish().map(|applied| (applied, pairs))
    }

    fn sample() -> BTreeMap<Vec<u8>, Vec<u8>> {
        (0u16..600)
            .map(|i| (i.to_be_bytes().to_vec(), vec![i as u8; usize::from(i % 7)]))
            .collect()
    }

    #[test]
    fn an_empty_store_exports_header_and_trailer() {
        let bytes = export_bytes(None, &BTreeMap::new());
        assert_eq!(bytes.len(), HEADER_LEN + TRAILER_LEN);
        assert_eq!(&bytes[..8], b"ALSTATE1");
        assert!(bytes[8..45].iter().all(|b| *b == 0));
        assert_eq!(bytes[45..53], 0u64.to_be_bytes());
        assert_eq!(bytes[53..], alloy_crypto::hash(&bytes[..53]));
    }

    #[test]
    fn exports_read_back_in_any_chunking() {
        let applied = Some(LogId {
            term: 3,
            leader: NodeId([9; 16]),
            index: 77,
        });
        let map = sample();
        let bytes = export_bytes(applied, &map);
        for chunk in [1, 5, 41, 4096, bytes.len()] {
            let (got, pairs) = read(&bytes, chunk).unwrap();
            assert_eq!(got, applied);
            assert_eq!(pairs.into_iter().collect::<BTreeMap<_, _>>(), map);
        }
    }

    #[test]
    fn every_broken_rule_is_malformed() {
        let bytes = export_bytes(None, &sample());
        let malformed = |bytes: &[u8]| matches!(read(bytes, 13), Err(StoreError::Malformed(_)));
        assert!(malformed(&bytes[..bytes.len() - 1]));
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(malformed(&extra));
        let mut magic = bytes.clone();
        magic[0] = b'X';
        assert!(malformed(&magic));
        let mut flag = bytes.clone();
        flag[8] = 2;
        assert!(malformed(&flag));
        let mut stray = bytes.clone();
        stray[20] = 1;
        assert!(malformed(&stray));
        let mut digest = bytes.clone();
        let last = digest.len() - 1;
        digest[last] ^= 1;
        assert!(malformed(&digest));
        let mut count = bytes.clone();
        count[last - 32] ^= 1;
        assert!(malformed(&count));
        let mut order = export_bytes(
            None,
            &BTreeMap::from([(vec![1], vec![]), (vec![2], vec![])]),
        );
        order[HEADER_LEN + 4] = 2;
        assert!(malformed(&order));
        assert!(malformed(&[]));
    }
}
