//! Helpers the checks share. Nothing here implements a seam: every check
//! reaches a store only through instances built from its [`Env`]'s registry.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use alloy_seam::{BuildError, ConfigValue, ImplName};

use crate::model::StateModel;
use crate::state::{
    ApplyBatch, LogId, MAX_CHUNK, MAX_SCAN, NodeId, Op, StateSnapshot, StateStore, StateStoreSeam,
    StoreError,
};
use crate::{Env, Failure, Implementation, Outcome};

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

    /// A key of 1 to 3 bytes from a small alphabet, so that keys collide,
    /// share prefixes, and include 0x00 and 0xFF.
    pub(crate) fn key(&mut self) -> Vec<u8> {
        const ALPHABET: [u8; 5] = [0x00, 0x01, b'a', 0x80, 0xFF];
        let len = 1 + self.below(3) as usize;
        (0..len)
            .map(|_| ALPHABET[self.below(ALPHABET.len() as u64) as usize])
            .collect()
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

/// The name of `implementation`.
fn impl_name(implementation: Implementation) -> Result<ImplName, Failure> {
    let text = match implementation {
        Implementation::Native => "native",
        Implementation::Mem => "mem",
    };
    ImplName::new(text).ctx("an implementation name")
}

/// The configuration of storage place `place` of `implementation`: `mem`
/// name `store-<place>`, or `native` directory `dirs[place]`.
pub(crate) fn place_config_of(
    env: &Env,
    implementation: Implementation,
    place: usize,
) -> Result<ConfigValue, Failure> {
    match implementation {
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

/// The configuration of storage place `place` of the implementation under
/// test.
pub(crate) fn place_config(env: &Env, place: usize) -> Result<ConfigValue, Failure> {
    place_config_of(env, env.implementation, place)
}

/// Builds a state store of `implementation` from `config`. The outer result
/// fails only for a broken suite; the inner one is the build's.
pub(crate) fn build_of(
    env: &Env,
    implementation: Implementation,
    config: &ConfigValue,
) -> Result<Result<Arc<dyn StateStore>, BuildError>, Failure> {
    let name = impl_name(implementation)?;
    Ok(env
        .registry
        .instantiate::<StateStoreSeam>(&env.runtime, &name, config))
}

/// Builds a state store of the implementation under test from `config`.
pub(crate) fn build(
    env: &Env,
    config: &ConfigValue,
) -> Result<Result<Arc<dyn StateStore>, BuildError>, Failure> {
    build_of(env, env.implementation, config)
}

/// A state store on storage place `place`.
pub(crate) fn open(env: &Env, place: usize) -> Result<Arc<dyn StateStore>, Failure> {
    build(env, &place_config(env, place)?)?.ctx("building a state store")
}

/// Drops `store`, its only reference, and builds a new instance on the same
/// place: a restart (section 5.2).
pub(crate) fn restart(
    env: &Env,
    place: usize,
    store: Arc<dyn StateStore>,
) -> Result<Arc<dyn StateStore>, Failure> {
    drop(store);
    open(env, place)
}

/// Node `n`.
pub(crate) fn node(n: u8) -> NodeId {
    let mut id = [0; 16];
    id[0] = 0xB2;
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

/// A `Put`.
pub(crate) fn put(key: &[u8], value: &[u8]) -> Op {
    Op::Put {
        key: key.to_vec(),
        value: value.to_vec(),
    }
}

/// A `Delete`.
pub(crate) fn delete(key: &[u8]) -> Op {
    Op::Delete { key: key.to_vec() }
}

/// A batch at index `index` of term 1.
pub(crate) fn batch(index: u64, ops: Vec<Op>) -> ApplyBatch {
    ApplyBatch {
        log_id: id(1, index),
        ops,
    }
}

/// Applies `batch` to both the store and the model and requires success.
pub(crate) async fn apply_ok(
    store: &dyn StateStore,
    model: &mut StateModel,
    batch: ApplyBatch,
) -> Outcome {
    model
        .apply(&batch)
        .map_err(|error| fail(format!("the model rejects a valid batch: {error:?}")))?;
    store.apply(batch).await.ctx("a valid apply")
}

/// Every pair `store` holds, read with `scan` in pages.
pub(crate) async fn contents(
    store: &dyn StateStore,
) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, Failure> {
    let mut all = BTreeMap::new();
    let mut start = Vec::new();
    loop {
        let page = store.scan(&start, None, MAX_SCAN).await.ctx("scan")?;
        let Some((last, _)) = page.last() else {
            return Ok(all);
        };
        let mut next = last.clone();
        next.push(0);
        for (key, value) in page {
            ensure!(
                all.insert(key, value).is_none(),
                "scan returned a key twice across pages"
            );
        }
        start = next;
    }
}

/// The whole export of `snapshot`, checking that every chunk is 1 to
/// `MAX_CHUNK` bytes (clause 5.4.5).
pub(crate) async fn export_of(snapshot: &dyn StateSnapshot) -> Result<Vec<u8>, Failure> {
    let mut all = Vec::new();
    while let Some(chunk) = snapshot.next_chunk().await.ctx("next_chunk")? {
        ensure!(
            (1..=MAX_CHUNK).contains(&chunk.len()),
            "a chunk of {} bytes is outside 1..={MAX_CHUNK}",
            chunk.len()
        );
        all.extend(chunk);
    }
    Ok(all)
}

/// The export of a snapshot of `store` taken now.
pub(crate) async fn export_now(store: &dyn StateStore) -> Result<Vec<u8>, Failure> {
    let snapshot = store.snapshot().await.ctx("snapshot")?;
    export_of(&*snapshot).await
}

/// Installs `export` into `store` in chunks of `chunk` bytes. An error from
/// `write` or `finish` is returned as is; after one from `write` the install
/// is dropped.
pub(crate) async fn install(
    store: &dyn StateStore,
    export: &[u8],
    chunk: usize,
) -> Result<Result<Option<LogId>, StoreError>, Failure> {
    let install = store.begin_install().await.ctx("begin_install")?;
    for piece in export.chunks(chunk.max(1)) {
        if let Err(error) = install.write(piece.to_vec()).await {
            return Ok(Err(error));
        }
    }
    Ok(install.finish().await)
}

/// Fails unless `store` holds exactly what `model` holds and exports exactly
/// its export.
pub(crate) async fn expect_model(
    store: &dyn StateStore,
    model: &StateModel,
    what: &str,
) -> Outcome {
    ensure_eq!(
        store.applied().await.ctx("applied")?,
        model.applied,
        "{what}: the applied position"
    );
    let held = contents(store).await?;
    ensure!(
        held == model.map,
        "{what}: the store holds {} pairs that differ from the model's {}",
        held.len(),
        model.map.len()
    );
    let export = export_now(store).await?;
    ensure!(
        export == model.export(),
        "{what}: the export differs from the encoding of section 4.1"
    );
    Ok(())
}
