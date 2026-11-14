//! Clauses 5.3.1 to 5.3.8: the behavior of a log store.

use std::fmt;
use std::sync::Arc;

use crate::model::{Kind, LogModel, kind, outcome};
use crate::state::{
    Entry, LogState, LogStore, MAX_APPEND_BYTES, MAX_ENTRY_PAYLOAD, MAX_READ_BYTES, StoreError,
};
use crate::support::{
    Ctx, Gen, build, entry, fail, id, mem_config, open, read_all, restart, run, seen, vote,
};
use crate::{Applies, Check, Env, Failure, Implementation, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0006#5.3.1",
            Applies::All,
            arfc0006_s5_3_1_fresh_store_holds_nothing
        ),
        check!(
            "arfc-0006#5.3.2",
            Applies::All,
            arfc0006_s5_3_2_every_mutation_survives_a_restart
        ),
        check!(
            "arfc-0006#5.3.3",
            Applies::All,
            arfc0006_s5_3_3_read_vote_returns_the_last_saved
        ),
        check!(
            "arfc-0006#5.3.4",
            Applies::All,
            arfc0006_s5_3_4_append_adds_entries_in_order
        ),
        check!(
            "arfc-0006#5.3.4",
            Applies::All,
            arfc0006_s5_3_4_empty_append_changes_nothing
        ),
        check!(
            "arfc-0006#5.3.4",
            Applies::All,
            arfc0006_s5_3_4_first_append_without_last_may_start_anywhere
        ),
        check!(
            "arfc-0006#5.3.4",
            Applies::All,
            arfc0006_s5_3_4_discontiguous_append_fails_not_contiguous
        ),
        check!(
            "arfc-0006#5.3.4",
            Applies::All,
            arfc0006_s5_3_4_append_after_purge_continues_from_last
        ),
        check!(
            "arfc-0006#5.3.4",
            Applies::All,
            arfc0006_s5_3_4_payload_over_limit_fails_too_large
        ),
        check!(
            "arfc-0006#5.3.4",
            Applies::All,
            arfc0006_s5_3_4_list_over_limit_fails_too_large
        ),
        check!(
            "arfc-0006#5.3.5",
            Applies::All,
            arfc0006_s5_3_5_first_above_last_fails_invalid_input
        ),
        check!(
            "arfc-0006#5.3.5",
            Applies::All,
            arfc0006_s5_3_5_read_returns_held_entries_in_range
        ),
        check!(
            "arfc-0006#5.3.5",
            Applies::All,
            arfc0006_s5_3_5_read_stops_after_the_entry_reaching_the_limit
        ),
        check!(
            "arfc-0006#5.3.6",
            Applies::All,
            arfc0006_s5_3_6_truncate_removes_entries_from_index
        ),
        check!(
            "arfc-0006#5.3.6",
            Applies::All,
            arfc0006_s5_3_6_truncating_every_entry_leaves_last_purged
        ),
        check!(
            "arfc-0006#5.3.6",
            Applies::All,
            arfc0006_s5_3_6_truncate_at_or_below_purged_fails_invalid_input
        ),
        check!(
            "arfc-0006#5.3.7",
            Applies::All,
            arfc0006_s5_3_7_purge_removes_entries_through_index
        ),
        check!(
            "arfc-0006#5.3.7",
            Applies::All,
            arfc0006_s5_3_7_purge_beyond_last_moves_last
        ),
        check!(
            "arfc-0006#5.3.7",
            Applies::All,
            arfc0006_s5_3_7_purge_at_or_below_purged_changes_nothing
        ),
        check!(
            "arfc-0006#5.3.8",
            Applies::Only(Implementation::Mem),
            arfc0006_s5_3_8_after_io_every_call_fails_io
        ),
        check!(
            "arfc-0006#5.3",
            Applies::All,
            arfc0006_s5_3_random_history_matches_the_model
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

/// Fails unless `store` shows exactly what `model` holds.
pub(crate) async fn expect_model(store: &dyn LogStore, model: &LogModel, what: &str) -> Outcome {
    let got = seen(store).await?;
    ensure_eq!(got.vote, model.vote, "{what}: the vote");
    ensure_eq!(got.state, model.state(), "{what}: the log state");
    let held = model.read(0, u64::MAX).map_err(|_| fail("model read"))?;
    ensure_eq!(got.entries, held, "{what}: the entries");
    Ok(())
}

/// Appends `entries` to both the store and the model and requires success.
async fn append_ok(store: &dyn LogStore, model: &mut LogModel, entries: Vec<Entry>) -> Outcome {
    model
        .append(&entries)
        .map_err(|error| fail(format!("the model rejects a valid append: {error:?}")))?;
    store.append(entries).await.ctx("a valid append")
}

/// A store at place 0 holding entries `1..=last` of term 1, and its model.
async fn with_log(env: &Env, last: u64) -> Result<(Arc<dyn LogStore>, LogModel), Failure> {
    let store = open(env, 0)?;
    let mut model = LogModel::default();
    append_ok(&*store, &mut model, run(1, 1, last)).await?;
    Ok((store, model))
}

/// A store never written holds no vote, no entry, and no positions.
async fn arfc0006_s5_3_1_fresh_store_holds_nothing(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    ensure_eq!(store.read_vote().await.ctx("read_vote")?, None, "the vote");
    ensure_eq!(
        store.state().await.ctx("state")?,
        LogState {
            last_purged: None,
            last: None
        },
        "the log state"
    );
    for (first, last) in [(0, u64::MAX), (0, 0), (1, 1), (u64::MAX, u64::MAX)] {
        let got = store.read(first, last).await.ctx("read")?;
        ensure!(
            got.is_empty(),
            "read({first}, {last}) of a fresh store returned {got:?}"
        );
    }
    Ok(())
}

/// Each mutating call that returned `Ok` is held by a new instance on the
/// same place.
async fn arfc0006_s5_3_2_every_mutation_survives_a_restart(env: Env) -> Outcome {
    let mut store = open(&env, 0)?;
    let mut model = LogModel::default();
    let steps: [(&str, u8); 9] = [
        ("save_vote", 0),
        ("append 1..=5", 1),
        ("save_vote again", 2),
        ("append 6..=8", 3),
        ("truncate_from 7", 4),
        ("purge_through 3", 5),
        ("append 7..=9 in term 2", 6),
        ("purge_through beyond last", 7),
        ("append after the purge", 8),
    ];
    for (what, step) in steps {
        match step {
            0 => {
                let v = vote(1, 1, false);
                store.save_vote(v).await.ctx(what)?;
                model.vote = Some(v);
            }
            1 => append_ok(&*store, &mut model, run(1, 1, 5)).await?,
            2 => {
                let v = vote(2, 2, true);
                store.save_vote(v).await.ctx(what)?;
                model.vote = Some(v);
            }
            3 => append_ok(&*store, &mut model, run(1, 6, 8)).await?,
            4 => {
                store.truncate_from(7).await.ctx(what)?;
                model.truncate_from(7).map_err(|_| fail("model truncate"))?;
            }
            5 => {
                let at = id(1, 3);
                store.purge_through(at).await.ctx(what)?;
                model.purge_through(at);
            }
            6 => append_ok(&*store, &mut model, run(2, 7, 9)).await?,
            7 => {
                let at = id(3, 20);
                store.purge_through(at).await.ctx(what)?;
                model.purge_through(at);
            }
            _ => append_ok(&*store, &mut model, run(3, 21, 22)).await?,
        }
        store = restart(&env, 0, store)?;
        expect_model(&*store, &model, &format!("after {what} and a restart")).await?;
    }
    Ok(())
}

/// `save_vote` replaces the vote, whatever its term, and `read_vote` returns
/// the last one saved.
async fn arfc0006_s5_3_3_read_vote_returns_the_last_saved(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    for v in [
        vote(1, 1, false),
        vote(1, 1, true),
        vote(5, 2, false),
        vote(3, 0, true),
        vote(3, 0, true),
        vote(u64::MAX, 255, false),
        vote(0, 7, false),
    ] {
        store.save_vote(v).await.ctx("save_vote")?;
        ensure_eq!(
            store.read_vote().await.ctx("read_vote")?,
            Some(v),
            "read_vote after save_vote"
        );
    }
    Ok(())
}

/// Entries appended in several calls are held in order, each as appended.
async fn arfc0006_s5_3_4_append_adds_entries_in_order(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = LogModel::default();
    let mut empty = entry(1, 4);
    empty.payload.clear();
    for batch in [run(1, 1, 3), vec![empty], run(2, 5, 9)] {
        append_ok(&*store, &mut model, batch).await?;
        expect_model(&*store, &model, "after an append").await?;
    }
    ensure_eq!(
        store.state().await.ctx("state")?.last,
        Some(id(2, 9)),
        "last after appending 1..=9"
    );
    Ok(())
}

/// An empty list changes nothing, on an empty log, a log with entries, and a
/// log whose `last` is its `last_purged`.
async fn arfc0006_s5_3_4_empty_append_changes_nothing(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = LogModel::default();
    store
        .append(Vec::new())
        .await
        .ctx("an empty append to an empty log")?;
    expect_model(&*store, &model, "after an empty append to an empty log").await?;
    append_ok(&*store, &mut model, run(1, 1, 3)).await?;
    store.append(Vec::new()).await.ctx("an empty append")?;
    expect_model(&*store, &model, "after an empty append").await?;
    let at = id(2, 6);
    store.purge_through(at).await.ctx("purge_through")?;
    model.purge_through(at);
    store
        .append(Vec::new())
        .await
        .ctx("an empty append after a purge")?;
    expect_model(&*store, &model, "after an empty append after a purge").await
}

/// Without `last`, nothing constrains the first entry's index.
async fn arfc0006_s5_3_4_first_append_without_last_may_start_anywhere(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = LogModel::default();
    append_ok(&*store, &mut model, run(4, 7, 8)).await?;
    expect_model(&*store, &model, "after a first append at index 7").await
}

/// A list that leaves a gap, overlaps, repeats, or goes backwards fails with
/// `NotContiguous` and changes nothing, not even its contiguous prefix.
async fn arfc0006_s5_3_4_discontiguous_append_fails_not_contiguous(env: Env) -> Outcome {
    let (store, model) = with_log(&env, 5).await?;
    let cases: [(&str, Vec<Entry>); 9] = [
        ("a gap after last", vec![entry(1, 7)]),
        ("a repeat of last", vec![entry(2, 5)]),
        ("an overlap", run(2, 3, 7)),
        ("a restart at 1", run(2, 1, 2)),
        ("a gap inside the list", vec![entry(1, 6), entry(1, 8)]),
        (
            "a duplicate inside the list",
            vec![entry(1, 6), entry(1, 6)],
        ),
        (
            "a repeat at the end of the list",
            vec![entry(1, 6), entry(1, 7), entry(1, 7)],
        ),
        ("a list going backwards", vec![entry(1, 7), entry(1, 6)]),
        ("index 0", vec![entry(1, 0)]),
    ];
    for (what, entries) in cases {
        expect_err(store.append(entries).await, Kind::NotContiguous, what)?;
        expect_model(&*store, &model, &format!("after {what}")).await?;
    }
    Ok(())
}

/// After a purge beyond the held entries, `last` is the purged position and
/// the next append must continue from it.
async fn arfc0006_s5_3_4_append_after_purge_continues_from_last(env: Env) -> Outcome {
    let (store, mut model) = with_log(&env, 3).await?;
    let at = id(5, 10);
    store.purge_through(at).await.ctx("purge_through")?;
    model.purge_through(at);
    for (what, index) in [
        ("the old next index", 4),
        ("a gap", 12),
        ("the purged index", 10),
    ] {
        expect_err(
            store.append(vec![entry(5, index)]).await,
            Kind::NotContiguous,
            what,
        )?;
    }
    expect_model(&*store, &model, "after rejected appends").await?;
    append_ok(&*store, &mut model, run(5, 11, 12)).await?;
    expect_model(&*store, &model, "after appending from last").await
}

/// An entry whose payload exceeds `MAX_ENTRY_PAYLOAD` fails with `TooLarge`
/// and changes nothing; one of exactly that size is held as appended.
async fn arfc0006_s5_3_4_payload_over_limit_fails_too_large(env: Env) -> Outcome {
    let (store, mut model) = with_log(&env, 2).await?;
    let mut big = entry(1, 3);
    big.payload = vec![0x5A; MAX_ENTRY_PAYLOAD + 1];
    expect_err(
        store.append(vec![big.clone()]).await,
        Kind::TooLarge,
        "an entry one byte over the limit",
    )?;
    expect_model(&*store, &model, "after an oversized entry").await?;
    big.log_id.index = 4;
    expect_err(
        store.append(vec![entry(1, 3), big]).await,
        Kind::TooLarge,
        "a small entry followed by an oversized one",
    )?;
    expect_model(&*store, &model, "after a list ending in an oversized entry").await?;

    let mut limit = entry(1, 3);
    limit.payload = vec![0xA5; MAX_ENTRY_PAYLOAD];
    append_ok(&*store, &mut model, vec![limit.clone()]).await?;
    let got = store
        .read(3, 3)
        .await
        .ctx("read of the entry at the limit")?;
    ensure!(
        got.len() == 1 && got[0] == limit,
        "the entry at the limit reads back as {got:?}"
    );
    Ok(())
}

/// A list whose payloads together exceed `MAX_APPEND_BYTES`, each within
/// `MAX_ENTRY_PAYLOAD`, fails with `TooLarge` and changes nothing.
async fn arfc0006_s5_3_4_list_over_limit_fails_too_large(env: Env) -> Outcome {
    let (store, model) = with_log(&env, 2).await?;
    let mut first = entry(1, 3);
    first.payload = vec![1; MAX_ENTRY_PAYLOAD];
    let mut second = entry(1, 4);
    second.payload = vec![2; MAX_APPEND_BYTES - MAX_ENTRY_PAYLOAD + 1];
    expect_err(
        store.append(vec![first, second]).await,
        Kind::TooLarge,
        "a list one byte over the limit",
    )?;
    expect_model(&*store, &model, "after an oversized list").await
}

/// `read(first, last)` with `first > last` fails with `InvalidInput`.
async fn arfc0006_s5_3_5_first_above_last_fails_invalid_input(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    expect_err(
        store.read(5, 4).await,
        Kind::InvalidInput,
        "read(5, 4) of an empty log",
    )?;
    let (store, _) = {
        drop(store);
        with_log(&env, 6).await?
    };
    for (first, last) in [(5, 4), (2, 1), (u64::MAX, 0), (7, 6)] {
        expect_err(
            store.read(first, last).await,
            Kind::InvalidInput,
            &format!("read({first}, {last})"),
        )?;
    }
    Ok(())
}

/// `read` returns the held entries in the range, ascending, each as appended.
async fn arfc0006_s5_3_5_read_returns_held_entries_in_range(env: Env) -> Outcome {
    let (store, mut model) = with_log(&env, 10).await?;
    let at = id(1, 3);
    store.purge_through(at).await.ctx("purge_through")?;
    model.purge_through(at);
    for (first, last) in [
        (0, u64::MAX),
        (5, 7),
        (7, 7),
        (1, 3),
        (1, 4),
        (0, 0),
        (9, 100),
        (11, 20),
        (u64::MAX, u64::MAX),
    ] {
        let want = model.read(first, last).map_err(|_| fail("model read"))?;
        let got = store.read(first, last).await.ctx("read")?;
        ensure_eq!(got, want, "read({first}, {last})");
    }
    Ok(())
}

/// `read` stops after the entry that brings the payload total to
/// `MAX_READ_BYTES` or more.
async fn arfc0006_s5_3_5_read_stops_after_the_entry_reaching_the_limit(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let sizes = [
        MAX_READ_BYTES / 2,
        MAX_READ_BYTES / 2 - 1,
        1,
        5,
        MAX_READ_BYTES,
        3,
    ];
    let entries: Vec<Entry> = sizes
        .iter()
        .zip(1u64..)
        .map(|(&len, index)| Entry {
            log_id: id(1, index),
            payload: vec![index as u8; len],
        })
        .collect();
    for one in &entries {
        store.append(vec![one.clone()]).await.ctx("append")?;
    }
    let ids = |got: &[Entry]| got.iter().map(|e| e.log_id.index).collect::<Vec<_>>();
    for (first, last, want) in [
        (1, 6, vec![1, 2, 3]),
        (1, 2, vec![1, 2]),
        (2, 6, vec![2, 3, 4, 5]),
        (4, 6, vec![4, 5]),
        (5, 6, vec![5]),
        (6, 6, vec![6]),
    ] {
        let got = store.read(first, last).await.ctx("read")?;
        ensure_eq!(ids(&got), want, "the indexes read({first}, {last}) returns");
        for one in &got {
            let index = one.log_id.index as usize;
            ensure!(
                *one == entries[index - 1],
                "entry {index} does not read back as appended"
            );
        }
    }
    Ok(())
}

/// `truncate_from(index)` removes every entry at or above `index`, and `last`
/// becomes the highest entry left.
async fn arfc0006_s5_3_6_truncate_removes_entries_from_index(env: Env) -> Outcome {
    let (store, mut model) = with_log(&env, 8).await?;
    for index in [6, 20, 9] {
        store.truncate_from(index).await.ctx("truncate_from")?;
        model
            .truncate_from(index)
            .map_err(|_| fail("model truncate"))?;
        expect_model(&*store, &model, &format!("after truncate_from({index})")).await?;
    }
    append_ok(&*store, &mut model, run(2, 6, 7)).await?;
    expect_model(&*store, &model, "after appending past the truncation").await?;
    store.truncate_from(1).await.ctx("truncate_from(1)")?;
    model.truncate_from(1).map_err(|_| fail("model truncate"))?;
    expect_model(&*store, &model, "after truncating every entry").await?;
    store
        .truncate_from(0)
        .await
        .ctx("truncate_from(0) of an empty log")?;
    expect_model(&*store, &model, "after truncate_from(0)").await
}

/// Truncating every held entry of a purged log leaves `last` at
/// `last_purged`.
async fn arfc0006_s5_3_6_truncating_every_entry_leaves_last_purged(env: Env) -> Outcome {
    let (store, mut model) = with_log(&env, 8).await?;
    let at = id(1, 3);
    store.purge_through(at).await.ctx("purge_through")?;
    model.purge_through(at);
    store.truncate_from(4).await.ctx("truncate_from(4)")?;
    model.truncate_from(4).map_err(|_| fail("model truncate"))?;
    ensure_eq!(
        store.state().await.ctx("state")?,
        LogState {
            last_purged: Some(at),
            last: Some(at)
        },
        "the log state"
    );
    expect_model(&*store, &model, "after truncating every held entry").await
}

/// `truncate_from` at or below `last_purged`'s index fails with
/// `InvalidInput` and changes nothing.
async fn arfc0006_s5_3_6_truncate_at_or_below_purged_fails_invalid_input(env: Env) -> Outcome {
    let (store, mut model) = with_log(&env, 8).await?;
    let at = id(1, 3);
    store.purge_through(at).await.ctx("purge_through")?;
    model.purge_through(at);
    for index in [3, 2, 0] {
        expect_err(
            store.truncate_from(index).await,
            Kind::InvalidInput,
            &format!("truncate_from({index}) with last_purged at 3"),
        )?;
        expect_model(&*store, &model, &format!("after truncate_from({index})")).await?;
    }
    Ok(())
}

/// `purge_through` removes every entry at or below the index and sets
/// `last_purged`; `last` stays the highest entry held.
async fn arfc0006_s5_3_7_purge_removes_entries_through_index(env: Env) -> Outcome {
    let (store, mut model) = with_log(&env, 8).await?;
    let at = id(1, 3);
    store.purge_through(at).await.ctx("purge_through(3)")?;
    model.purge_through(at);
    ensure_eq!(
        store.state().await.ctx("state")?,
        LogState {
            last_purged: Some(at),
            last: Some(id(1, 8))
        },
        "the log state after purging through 3"
    );
    expect_model(&*store, &model, "after purge_through(3)").await?;
    let at = id(1, 8);
    store.purge_through(at).await.ctx("purge_through(8)")?;
    model.purge_through(at);
    expect_model(&*store, &model, "after purging through last").await
}

/// A purge above `last`'s index makes `last` the purged position, on a log
/// with entries and on a fresh one.
async fn arfc0006_s5_3_7_purge_beyond_last_moves_last(env: Env) -> Outcome {
    let (store, mut model) = with_log(&env, 4).await?;
    let at = id(9, 10);
    store.purge_through(at).await.ctx("purge_through")?;
    model.purge_through(at);
    ensure_eq!(
        store.state().await.ctx("state")?,
        LogState {
            last_purged: Some(at),
            last: Some(at)
        },
        "the log state after a purge beyond last"
    );
    expect_model(&*store, &model, "after a purge beyond last").await?;
    drop(store);

    let fresh = open(&env, 1)?;
    let at = id(2, 5);
    fresh
        .purge_through(at)
        .await
        .ctx("purge_through on a fresh log")?;
    ensure_eq!(
        fresh.state().await.ctx("state")?,
        LogState {
            last_purged: Some(at),
            last: Some(at)
        },
        "the log state after purging a fresh log"
    );
    Ok(())
}

/// A purge at or below `last_purged`'s index changes nothing, whatever its
/// term.
async fn arfc0006_s5_3_7_purge_at_or_below_purged_changes_nothing(env: Env) -> Outcome {
    let (store, mut model) = with_log(&env, 8).await?;
    let at = id(1, 5);
    store.purge_through(at).await.ctx("purge_through(5)")?;
    model.purge_through(at);
    for again in [id(7, 5), id(1, 5), id(1, 2), id(9, 0)] {
        store
            .purge_through(again)
            .await
            .ctx("a purge at or below last_purged")?;
        expect_model(
            &*store,
            &model,
            &format!("after purge_through at index {}", again.index),
        )
        .await?;
    }
    Ok(())
}

/// After a call fails with `Io`, every later call on the instance fails with
/// `Io`. Only `mem` fails on demand, through `fail_after` (section 5.2).
async fn arfc0006_s5_3_8_after_io_every_call_fails_io(env: Env) -> Outcome {
    let store = build(&env, &mem_config("store-0", 2)?)?.ctx("building with fail_after 2")?;
    store.save_vote(vote(1, 1, false)).await.ctx("save_vote")?;
    store.append(run(1, 1, 2)).await.ctx("append")?;
    expect_io(
        store.append(run(1, 3, 3)).await,
        "the append after fail_after",
    )?;
    expect_io(store.read_vote().await, "read_vote after Io")?;
    expect_io(store.state().await, "state after Io")?;
    expect_io(store.read(0, u64::MAX).await, "read after Io")?;
    expect_io(
        store.save_vote(vote(2, 1, false)).await,
        "save_vote after Io",
    )?;
    expect_io(store.append(run(1, 3, 3)).await, "append after Io")?;
    expect_io(store.truncate_from(2).await, "truncate_from after Io")?;
    expect_io(
        store.purge_through(id(1, 1)).await,
        "purge_through after Io",
    )?;
    expect_io(store.read_vote().await, "read_vote after more failures")?;
    Ok(())
}

/// A random history of every call, with restarts, matches the model of
/// clauses 5.3.1 to 5.3.7, results and errors alike.
async fn arfc0006_s5_3_random_history_matches_the_model(env: Env) -> Outcome {
    let mut data = Gen::new(env.seed);
    let mut model = LogModel::default();
    let mut store = open(&env, 0)?;
    let mut term = 1;
    for step in 0..600u32 {
        let what = format!("step {step} (seed {})", env.seed);
        match data.below(12) {
            0 => {
                let v = vote(data.below(20), data.below(4) as u8, data.below(2) == 0);
                store.save_vote(v).await.ctx(&what)?;
                model.vote = Some(v);
            }
            1..=4 => {
                if data.below(4) == 0 {
                    term += 1;
                }
                let start = match data.below(8) {
                    0 => model.next_index() + 1,
                    1 => model.next_index().saturating_sub(1),
                    _ => model.next_index(),
                };
                let count = data.below(5);
                let mut entries = run(term, start, start + count);
                entries.truncate(count as usize);
                if count >= 2 && data.below(6) == 0 {
                    entries[count as usize - 1].log_id.index += 1;
                }
                let want = model.clone().append(&entries).err();
                let got = store.append(entries.clone()).await;
                ensure_eq!(outcome(&got), want, "{what}: append");
                if want.is_none() {
                    model.append(&entries).map_err(|_| fail("model append"))?;
                }
            }
            5 | 6 => {
                let index = data.below(model.next_index() + 2);
                let want = model.truncate_from(index).err();
                let got = store.truncate_from(index).await;
                ensure_eq!(outcome(&got), want, "{what}: truncate_from({index})");
            }
            7 => {
                let index = data.below(model.next_index() + 3);
                let at = match model.held(index) {
                    Some(held) => held.log_id,
                    None => id(term + data.below(3), index),
                };
                store.purge_through(at).await.ctx(&what)?;
                model.purge_through(at);
            }
            8 | 9 => {
                let first = data.below(model.next_index() + 2);
                let last = data.below(model.next_index() + 2);
                let want = model.read(first, last);
                let got = store.read(first, last).await;
                match (&got, &want) {
                    (Ok(got), Ok(want)) => ensure_eq!(got, want, "{what}: read({first}, {last})"),
                    _ => ensure_eq!(
                        outcome(&got),
                        want.as_ref().err().copied(),
                        "{what}: read({first}, {last})"
                    ),
                }
            }
            10 => {
                ensure_eq!(
                    store.state().await.ctx(&what)?,
                    model.state(),
                    "{what}: state"
                );
                ensure_eq!(
                    store.read_vote().await.ctx(&what)?,
                    model.vote,
                    "{what}: vote"
                );
            }
            _ => {
                store = restart(&env, 0, store)?;
                expect_model(&*store, &model, &format!("{what}: after a restart")).await?;
            }
        }
    }
    expect_model(&*store, &model, "at the end of the history").await?;
    let tail = read_all(&*store, 0, u64::MAX).await?;
    ensure!(
        tail.windows(2)
            .all(|pair| pair[1].log_id.index == pair[0].log_id.index + 1),
        "the held entries are not contiguous"
    );
    Ok(())
}
