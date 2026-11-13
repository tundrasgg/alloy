//! Suite `disk-io` (ARFC 0005 section 5): the `DiskIo` seam, `DiskFile`,
//! `RtPath`, and `IoBuf`, clauses 2.1, 2.4, 3.1.3, 3.1.10, 3.1.11, and
//! 3.1.13. Durability across a crash (clause 3.1.12) needs the simulated
//! world and is the simulator suite's.

use std::sync::Arc;

use alloy_rt::{
    Access, Create, DiskError, DiskFile, DiskIo, DiskIoSeam, EntryKind, IoBuf, Metadata,
    OpenOptions, ReadOp, RtPath, SyncKind, WriteOp,
};

use crate::support::{
    Ctx, Gen, PATIENCE, SETTLE, fail, fresh, name, poll_once, skip, sleep, within,
};
use crate::{Applies, Check, Env, Failure, Implementation, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_the_root_is_a_directory
        ),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_open_creating_with_read_access_is_invalid_input
        ),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_open_under_a_missing_parent_is_not_found
        ),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_open_of_a_directory_is_is_directory
        ),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_exclusive_on_a_regular_file_is_invalid_input
        ),
        check!("arfc-0005#2.4", Applies::All, arfc0005_s2_4_create_modes),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_an_instance_holds_at_most_65536_open_handles
        ),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_list_returns_every_entry_sorted_bytewise
        ),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_list_above_1048576_entries_is_too_many_entries
        ),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_create_dir_rules
        ),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_remove_rules
        ),
        check!(
            "arfc-0005#3.1.10",
            Applies::All,
            arfc0005_s3_1_10_rename_replaces_a_file_and_refuses_a_directory
        ),
        check!(
            "arfc-0005#2.4",
            Applies::All,
            arfc0005_s2_4_metadata_and_info_report_kind_and_length
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_a_batch_above_256_fails_whole
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_an_unaligned_direct_operation_fails_the_whole_batch
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_a_write_on_a_read_handle_fails_the_whole_batch
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_overlapping_writes_fail_the_whole_batch
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_an_operation_past_the_end_of_a_block_device_fails_the_whole_batch
        ),
        check!(
            "arfc-0005#2.4",
            Applies::All,
            arfc0005_s2_4_results_come_in_order_with_their_buffers
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_a_read_is_short_only_at_end_of_file
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_a_write_extends_a_file_filling_the_gap_with_zeros
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_a_completed_write_is_seen_on_every_handle
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_sync_fixes_the_outcome_of_a_dropped_write
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_truncate_sets_the_length_of_a_regular_file
        ),
        check!(
            "arfc-0005#3.1.11",
            Applies::All,
            arfc0005_s3_1_11_block_size_is_512_or_4096
        ),
        check!(
            "arfc-0005#2.4",
            Applies::All,
            arfc0005_s2_4_exclusive_block_device_is_busy_while_held
        ),
        check!(
            "arfc-0005#3.1.3",
            Applies::All,
            arfc0005_s3_1_3_disk_methods_may_be_called_concurrently
        ),
        check!(
            "arfc-0005#3.1.3",
            Applies::All,
            arfc0005_s3_1_3_hostile_arguments_return_without_panic
        ),
        check!(
            "arfc-0005#3.1.3",
            Applies::Only(Implementation::Mem),
            arfc0005_s3_1_3_mem_never_returns_an_os_error
        ),
    ]
}

/// Open handles per `disk-io` instance (clause 3.1.10).
const HANDLES: usize = 65_536;

/// Entries `list` returns at most (clause 3.1.10).
const ENTRIES: usize = 1_048_576;

fn options(access: Access, create: Create) -> OpenOptions {
    OpenOptions {
        access,
        create,
        direct: false,
        exclusive: false,
    }
}

fn path(env: &Env, file: &str) -> Result<RtPath, Failure> {
    env.scratch
        .join(file)
        .ctx("a path in the scratch directory")
}

async fn open(
    env: &Env,
    disk: &dyn DiskIo,
    at: &RtPath,
    how: OpenOptions,
) -> Result<Box<dyn DiskFile>, Failure> {
    within(env, PATIENCE, disk.open(at, how))
        .await
        .ctx("open")?
        .ctx(&format!("open {} with {how:?}", at.as_str()))
}

async fn open_result(
    env: &Env,
    disk: &dyn DiskIo,
    at: &RtPath,
    how: OpenOptions,
) -> Result<Result<Box<dyn DiskFile>, DiskError>, Failure> {
    within(env, PATIENCE, disk.open(at, how)).await.ctx("open")
}

/// Creates the file `at` holding `bytes`.
async fn create_file(env: &Env, at: &RtPath, bytes: &[u8]) -> Result<Box<dyn DiskFile>, Failure> {
    let disk = env.runtime.disk_io().clone();
    let file = open(
        env,
        disk.as_ref(),
        at,
        options(Access::ReadWrite, Create::New),
    )
    .await?;
    if !bytes.is_empty() {
        write(env, file.as_ref(), 0, bytes).await?;
    }
    Ok(file)
}

async fn write(env: &Env, file: &dyn DiskFile, offset: u64, bytes: &[u8]) -> Result<(), Failure> {
    let op = WriteOp {
        offset,
        buf: IoBuf::from_slice(bytes).ctx("IoBuf::from_slice")?,
    };
    let done = within(env, PATIENCE, file.write_at(vec![op]))
        .await
        .ctx("write_at")?;
    ensure_eq!(done.len(), 1, "results of a one-write batch");
    done[0].result.clone().ctx("write_at")
}

/// Reads `len` bytes at `offset`; the result is shorter only at end of file.
async fn read(env: &Env, file: &dyn DiskFile, offset: u64, len: usize) -> Result<Vec<u8>, Failure> {
    let op = ReadOp {
        offset,
        buf: IoBuf::zeroed(len).ctx("IoBuf::zeroed")?,
    };
    let done = within(env, PATIENCE, file.read_at(vec![op]))
        .await
        .ctx("read_at")?;
    ensure_eq!(done.len(), 1, "results of a one-read batch");
    let count = done[0].result.clone().ctx("read_at")?;
    ensure!(count <= len, "read_at of {len} bytes returned {count}");
    ensure_eq!(done[0].buf.len(), len, "length of the returned buffer");
    Ok(done[0].buf[..count].to_vec())
}

async fn metadata(env: &Env, at: &RtPath) -> Result<Result<Metadata, DiskError>, Failure> {
    within(env, PATIENCE, env.runtime.disk_io().metadata(at))
        .await
        .ctx("metadata")
}

async fn info_len(env: &Env, file: &dyn DiskFile) -> Result<u64, Failure> {
    Ok(within(env, PATIENCE, file.info())
        .await
        .ctx("info")?
        .ctx("info")?
        .len)
}

/// The block device the harness provides, or a skip.
fn block_device(env: &Env) -> Result<RtPath, Failure> {
    env.block_device
        .clone()
        .ok_or_else(|| Failure::Skipped("the harness provides no block device".into()))
}

/// The root exists and is a directory.
async fn arfc0005_s3_1_10_the_root_is_a_directory(env: Env) -> Outcome {
    let root = metadata(&env, &RtPath::root())
        .await?
        .ctx("metadata of /")?;
    ensure_eq!(root.kind, EntryKind::Dir, "kind of /");
    Ok(())
}

/// `open` with `Create::IfMissing` or `Create::New` and `Access::Read`
/// returns `InvalidInput`, and creates nothing.
async fn arfc0005_s3_1_10_open_creating_with_read_access_is_invalid_input(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let at = path(&env, "read-create")?;
    for create in [Create::IfMissing, Create::New] {
        let result = open_result(&env, disk.as_ref(), &at, options(Access::Read, create)).await?;
        ensure!(
            matches!(result, Err(DiskError::InvalidInput(_))),
            "open with Read and {create:?}: {:?}",
            result.err()
        );
    }
    ensure_eq!(
        metadata(&env, &at).await?,
        Err(DiskError::NotFound),
        "metadata after refused opens"
    );
    Ok(())
}

/// `open` under a missing parent returns `NotFound`.
async fn arfc0005_s3_1_10_open_under_a_missing_parent_is_not_found(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let at = path(&env, "missing")?.join("file").ctx("join")?;
    for create in [Create::Never, Create::IfMissing, Create::New] {
        let result =
            open_result(&env, disk.as_ref(), &at, options(Access::ReadWrite, create)).await?;
        ensure_eq!(
            result.err(),
            Some(DiskError::NotFound),
            "open under a missing parent with {create:?}"
        );
    }
    Ok(())
}

/// `open` of a directory returns `IsDirectory`.
async fn arfc0005_s3_1_10_open_of_a_directory_is_is_directory(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    for (access, create) in [
        (Access::Read, Create::Never),
        (Access::ReadWrite, Create::Never),
        (Access::ReadWrite, Create::IfMissing),
    ] {
        let result =
            open_result(&env, disk.as_ref(), &env.scratch, options(access, create)).await?;
        ensure_eq!(
            result.err(),
            Some(DiskError::IsDirectory),
            "open of a directory with {access:?}, {create:?}"
        );
    }
    Ok(())
}

/// `exclusive` on anything but a block device returns `InvalidInput`.
async fn arfc0005_s3_1_10_exclusive_on_a_regular_file_is_invalid_input(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let at = path(&env, "plain")?;
    drop(create_file(&env, &at, b"x").await?);
    let how = OpenOptions {
        exclusive: true,
        ..options(Access::ReadWrite, Create::Never)
    };
    let result = open_result(&env, disk.as_ref(), &at, how).await?;
    ensure!(
        matches!(result, Err(DiskError::InvalidInput(_))),
        "exclusive open of a regular file: {:?}",
        result.err()
    );
    Ok(())
}

/// `Create::Never` fails with `NotFound` when missing, `Create::New` with
/// `AlreadyExists` when present, and `Create::IfMissing` opens either way.
async fn arfc0005_s2_4_create_modes(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let at = path(&env, "modes")?;
    let never = open_result(
        &env,
        disk.as_ref(),
        &at,
        options(Access::ReadWrite, Create::Never),
    )
    .await?;
    ensure_eq!(
        never.err(),
        Some(DiskError::NotFound),
        "Create::Never on a missing file"
    );
    let created = open(
        &env,
        disk.as_ref(),
        &at,
        options(Access::ReadWrite, Create::New),
    )
    .await?;
    write(&env, created.as_ref(), 0, b"kept").await?;
    drop(created);
    let again = open_result(
        &env,
        disk.as_ref(),
        &at,
        options(Access::ReadWrite, Create::New),
    )
    .await?;
    ensure_eq!(
        again.err(),
        Some(DiskError::AlreadyExists),
        "Create::New on an existing file"
    );
    let reopened = open(
        &env,
        disk.as_ref(),
        &at,
        options(Access::ReadWrite, Create::IfMissing),
    )
    .await?;
    ensure_eq!(
        read(&env, reopened.as_ref(), 0, 16).await?,
        b"kept".to_vec(),
        "content after IfMissing"
    );
    let other = path(&env, "if-missing")?;
    let made = open(
        &env,
        disk.as_ref(),
        &other,
        options(Access::ReadWrite, Create::IfMissing),
    )
    .await?;
    ensure_eq!(
        info_len(&env, made.as_ref()).await?,
        0,
        "length of a file IfMissing created"
    );
    let read_only = open(
        &env,
        disk.as_ref(),
        &at,
        options(Access::Read, Create::Never),
    )
    .await?;
    ensure_eq!(
        read(&env, read_only.as_ref(), 0, 16).await?,
        b"kept".to_vec(),
        "content through a read handle"
    );
    Ok(())
}

/// An instance holds at most 65,536 open handles, beyond which `open`
/// returns `TooManyOpen`. `os` may meet a lower platform limit first, and
/// must then still report `TooManyOpen`.
async fn arfc0005_s3_1_10_an_instance_holds_at_most_65536_open_handles(env: Env) -> Outcome {
    let at = path(&env, "many-handles")?;
    drop(create_file(&env, &at, b"h").await?);
    let disk: Arc<dyn DiskIo> = fresh::<DiskIoSeam>(&env, &env.config.disk_io)?;
    let mut handles = Vec::with_capacity(HANDLES);
    let refused = loop {
        if handles.len() > HANDLES {
            return Err(fail("an instance held more than 65536 open handles"));
        }
        match open_result(
            &env,
            disk.as_ref(),
            &at,
            options(Access::Read, Create::Never),
        )
        .await?
        {
            Ok(handle) => handles.push(handle),
            Err(error) => break error,
        }
    };
    ensure_eq!(
        refused,
        DiskError::TooManyOpen,
        "open beyond the handle limit"
    );
    if env.implementation == Implementation::Mem {
        ensure_eq!(handles.len(), HANDLES, "handles a fresh mem instance held");
    }
    Ok(())
}

/// `list` returns every entry, with its kind, sorted by name ascending
/// bytewise, and never `.` or `..`.
async fn arfc0005_s3_1_10_list_returns_every_entry_sorted_bytewise(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let dir = path(&env, "listing")?;
    within(&env, PATIENCE, disk.create_dir(&dir))
        .await
        .ctx("create_dir")?
        .ctx("create_dir")?;
    let files = ["b", "a", "B", "a0", "~", "Z", "\u{e9}", "a.b", "-", " x"];
    let dirs = ["sub", "A", "\u{4e2d}"];
    for file in files {
        let at = dir.join(file).ctx("join")?;
        drop(create_file(&env, &at, b"").await?);
    }
    for sub in dirs {
        let at = dir.join(sub).ctx("join")?;
        within(&env, PATIENCE, disk.create_dir(&at))
            .await
            .ctx("create_dir")?
            .ctx("create_dir")?;
    }
    let listed = within(&env, PATIENCE, disk.list(&dir))
        .await
        .ctx("list")?
        .ctx("list")?;
    let mut expected: Vec<(String, EntryKind)> = files
        .iter()
        .map(|file| (file.to_string(), EntryKind::File))
        .chain(dirs.iter().map(|sub| (sub.to_string(), EntryKind::Dir)))
        .collect();
    expected.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let got: Vec<(String, EntryKind)> = listed
        .into_iter()
        .map(|entry| (entry.name, entry.kind))
        .collect();
    ensure_eq!(got, expected, "entries of a directory");
    Ok(())
}

/// `list` of a directory with more than 1,048,576 entries returns
/// `TooManyEntries`; with exactly 1,048,576 it lists them all.
async fn arfc0005_s3_1_10_list_above_1048576_entries_is_too_many_entries(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let dir = path(&env, "crowd")?;
    within(&env, PATIENCE, disk.create_dir(&dir))
        .await
        .ctx("create_dir")?
        .ctx("create_dir")?;
    for index in 0..=ENTRIES {
        let at = dir.join(&format!("{index:07}")).ctx("join")?;
        within(&env, PATIENCE, disk.create_dir(&at))
            .await
            .ctx("create_dir")?
            .ctx(&format!("create_dir of entry {index}"))?;
    }
    let crowded = within(&env, PATIENCE, disk.list(&dir)).await.ctx("list")?;
    ensure_eq!(
        crowded.err(),
        Some(DiskError::TooManyEntries),
        "list of 1048577 entries"
    );
    let last = dir.join(&format!("{ENTRIES:07}")).ctx("join")?;
    within(&env, PATIENCE, disk.remove(&last))
        .await
        .ctx("remove")?
        .ctx("remove")?;
    let full = within(&env, PATIENCE, disk.list(&dir))
        .await
        .ctx("list")?
        .ctx("list of exactly 1048576 entries")?;
    ensure_eq!(full.len(), ENTRIES, "entries listed");
    Ok(())
}

/// `create_dir` needs an existing parent and returns `AlreadyExists` for an
/// existing path.
async fn arfc0005_s3_1_10_create_dir_rules(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let orphan = path(&env, "no-parent")?.join("child").ctx("join")?;
    let result = within(&env, PATIENCE, disk.create_dir(&orphan))
        .await
        .ctx("create_dir")?;
    ensure_eq!(
        result,
        Err(DiskError::NotFound),
        "create_dir under a missing parent"
    );
    let dir = path(&env, "made")?;
    within(&env, PATIENCE, disk.create_dir(&dir))
        .await
        .ctx("create_dir")?
        .ctx("create_dir")?;
    ensure_eq!(
        metadata(&env, &dir).await?.map(|m| m.kind),
        Ok(EntryKind::Dir),
        "kind of a created directory"
    );
    let again = within(&env, PATIENCE, disk.create_dir(&dir))
        .await
        .ctx("create_dir")?;
    ensure_eq!(
        again,
        Err(DiskError::AlreadyExists),
        "create_dir of an existing directory"
    );
    let file = path(&env, "a-file")?;
    drop(create_file(&env, &file, b"f").await?);
    let over_file = within(&env, PATIENCE, disk.create_dir(&file))
        .await
        .ctx("create_dir")?;
    ensure_eq!(
        over_file,
        Err(DiskError::AlreadyExists),
        "create_dir of an existing file"
    );
    Ok(())
}

/// `remove` removes a file or an empty directory, and returns `NotEmpty` for
/// a directory with entries.
async fn arfc0005_s3_1_10_remove_rules(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let file = path(&env, "doomed")?;
    drop(create_file(&env, &file, b"bye").await?);
    within(&env, PATIENCE, disk.remove(&file))
        .await
        .ctx("remove")?
        .ctx("remove of a file")?;
    ensure_eq!(
        metadata(&env, &file).await?,
        Err(DiskError::NotFound),
        "metadata of a removed file"
    );

    let dir = path(&env, "full")?;
    within(&env, PATIENCE, disk.create_dir(&dir))
        .await
        .ctx("create_dir")?
        .ctx("create_dir")?;
    let inner = dir.join("inner").ctx("join")?;
    drop(create_file(&env, &inner, b"").await?);
    let refused = within(&env, PATIENCE, disk.remove(&dir))
        .await
        .ctx("remove")?;
    ensure_eq!(
        refused,
        Err(DiskError::NotEmpty),
        "remove of a directory with an entry"
    );
    ensure_eq!(
        metadata(&env, &inner).await?.map(|m| m.kind),
        Ok(EntryKind::File),
        "the entry after a refused remove"
    );
    within(&env, PATIENCE, disk.remove(&inner))
        .await
        .ctx("remove")?
        .ctx("remove of the entry")?;
    within(&env, PATIENCE, disk.remove(&dir))
        .await
        .ctx("remove")?
        .ctx("remove of an empty directory")?;
    ensure_eq!(
        metadata(&env, &dir).await?,
        Err(DiskError::NotFound),
        "metadata of a removed directory"
    );
    Ok(())
}

/// `rename` replaces a file at `to`, and returns `IsDirectory` when `to` is a
/// directory, leaving the source in place.
async fn arfc0005_s3_1_10_rename_replaces_a_file_and_refuses_a_directory(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let from = path(&env, "new")?;
    let to = path(&env, "old")?;
    drop(create_file(&env, &from, b"new bytes").await?);
    drop(create_file(&env, &to, b"old").await?);
    within(&env, PATIENCE, disk.rename(&from, &to))
        .await
        .ctx("rename")?
        .ctx("rename onto a file")?;
    ensure_eq!(
        metadata(&env, &from).await?,
        Err(DiskError::NotFound),
        "the source after rename"
    );
    let file = open(
        &env,
        disk.as_ref(),
        &to,
        options(Access::Read, Create::Never),
    )
    .await?;
    ensure_eq!(
        read(&env, file.as_ref(), 0, 64).await?,
        b"new bytes".to_vec(),
        "the target after rename"
    );

    let dir = path(&env, "a-dir")?;
    within(&env, PATIENCE, disk.create_dir(&dir))
        .await
        .ctx("create_dir")?
        .ctx("create_dir")?;
    let refused = within(&env, PATIENCE, disk.rename(&to, &dir))
        .await
        .ctx("rename")?;
    ensure_eq!(
        refused,
        Err(DiskError::IsDirectory),
        "rename onto a directory"
    );
    ensure_eq!(
        metadata(&env, &to).await?.map(|m| m.len),
        Ok(9),
        "the source after a refused rename"
    );
    within(&env, PATIENCE, disk.sync_dir(&env.scratch))
        .await
        .ctx("sync_dir")?
        .ctx("sync_dir")?;
    Ok(())
}

/// `metadata` and `info` report the kind and length of a file, and
/// `metadata` the kind of a directory.
async fn arfc0005_s2_4_metadata_and_info_report_kind_and_length(env: Env) -> Outcome {
    let at = path(&env, "sized")?;
    let file = create_file(&env, &at, &[3u8; 1_234]).await?;
    ensure_eq!(
        metadata(&env, &at).await?,
        Ok(Metadata {
            kind: EntryKind::File,
            len: 1_234
        }),
        "metadata of a 1234-byte file"
    );
    let info = within(&env, PATIENCE, file.info())
        .await
        .ctx("info")?
        .ctx("info")?;
    ensure_eq!(
        (info.kind, info.len),
        (EntryKind::File, 1_234),
        "info of a 1234-byte file"
    );
    ensure_eq!(
        metadata(&env, &env.scratch).await?.map(|m| m.kind),
        Ok(EntryKind::Dir),
        "kind of the scratch directory"
    );
    Ok(())
}

/// Every result of a failed batch is the error, and each holds the buffer of
/// its operation, in order.
fn all_failed<T>(
    results: &[(Result<T, DiskError>, usize)],
    expected: &dyn Fn(&DiskError) -> bool,
    lengths: &[usize],
    what: &str,
) -> Outcome {
    ensure_eq!(results.len(), lengths.len(), "results of {what}");
    for (index, ((result, len), want)) in results.iter().zip(lengths).enumerate() {
        ensure!(
            matches!(result, Err(error) if expected(error)),
            "result {index} of {what} is {:?}",
            result.as_ref().err()
        );
        ensure_eq!(len, want, "buffer length of result {index} of {what}");
    }
    Ok(())
}

/// A batch of more than 256 operations fails whole with `BatchTooLarge`,
/// performs nothing, and returns every buffer; a batch of 256 runs.
async fn arfc0005_s3_1_11_a_batch_above_256_fails_whole(env: Env) -> Outcome {
    let at = path(&env, "batch")?;
    let file = create_file(&env, &at, &[]).await?;
    let lengths: Vec<usize> = (1..=257).collect();
    let mut offset = 0;
    let mut writes = Vec::new();
    for &len in &lengths {
        writes.push(WriteOp {
            offset,
            buf: IoBuf::from_slice(&vec![1u8; len]).ctx("IoBuf")?,
        });
        offset += len as u64;
    }
    let done = within(&env, PATIENCE, file.write_at(writes))
        .await
        .ctx("write_at")?;
    let results: Vec<_> = done.into_iter().map(|d| (d.result, d.buf.len())).collect();
    all_failed(
        &results,
        &|e| *e == DiskError::BatchTooLarge,
        &lengths,
        "257 writes",
    )?;
    ensure_eq!(
        info_len(&env, file.as_ref()).await?,
        0,
        "length after a refused batch"
    );

    let reads: Vec<ReadOp> = lengths
        .iter()
        .map(|&len| IoBuf::zeroed(len).map(|buf| ReadOp { offset: 0, buf }))
        .collect::<Result<_, _>>()
        .ctx("IoBuf")?;
    let done = within(&env, PATIENCE, file.read_at(reads))
        .await
        .ctx("read_at")?;
    let results: Vec<_> = done.into_iter().map(|d| (d.result, d.buf.len())).collect();
    all_failed(
        &results,
        &|e| *e == DiskError::BatchTooLarge,
        &lengths,
        "257 reads",
    )?;

    let writes: Vec<WriteOp> = (0..256u64)
        .map(|index| IoBuf::from_slice(&[index as u8]).map(|buf| WriteOp { offset: index, buf }))
        .collect::<Result<_, _>>()
        .ctx("IoBuf")?;
    let done = within(&env, PATIENCE, file.write_at(writes))
        .await
        .ctx("write_at")?;
    ensure_eq!(done.len(), 256, "results of 256 writes");
    for (index, write) in done.iter().enumerate() {
        ensure_eq!(write.result, Ok(()), "write {index} of a batch of 256");
    }
    let expected: Vec<u8> = (0..=255).collect();
    ensure_eq!(
        read(&env, file.as_ref(), 0, 256).await?,
        expected,
        "content after 256 writes"
    );
    Ok(())
}

/// On a direct handle, an operation whose offset or length is not a multiple
/// of `block_size` fails the whole batch with `Unaligned`.
async fn arfc0005_s3_1_11_an_unaligned_direct_operation_fails_the_whole_batch(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let at = path(&env, "direct")?;
    let how = OpenOptions {
        direct: true,
        ..options(Access::ReadWrite, Create::New)
    };
    let file = match open_result(&env, disk.as_ref(), &at, how).await? {
        Ok(file) => file,
        Err(DiskError::Unsupported) => {
            return skip("direct I/O is unsupported on this file system");
        }
        Err(error) => return Err(fail(format!("direct open: {error:?}"))),
    };
    let block = within(&env, PATIENCE, file.info())
        .await
        .ctx("info")?
        .ctx("info")?
        .block_size as usize;
    let cases: [(u64, usize); 3] = [(1, block), (0, 100), (block as u64, block + 1)];
    for (offset, len) in cases {
        let writes = vec![
            WriteOp {
                offset: 4 * block as u64,
                buf: IoBuf::zeroed(block).ctx("IoBuf")?,
            },
            WriteOp {
                offset,
                buf: IoBuf::zeroed(len).ctx("IoBuf")?,
            },
        ];
        let done = within(&env, PATIENCE, file.write_at(writes))
            .await
            .ctx("write_at")?;
        let results: Vec<_> = done.into_iter().map(|d| (d.result, d.buf.len())).collect();
        all_failed(
            &results,
            &|e| *e == DiskError::Unaligned,
            &[block, len],
            "an unaligned direct write batch",
        )?;
        ensure_eq!(
            info_len(&env, file.as_ref()).await?,
            0,
            "length after a refused direct batch"
        );

        let reads = vec![
            ReadOp {
                offset: 0,
                buf: IoBuf::zeroed(block).ctx("IoBuf")?,
            },
            ReadOp {
                offset,
                buf: IoBuf::zeroed(len).ctx("IoBuf")?,
            },
        ];
        let done = within(&env, PATIENCE, file.read_at(reads))
            .await
            .ctx("read_at")?;
        let results: Vec<_> = done.into_iter().map(|d| (d.result, d.buf.len())).collect();
        all_failed(
            &results,
            &|e| *e == DiskError::Unaligned,
            &[block, len],
            "an unaligned direct read batch",
        )?;
    }
    let aligned = vec![WriteOp {
        offset: block as u64,
        buf: IoBuf::from_slice(&vec![5u8; block]).ctx("IoBuf")?,
    }];
    let done = within(&env, PATIENCE, file.write_at(aligned))
        .await
        .ctx("write_at")?;
    ensure_eq!(done[0].result, Ok(()), "an aligned direct write");
    Ok(())
}

/// A write on an `Access::Read` handle fails the whole batch with
/// `ReadOnly` and changes nothing.
async fn arfc0005_s3_1_11_a_write_on_a_read_handle_fails_the_whole_batch(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let at = path(&env, "read-only")?;
    drop(create_file(&env, &at, b"abc").await?);
    let file = open(
        &env,
        disk.as_ref(),
        &at,
        options(Access::Read, Create::Never),
    )
    .await?;
    let writes = vec![
        WriteOp {
            offset: 0,
            buf: IoBuf::from_slice(b"X").ctx("IoBuf")?,
        },
        WriteOp {
            offset: 10,
            buf: IoBuf::from_slice(b"YZ").ctx("IoBuf")?,
        },
    ];
    let done = within(&env, PATIENCE, file.write_at(writes))
        .await
        .ctx("write_at")?;
    let results: Vec<_> = done.into_iter().map(|d| (d.result, d.buf.len())).collect();
    all_failed(
        &results,
        &|e| *e == DiskError::ReadOnly,
        &[1, 2],
        "writes on a read handle",
    )?;
    ensure_eq!(
        read(&env, file.as_ref(), 0, 64).await?,
        b"abc".to_vec(),
        "content after refused writes"
    );
    let truncated = within(&env, PATIENCE, file.truncate(1))
        .await
        .ctx("truncate")?;
    ensure_eq!(
        truncated,
        Err(DiskError::ReadOnly),
        "truncate on a read handle"
    );
    Ok(())
}

/// Two writes of one batch with overlapping byte ranges fail the whole
/// batch with `InvalidInput`; adjacent ranges do not overlap.
async fn arfc0005_s3_1_11_overlapping_writes_fail_the_whole_batch(env: Env) -> Outcome {
    let at = path(&env, "overlap")?;
    let file = create_file(&env, &at, &[]).await?;
    for (first, second) in [
        ((0u64, 10usize), (5u64, 10usize)),
        ((20, 5), (0, 21)),
        ((7, 1), (7, 1)),
    ] {
        let writes = vec![
            WriteOp {
                offset: first.0,
                buf: IoBuf::from_slice(&vec![1u8; first.1]).ctx("IoBuf")?,
            },
            WriteOp {
                offset: second.0,
                buf: IoBuf::from_slice(&vec![2u8; second.1]).ctx("IoBuf")?,
            },
        ];
        let done = within(&env, PATIENCE, file.write_at(writes))
            .await
            .ctx("write_at")?;
        let results: Vec<_> = done.into_iter().map(|d| (d.result, d.buf.len())).collect();
        all_failed(
            &results,
            &|e| matches!(e, DiskError::InvalidInput(_)),
            &[first.1, second.1],
            "overlapping writes",
        )?;
        ensure_eq!(
            info_len(&env, file.as_ref()).await?,
            0,
            "length after overlapping writes"
        );
    }
    let writes = vec![
        WriteOp {
            offset: 0,
            buf: IoBuf::from_slice(&[1u8; 10]).ctx("IoBuf")?,
        },
        WriteOp {
            offset: 10,
            buf: IoBuf::from_slice(&[2u8; 10]).ctx("IoBuf")?,
        },
    ];
    let done = within(&env, PATIENCE, file.write_at(writes))
        .await
        .ctx("write_at")?;
    ensure!(
        done.iter().all(|d| d.result.is_ok()),
        "adjacent writes failed: {done:?}"
    );
    ensure_eq!(
        info_len(&env, file.as_ref()).await?,
        20,
        "length after adjacent writes"
    );
    Ok(())
}

/// An operation past the end of a block device fails the whole batch with
/// `InvalidInput`; `truncate` on a block device returns `Unsupported`.
async fn arfc0005_s3_1_11_an_operation_past_the_end_of_a_block_device_fails_the_whole_batch(
    env: Env,
) -> Outcome {
    let device = block_device(&env)?;
    let disk = env.runtime.disk_io().clone();
    let file = open(
        &env,
        disk.as_ref(),
        &device,
        options(Access::ReadWrite, Create::Never),
    )
    .await?;
    let info = within(&env, PATIENCE, file.info())
        .await
        .ctx("info")?
        .ctx("info")?;
    ensure_eq!(info.kind, EntryKind::Block, "kind of the block device");
    let block = info.block_size as usize;
    let before = read(&env, file.as_ref(), 0, block).await?;
    let writes = vec![
        WriteOp {
            offset: 0,
            buf: IoBuf::from_slice(&vec![0xABu8; block]).ctx("IoBuf")?,
        },
        WriteOp {
            offset: info.len,
            buf: IoBuf::zeroed(block).ctx("IoBuf")?,
        },
    ];
    let done = within(&env, PATIENCE, file.write_at(writes))
        .await
        .ctx("write_at")?;
    let results: Vec<_> = done.into_iter().map(|d| (d.result, d.buf.len())).collect();
    all_failed(
        &results,
        &|e| matches!(e, DiskError::InvalidInput(_)),
        &[block, block],
        "a write batch past the device's end",
    )?;
    ensure_eq!(
        read(&env, file.as_ref(), 0, block).await?,
        before,
        "first block after a refused batch"
    );
    let reads = vec![
        ReadOp {
            offset: 0,
            buf: IoBuf::zeroed(block).ctx("IoBuf")?,
        },
        ReadOp {
            offset: info.len - block as u64 / 2,
            buf: IoBuf::zeroed(block).ctx("IoBuf")?,
        },
    ];
    let done = within(&env, PATIENCE, file.read_at(reads))
        .await
        .ctx("read_at")?;
    let results: Vec<_> = done.into_iter().map(|d| (d.result, d.buf.len())).collect();
    all_failed(
        &results,
        &|e| matches!(e, DiskError::InvalidInput(_)),
        &[block, block],
        "a read batch past the device's end",
    )?;
    let truncated = within(&env, PATIENCE, file.truncate(0))
        .await
        .ctx("truncate")?;
    ensure_eq!(
        truncated,
        Err(DiskError::Unsupported),
        "truncate on a block device"
    );
    ensure_eq!(
        metadata(&env, &device).await?.map(|m| m.kind),
        Ok(EntryKind::Block),
        "metadata kind of the block device"
    );
    Ok(())
}

/// `read_at` and `write_at` return one result per operation, in the order
/// given, each with its operation's buffer.
async fn arfc0005_s2_4_results_come_in_order_with_their_buffers(env: Env) -> Outcome {
    let at = path(&env, "ordered")?;
    let file = create_file(&env, &at, &[]).await?;
    let mut generator = Gen::new(env.seed);
    let mut offset = 0u64;
    let mut writes = Vec::new();
    let mut contents = Vec::new();
    for len in [3usize, 1, 4_096, 17, 600] {
        let bytes = generator.bytes(len);
        writes.push(WriteOp {
            offset,
            buf: IoBuf::from_slice(&bytes).ctx("IoBuf")?,
        });
        contents.push((offset, bytes));
        offset += len as u64;
    }
    let done = within(&env, PATIENCE, file.write_at(writes))
        .await
        .ctx("write_at")?;
    ensure_eq!(done.len(), contents.len(), "results of five writes");
    for (index, (write, (_, bytes))) in done.iter().zip(&contents).enumerate() {
        ensure_eq!(write.result, Ok(()), "write {index}");
        ensure_eq!(
            &write.buf[..],
            &bytes[..],
            "buffer returned with write {index}"
        );
    }
    let reads: Vec<ReadOp> = contents
        .iter()
        .rev()
        .map(|(offset, bytes)| {
            IoBuf::zeroed(bytes.len()).map(|buf| ReadOp {
                offset: *offset,
                buf,
            })
        })
        .collect::<Result<_, _>>()
        .ctx("IoBuf")?;
    let done = within(&env, PATIENCE, file.read_at(reads))
        .await
        .ctx("read_at")?;
    ensure_eq!(done.len(), contents.len(), "results of five reads");
    for (index, (read, (_, bytes))) in done.iter().zip(contents.iter().rev()).enumerate() {
        ensure_eq!(read.result, Ok(bytes.len()), "read {index}");
        ensure_eq!(&read.buf[..], &bytes[..], "bytes of read {index}");
    }
    let empty = within(&env, PATIENCE, file.read_at(Vec::new()))
        .await
        .ctx("read_at")?;
    ensure_eq!(empty.len(), 0, "results of an empty read batch");
    let empty = within(&env, PATIENCE, file.write_at(Vec::new()))
        .await
        .ctx("write_at")?;
    ensure_eq!(empty.len(), 0, "results of an empty write batch");
    Ok(())
}

/// A read returns `buf.len()` bytes, or fewer only when it reaches the end
/// of the file.
async fn arfc0005_s3_1_11_a_read_is_short_only_at_end_of_file(env: Env) -> Outcome {
    let at = path(&env, "short")?;
    let bytes = Gen::new(env.seed).bytes(10_000);
    let file = create_file(&env, &at, &bytes).await?;
    for (offset, len, expected) in [
        (0u64, 4_096usize, 4_096usize),
        (4_096, 4_096, 4_096),
        (8_192, 4_096, 1_808),
        (9_999, 10, 1),
        (10_000, 10, 0),
        (20_000, 10, 0),
    ] {
        let got = read(&env, file.as_ref(), offset, len).await?;
        ensure_eq!(got.len(), expected, "bytes read at {offset} of {len}");
        ensure_eq!(
            &got[..],
            &bytes[(offset as usize).min(10_000)..(offset as usize + expected).min(10_000)],
            "content read at {offset}"
        );
    }
    Ok(())
}

/// A write writes all its bytes and extends a regular file, filling any gap
/// with zeros.
async fn arfc0005_s3_1_11_a_write_extends_a_file_filling_the_gap_with_zeros(env: Env) -> Outcome {
    let at = path(&env, "gap")?;
    let file = create_file(&env, &at, b"head").await?;
    write(&env, file.as_ref(), 10_000, b"tail").await?;
    ensure_eq!(
        info_len(&env, file.as_ref()).await?,
        10_004,
        "length after a write past the end"
    );
    let all = read(&env, file.as_ref(), 0, 10_004).await?;
    ensure_eq!(&all[..4], b"head", "bytes before the gap");
    ensure!(
        all[4..10_000].iter().all(|&byte| byte == 0),
        "the gap is not all zeros"
    );
    ensure_eq!(&all[10_000..], b"tail", "bytes after the gap");
    Ok(())
}

/// A completed write is seen by every later read on any handle of the same
/// file in the instance.
async fn arfc0005_s3_1_11_a_completed_write_is_seen_on_every_handle(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let at = path(&env, "shared")?;
    let writer = create_file(&env, &at, &[]).await?;
    let reader = open(
        &env,
        disk.as_ref(),
        &at,
        options(Access::Read, Create::Never),
    )
    .await?;
    let other = open(
        &env,
        disk.as_ref(),
        &at,
        options(Access::ReadWrite, Create::Never),
    )
    .await?;
    let mut generator = Gen::new(env.seed);
    for round in 0..8u64 {
        let bytes = generator.bytes(512);
        write(&env, writer.as_ref(), round * 300, &bytes).await?;
        for (label, handle) in [("reader", &reader), ("other", &other)] {
            ensure_eq!(
                read(&env, handle.as_ref(), round * 300, 512).await?,
                bytes,
                "round {round} through the {label} handle"
            );
        }
    }
    Ok(())
}

/// `sync` returns `Ok` for both kinds, and after a `sync` the outcome of a
/// dropped write is fixed: reads agree, and each block holds old or new
/// bytes.
async fn arfc0005_s3_1_11_sync_fixes_the_outcome_of_a_dropped_write(env: Env) -> Outcome {
    let at = path(&env, "dropped")?;
    let file = create_file(&env, &at, &[]).await?;
    let block = within(&env, PATIENCE, file.info())
        .await
        .ctx("info")?
        .ctx("info")?
        .block_size as usize;
    write(&env, file.as_ref(), 0, &vec![0x11u8; 2 * block]).await?;
    for kind in [SyncKind::Data, SyncKind::All] {
        let synced = within(&env, PATIENCE, file.sync(kind)).await.ctx("sync")?;
        ensure_eq!(synced, Ok(()), "sync({kind:?})");
    }
    let mut writing = file.write_at(vec![WriteOp {
        offset: 0,
        buf: IoBuf::from_slice(&vec![0x22u8; 2 * block]).ctx("IoBuf")?,
    }]);
    let _ = poll_once(&mut writing).await;
    drop(writing);
    let synced = within(&env, PATIENCE, file.sync(SyncKind::Data))
        .await
        .ctx("sync")?;
    ensure_eq!(synced, Ok(()), "sync after a dropped write");
    let first = read(&env, file.as_ref(), 0, 2 * block).await?;
    sleep(&env, SETTLE).await;
    let second = read(&env, file.as_ref(), 0, 2 * block).await?;
    ensure!(first == second, "the content changed after sync fixed it");
    for (index, unit) in first.chunks(block).enumerate() {
        ensure!(
            unit.iter().all(|&byte| byte == 0x11) || unit.iter().all(|&byte| byte == 0x22),
            "block {index} holds neither its old nor its new bytes"
        );
    }
    Ok(())
}

/// `truncate` sets the length of a regular file, filling with zeros when it
/// grows.
async fn arfc0005_s3_1_11_truncate_sets_the_length_of_a_regular_file(env: Env) -> Outcome {
    let at = path(&env, "truncated")?;
    let file = create_file(&env, &at, &[9u8; 5_000]).await?;
    for (len, label) in [(100u64, "shrink"), (0, "empty"), (3_000, "grow")] {
        within(&env, PATIENCE, file.truncate(len))
            .await
            .ctx("truncate")?
            .ctx(&format!("truncate to {len}"))?;
        ensure_eq!(
            info_len(&env, file.as_ref()).await?,
            len,
            "length after {label}"
        );
    }
    let grown = read(&env, file.as_ref(), 0, 3_000).await?;
    ensure!(
        grown.iter().all(|&byte| byte == 0),
        "bytes a truncate grew are not zeros"
    );
    Ok(())
}

/// `block_size` is 512 or 4,096; for `mem` it is the namespace's configured
/// size.
async fn arfc0005_s3_1_11_block_size_is_512_or_4096(env: Env) -> Outcome {
    let at = path(&env, "block")?;
    let file = create_file(&env, &at, b"b").await?;
    let size = within(&env, PATIENCE, file.info())
        .await
        .ctx("info")?
        .ctx("info")?
        .block_size;
    ensure!(size == 512 || size == 4_096, "block_size {size}");
    if let Some(configured) = env.file_block_size {
        ensure_eq!(size, configured, "block_size of the file namespace");
    }
    Ok(())
}

/// A block device opened with `exclusive` fails with `Busy` while another
/// holder has it, and opens again once that holder is dropped.
async fn arfc0005_s2_4_exclusive_block_device_is_busy_while_held(env: Env) -> Outcome {
    let device = block_device(&env)?;
    let disk = env.runtime.disk_io().clone();
    let how = OpenOptions {
        exclusive: true,
        ..options(Access::ReadWrite, Create::Never)
    };
    let holder = open(&env, disk.as_ref(), &device, how).await?;
    let second = open_result(&env, disk.as_ref(), &device, how).await?;
    ensure_eq!(
        second.err(),
        Some(DiskError::Busy),
        "a second exclusive open"
    );
    drop(holder);
    let deadline = env.runtime.clock().now().saturating_add(PATIENCE);
    loop {
        match open_result(&env, disk.as_ref(), &device, how).await? {
            Ok(_) => return Ok(()),
            Err(DiskError::Busy) if env.runtime.clock().now() < deadline => {
                sleep(&env, std::time::Duration::from_millis(1)).await;
            }
            Err(error) => {
                return Err(fail(format!(
                    "exclusive open after the holder was dropped: {error:?}"
                )));
            }
        }
    }
}

/// Disk methods may be called concurrently from several tasks.
async fn arfc0005_s3_1_3_disk_methods_may_be_called_concurrently(env: Env) -> Outcome {
    let mut handles = Vec::new();
    for task in 0..8u8 {
        let env_task = env.clone();
        let body = async move {
            let at = path(&env_task, &format!("concurrent-{task}"))?;
            let bytes = vec![task; 4_096 + usize::from(task)];
            let file = create_file(&env_task, &at, &bytes).await?;
            let back = read(&env_task, file.as_ref(), 0, bytes.len()).await?;
            ensure_eq!(back, bytes, "content written by task {task}");
            Ok(())
        };
        handles.push(
            env.runtime
                .spawn(name(&format!("disk-{task}"))?, body)
                .ctx("spawn")?,
        );
    }
    for handle in handles {
        within(&env, PATIENCE, handle.join())
            .await
            .ctx("join")?
            .ctx("join")??;
    }
    Ok(())
}

/// Hostile arguments return a result and never panic: offsets at the end of
/// the `u64` range, a write whose range overflows, and a huge truncate.
async fn arfc0005_s3_1_3_hostile_arguments_return_without_panic(env: Env) -> Outcome {
    let at = path(&env, "hostile")?;
    let file = create_file(&env, &at, b"data").await?;
    let reads = vec![ReadOp {
        offset: u64::MAX,
        buf: IoBuf::zeroed(16).ctx("IoBuf")?,
    }];
    let done = within(&env, PATIENCE, file.read_at(reads))
        .await
        .ctx("read_at")?;
    ensure_eq!(done.len(), 1, "results of a read at u64::MAX");
    if let Ok(count) = done[0].result {
        ensure_eq!(count, 0, "bytes read at u64::MAX");
    }
    let writes = vec![WriteOp {
        offset: u64::MAX - 4,
        buf: IoBuf::zeroed(16).ctx("IoBuf")?,
    }];
    let done = within(&env, PATIENCE, file.write_at(writes))
        .await
        .ctx("write_at")?;
    ensure_eq!(done.len(), 1, "results of a write past u64::MAX");
    ensure!(
        done[0].result.is_err(),
        "a write whose range passes u64::MAX succeeded"
    );
    let _ = within(&env, PATIENCE, file.truncate(u64::MAX))
        .await
        .ctx("truncate")?;
    let _ = within(
        &env,
        PATIENCE,
        file.read_at(vec![ReadOp {
            offset: 0,
            buf: IoBuf::zeroed(0).ctx("IoBuf")?,
        }]),
    )
    .await
    .ctx("read_at")?;
    let disk = env.runtime.disk_io().clone();
    let deep = RtPath::new(&format!("/{}", vec!["d"; 2_048].join("/"))).ctx("a deep path")?;
    let _ = within(&env, PATIENCE, disk.metadata(&deep))
        .await
        .ctx("metadata")?;
    let _ = within(&env, PATIENCE, disk.list(&deep)).await.ctx("list")?;
    let _ = within(&env, PATIENCE, disk.rename(&env.scratch, &deep))
        .await
        .ctx("rename")?;
    let _ = within(&env, PATIENCE, disk.sync_dir(&deep))
        .await
        .ctx("sync_dir")?;
    Ok(())
}

/// `mem` never returns an `Os` error: a battery of failing calls each
/// returns one of the named variants.
async fn arfc0005_s3_1_3_mem_never_returns_an_os_error(env: Env) -> Outcome {
    let disk = env.runtime.disk_io().clone();
    let missing = path(&env, "absent")?;
    let dir = path(&env, "dir")?;
    within(&env, PATIENCE, disk.create_dir(&dir))
        .await
        .ctx("create_dir")?
        .ctx("create_dir")?;
    let inner = dir.join("inner").ctx("join")?;
    drop(create_file(&env, &inner, b"x").await?);
    let mut errors: Vec<DiskError> = Vec::new();
    errors.extend(
        open_result(
            &env,
            disk.as_ref(),
            &missing,
            options(Access::Read, Create::Never),
        )
        .await?
        .err(),
    );
    errors.extend(
        within(&env, PATIENCE, disk.metadata(&missing))
            .await
            .ctx("metadata")?
            .err(),
    );
    errors.extend(
        within(&env, PATIENCE, disk.list(&missing))
            .await
            .ctx("list")?
            .err(),
    );
    errors.extend(
        within(&env, PATIENCE, disk.create_dir(&dir))
            .await
            .ctx("create_dir")?
            .err(),
    );
    errors.extend(
        within(&env, PATIENCE, disk.remove(&dir))
            .await
            .ctx("remove")?
            .err(),
    );
    errors.extend(
        within(&env, PATIENCE, disk.remove(&missing))
            .await
            .ctx("remove")?
            .err(),
    );
    errors.extend(
        within(&env, PATIENCE, disk.rename(&missing, &inner))
            .await
            .ctx("rename")?
            .err(),
    );
    errors.extend(
        within(&env, PATIENCE, disk.list(&inner))
            .await
            .ctx("list of a file")?
            .err(),
    );
    ensure!(
        errors.len() >= 7,
        "only {} of the failing calls failed: {errors:?}",
        errors.len()
    );
    for error in &errors {
        ensure!(
            !matches!(error, DiskError::Os { .. }),
            "mem returned {error:?}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_rt::{
        DiskError, IoBuf, MAX_DISK_BATCH, ReadDone, ReadOp, RtPath, WriteDone, WriteOp,
    };
    use alloy_seam::Detail;

    use crate::support::Gen;

    /// The rules of clause 3.1.10 for `RtPath::new`.
    fn valid_path(path: &str) -> bool {
        if path.len() > 4_096
            || !path.starts_with('/')
            || path.contains('\0')
            || path.contains('\\')
        {
            return false;
        }
        if path == "/" {
            return true;
        }
        path[1..].split('/').all(|component| {
            (1..=255).contains(&component.len()) && component != "." && component != ".."
        })
    }

    /// The rules of clause 3.1.10 for one component given to `join`.
    fn valid_component(component: &str) -> bool {
        (1..=255).contains(&component.len())
            && component != "."
            && component != ".."
            && !component.contains(['/', '\0', '\\'])
    }

    fn check_new(text: &str) {
        match RtPath::new(text) {
            Ok(path) => {
                assert!(valid_path(text), "{text:?} was accepted");
                assert_eq!(path.as_str(), text);
            }
            Err(error) => {
                assert!(!valid_path(text), "{text:?} was refused");
                assert_eq!(error, DiskError::InvalidPath);
            }
        }
    }

    fn check_join(base: &RtPath, component: &str) {
        let expected = if base.as_str() == "/" {
            format!("/{component}")
        } else {
            format!("{}/{component}", base.as_str())
        };
        let accept = valid_component(component) && expected.len() <= 4_096;
        match base.join(component) {
            Ok(joined) => {
                assert!(
                    accept,
                    "join({:?}, {component:?}) was accepted",
                    base.as_str()
                );
                assert_eq!(joined.as_str(), expected);
                assert_eq!(joined.parent().as_ref(), Some(base));
                assert_eq!(joined.file_name(), Some(component));
                assert_eq!(RtPath::new(&expected).as_ref(), Ok(&joined));
            }
            Err(error) => {
                assert!(
                    !accept,
                    "join({:?}, {component:?}) was refused",
                    base.as_str()
                );
                assert_eq!(error, DiskError::InvalidPath);
            }
        }
    }

    const TOKENS: [&str; 10] = ["/", "a", "b", ".", "..", "\\", "\0", "\u{e9}", "x.y", "//"];

    fn generated(generator: &mut Gen, max_tokens: u64) -> String {
        let count = generator.below(max_tokens + 1);
        (0..count)
            .map(|_| TOKENS[generator.below(TOKENS.len() as u64) as usize])
            .collect()
    }

    #[test]
    fn arfc0005_s3_1_10_rt_path_new_accepts_exactly_its_rules() {
        let c255 = "c".repeat(255);
        let c256 = "c".repeat(256);
        // A `/` and 16 components of 255 bytes joined by 15 `/` is 4,096
        // bytes; one byte more is over the limit.
        let at_limit = format!("/{}", vec![c255.as_str(); 16].join("/"));
        assert_eq!(at_limit.len(), 4_096);
        let also_at_limit = format!("{}/y", &at_limit[..4_094]);
        assert_eq!(also_at_limit.len(), 4_096);
        let too_long = format!("{at_limit}/y");
        for text in [
            "", "/", "//", "/a", "/a/", "a", "a/b", "/a//b", "/.", "/..", "/a/./b", "/a/../b",
            "/.a", "/a.", "/...", "/a\0", "/a\\b", "/\u{e9}",
        ] {
            check_new(text);
        }
        for text in [
            format!("/{c255}"),
            format!("/{c256}"),
            format!("/a/{c255}"),
            at_limit.clone(),
            also_at_limit,
            too_long,
        ] {
            check_new(&text);
        }
        assert!(RtPath::new(&at_limit).is_ok());
        for seed in 0..32 {
            let mut generator = Gen::new(seed);
            for _ in 0..500 {
                check_new(&generated(&mut generator, 8));
            }
        }
    }

    #[test]
    fn arfc0005_s3_1_10_join_accepts_exactly_one_valid_component() {
        let root = RtPath::root();
        let nested = RtPath::new("/var/lib").expect("a valid path");
        let c255 = "c".repeat(255);
        let long_base = RtPath::new(&format!("/{}", vec![c255.as_str(); 15].join("/")))
            .expect("a valid path of 3840 bytes");
        for base in [&root, &nested, &long_base] {
            for component in [
                "",
                "a",
                ".",
                "..",
                "...",
                "a/b",
                "/",
                "a\0",
                "a\\b",
                "\u{e9}",
                &c255,
                &"d".repeat(256),
            ] {
                check_join(base, component);
            }
            for seed in 0..8 {
                let mut generator = Gen::new(seed);
                for _ in 0..300 {
                    check_join(base, &generated(&mut generator, 4));
                }
            }
        }
        // 3,840 bytes, a `/`, and 255 bytes is 4,096; one more byte is over.
        check_join(&long_base, &c255);
        let over_base = long_base.join("e").expect("a valid join");
        check_join(&over_base, &"f".repeat(254));
    }

    #[test]
    fn arfc0005_s2_4_rt_path_root_parent_and_file_name() {
        let root = RtPath::root();
        assert_eq!(root.as_str(), "/");
        assert_eq!(RtPath::new("/").expect("the root"), root);
        assert_eq!(root.parent(), None);
        assert_eq!(root.file_name(), None);
        let deep = RtPath::new("/a/b/c").expect("a valid path");
        assert_eq!(deep.file_name(), Some("c"));
        let parent = deep.parent().expect("a parent");
        assert_eq!(parent.as_str(), "/a/b");
        let top = RtPath::new("/a").expect("a valid path");
        assert_eq!(top.parent(), Some(root.clone()));
        assert_eq!(top.file_name(), Some("a"));
        let built = root
            .join("a")
            .and_then(|a| a.join("b"))
            .and_then(|b| b.join("c"))
            .expect("valid joins");
        assert_eq!(built, deep);
    }

    fn aligned(buf: &IoBuf) -> bool {
        buf.is_empty() || (buf.as_ptr() as usize).is_multiple_of(IoBuf::ALIGN)
    }

    #[test]
    fn arfc0005_s3_1_13_io_buf_is_aligned_zeroed_and_bounded() {
        assert_eq!(IoBuf::ALIGN, 4_096);
        assert_eq!(IoBuf::MAX_LEN, 67_108_864);
        for len in [0, 1, 511, 512, 4_095, 4_096, 4_097, 1 << 20, IoBuf::MAX_LEN] {
            let buf = IoBuf::zeroed(len).expect("a length within the bound");
            assert_eq!(buf.len(), len);
            assert_eq!(buf.is_empty(), len == 0);
            assert!(aligned(&buf), "IoBuf::zeroed({len}) is not aligned");
            assert!(buf.iter().all(|&byte| byte == 0));
            let copy = buf.clone();
            assert!(
                aligned(&copy),
                "a clone of IoBuf::zeroed({len}) is not aligned"
            );
        }
        assert!(matches!(
            IoBuf::zeroed(IoBuf::MAX_LEN + 1),
            Err(DiskError::InvalidInput(_))
        ));
        assert!(matches!(
            IoBuf::zeroed(usize::MAX),
            Err(DiskError::InvalidInput(_))
        ));
        let mut generator = Gen::new(9);
        for len in [1, 100, 4_096, 9_000] {
            let bytes = generator.bytes(len);
            let mut buf = IoBuf::from_slice(&bytes).expect("a length within the bound");
            assert!(aligned(&buf));
            assert_eq!(&buf[..], &bytes[..]);
            buf[0] ^= 0xFF;
            assert_eq!(buf[0], bytes[0] ^ 0xFF);
            let copy = buf.clone();
            assert!(aligned(&copy));
            assert_eq!(&copy[..], &buf[..]);
        }
        let too_long = vec![0u8; IoBuf::MAX_LEN + 1];
        assert!(matches!(
            IoBuf::from_slice(&too_long),
            Err(DiskError::InvalidInput(_))
        ));
    }

    #[test]
    fn arfc0005_s3_1_13_io_buf_debug_prints_the_length_only() {
        let buf = IoBuf::from_slice(b"key").expect("a short buffer");
        assert_eq!(format!("{buf:?}"), "IoBuf { len: 3 }");
        assert_eq!(format!("{buf:#?}"), "IoBuf {\n    len: 3,\n}");
        let read_op = ReadOp {
            offset: 5,
            buf: buf.clone(),
        };
        assert_eq!(
            format!("{read_op:?}"),
            "ReadOp { offset: 5, buf: IoBuf { len: 3 } }"
        );
        let read_done = ReadDone {
            buf: buf.clone(),
            result: Ok(3),
        };
        assert_eq!(
            format!("{read_done:?}"),
            "ReadDone { buf: IoBuf { len: 3 }, result: Ok(3) }"
        );
        let write_op = WriteOp {
            offset: 0,
            buf: buf.clone(),
        };
        assert_eq!(
            format!("{write_op:?}"),
            "WriteOp { offset: 0, buf: IoBuf { len: 3 } }"
        );
        let write_done = WriteDone {
            buf,
            result: Err(DiskError::NoSpace),
        };
        assert_eq!(
            format!("{write_done:?}"),
            "WriteDone { buf: IoBuf { len: 3 }, result: Err(NoSpace) }"
        );
    }

    #[test]
    fn arfc0005_s2_4_disk_constants_and_error_text() {
        assert_eq!(MAX_DISK_BATCH, 256);
        let cases = [
            (DiskError::NotFound, "not found"),
            (DiskError::AlreadyExists, "already exists"),
            (DiskError::PermissionDenied, "permission denied"),
            (DiskError::NoSpace, "no space left"),
            (DiskError::Io, "device I/O error"),
            (
                DiskError::Unaligned,
                "offset, length, or buffer not aligned for direct I/O",
            ),
            (DiskError::InvalidPath, "invalid path"),
            (
                DiskError::InvalidInput(Detail::new("odd")),
                "invalid input: odd",
            ),
            (DiskError::BatchTooLarge, "batch longer than 256"),
            (DiskError::Busy, "device is held by another user"),
            (DiskError::ReadOnly, "handle is read only"),
            (DiskError::IsDirectory, "is a directory"),
            (DiskError::NotDirectory, "not a directory"),
            (DiskError::NotEmpty, "directory not empty"),
            (DiskError::TooManyOpen, "too many open files"),
            (
                DiskError::TooManyEntries,
                "directory has more than 1048576 entries",
            ),
            (
                DiskError::Unsupported,
                "not supported on this file or platform",
            ),
            (DiskError::Os { code: 5 }, "operating system error 5"),
        ];
        for (error, text) in cases {
            assert_eq!(error.to_string(), text);
        }
    }

    /// Error types derive `Debug`, `Clone`, `PartialEq`, and `Eq` and are
    /// `std::error::Error`; the seam traits are object safe (section 2.1).
    #[test]
    fn arfc0005_s2_1_error_types_and_object_safety() {
        fn error<
            E: std::error::Error + std::fmt::Debug + Clone + PartialEq + Eq + Send + Sync + 'static,
        >() {
        }
        error::<alloy_rt::RngError>();
        error::<alloy_rt::SpawnError>();
        error::<alloy_rt::JoinError>();
        error::<alloy_rt::NetError>();
        error::<DiskError>();
        error::<alloy_rt::Elapsed>();
        fn object_safe<T: ?Sized>(_: Option<&T>) {}
        object_safe::<dyn alloy_rt::Clock>(None);
        object_safe::<dyn alloy_rt::Net>(None);
        object_safe::<dyn alloy_rt::DiskIo>(None);
        object_safe::<dyn alloy_rt::Rng>(None);
        object_safe::<dyn alloy_rt::Spawn>(None);
        object_safe::<dyn alloy_rt::TaskHandle>(None);
        object_safe::<dyn alloy_rt::UdpSocket>(None);
        object_safe::<dyn alloy_rt::Stream>(None);
        object_safe::<dyn alloy_rt::Listener>(None);
        object_safe::<dyn alloy_rt::DiskFile>(None);
        fn send<T: Send>() {}
        send::<alloy_rt::BoxFuture<'static, ()>>();
    }
}
