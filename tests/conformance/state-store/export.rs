//! Section 4.1 and clauses 5.4.5 and 5.4.6: the export format, snapshots,
//! and installs.

use crate::model::{Header, Kind, StateModel, encode, kind};
use crate::state::{MAX_KEY, MAX_VALUE, StateStore};
use crate::store::expect_err;
use crate::support::{
    Ctx, Gen, apply_ok, batch, contents, delete, expect_model, export_now, export_of, fail, id,
    install, open, put,
};
use crate::{Applies, Check, Env, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_export_is_the_encoding_of_the_contents
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_equal_contents_export_identical_bytes
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_an_export_ending_early
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_a_wrong_magic
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_has_applied_other_than_0_or_1
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_an_unset_position_with_nonzero_bytes
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_a_key_length_out_of_range
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_a_value_length_out_of_range
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_keys_not_strictly_ascending
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_a_wrong_count
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_a_wrong_digest
        ),
        check!(
            "arfc-0006#4.1",
            Applies::All,
            arfc0006_s4_1_install_rejects_bytes_after_the_digest
        ),
        check!(
            "arfc-0006#5.4.5",
            Applies::All,
            arfc0006_s5_4_5_snapshot_is_unchanged_by_later_applies
        ),
        check!(
            "arfc-0006#5.4.5",
            Applies::All,
            arfc0006_s5_4_5_chunks_hold_1_to_max_chunk_bytes
        ),
        check!(
            "arfc-0006#5.4.6",
            Applies::All,
            arfc0006_s5_4_6_second_install_fails_busy
        ),
        check!(
            "arfc-0006#5.4.6",
            Applies::All,
            arfc0006_s5_4_6_finish_replaces_contents_and_position
        ),
        check!(
            "arfc-0006#5.4.6",
            Applies::All,
            arfc0006_s5_4_6_reads_see_old_contents_until_finish
        ),
        check!(
            "arfc-0006#5.4.6",
            Applies::All,
            arfc0006_s5_4_6_dropped_install_changes_nothing
        ),
        check!(
            "arfc-0006#5.4.6",
            Applies::All,
            arfc0006_s5_4_6_install_accepts_chunks_of_any_size
        ),
    ]
}

/// A model with random small contents at a random position.
fn random_model(data: &mut Gen, pairs: u64) -> StateModel {
    let mut model = StateModel::default();
    for n in 0..pairs {
        let mut key = data.key();
        key.extend_from_slice(&n.to_be_bytes()[6..]);
        let len = data.below(40) as usize;
        model.map.insert(key, data.bytes(len));
    }
    model.applied = Some(id(1 + data.below(5), 1 + data.below(1_000)));
    model
}

/// A store at place 0 holding `model`, put there through `apply`.
async fn store_holding(
    env: &Env,
    model: &StateModel,
) -> Result<std::sync::Arc<dyn StateStore>, crate::Failure> {
    let store = open(env, 0)?;
    let ops = model
        .map
        .iter()
        .map(|(key, value)| put(key, value))
        .collect();
    let at = model.applied.ctx("the model has a position")?;
    store
        .apply(crate::state::ApplyBatch { log_id: at, ops })
        .await
        .ctx("apply")?;
    Ok(store)
}

/// The records of the base contents the malformed exports start from.
fn base_records() -> Vec<(Vec<u8>, Vec<u8>)> {
    vec![
        (b"a".to_vec(), b"1".to_vec()),
        (b"b".to_vec(), Vec::new()),
        (b"c".to_vec(), b"xyz".to_vec()),
    ]
}

/// The header of the base export.
fn base_header() -> Header {
    Header::of(Some(id(2, 9)))
}

/// Installs each export into a store with contents, in one chunk and in
/// chunks of 7 bytes, and requires `Malformed` from `write` or `finish`,
/// unchanged contents, and no install left in progress.
async fn rejects(env: Env, exports: Vec<(String, Vec<u8>)>) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    apply_ok(&*store, &mut model, batch(3, vec![put(b"held", b"before")])).await?;
    for (what, export) in exports {
        for chunk in [export.len().max(1), 7] {
            let got = install(&*store, &export, chunk).await?;
            match got {
                Err(error) if kind(&error) == Kind::Malformed => {}
                other => {
                    return Err(fail(format!(
                        "installing {what} in chunks of {chunk}: got {other:?}, expected Malformed"
                    )));
                }
            }
            expect_model(&*store, &model, &format!("after installing {what}")).await?;
            let next = store
                .begin_install()
                .await
                .ctx(&format!("begin_install after {what} failed"))?;
            drop(next);
        }
    }
    Ok(())
}

/// A snapshot's export is the encoding of section 4.1 of the contents and
/// position, with or without a position.
async fn arfc0006_s4_1_export_is_the_encoding_of_the_contents(env: Env) -> Outcome {
    let mut data = Gen::new(env.seed);
    let model = random_model(&mut data, 50);
    let store = store_holding(&env, &model).await?;
    expect_model(&*store, &model, "a store with random contents").await?;
    let mut unset = random_model(&mut data, 5);
    unset.applied = None;
    install(&*store, &unset.export(), 1_000)
        .await?
        .ctx("installing an export without a position")?;
    expect_model(&*store, &unset, "a store without a position").await
}

/// Two stores whose equal contents and positions came from different
/// histories export identical bytes.
async fn arfc0006_s4_1_equal_contents_export_identical_bytes(env: Env) -> Outcome {
    let direct = open(&env, 0)?;
    let winding = open(&env, 1)?;
    let mut model = StateModel::default();
    apply_ok(
        &*direct,
        &mut model,
        batch(9, vec![put(b"a", b"1"), put(b"c", b"3")]),
    )
    .await?;
    let mut other = StateModel::default();
    for (index, ops) in [
        (
            2,
            vec![put(b"c", b"old"), put(b"b", b"gone"), put(b"z", b"z")],
        ),
        (5, vec![delete(b"b"), put(b"a", b"1"), delete(b"z")]),
        (9, vec![put(b"c", b"3")]),
    ] {
        apply_ok(&*winding, &mut other, batch(index, ops)).await?;
    }
    ensure!(model == other, "the suite built different contents");
    let one = export_now(&*direct).await?;
    let two = export_now(&*winding).await?;
    ensure!(one == two, "equal contents exported different bytes");
    ensure!(
        one == model.export(),
        "the export differs from the encoding of section 4.1"
    );
    Ok(())
}

/// An install of an export that ends early fails with `Malformed`.
async fn arfc0006_s4_1_install_rejects_an_export_ending_early(env: Env) -> Outcome {
    let whole = encode(&base_header(), &base_records(), 3);
    let cuts = [
        1,
        7,
        8,
        9,
        40,
        41,
        43,
        45,
        47,
        whole.len() / 2,
        whole.len() - 33,
        whole.len() - 1,
    ];
    let exports = cuts
        .iter()
        .map(|&cut| (format!("the first {cut} bytes"), whole[..cut].to_vec()))
        .collect();
    rejects(env, exports).await
}

/// An install whose `magic` differs fails with `Malformed`.
async fn arfc0006_s4_1_install_rejects_a_wrong_magic(env: Env) -> Outcome {
    let exports = [*b"ALSTATE2", *b"alstate1", [0; 8], *b"ALSTATE0"]
        .into_iter()
        .map(|magic| {
            let header = Header {
                magic,
                ..base_header()
            };
            (
                format!("magic {magic:?}"),
                encode(&header, &base_records(), 3),
            )
        })
        .collect();
    rejects(env, exports).await
}

/// An install whose `has_applied` is neither 0 nor 1 fails with `Malformed`.
async fn arfc0006_s4_1_install_rejects_has_applied_other_than_0_or_1(env: Env) -> Outcome {
    let exports = [2, 0x80, 0xFF]
        .into_iter()
        .map(|has_applied| {
            let header = Header {
                has_applied,
                ..base_header()
            };
            (
                format!("has_applied {has_applied}"),
                encode(&header, &base_records(), 3),
            )
        })
        .collect();
    rejects(env, exports).await
}

/// An install with `has_applied` 0 and any of bytes 9 to 40 set fails with
/// `Malformed`.
async fn arfc0006_s4_1_install_rejects_an_unset_position_with_nonzero_bytes(env: Env) -> Outcome {
    let unset = Header::of(None);
    let mut leader = [0; 16];
    leader[15] = 1;
    let headers = [
        (
            "term 1",
            Header {
                term: 1,
                ..unset.clone()
            },
        ),
        (
            "a leader byte",
            Header {
                leader,
                ..unset.clone()
            },
        ),
        (
            "index 1",
            Header {
                index: 1,
                ..unset.clone()
            },
        ),
        (
            "index 2^63",
            Header {
                index: 1 << 63,
                ..unset
            },
        ),
    ];
    let exports = headers
        .into_iter()
        .map(|(what, header)| {
            (
                format!("has_applied 0 with {what}"),
                encode(&header, &base_records(), 3),
            )
        })
        .collect();
    rejects(env, exports).await
}

/// An install with a `key_len` of 0 or above 4,096 fails with `Malformed`.
async fn arfc0006_s4_1_install_rejects_a_key_length_out_of_range(env: Env) -> Outcome {
    let mut empty = base_records();
    empty.insert(0, (Vec::new(), b"v".to_vec()));
    let mut long = base_records();
    long.push((vec![b'd'; MAX_KEY + 1], b"v".to_vec()));
    rejects(
        env,
        vec![
            (
                "a record with an empty key".to_owned(),
                encode(&base_header(), &empty, 4),
            ),
            (
                "a record with a 4,097-byte key".to_owned(),
                encode(&base_header(), &long, 4),
            ),
        ],
    )
    .await
}

/// An install with a `value_len` above 2,097,152 fails with `Malformed`.
async fn arfc0006_s4_1_install_rejects_a_value_length_out_of_range(env: Env) -> Outcome {
    let mut records = base_records();
    records.push((b"d".to_vec(), vec![9; MAX_VALUE + 1]));
    rejects(
        env,
        vec![(
            "a record with a 2,097,153-byte value".to_owned(),
            encode(&base_header(), &records, 4),
        )],
    )
    .await
}

/// An install whose keys are not in strictly ascending bytewise order fails
/// with `Malformed`.
async fn arfc0006_s4_1_install_rejects_keys_not_strictly_ascending(env: Env) -> Outcome {
    let mut swapped = base_records();
    swapped.swap(0, 1);
    let mut repeated = base_records();
    repeated.insert(1, (b"a".to_vec(), b"again".to_vec()));
    let prefix_after = vec![(b"ab".to_vec(), Vec::new()), (b"a".to_vec(), Vec::new())];
    let signed = vec![(vec![0x80], Vec::new()), (vec![0x7F], Vec::new())];
    rejects(
        env,
        vec![
            (
                "descending keys".to_owned(),
                encode(&base_header(), &swapped, 3),
            ),
            (
                "a repeated key".to_owned(),
                encode(&base_header(), &repeated, 4),
            ),
            (
                "a key after its extension".to_owned(),
                encode(&base_header(), &prefix_after, 2),
            ),
            (
                "0x80 before 0x7F".to_owned(),
                encode(&base_header(), &signed, 2),
            ),
        ],
    )
    .await
}

/// An install whose `count` differs from the records read fails with
/// `Malformed`.
async fn arfc0006_s4_1_install_rejects_a_wrong_count(env: Env) -> Outcome {
    let exports = [0, 2, 4, u64::MAX]
        .into_iter()
        .map(|count| {
            (
                format!("count {count} for 3 records"),
                encode(&base_header(), &base_records(), count),
            )
        })
        .collect();
    rejects(env, exports).await
}

/// An install whose `digest` does not match fails with `Malformed`.
async fn arfc0006_s4_1_install_rejects_a_wrong_digest(env: Env) -> Outcome {
    let whole = encode(&base_header(), &base_records(), 3);
    let mut digest = whole.clone();
    let last = digest.len() - 1;
    digest[last] ^= 1;
    let mut body = whole.clone();
    body[45] ^= 0x20;
    let mut zeroed = whole;
    let start = zeroed.len() - 32;
    zeroed[start..].fill(0);
    rejects(
        env,
        vec![
            ("a flipped digest bit".to_owned(), digest),
            ("a flipped key byte".to_owned(), body),
            ("a zero digest".to_owned(), zeroed),
        ],
    )
    .await
}

/// An install with any byte after `digest` fails with `Malformed`.
async fn arfc0006_s4_1_install_rejects_bytes_after_the_digest(env: Env) -> Outcome {
    let whole = encode(&base_header(), &base_records(), 3);
    let mut one = whole.clone();
    one.push(0);
    let mut twice = whole.clone();
    twice.extend_from_slice(&whole);
    rejects(
        env,
        vec![
            ("one byte after the digest".to_owned(), one),
            ("a second export after the first".to_owned(), twice),
        ],
    )
    .await
}

/// A snapshot exports the store as of the call, whatever is applied after
/// it, even between its chunks.
async fn arfc0006_s5_4_5_snapshot_is_unchanged_by_later_applies(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    let ops = (0..3u8)
        .map(|n| put(&[b'k', n], &vec![n; MAX_VALUE * 3 / 4]))
        .collect();
    apply_ok(&*store, &mut model, batch(1, ops)).await?;
    let before = model.clone();
    let snapshot = store.snapshot().await.ctx("snapshot")?;
    let first = snapshot
        .next_chunk()
        .await
        .ctx("the first chunk")?
        .ctx("a first chunk of a non-empty export")?;
    apply_ok(
        &*store,
        &mut model,
        batch(
            2,
            vec![put(b"k\x00", b"new"), delete(b"k\x01"), put(b"z", b"z")],
        ),
    )
    .await?;
    ensure_eq!(
        snapshot.applied(),
        before.applied,
        "the snapshot's position"
    );
    let mut export = first;
    export.extend(export_of(&*snapshot).await?);
    ensure!(
        export == before.export(),
        "the snapshot's export shows applies made after it"
    );
    expect_model(&*store, &model, "a snapshot taken after the apply").await
}

/// Every chunk holds 1 to `MAX_CHUNK` bytes and the chunks join into the
/// export.
async fn arfc0006_s5_4_5_chunks_hold_1_to_max_chunk_bytes(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    let ops = (0..5u8)
        .map(|n| put(&[n], &vec![n; MAX_VALUE - usize::from(n)]))
        .collect();
    apply_ok(&*store, &mut model, batch(1, ops)).await?;
    expect_model(&*store, &model, "a store with a 10 MiB export").await
}

/// `begin_install` fails with `Busy` while another install is in progress,
/// and succeeds once it is dropped or finished.
async fn arfc0006_s5_4_6_second_install_fails_busy(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let first = store.begin_install().await.ctx("the first begin_install")?;
    expect_err(
        store.begin_install().await,
        Kind::Busy,
        "a second begin_install",
    )?;
    drop(first);
    let second = store
        .begin_install()
        .await
        .ctx("begin_install after a drop")?;
    expect_err(
        store.begin_install().await,
        Kind::Busy,
        "begin_install during the second",
    )?;
    second
        .write(StateModel::default().export())
        .await
        .ctx("write")?;
    second.finish().await.ctx("finish")?;
    let third = store
        .begin_install()
        .await
        .ctx("begin_install after a finish")?;
    drop(third);
    Ok(())
}

/// `finish` replaces the whole contents and position with the export's, even
/// a lower position or none, and returns the export's position.
async fn arfc0006_s5_4_6_finish_replaces_contents_and_position(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    apply_ok(
        &*store,
        &mut model,
        batch(
            50,
            vec![put(b"a", b"old"), put(b"only-old", b"x"), put(b"m", b"m")],
        ),
    )
    .await?;
    let lower = StateModel::with(Some(id(7, 12)), &[(b"a", b"new"), (b"only-new", b"")]);
    for (what, incoming) in [
        ("an export at a lower position", lower),
        ("an empty export", StateModel::default()),
    ] {
        let got = install(&*store, &incoming.export(), 3).await?.ctx(what)?;
        ensure_eq!(
            got,
            incoming.applied,
            "the position finish returns for {what}"
        );
        expect_model(&*store, &incoming, &format!("after installing {what}")).await?;
    }
    Ok(())
}

/// Until `finish` returns, reads see the contents from before the install.
async fn arfc0006_s5_4_6_reads_see_old_contents_until_finish(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    apply_ok(
        &*store,
        &mut model,
        batch(4, vec![put(b"a", b"old"), put(b"b", b"old")]),
    )
    .await?;
    let incoming = StateModel::with(Some(id(2, 30)), &[(b"a", b"new"), (b"c", b"new")]);
    let pending = store.begin_install().await.ctx("begin_install")?;
    for piece in incoming.export().chunks(10) {
        pending.write(piece.to_vec()).await.ctx("write")?;
        ensure_eq!(
            store.get(b"a").await.ctx("get")?,
            Some(b"old".to_vec()),
            "get during the install"
        );
        ensure_eq!(
            store.get(b"c").await.ctx("get")?,
            None,
            "get of a new key during the install"
        );
    }
    ensure_eq!(
        store.applied().await.ctx("applied")?,
        model.applied,
        "the position before finish"
    );
    ensure!(
        contents(&*store).await? == model.map,
        "scan before finish shows the install"
    );
    let snapshot = store.snapshot().await.ctx("snapshot during the install")?;
    ensure!(
        export_of(&*snapshot).await? == model.export(),
        "a snapshot before finish shows the install"
    );
    pending.finish().await.ctx("finish")?;
    expect_model(&*store, &incoming, "after finish").await
}

/// An install dropped before `finish`, after part or all of the export was
/// written, leaves the store unchanged.
async fn arfc0006_s5_4_6_dropped_install_changes_nothing(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut model = StateModel::default();
    apply_ok(&*store, &mut model, batch(4, vec![put(b"a", b"old")])).await?;
    let incoming = StateModel::with(Some(id(2, 30)), &[(b"b", b"new")]);
    let export = incoming.export();
    for written in [0, export.len() / 2, export.len()] {
        let pending = store.begin_install().await.ctx("begin_install")?;
        if written > 0 {
            pending
                .write(export[..written].to_vec())
                .await
                .ctx("write")?;
        }
        drop(pending);
        expect_model(
            &*store,
            &model,
            &format!("after dropping an install of {written} bytes"),
        )
        .await?;
    }
    Ok(())
}

/// An install accepts the export in chunks of any size.
async fn arfc0006_s5_4_6_install_accepts_chunks_of_any_size(env: Env) -> Outcome {
    let store = open(&env, 0)?;
    let mut data = Gen::new(env.seed);
    let mut sizes = vec![1, 2, 41, 4_096, usize::MAX];
    sizes.push(1 + data.below(100) as usize);
    for size in sizes {
        let incoming = random_model(&mut data, 20);
        let export = incoming.export();
        let got = install(&*store, &export, size.min(export.len()))
            .await?
            .ctx("install")?;
        ensure_eq!(got, incoming.applied, "finish after chunks of {size}");
        expect_model(&*store, &incoming, &format!("after chunks of {size}")).await?;
    }
    Ok(())
}
