//! The event trace (ARFC 0005 section 4.1): the encoder, the trace hash, the
//! file it streams to under its cap, the last 200 records, and the observers
//! that see each record as it is written (clause 3.6.1).

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::Duration;

use alloy_seam::Detail;

use crate::exec::{drop_quietly, panic_text};
use crate::hook;
use crate::run::RunResult;
use crate::world::{HostId, WorldError};

/// Kind `host-start`.
pub(crate) const KIND_HOST_START: u16 = 0x0001;
/// Kind `host-crash`.
pub(crate) const KIND_HOST_CRASH: u16 = 0x0002;
/// Kind `host-stall`: the stall length.
pub(crate) const KIND_HOST_STALL: u16 = 0x0003;
/// Kind `task-spawn`: the task and its name.
pub(crate) const KIND_TASK_SPAWN: u16 = 0x0010;
/// Kind `task-poll`: the task.
pub(crate) const KIND_TASK_POLL: u16 = 0x0011;
/// Kind `task-end`: the task and how it ended.
pub(crate) const KIND_TASK_END: u16 = 0x0012;
/// Kind `timer-fire`: the task woken and the deadline.
pub(crate) const KIND_TIMER_FIRE: u16 = 0x0020;
/// Kind `clock-fault`: the rate offset after the fault and the wall step.
pub(crate) const KIND_CLOCK_FAULT: u16 = 0x0021;
/// Kind `rng`: the bytes one `fill` filled.
pub(crate) const KIND_RNG: u16 = 0x0060;
/// Kind `fault`: a text describing the fault.
pub(crate) const KIND_FAULT: u16 = 0x0070;
/// Kind `note`: text from the scenario.
pub(crate) const KIND_NOTE: u16 = 0x00F0;
/// Kind `run-end`: the result of the run.
pub(crate) const KIND_RUN_END: u16 = 0x00FF;

/// The `host` field of a record that belongs to no host.
const WORLD_HOST: u32 = 0xFFFF_FFFF;
/// The bytes of a record before its body.
const RECORD_HEAD: usize = 16;
/// The longest `text` field, in bytes (section 7).
const MAX_TEXT: usize = 1_024;
/// The records kept for failure output (section 6).
const KEPT_RECORDS: usize = 200;
/// Observers per world (section 7).
const MAX_OBSERVERS: usize = 64;

/// The name of a record kind in section 4.1, or `unknown`.
pub(crate) fn kind_name(kind: u16) -> &'static str {
    match kind {
        KIND_HOST_START => "host-start",
        KIND_HOST_CRASH => "host-crash",
        KIND_HOST_STALL => "host-stall",
        KIND_TASK_SPAWN => "task-spawn",
        KIND_TASK_POLL => "task-poll",
        KIND_TASK_END => "task-end",
        KIND_TIMER_FIRE => "timer-fire",
        KIND_CLOCK_FAULT => "clock-fault",
        0x0030 => "udp-send",
        0x0031 => "udp-deliver",
        0x0032 => "packet-drop",
        0x0033 => "packet-fault",
        0x0040 => "stream-open",
        0x0041 => "stream-data",
        0x0042 => "stream-close",
        0x0050 => "disk-op",
        KIND_RNG => "rng",
        KIND_FAULT => "fault",
        KIND_NOTE => "note",
        KIND_RUN_END => "run-end",
        _ => "unknown",
    }
}

/// A `text` field: a u16 length, then the UTF-8 bytes of `text` cut at the
/// last character boundary at or below 1,024 bytes.
pub(crate) fn text_field(text: &str) -> Vec<u8> {
    let mut end = text.len().min(MAX_TEXT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut field = Vec::with_capacity(2 + end);
    // `end` is at most 1,024, so it fits a u16.
    field.extend_from_slice(&(end as u16).to_le_bytes());
    field.extend_from_slice(&text.as_bytes()[..end]);
    field
}

/// Sees every trace record as it is written (ARFC 0005 clause 3.6.1).
///
/// An observer is called synchronously, on the run's thread, before the
/// simulator does anything else. A world call made from inside
/// [`Observer::record`], on any world, changes nothing: every
/// [`World`](crate::World) method that returns a `Result` returns
/// [`WorldError::Busy`] (clause 3.2.9). An
/// observer that panics is removed and ends the run with `fail`, unless the
/// record is `run-end`.
pub trait Observer: Send {
    /// Called once for each record, in trace order.
    fn record(&mut self, record: &Record<'_>);
}

/// One trace record as an observer sees it.
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    /// The world time of the record.
    pub time: Duration,
    /// The host the record belongs to. `None` for `FFFFFFFF`, the world.
    pub host: Option<HostId>,
    /// The record kind of section 4.1.
    pub kind: u16,
    /// The body of section 4.1.
    pub body: &'a [u8],
    /// The bytes the record's `digest` covers. Empty when it has none.
    pub data: &'a [u8],
}

/// The causes that decide a run's result (clause 3.2.8).
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    failed: bool,
    liveness: bool,
    first: Option<Detail>,
}

impl Outcome {
    /// Records a cause of `fail`.
    pub(crate) fn fail(&mut self, cause: Detail) {
        self.failed = true;
        self.first.get_or_insert(cause);
    }

    /// Records a cause of `liveness`.
    pub(crate) fn liveness(&mut self, cause: Detail) {
        self.liveness = true;
        self.first.get_or_insert(cause);
    }

    fn result(&self) -> RunResult {
        if self.failed {
            RunResult::Fail
        } else if self.liveness {
            RunResult::Liveness
        } else {
            RunResult::Pass
        }
    }
}

/// A record kept for failure output: everything but its body bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Kept {
    pub(crate) time: u64,
    pub(crate) host: Option<HostId>,
    pub(crate) kind: u16,
    pub(crate) len: u16,
}

/// The trace file and how much of the cap it has used.
#[derive(Debug)]
struct TraceFile {
    writer: BufWriter<File>,
    written: u64,
    cap: u64,
    /// Set once a record did not fit: nothing more is written.
    full: bool,
}

/// What a closed trace leaves for the summary.
pub(crate) struct Closed {
    pub(crate) hash: [u8; 32],
    pub(crate) events: u64,
    pub(crate) result: RunResult,
    pub(crate) failure: Option<Detail>,
    pub(crate) kept: VecDeque<Kept>,
    /// Objects of the world to drop once no lock is held.
    pub(crate) observers: Vec<Box<dyn Observer>>,
}

/// The trace of one run.
pub(crate) struct Trace {
    hasher: alloy_crypto::Hasher,
    header: [u8; 16],
    events: u64,
    kept: VecDeque<Kept>,
    file: Option<TraceFile>,
    observers: Vec<Box<dyn Observer>>,
    pub(crate) outcome: Outcome,
    /// False once the run has ended or is ending: records other than
    /// `run-end` are no longer written.
    open: bool,
}

impl Trace {
    /// A trace holding only the header of `seed`.
    pub(crate) fn new(seed: u64) -> Trace {
        let mut header = [0u8; 16];
        header[0..4].copy_from_slice(b"ALTR");
        header[4..6].copy_from_slice(&1u16.to_le_bytes());
        header[8..16].copy_from_slice(&seed.to_le_bytes());
        let mut hasher = alloy_crypto::Hasher::new();
        hasher.update(&header);
        Trace {
            hasher,
            header,
            events: 0,
            kept: VecDeque::with_capacity(KEPT_RECORDS),
            file: None,
            observers: Vec::new(),
            outcome: Outcome::default(),
            open: true,
        }
    }

    /// Creates the trace file at `path`, replacing any file there, and writes
    /// the header to it. A file that cannot be created or written makes the
    /// run `fail` and is not used; the run goes on.
    pub(crate) fn open_file(&mut self, path: &Path, cap: u64) {
        let attempt = File::create(path).and_then(|file| {
            let mut writer = BufWriter::new(file);
            writer.write_all(&self.header)?;
            Ok(writer)
        });
        match attempt {
            Ok(writer) => {
                self.file = Some(TraceFile {
                    writer,
                    written: self.header.len() as u64,
                    cap,
                    full: false,
                });
            }
            Err(error) => self.file_failed(&error),
        }
    }

    fn file_failed(&mut self, error: &std::io::Error) {
        self.file = None;
        self.outcome.fail(Detail::new(&format!(
            "the trace file could not be written: {error}"
        )));
    }

    /// Adds an observer.
    ///
    /// # Errors
    ///
    /// [`WorldError::Limit`] when the world holds 64 observers.
    pub(crate) fn add_observer(&mut self, observer: Box<dyn Observer>) -> Result<(), WorldError> {
        if self.observers.len() >= MAX_OBSERVERS {
            return Err(WorldError::Limit(Detail::new("64 observers")));
        }
        self.observers.push(observer);
        Ok(())
    }

    /// Writes one record while the trace is open. Returns true when an
    /// observer panicked, which ends the run: the trace is then closed to
    /// every record but `run-end`.
    pub(crate) fn write(
        &mut self,
        time: u64,
        host: Option<HostId>,
        kind: u16,
        body: &[u8],
        data: &[u8],
    ) -> bool {
        if !self.open {
            return false;
        }
        match self.emit(time, host, kind, body, data) {
            Some(text) => {
                self.outcome
                    .fail(Detail::new(&format!("an observer panicked: {text}")));
                self.open = false;
                true
            }
            None => false,
        }
    }

    /// Encodes, hashes, files, and keeps one record, then shows it to every
    /// observer. Returns the message of the first observer panic.
    fn emit(
        &mut self,
        time: u64,
        host: Option<HostId>,
        kind: u16,
        body: &[u8],
        data: &[u8],
    ) -> Option<String> {
        // Every body of section 4.1 is built within 65,535 bytes: the longest
        // is a `text` field, cut to 1,024 bytes.
        let len = u16::try_from(body.len()).expect("record bodies are at most 65,535 bytes");
        let mut head = [0u8; RECORD_HEAD];
        head[0..8].copy_from_slice(&time.to_le_bytes());
        head[8..12].copy_from_slice(&host.map_or(WORLD_HOST, |h| h.0).to_le_bytes());
        head[12..14].copy_from_slice(&kind.to_le_bytes());
        head[14..16].copy_from_slice(&len.to_le_bytes());
        self.hasher.update(&head).update(body);
        self.events += 1;
        self.file_record(&head, body);
        if self.kept.len() == KEPT_RECORDS {
            self.kept.pop_front();
        }
        self.kept.push_back(Kept {
            time,
            host,
            kind,
            len,
        });
        if self.observers.is_empty() {
            return None;
        }
        let record = Record {
            time: Duration::from_nanos(time),
            host,
            kind,
            body,
            data,
        };
        let mut first = None;
        let inside = hook::enter();
        let mut index = 0;
        while index < self.observers.len() {
            let observer = &mut self.observers[index];
            match catch_unwind(AssertUnwindSafe(|| observer.record(&record))) {
                Ok(()) => index += 1,
                Err(payload) => {
                    first.get_or_insert_with(|| panic_text(payload.as_ref()));
                    drop_quietly(payload);
                    drop_quietly(self.observers.remove(index));
                }
            }
        }
        drop(inside);
        first
    }

    /// Streams one record to the file until the next record would take the
    /// file past its cap.
    fn file_record(&mut self, head: &[u8], body: &[u8]) {
        let Some(file) = self.file.as_mut().filter(|file| !file.full) else {
            return;
        };
        let size = (head.len() + body.len()) as u64;
        if file.written.saturating_add(size) > file.cap {
            file.full = true;
            return;
        }
        let written = file
            .writer
            .write_all(head)
            .and_then(|()| file.writer.write_all(body));
        match written {
            Ok(()) => file.written += size,
            Err(error) => self.file_failed(&error),
        }
    }

    /// Writes `run-end` at `time` and closes the trace. The result is fixed
    /// before the record is written, so nothing after it changes the result.
    pub(crate) fn finish(&mut self, time: u64) -> Closed {
        // Buffered records reach the file before the result is fixed, so a
        // write error among them makes this run `fail`.
        if let Some(file) = self.file.as_mut()
            && let Err(error) = file.writer.flush()
        {
            self.file_failed(&error);
        }
        let result = self.outcome.result();
        let failure = match result {
            RunResult::Pass => None,
            RunResult::Fail | RunResult::Liveness => self.outcome.first.clone(),
        };
        // An observer that panics on `run-end` is removed like any other, but
        // the result is already fixed.
        let _ = self.emit(time, None, KIND_RUN_END, &[result.code()], &[]);
        self.open = false;
        if let Some(mut file) = self.file.take() {
            // The result is fixed, so an error here can no longer change it.
            let _ = file.writer.flush();
        }
        Closed {
            hash: self.hasher.finalize(),
            events: self.events,
            result,
            failure,
            kept: std::mem::take(&mut self.kept),
            observers: std::mem::take(&mut self.observers),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::exec::lock;

    /// Collects every record it sees as `(time, host, kind, body)`.
    type Seen = Arc<Mutex<Vec<(u64, Option<HostId>, u16, Vec<u8>)>>>;

    struct Collect(Seen);

    impl Observer for Collect {
        fn record(&mut self, record: &Record<'_>) {
            lock(&self.0).push((
                record.time.as_nanos() as u64,
                record.host,
                record.kind,
                record.body.to_vec(),
            ));
        }
    }

    struct Panics;

    impl Observer for Panics {
        fn record(&mut self, _: &Record<'_>) {
            panic!("observer failure");
        }
    }

    #[test]
    fn the_hash_covers_the_header_and_every_record() {
        let mut trace = Trace::new(0x0102_0304_0506_0708);
        trace.write(5, None, KIND_NOTE, &text_field("hi"), &[]);
        trace.write(9, Some(HostId(3)), 0x0011, &7u64.to_le_bytes(), &[]);
        let closed = trace.finish(9);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ALTR\x01\x00\x00\x00");
        bytes.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        bytes.extend_from_slice(&5u64.to_le_bytes());
        bytes.extend_from_slice(&[0xFF; 4]);
        bytes.extend_from_slice(&[0xF0, 0x00, 4, 0, 2, 0, b'h', b'i']);
        bytes.extend_from_slice(&9u64.to_le_bytes());
        bytes.extend_from_slice(&[3, 0, 0, 0, 0x11, 0x00, 8, 0]);
        bytes.extend_from_slice(&7u64.to_le_bytes());
        bytes.extend_from_slice(&9u64.to_le_bytes());
        bytes.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 1, 0, 0]);
        assert_eq!(closed.hash, alloy_crypto::hash(&bytes));
        assert_eq!(closed.events, 3);
        assert_eq!(closed.result, RunResult::Pass);
        assert_eq!(closed.failure, None);
    }

    #[test]
    fn text_is_cut_at_a_character_boundary() {
        assert_eq!(text_field(""), [0, 0]);
        let long = format!("{}é", "a".repeat(1_023));
        let field = text_field(&long);
        assert_eq!(field[..2], 1_023u16.to_le_bytes());
        assert_eq!(field.len(), 2 + 1_023);
        assert_eq!(text_field(&"b".repeat(5_000)).len(), 2 + 1_024);
    }

    #[test]
    fn the_last_200_records_are_kept() {
        let mut trace = Trace::new(1);
        for t in 0..250 {
            trace.write(t, None, KIND_NOTE, &text_field("x"), &[]);
        }
        let closed = trace.finish(250);
        assert_eq!(closed.events, 251);
        assert_eq!(closed.kept.len(), 200);
        assert_eq!(closed.kept.front().map(|k| k.time), Some(51));
        assert_eq!(
            closed.kept.back(),
            Some(&Kept {
                time: 250,
                host: None,
                kind: KIND_RUN_END,
                len: 1
            })
        );
    }

    #[test]
    fn observers_see_records_and_a_panic_removes_only_the_panicking_one() {
        let seen: Seen = Arc::default();
        let mut trace = Trace::new(1);
        trace
            .add_observer(Box::new(Collect(Arc::clone(&seen))))
            .expect("room for an observer");
        trace.add_observer(Box::new(Panics)).expect("room");
        let after: Seen = Arc::default();
        trace
            .add_observer(Box::new(Collect(Arc::clone(&after))))
            .expect("room");
        assert!(trace.write(1, None, KIND_NOTE, &text_field("a"), &[]));
        assert!(!hook::inside());
        // Closed to everything but `run-end`.
        assert!(!trace.write(2, None, KIND_NOTE, &text_field("b"), &[]));
        let closed = trace.finish(2);
        assert_eq!(closed.events, 2);
        assert_eq!(closed.result, RunResult::Fail);
        assert_eq!(
            closed.failure.as_ref().map(Detail::as_str),
            Some("an observer panicked: observer failure")
        );
        assert_eq!(closed.observers.len(), 2);
        let kinds = |s: &Seen| lock(s).iter().map(|r| r.2).collect::<Vec<_>>();
        assert_eq!(kinds(&seen), [KIND_NOTE, KIND_RUN_END]);
        assert_eq!(kinds(&after), [KIND_NOTE, KIND_RUN_END]);
        assert_eq!(lock(&seen)[1].3, [1]);
    }

    #[test]
    fn at_most_64_observers() {
        let mut trace = Trace::new(1);
        for _ in 0..64 {
            trace.add_observer(Box::new(Panics)).expect("room");
        }
        assert_eq!(
            trace.add_observer(Box::new(Panics)),
            Err(WorldError::Limit(Detail::new("64 observers")))
        );
    }

    #[test]
    fn the_first_cause_wins_and_fail_outranks_liveness() {
        let mut outcome = Outcome::default();
        assert_eq!(outcome.result(), RunResult::Pass);
        outcome.liveness(Detail::new("budget"));
        assert_eq!(outcome.result(), RunResult::Liveness);
        outcome.fail(Detail::new("file"));
        assert_eq!(outcome.result(), RunResult::Fail);
        assert_eq!(outcome.first, Some(Detail::new("budget")));
    }

    #[test]
    fn every_kind_has_its_name() {
        assert_eq!(kind_name(0x0001), "host-start");
        assert_eq!(kind_name(0x0033), "packet-fault");
        assert_eq!(kind_name(KIND_NOTE), "note");
        assert_eq!(kind_name(KIND_RUN_END), "run-end");
        assert_eq!(kind_name(0x1234), "unknown");
    }
}
