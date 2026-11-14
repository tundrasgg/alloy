//! Helpers the checks share. Nothing here implements a seam: every check
//! reaches a store only through instances built from its [`Env`]'s registry.

use std::fmt;
use std::sync::Arc;

use alloy_seam::{BuildError, ConfigValue, ImplName};

use crate::state::{Entry, LogId, LogState, LogStore, LogStoreSeam, NodeId, Vote};
use crate::{Env, Failure, Implementation};

/// Turns an unexpected error or a missing value into a failure.
pub(crate) trait Ctx<T> {
    fn ctx(self, what: &str) -> Result<T, Failure>;
}

impl<T, E: fmt::Debug> Ctx<T> for Result<T, E> {
    fn ctx(self, what: &str) -> Result<T, Failure> {
        self.map_err(|error| Failure(format!("{what}: unexpected error {error:?}")))
    }
}

impl<T> Ctx<T> for Option<T> {
    fn ctx(self, what: &str) -> Result<T, Failure> {
        self.ok_or_else(|| Failure(format!("{what}: missing")))
    }
}

/// A failure with `text`.
pub(crate) fn fail(text: impl Into<String>) -> Failure {
    Failure(text.into())
}

/// A deterministic generator for test data (SplitMix64). It is test data
/// only, never a stand-in for the `rng` seam.
#[derive(Clone, Debug)]
pub(crate) struct Gen(u64);

impl Gen {
    pub(crate) fn new(seed: u64) -> Gen {
        Gen(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`. `bound` is not 0.
    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u64().to_le_bytes()[0]).collect()
    }
}

/// A protobuf `string` or `bytes` field (wire type 2).
pub(crate) fn proto_bytes(field: u32, value: &[u8]) -> Vec<u8> {
    let mut out = proto_varint_raw(u64::from(field << 3 | 2));
    out.extend(proto_varint_raw(value.len() as u64));
    out.extend_from_slice(value);
    out
}

/// A protobuf varint field (wire type 0).
pub(crate) fn proto_varint(field: u32, value: u64) -> Vec<u8> {
    let mut out = proto_varint_raw(u64::from(field << 3));
    out.extend(proto_varint_raw(value));
    out
}

fn proto_varint_raw(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let low = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(low);
            return out;
        }
        out.push(low | 0x80);
    }
}

/// `MemStoreConfig { name, fail_after }` in version 1 (section 5.2).
pub(crate) fn mem_config(name: &str, fail_after: u64) -> Result<ConfigValue, Failure> {
    let mut payload = proto_bytes(1, name.as_bytes());
    if fail_after != 0 {
        payload.extend(proto_varint(2, fail_after));
    }
    ConfigValue::new(1, payload).ctx("encoding a MemStoreConfig")
}

/// `NativeStoreConfig { dir }` in version 1 (section 5.2).
pub(crate) fn native_config(dir: &str) -> Result<ConfigValue, Failure> {
    ConfigValue::new(1, proto_bytes(1, dir.as_bytes())).ctx("encoding a NativeStoreConfig")
}

/// The implementation name under test.
pub(crate) fn impl_name(implementation: Implementation) -> Result<ImplName, Failure> {
    let text = match implementation {
        Implementation::Native => "native",
        Implementation::Mem => "mem",
    };
    ImplName::new(text).ctx("an implementation name")
}

/// The configuration of storage place `place`: `mem` name `store-<place>`,
/// or `native` directory `dirs[place]`.
pub(crate) fn place_config(env: &Env, place: usize) -> Result<ConfigValue, Failure> {
    match env.implementation {
        Implementation::Mem => mem_config(&format!("store-{place}"), 0),
        Implementation::Native => {
            let dir = env
                .dirs
                .get(place)
                .ctx("the harness gives a native check two directories")?;
            native_config(dir)
        }
    }
}

/// Builds a log store from `config` with the implementation under test. The
/// outer result fails only for a broken suite; the inner one is the build's.
pub(crate) fn build(
    env: &Env,
    config: &ConfigValue,
) -> Result<Result<Arc<dyn LogStore>, BuildError>, Failure> {
    let name = impl_name(env.implementation)?;
    Ok(env
        .registry
        .instantiate::<LogStoreSeam>(&env.runtime, &name, config))
}

/// A log store on storage place `place`.
pub(crate) fn open(env: &Env, place: usize) -> Result<Arc<dyn LogStore>, Failure> {
    build(env, &place_config(env, place)?)?.ctx("building a log store")
}

/// Drops `store`, its only reference, and builds a new instance on the same
/// place: a restart (section 5.2).
pub(crate) fn restart(
    env: &Env,
    place: usize,
    store: Arc<dyn LogStore>,
) -> Result<Arc<dyn LogStore>, Failure> {
    drop(store);
    open(env, place)
}

/// Node `n`.
pub(crate) fn node(n: u8) -> NodeId {
    let mut id = [0; 16];
    id[0] = 0xA1;
    id[15] = n;
    NodeId(id)
}

/// The position `index` written in `term` by node `term % 3`.
pub(crate) fn id(term: u64, index: u64) -> LogId {
    LogId {
        term,
        leader: node((term % 3) as u8),
        index,
    }
}

/// An entry at `index` in `term` with a payload derived from both.
pub(crate) fn entry(term: u64, index: u64) -> Entry {
    let mut data = Gen::new(term.wrapping_mul(31).wrapping_add(index));
    let len = data.below(48) as usize;
    Entry {
        log_id: id(term, index),
        payload: data.bytes(len),
    }
}

/// Entries `first..=last` in `term`.
pub(crate) fn run(term: u64, first: u64, last: u64) -> Vec<Entry> {
    (first..=last).map(|index| entry(term, index)).collect()
}

/// A vote.
pub(crate) fn vote(term: u64, n: u8, committed: bool) -> Vote {
    Vote {
        term,
        node: node(n),
        committed,
    }
}

/// Everything a log store shows: its vote, state, and entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) vote: Option<Vote>,
    pub(crate) state: LogState,
    pub(crate) entries: Vec<Entry>,
}

/// Reads everything `store` shows. Entries are read in as many calls as the
/// read limit needs.
pub(crate) async fn seen(store: &dyn LogStore) -> Result<Seen, Failure> {
    let vote = store.read_vote().await.ctx("read_vote")?;
    let state = store.state().await.ctx("state")?;
    let entries = read_all(store, 0, u64::MAX).await?;
    Ok(Seen {
        vote,
        state,
        entries,
    })
}

/// Reads entries `first..=last` in as many calls as the read limit needs.
pub(crate) async fn read_all(
    store: &dyn LogStore,
    mut first: u64,
    last: u64,
) -> Result<Vec<Entry>, Failure> {
    let mut all = Vec::new();
    loop {
        let got = store.read(first, last).await.ctx("read")?;
        let Some(tail) = got.last() else {
            return Ok(all);
        };
        let next = tail.log_id.index;
        all.extend(got);
        if next >= last {
            return Ok(all);
        }
        first = next + 1;
    }
}
