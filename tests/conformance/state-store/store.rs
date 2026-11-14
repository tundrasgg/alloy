//! Clauses 5.4.1 to 5.4.4 and 5.4.7: applying batches and reading a state
//! store, and a random history against the model.

use std::fmt;

use crate::model::{Kind, StateModel, kind, outcome};
use crate::state::{
    ApplyBatch, MAX_BATCH_BYTES, MAX_KEY, MAX_READ_BYTES, MAX_SCAN, MAX_VALUE, Op, StoreError,
};
use crate::support::{
    Ctx, Gen, apply_ok, batch, build, contents, delete, expect_model, export_now, fail, id,
    install, mem_config, open, put, restart,
};
use crate::{Applies, Check, Env, Implementation, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0006#5.4.1",
            Applies::All,
            arfc0006_s5_4_1_fresh_store_holds_nothing
        ),
        check!(
            "arfc-0006#5.4.2",
            Applies::All,
            arfc0006_s5_4_2_every_apply_survives_a_restart
        ),
        check!(
            "arfc-0006#5.4.2",
            Applies::All,
            arfc0006_s5_4_2_a_finished_install_survives_a_restart
        ),
        check!(
            "arfc-0006#5.4.3",
            Applies::All,
            arfc0006_s5_4_3_apply_performs_ops_in_order
        ),
        check!(
            "arfc-0006#5.4.3",
            Applies::All,
            arfc0006_s5_4_3_empty_ops_only_set_the_position
        ),
        check!(
            "arfc-0006#5.4.3",
            Applies::All,
            arfc0006_s5_4_3_stale_index_fails_stale
        ),
        check!(
            "arfc-0006#5.4.3",
            Applies::All,
            arfc0006_s5_4_3_empty_key_fails_invalid_input
        ),
        check!(
            "arfc-0006#5.4.3",
            Applies::All,
            arfc0006_s5_4_3_key_over_limit_fails_too_large
        ),
        check!(
            "arfc-0006#5.4.3",
            Applies::All,
            arfc0006_s5_4_3_value_over_limit_fails_too_large
        ),
        check!(
            "arfc-0006#5.4.3",
            Applies::All,
            arfc0006_s5_4_3_batch_over_limit_fails_too_large
        ),
        check!(
            "arfc-0006#5.4.3",
            Applies::All,
            arfc0006_s5_4_3_apply_during_install_fails_busy
        ),
        check!(
            "arfc-0006#5.4.4",
            Applies::All,
            arfc0006_s5_4_4_reads_reflect_every_returned_apply
        ),
        check!(
            "arfc-0006#5.4.4",
            Applies::All,
            arfc0006_s5_4_4_scan_returns_the_range_ascending_bytewise
        ),
        check!(
            "arfc-0006#5.4.4",
            Applies::All,
            arfc0006_s5_4_4_scan_returns_at_most_limit_pairs
        ),
        check!(
            "arfc-0006#5.4.4",
            Applies::All,
            arfc0006_s5_4_4_scan_limit_out_of_range_fails_invalid_input
        ),
        check!(
            "arfc-0006#5.4.4",
            Applies::All,
            arfc0006_s5_4_4_scan_stops_after_the_pair_reaching_the_limit
        ),
        check!(
            "arfc-0006#5.4.7",
            Applies::Only(Implementation::Mem),
            arfc0006_s5_4_7_after_io_every_call_fails_io
        ),
        check!(
            "arfc-0006#5.4",
            Applies::All,
            arfc0006_s5_4_random_history_matches_the_model
        ),
    ]
}

/// Fails unless `result` is an error of kind `want`.
pub(crate) fn expect_err<T: fmt::Debug>(
    result: Result<T, StoreError>,
    want: Kind,
    what: &str,
) -> Outcome {
    match result {
        Err(error) if kind(&error) == want => Ok(()),
        other => Err(fail(format!("{what}: got {other:?}, expected {want:?}"))),
    }
}

/// Fails unless `result` is `Io`.
pub(crate) fn expect_io<T: fmt::Debug>(result: Result<T, StoreError>, what: &str) -> Outcome {
    match result {
        Err(StoreError::Io(_)) => Ok(()),
        other => Err(fail(format!("{what}: got {other:?}, expected Io"))),
    }
}

/// A store never written holds no key and no applied position.
async fn arfc0006_s5_4_1_fresh_store_holds_nothing(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    ensure_eq!(
        store.applied().await.ctx("applied")?,
        None,
        "the applied position"
    );
    let keys: [&[u8]; 4] = [b"a", &[0x00], &[0xFF; 8], &[0x41; MAX_KEY]];
    for key in keys {
        ensure_eq!(
            store.get(key).await.ctx("get")?,
            None,
            "get of a fresh store"
        );
    }
    let all = store.scan(b"", None, MAX_SCAN).await.ctx("scan")?;
    ensure!(
        all.is_empty(),
        "scan of a fresh store returned {} pairs",
        all.len()
    );
    expect_model(&*store, &StateModel::default(), "a fresh store").await
}

/// Each apply that returned `Ok` is held by a new instance on the same place.
async fn arfc0006_s5_4_2_every_apply_survives_a_restart(env: Env) -> Outcome {
    let mut store = open(&env, 0)?;
    let mut model = StateModel::default();
    let batches = [
        batch(1, vec![put(b"a", b"1"), put(b"b", b"2")]),
        batch(2, vec![delete(b"a"), put(b"c", b"")]),
        batch(5, Vec::new()),
        batch(9, vec![put(b"b", &[7; 1000]), delete(b"zz")]),
    ];
    for (step, next) in batches.into_iter().enumerate() {
        apply_ok(&*store, &mut model, next).await?;
        store = restart(&env, 0, store)?;
        expect_model(
            &*store,
            &model,
            &format!("after apply {step} and a restart"),
        )
        .await?;
    }
    Ok(())
}

/// A finished install is held by a new instance on the same place.
async fn arfc0006_s5_4_2_a_finished_install_survives_a_restart(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut old = StateModel::default();
    apply_ok(&*store, &mut old, batch(4, vec![put(b"old", b"x")])).await?;
    let new = StateModel::with(Some(id(3, 40)), &[(b"k1", b"v1"), (b"k2", b"")]);
    install(&*store, &new.export(), 16)
        .await?
        .ctx("installing an export")?;
    let store = restart(&env, 0, store)?;
    expect_model(&*store, &new, "after an install and a restart").await
}

/// The ops run in order, `Put` setting and `Delete` removing, and the
/// position becomes the batch's.
async fn arfc0006_s5_4_3_apply_performs_ops_in_order(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    let ops = vec![
        put(b"a", b"1"),
        put(b"b", b"2"),
        delete(b"a"),
        put(b"a", b"3"),
        delete(b"missing"),
        put(b"c", b""),
        put(b"b", b"4"),
        put(b"d", b"5"),
        delete(b"d"),
    ];
    apply_ok(&*store, &mut model, batch(3, ops)).await?;
    let expected: [(&[u8], Option<&[u8]>); 5] = [
        (b"a", Some(b"3")),
        (b"b", Some(b"4")),
        (b"c", Some(b"")),
        (b"d", None),
        (b"missing", None),
    ];
    for (key, want) in expected {
        ensure_eq!(
            store.get(key).await.ctx("get")?,
            want.map(<[u8]>::to_vec),
            "get({key:?})"
        );
    }
    ensure_eq!(
        store.applied().await.ctx("applied")?,
        Some(id(1, 3)),
        "the applied position"
    );
    expect_model(&*store, &model, "after the batch").await
}

/// An empty `ops` only sets the position.
async fn arfc0006_s5_4_3_empty_ops_only_set_the_position(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    apply_ok(&*store, &mut model, batch(1, Vec::new())).await?;
    expect_model(&*store, &model, "after an empty batch on a fresh store").await?;
    apply_ok(&*store, &mut model, batch(2, vec![put(b"k", b"v")])).await?;
    let at = id(4, 7);
    apply_ok(
        &*store,
        &mut model,
        ApplyBatch {
            log_id: at,
            ops: Vec::new(),
        },
    )
    .await?;
    ensure_eq!(
        store.applied().await.ctx("applied")?,
        Some(at),
        "the applied position"
    );
    expect_model(&*store, &model, "after an empty batch").await
}

/// An index at or below the applied index fails with `Stale` naming both,
/// whatever the term, and changes nothing.
async fn arfc0006_s5_4_3_stale_index_fails_stale(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    apply_ok(&*store, &mut model, batch(10, vec![put(b"k", b"v")])).await?;
    for (term, index) in [(1, 10), (9, 10), (1, 9), (5, 1), (1, 0)] {
        let stale = ApplyBatch {
            log_id: id(term, index),
            ops: vec![put(b"k", b"stale"), put(b"other", b"x")],
        };
        match store.apply(stale).await {
            Err(StoreError::Stale {
                index: got,
                applied: 10,
            }) if got == index => {}
            other => {
                return Err(fail(format!(
                    "apply at index {index} after 10: got {other:?}, expected Stale"
                )));
            }
        }
        expect_model(&*store, &model, &format!("after a stale apply at {index}")).await?;
    }
    Ok(())
}

/// An empty key, in a `Put` or a `Delete`, fails with `InvalidInput` and
/// changes nothing, not even the ops before it.
async fn arfc0006_s5_4_3_empty_key_fails_invalid_input(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    apply_ok(&*store, &mut model, batch(1, vec![put(b"k", b"v")])).await?;
    for (what, op) in [("a Put", put(b"", b"v")), ("a Delete", delete(b""))] {
        let bad = batch(2, vec![put(b"before", b"x"), delete(b"k"), op]);
        expect_err(
            store.apply(bad).await,
            Kind::InvalidInput,
            &format!("an empty key in {what}"),
        )?;
        expect_model(&*store, &model, &format!("after an empty key in {what}")).await?;
    }
    Ok(())
}

/// A key of `MAX_KEY` bytes applies; one byte more fails with `TooLarge`, in
/// a `Put` or a `Delete`, and changes nothing.
async fn arfc0006_s5_4_3_key_over_limit_fails_too_large(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    let longest = vec![0x6B; MAX_KEY];
    apply_ok(&*store, &mut model, batch(1, vec![put(&longest, b"v")])).await?;
    let over = vec![0x6B; MAX_KEY + 1];
    for (what, op) in [("a Put", put(&over, b"v")), ("a Delete", delete(&over))] {
        let bad = batch(2, vec![put(b"before", b"x"), op]);
        expect_err(
            store.apply(bad).await,
            Kind::TooLarge,
            &format!("a long key in {what}"),
        )?;
        expect_model(&*store, &model, &format!("after a long key in {what}")).await?;
    }
    Ok(())
}

/// A value of `MAX_VALUE` bytes applies; one byte more fails with `TooLarge`
/// and changes nothing.
async fn arfc0006_s5_4_3_value_over_limit_fails_too_large(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    apply_ok(
        &*store,
        &mut model,
        batch(1, vec![put(b"k", &vec![1; MAX_VALUE])]),
    )
    .await?;
    ensure!(
        store.get(b"k").await.ctx("get")? == Some(vec![1; MAX_VALUE]),
        "the value at the limit does not read back"
    );
    let bad = batch(
        2,
        vec![put(b"before", b"x"), put(b"k", &vec![2; MAX_VALUE + 1])],
    );
    expect_err(
        store.apply(bad).await,
        Kind::TooLarge,
        "a value one byte over the limit",
    )?;
    expect_model(&*store, &model, "after an oversized value").await
}

/// A batch whose keys and values total `MAX_BATCH_BYTES` applies; one byte
/// more fails with `TooLarge` and changes nothing.
async fn arfc0006_s5_4_3_batch_over_limit_fails_too_large(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    let at_limit = limit_ops(MAX_BATCH_BYTES);
    let total: usize = at_limit.iter().map(op_bytes).sum();
    ensure!(total == MAX_BATCH_BYTES, "the suite built {total} bytes");
    apply_ok(&*store, &mut model, batch(1, at_limit)).await?;
    let mut over = limit_ops(MAX_BATCH_BYTES);
    over.push(delete(b"x"));
    expect_err(
        store.apply(batch(2, over)).await,
        Kind::TooLarge,
        "a batch one byte over",
    )?;
    ensure_eq!(
        store.applied().await.ctx("applied")?,
        Some(id(1, 1)),
        "the applied position"
    );
    ensure_eq!(
        store.get(b"x").await.ctx("get")?,
        None,
        "a key of the rejected batch"
    );
    let held = contents(&*store).await?;
    ensure!(
        held == model.map,
        "the contents changed after an oversized batch"
    );
    Ok(())
}

/// The key and value bytes of `op`.
fn op_bytes(op: &Op) -> usize {
    match op {
        Op::Put { key, value } => key.len() + value.len(),
        Op::Delete { key } => key.len(),
    }
}

/// `Put`s of 4-byte keys and values of at most `MAX_VALUE` whose key and
/// value bytes total exactly `total`.
fn limit_ops(total: usize) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut left = total;
    let mut n = 0u32;
    while left > 4 {
        let value = (left - 4).min(MAX_VALUE);
        ops.push(put(&n.to_be_bytes(), &vec![n as u8; value]));
        left -= 4 + value;
        n += 1;
    }
    ops
}

/// While an install is in progress, `apply` fails with `Busy` and changes
/// nothing; once the install is dropped, it applies.
async fn arfc0006_s5_4_3_apply_during_install_fails_busy(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    apply_ok(&*store, &mut model, batch(1, vec![put(b"k", b"v")])).await?;
    let pending = store.begin_install().await.ctx("begin_install")?;
    expect_err(
        store.apply(batch(2, vec![put(b"k", b"w")])).await,
        Kind::Busy,
        "apply during an install",
    )?;
    pending
        .write(StateModel::default().export())
        .await
        .ctx("write")?;
    expect_err(
        store.apply(batch(3, Vec::new())).await,
        Kind::Busy,
        "apply during an install with bytes written",
    )?;
    drop(pending);
    expect_model(&*store, &model, "after the dropped install").await?;
    apply_ok(&*store, &mut model, batch(2, vec![put(b"k", b"w")])).await?;
    expect_model(
        &*store,
        &model,
        "after an apply once the install is dropped",
    )
    .await
}

/// `get` and `scan` reflect every apply that returned before them.
async fn arfc0006_s5_4_4_reads_reflect_every_returned_apply(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    let mut data = Gen::new(env.seed);
    for index in 1..=60 {
        let ops = (0..data.below(4))
            .map(|_| {
                let key = data.key();
                if data.below(3) == 0 {
                    delete(&key)
                } else {
                    let len = data.below(16) as usize;
                    put(&key, &data.bytes(len))
                }
            })
            .collect();
        apply_ok(&*store, &mut model, batch(index, ops)).await?;
        let key = data.key();
        ensure_eq!(
            store.get(&key).await.ctx("get")?,
            model.map.get(&key).cloned(),
            "get({key:?}) after apply {index}"
        );
        let start = data.key();
        let want = model
            .scan(&start, None, MAX_SCAN)
            .map_err(|_| fail("model scan"))?;
        ensure_eq!(
            store.scan(&start, None, MAX_SCAN).await.ctx("scan")?,
            want,
            "scan from {start:?} after apply {index}"
        );
    }
    Ok(())
}

/// The arguments of one `scan`.
type Range<'a> = (&'a [u8], Option<&'a [u8]>, usize);

/// `scan` returns the pairs from `start` to before `end`, ascending
/// bytewise.
async fn arfc0006_s5_4_4_scan_returns_the_range_ascending_bytewise(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    let keys: [&[u8]; 11] = [
        b"b",
        &[0xFF, 0x00],
        b"a",
        &[0x00, 0x00],
        &[0x80],
        &[0x00],
        b"ab",
        &[0x7F],
        &[0xFF],
        &[0x01],
        b"a\x00",
    ];
    let ops = keys
        .iter()
        .map(|key| put(key, &[key.len() as u8]))
        .collect();
    apply_ok(&*store, &mut model, batch(1, ops)).await?;
    let ranges: [Range<'_>; 11] = [
        (b"", None, MAX_SCAN),
        (b"a", None, MAX_SCAN),
        (b"a", Some(b"b"), MAX_SCAN),
        (b"a", Some(b"a"), MAX_SCAN),
        (b"b", Some(b"a"), MAX_SCAN),
        (&[0x00], Some(&[0x00, 0x00]), MAX_SCAN),
        (&[0x00, 0x01], Some(&[0x80]), MAX_SCAN),
        (&[0xFF], None, MAX_SCAN),
        (&[0xFF, 0x00, 0x00], None, MAX_SCAN),
        (b"", None, 3),
        (b"a", Some(&[0xFF]), 2),
    ];
    for (start, end, limit) in ranges {
        let want = model
            .scan(start, end, limit)
            .map_err(|_| fail("model scan"))?;
        let got = store.scan(start, end, limit).await.ctx("scan")?;
        ensure_eq!(got, want, "scan({start:?}, {end:?}, {limit})");
    }
    Ok(())
}

/// `scan` returns at most `limit` pairs, up to `MAX_SCAN`.
async fn arfc0006_s5_4_4_scan_returns_at_most_limit_pairs(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let ops = (0..MAX_SCAN as u32 + 5)
        .map(|n| put(&n.to_be_bytes(), b""))
        .collect();
    store.apply(batch(1, ops)).await.ctx("apply")?;
    for limit in [1, 2, 999, MAX_SCAN] {
        let got = store.scan(b"", None, limit).await.ctx("scan")?;
        ensure_eq!(
            got.len(),
            limit,
            "pairs returned by scan with limit {limit}"
        );
        let first: Vec<u8> = 0u32.to_be_bytes().to_vec();
        ensure!(
            got[0].0 == first,
            "scan with limit {limit} does not start at the lowest key"
        );
    }
    let tail = store
        .scan(&(MAX_SCAN as u32).to_be_bytes(), None, MAX_SCAN)
        .await
        .ctx("scan of the tail")?;
    ensure_eq!(tail.len(), 5, "pairs at and after key {MAX_SCAN}");
    Ok(())
}

/// A `limit` of 0 or above `MAX_SCAN` fails with `InvalidInput`.
async fn arfc0006_s5_4_4_scan_limit_out_of_range_fails_invalid_input(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    store
        .apply(batch(1, vec![put(b"k", b"v")]))
        .await
        .ctx("apply")?;
    for limit in [0, MAX_SCAN + 1, usize::MAX] {
        expect_err(
            store.scan(b"", None, limit).await,
            Kind::InvalidInput,
            &format!("scan with limit {limit}"),
        )?;
    }
    Ok(())
}

/// `scan` stops after the pair that brings key and value bytes to
/// `MAX_READ_BYTES` or more.
async fn arfc0006_s5_4_4_scan_stops_after_the_pair_reaching_the_limit(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut ops = limit_ops(MAX_READ_BYTES);
    let reaching = ops.len();
    ops.push(put(&u32::MAX.to_be_bytes(), b"after"));
    let second = ops.split_off(ops.len() / 2);
    store.apply(batch(1, ops)).await.ctx("the first half")?;
    store.apply(batch(2, second)).await.ctx("the second half")?;
    let got = store.scan(b"", None, MAX_SCAN).await.ctx("scan")?;
    ensure_eq!(got.len(), reaching, "pairs returned up to the byte limit");
    let rest = store
        .scan(&1u32.to_be_bytes(), None, MAX_SCAN)
        .await
        .ctx("scan from key 1")?;
    ensure_eq!(
        rest.len(),
        reaching,
        "pairs from key 1, under the byte limit"
    );
    ensure!(
        rest.last()
            .is_some_and(|(key, _)| key == &u32::MAX.to_be_bytes()),
        "the scan from key 1 does not reach the last key"
    );
    Ok(())
}

/// After a call fails with `Io`, every later call on the instance fails with
/// `Io`. Only `mem` fails on demand, through `fail_after` (section 5.2).
async fn arfc0006_s5_4_7_after_io_every_call_fails_io(env: Env) -> Outcome {
    let store = build(&env, &mem_config("store-0", 1)?)?.ctx("building with fail_after 1")?;
    store
        .apply(batch(1, vec![put(b"k", b"v")]))
        .await
        .ctx("apply")?;
    expect_io(
        store.apply(batch(2, Vec::new())).await,
        "the apply after fail_after",
    )?;
    expect_io(store.applied().await, "applied after Io")?;
    expect_io(store.get(b"k").await, "get after Io")?;
    expect_io(store.scan(b"", None, 1).await, "scan after Io")?;
    expect_io(store.snapshot().await, "snapshot after Io")?;
    expect_io(store.begin_install().await, "begin_install after Io")?;
    expect_io(store.apply(batch(3, Vec::new())).await, "apply after Io")?;
    Ok(())
}

/// A random history of applies, reads, snapshots, installs, and restarts
/// matches the model, results and errors alike.
async fn arfc0006_s5_4_random_history_matches_the_model(env: Env) -> Outcome {
    let mut data = Gen::new(env.seed);
    let mut model = StateModel::default();
    let mut store = open(&env, 0)?;
    let mut next = 1u64;
    for step in 0..400u32 {
        let what = format!("step {step} (seed {})", env.seed);
        match data.below(10) {
            0..=3 => {
                let stale = model.applied.is_some() && data.below(8) == 0;
                let index = if stale {
                    next.saturating_sub(1 + data.below(3))
                } else {
                    next + data.below(3)
                };
                let mut ops: Vec<Op> = (0..data.below(5))
                    .map(|_| {
                        let key = data.key();
                        if data.below(3) == 0 {
                            delete(&key)
                        } else {
                            let len = data.below(24) as usize;
                            put(&key, &data.bytes(len))
                        }
                    })
                    .collect();
                if !stale && data.below(10) == 0 {
                    ops.push(put(b"", b"empty key"));
                }
                let next_batch = ApplyBatch {
                    log_id: id(1 + data.below(3), index),
                    ops,
                };
                let want = model.apply(&next_batch).err();
                let got = store.apply(next_batch).await;
                ensure_eq!(outcome(&got), want, "{what}: apply at {index}");
                if want.is_none() {
                    next = index + 1;
                }
            }
            4 | 5 => {
                let key = data.key();
                ensure_eq!(
                    store.get(&key).await.ctx(&what)?,
                    model.map.get(&key).cloned(),
                    "{what}: get"
                );
                let start = data.key();
                let end = (data.below(2) == 0).then(|| data.key());
                let limit = data.below(6) as usize;
                let want = model.scan(&start, end.as_deref(), limit);
                let got = store.scan(&start, end.as_deref(), limit).await;
                match (&got, &want) {
                    (Ok(got), Ok(want)) => ensure_eq!(got, want, "{what}: scan"),
                    _ => ensure_eq!(outcome(&got), want.as_ref().err().copied(), "{what}: scan"),
                }
            }
            6 => {
                let snapshot = store.snapshot().await.ctx(&what)?;
                let before = model.clone();
                if model
                    .apply(&batch(next, vec![put(b"after", b"snapshot")]))
                    .is_ok()
                {
                    store
                        .apply(batch(next, vec![put(b"after", b"snapshot")]))
                        .await
                        .ctx(&what)?;
                    next += 1;
                }
                ensure_eq!(
                    snapshot.applied(),
                    before.applied,
                    "{what}: snapshot applied"
                );
                let export = crate::support::export_of(&*snapshot).await?;
                ensure!(
                    export == before.export(),
                    "{what}: the snapshot's export differs"
                );
            }
            7 => {
                let mut incoming = StateModel::default();
                for _ in 0..data.below(6) {
                    let len = data.below(8) as usize;
                    incoming.map.insert(data.key(), data.bytes(len));
                }
                if data.below(4) != 0 {
                    incoming.applied = Some(id(2, data.below(next + 5)));
                }
                let chunk = 1 + data.below(64) as usize;
                let got = install(&*store, &incoming.export(), chunk)
                    .await?
                    .ctx(&what)?;
                ensure_eq!(got, incoming.applied, "{what}: finish");
                model = incoming;
                next = model.applied.map_or(1, |at| at.index + 1);
            }
            8 => {
                store = restart(&env, 0, store)?;
                expect_model(&*store, &model, &format!("{what}: after a restart")).await?;
            }
            _ => {
                ensure_eq!(
                    store.applied().await.ctx(&what)?,
                    model.applied,
                    "{what}: applied"
                );
                let export = export_now(&*store).await?;
                ensure!(export == model.export(), "{what}: the export differs");
            }
        }
    }
    expect_model(&*store, &model, "at the end of the history").await
}
