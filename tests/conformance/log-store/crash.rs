//! Section 5.5: a `native` log store in a child process killed during a
//! stream of appends. Afterwards the log holds every acknowledged entry and
//! nothing that breaks clause 5.3.4; with clause 5.3.2, each append is held
//! whole or not at all.
//!
//! Only the harness can start and kill a process, so the check comes in two
//! halves. The harness runs [`child`] in a child process with an [`Env`]
//! whose `dirs[0]` is a directory it shares with the parent, records every
//! index the child acknowledges, and kills the child at a point it picks
//! from the seed, while appends are still running. It then runs [`after_kill`]
//! in the parent with an [`Env`] on the same directory, the same seed, and
//! the highest index acknowledged, 0 when none was. Both halves derive the
//! same stream of appends from the seed.

use alloy_rt::BoxFuture;

use crate::state::{Entry, LogState};
use crate::support::{Ctx, Gen, fail, id, open, read_all};
use crate::{Env, Outcome};

/// The name of the parent half, which names its clause.
pub const CHECK: &str = "arfc0006_s5_5_native_kill_keeps_every_acknowledged_entry";

/// The number of appends in the stream.
pub const STREAM_APPENDS: u64 = 4_096;

/// The appends of the stream for `seed`: 1 to 4 entries each, from index 1,
/// with payloads of 0 to 1,024 bytes.
fn stream(seed: u64) -> Vec<Vec<Entry>> {
    let mut data = Gen::new(seed);
    let mut next = 1;
    (0..STREAM_APPENDS)
        .map(|append| {
            let count = 1 + data.below(4);
            (0..count)
                .map(|_| {
                    let len = data.below(1_025) as usize;
                    let entry = Entry {
                        log_id: id(1 + append / 512, next),
                        payload: data.bytes(len),
                    };
                    next += 1;
                    entry
                })
                .collect()
        })
        .collect()
}

/// The child half: builds a `native` log store on `dirs[0]` and makes the
/// stream's appends in order, calling `ack` with the last index of each
/// append after it returns `Ok`. Returns once the stream ends, which a
/// harness that kills the child in time never sees.
pub fn child(env: Env, mut ack: Box<dyn FnMut(u64) + Send>) -> BoxFuture<'static, Outcome> {
    Box::pin(async move {
        let store = open(&env, 0)?;
        for entries in stream(env.seed) {
            let last = entries.last().map_or(0, |entry| entry.log_id.index);
            store.append(entries).await.ctx("an append of the stream")?;
            ack(last);
        }
        Ok(())
    })
}

/// The parent half: after the kill, a new instance on the same directory
/// holds every entry up to `acknowledged`, ends at the end of one whole
/// append, holds every entry as the stream appended it, and accepts the
/// stream's next append.
pub fn after_kill(env: Env, acknowledged: u64) -> BoxFuture<'static, Outcome> {
    Box::pin(async move {
        let appends = stream(env.seed);
        let store = open(&env, 0)?;
        let state = store.state().await.ctx("state after the kill")?;
        let held = state.last.map_or(0, |last| last.index);
        ensure!(
            held >= acknowledged,
            "the log ends at {held}, before the acknowledged index {acknowledged}"
        );
        ensure!(
            state.last_purged.is_none(),
            "the log shows a purge that never happened: {state:?}"
        );

        let mut whole = 0;
        let mut next = None;
        for entries in &appends {
            if whole == held {
                next = Some(entries.clone());
                break;
            }
            whole += entries.len() as u64;
        }
        ensure!(
            whole == held,
            "the log ends at {held}, inside an append: an append was held in part"
        );
        let want: Vec<Entry> = appends
            .iter()
            .flatten()
            .take(held as usize)
            .cloned()
            .collect();
        let got = if held == 0 {
            Vec::new()
        } else {
            read_all(&*store, 1, held).await?
        };
        ensure!(
            got == want,
            "the entries held after the kill differ from those appended"
        );
        if held == 0 {
            ensure_eq!(
                state,
                LogState {
                    last_purged: None,
                    last: None
                },
                "the log state of a log with no append held"
            );
        }
        if let Some(entries) = next {
            store
                .append(entries)
                .await
                .map_err(|error| fail(format!("the next append after the kill: {error:?}")))?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_is_contiguous_from_index_1_and_fixed_by_the_seed() {
        let appends = stream(7);
        assert_eq!(appends.len() as u64, STREAM_APPENDS);
        let indexes: Vec<u64> = appends.iter().flatten().map(|e| e.log_id.index).collect();
        assert!(indexes.iter().copied().eq(1..=indexes.len() as u64));
        assert!(appends.iter().all(|a| (1..=4).contains(&a.len())));
        assert_eq!(appends, stream(7));
        assert_ne!(appends, stream(8));
    }
}
