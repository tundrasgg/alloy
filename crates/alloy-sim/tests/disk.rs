//! The `mem` disk through the world: durability at a crash (ARFC 0005
//! clauses 3.1.12 and 3.3.4), capacity, read faults, and devices (clause
//! 3.3.7), and the `disk-op` records (section 4.1).

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use alloy_rt::{
    Access, BoxFuture, Create, DiskError, DiskFile, DiskIo, EntryKind, IoBuf, OpenOptions, ReadOp,
    RtPath, Runtime, SyncKind, WriteOp,
};
use alloy_seam::Detail;
use alloy_sim::{
    Boot, DeviceConfig, DiskTarget, Failure, Fault, HostBoot, HostConfig, Observer, Ppm, Record,
    RunConfig, RunResult, RunSummary, Unsynced, World, WorldError, run,
};

const DISK_OP: u16 = 0x0050;

/// A change to a default `HostConfig`.
type Tweak = Box<dyn FnOnce(&mut HostConfig) + Send>;

/// A `disk-op` record as an observer saw it: host, body, and data.
type Seen = (Option<u32>, Vec<u8>, Vec<u8>);

fn path(text: &str) -> RtPath {
    RtPath::new(text).expect("a valid path")
}

fn options(access: Access, create: Create) -> OpenOptions {
    OpenOptions {
        access,
        create,
        direct: false,
        exclusive: false,
    }
}

async fn open(disk: &dyn DiskIo, at: &str) -> Box<dyn DiskFile> {
    let how = options(Access::ReadWrite, Create::IfMissing);
    disk.open(&path(at), how).await.expect("open")
}

async fn write(file: &dyn DiskFile, offset: u64, bytes: &[u8]) -> Result<(), DiskError> {
    let buf = IoBuf::from_slice(bytes).expect("a buffer");
    file.write_at(vec![WriteOp { offset, buf }]).await[0]
        .result
        .clone()
}

async fn read(file: &dyn DiskFile, offset: u64, len: usize) -> Result<Vec<u8>, DiskError> {
    let buf = IoBuf::zeroed(len).expect("a buffer");
    let done = file.read_at(vec![ReadOp { offset, buf }]).await;
    let count = done[0].result.clone()?;
    Ok(done[0].buf[..count].to_vec())
}

/// The bytes of the file at `at`, or `None` when it cannot be opened.
async fn content(disk: &dyn DiskIo, at: &str) -> Option<Vec<u8>> {
    let how = options(Access::Read, Create::Never);
    let file = disk.open(&path(at), how).await.ok()?;
    let len = file.info().await.ok()?.len as usize;
    read(file.as_ref(), 0, len).await.ok()
}

async fn names(disk: &dyn DiskIo, dir: &str) -> Vec<String> {
    let entries = disk.list(&path(dir)).await.unwrap_or_default();
    entries.into_iter().map(|entry| entry.name).collect()
}

/// What one start of a host does, given its runtime and the world.
type Phase = Arc<dyn Fn(Runtime, World) -> BoxFuture<'static, Vec<String>> + Send + Sync>;

fn phase<F, Fut>(f: F) -> Phase
where
    F: Fn(Runtime, World) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Vec<String>> + Send + 'static,
{
    Arc::new(move |runtime, world| Box::pin(f(runtime, world)))
}

/// Runs one host whose first start runs `first`. When `second` is given,
/// the host then crashes with `mode`, restarts, and runs `second`. Returns
/// what each start returned.
fn starts(
    seed: u64,
    tweak: impl FnOnce(&mut HostConfig) + Send + 'static,
    mode: Unsynced,
    first: Phase,
    second: Option<Phase>,
) -> (Vec<Vec<String>>, RunSummary) {
    let outs: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let kept = Arc::clone(&outs);
    let summary = run(RunConfig::new(seed), move |world| async move {
        let fail = |error: WorldError| Failure(Detail::new(&error.to_string()));
        let (inner, again, world_in) = (Arc::clone(&kept), second.clone(), world.clone());
        let boot: Boot = Arc::new(move |b: HostBoot| {
            let phase = if b.start == 1 {
                Some(Arc::clone(&first))
            } else {
                again.clone()
            };
            let (outs, world) = (Arc::clone(&inner), world_in.clone());
            Box::pin(async move {
                if let Some(phase) = phase {
                    let out = phase(b.runtime, world).await;
                    outs.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(out);
                }
            })
        });
        let mut config = HostConfig::new(boot);
        tweak(&mut config);
        let host = world.add_host(config).map_err(fail)?;
        let done = |n: usize| kept.lock().unwrap_or_else(PoisonError::into_inner).len() >= n;
        while !done(1) {
            world.sleep(Duration::from_millis(1)).await;
        }
        if second.is_some() {
            let crash = Fault::Crash {
                host,
                after_polls: 0,
                unsynced: mode,
            };
            world.apply(crash).map_err(fail)?;
            let restart = Fault::Restart {
                host,
                rng_replay: None,
            };
            world.apply(restart).map_err(fail)?;
            while !done(2) {
                world.sleep(Duration::from_millis(1)).await;
            }
        }
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);
    let outs = outs.lock().unwrap_or_else(PoisonError::into_inner).clone();
    (outs, summary)
}

#[test]
fn a_lost_crash_keeps_exactly_what_syncs_cover() {
    let first = phase(|rt, _| async move {
        let disk = rt.disk_io().as_ref();
        let a = open(disk, "/a").await;
        write(a.as_ref(), 0, b"one").await.expect("write");
        a.sync(SyncKind::Data).await.expect("sync");
        disk.sync_dir(&path("/")).await.expect("sync_dir");
        // Unsynced data, and a length change.
        write(a.as_ref(), 3, b"two").await.expect("write");
        disk.create_dir(&path("/d")).await.expect("create_dir");
        disk.sync_dir(&path("/")).await.expect("sync_dir");
        // Data synced, but the create is not: the file is lost.
        let f = open(disk, "/d/f").await;
        write(f.as_ref(), 0, b"f").await.expect("write");
        f.sync(SyncKind::All).await.expect("sync");
        // A create synced, its data not.
        let g = open(disk, "/g").await;
        disk.sync_dir(&path("/")).await.expect("sync_dir");
        write(g.as_ref(), 0, b"g").await.expect("write");
        // A rename with only one parent synced is undone whole.
        disk.rename(&path("/a"), &path("/d/a"))
            .await
            .expect("rename");
        disk.sync_dir(&path("/")).await.expect("sync_dir");
        // An unsynced remove is undone.
        let h = open(disk, "/h").await;
        write(h.as_ref(), 0, b"h").await.expect("write");
        h.sync(SyncKind::Data).await.expect("sync");
        disk.sync_dir(&path("/")).await.expect("sync_dir");
        drop(h);
        disk.remove(&path("/h")).await.expect("remove");
        vec![format!("{:?}", names(disk, "/").await)]
    });
    let second = phase(|rt, _| async move {
        let disk = rt.disk_io().as_ref();
        let mut out = vec![format!("{:?}", names(disk, "/").await)];
        out.push(format!("{:?}", names(disk, "/d").await));
        for file in ["/a", "/g", "/h"] {
            out.push(format!("{:?}", content(disk, file).await));
        }
        out
    });
    let (outs, _) = starts(1, |_| {}, Unsynced::Lost, first, Some(second));
    assert_eq!(outs[0], [r#"["d", "g"]"#]);
    assert_eq!(
        outs[1],
        [
            r#"["a", "d", "g", "h"]"#,
            "[]",
            "Some([111, 110, 101])",
            "Some([])",
            "Some([104])",
        ]
    );
}

/// For each run, the 8 blocks of a file synced as `A` and then overwritten
/// with `B` in one unsynced write, as each block read after the crash.
fn blocks_after(seed: u64, mode: Unsynced) -> String {
    let first = phase(|rt, _| async move {
        let disk = rt.disk_io().as_ref();
        let t = open(disk, "/t").await;
        write(t.as_ref(), 0, &[b'A'; 8 * 4_096])
            .await
            .expect("write");
        t.sync(SyncKind::Data).await.expect("sync");
        disk.sync_dir(&path("/")).await.expect("sync_dir");
        write(t.as_ref(), 0, &[b'B'; 8 * 4_096])
            .await
            .expect("write");
        Vec::new()
    });
    let second = phase(|rt, _| async move {
        let bytes = content(rt.disk_io().as_ref(), "/t")
            .await
            .unwrap_or_default();
        let blocks = bytes.chunks(4_096).map(|block| {
            if block.iter().all(|b| *b == block[0]) {
                char::from(block[0])
            } else {
                '?'
            }
        });
        vec![blocks.collect()]
    });
    let (outs, summary) = starts(seed, |_| {}, mode, first.clone(), Some(second.clone()));
    let (again, repeat) = starts(seed, |_| {}, mode, first, Some(second));
    assert_eq!((outs.clone(), summary.line()), (again, repeat.line()));
    outs[1][0].clone()
}

#[test]
fn seeded_keeps_a_write_whole_and_torn_keeps_each_block_or_not() {
    let seeded: Vec<String> = (1..=12)
        .map(|s| blocks_after(s, Unsynced::Seeded))
        .collect();
    assert!(
        seeded.iter().all(|b| b == "AAAAAAAA" || b == "BBBBBBBB"),
        "{seeded:?}"
    );
    assert!(seeded.iter().any(|b| b == "AAAAAAAA") && seeded.iter().any(|b| b == "BBBBBBBB"));
    let torn: Vec<String> = (1..=6).map(|s| blocks_after(s, Unsynced::Torn)).collect();
    assert!(
        torn.iter().all(|b| b.len() == 8 && !b.contains('?')),
        "{torn:?}"
    );
    assert!(
        torn.iter().any(|b| b.contains('A') && b.contains('B')),
        "{torn:?}"
    );
    assert_eq!(blocks_after(1, Unsynced::Lost), "AAAAAAAA");
}

#[test]
fn capacity_and_read_faults_apply_to_their_targets() {
    let first = phase(|rt, world| async move {
        let disk = rt.disk_io().as_ref();
        let host = alloy_sim::HostId(0);
        let mut out = Vec::new();
        let c = open(disk, "/c").await;
        out.push(format!("{:?}", write(c.as_ref(), 0, &[1; 4_000]).await));
        out.push(format!("{:?}", write(c.as_ref(), 4_096, &[2]).await));
        out.push(format!("{:?}", write(c.as_ref(), 4_000, &[3; 96]).await));
        out.push(format!("{:?}", c.truncate(5_000).await));
        out.push(format!("{:?}", c.truncate(512).await));
        let capacity = Fault::Capacity { host, bytes: 0 };
        world.apply(capacity).expect("capacity");
        out.push(format!("{:?}", write(c.as_ref(), 512, &[4]).await));
        out.push(format!("{:?}", write(c.as_ref(), 0, &[5]).await));
        let rates = |target: DiskTarget, read_error, corrupt_read| Fault::Disk {
            host,
            target,
            read_error,
            corrupt_read,
        };
        world
            .apply(rates(DiskTarget::Files, Ppm::ALWAYS, Ppm::ZERO))
            .expect("disk");
        out.push(format!("{:?}", read(c.as_ref(), 0, 512).await));
        world
            .apply(rates(DiskTarget::Files, Ppm::ZERO, Ppm::ALWAYS))
            .expect("disk");
        let flipped = read(c.as_ref(), 0, 512).await.expect("read");
        world
            .apply(rates(DiskTarget::Files, Ppm::ZERO, Ppm::ZERO))
            .expect("disk");
        let stored = read(c.as_ref(), 0, 512).await.expect("read");
        let bits: u32 = flipped
            .iter()
            .zip(&stored)
            .map(|(a, b)| (a ^ b).count_ones())
            .sum();
        out.push(format!("{bits} bit flipped"));
        let device = disk
            .open(&path("/dev/x"), options(Access::Read, Create::Never))
            .await
            .expect("open the device");
        let dev = DiskTarget::Device(path("/dev/x"));
        world
            .apply(rates(dev, Ppm::ALWAYS, Ppm::ZERO))
            .expect("disk");
        out.push(format!("{:?}", read(device.as_ref(), 0, 512).await));
        out.push(format!("{:?}", read(c.as_ref(), 0, 1).await));
        let nowhere = rates(DiskTarget::Device(path("/c")), Ppm::ZERO, Ppm::ZERO);
        out.push(format!("{:?}", world.apply(nowhere)));
        out
    });
    let tweak = |config: &mut HostConfig| {
        config.capacity = 4_096;
        config.block_size = 512;
        config.devices.push(DeviceConfig {
            path: path("/dev/x"),
            len: 4_096,
            block_size: 4_096,
        });
    };
    let (outs, _) = starts(2, tweak, Unsynced::Lost, first, None);
    assert_eq!(
        outs[0],
        [
            "Ok(())",
            "Err(NoSpace)",
            "Ok(())",
            "Err(NoSpace)",
            "Ok(())",
            "Err(NoSpace)",
            "Ok(())",
            "Err(Io)",
            "1 bit flipped",
            "Err(Io)",
            "Ok([5])",
            "Err(Unknown(Detail(\"device /c\")))",
        ]
    );
}

#[test]
fn devices_are_created_durable_and_bad_configs_are_invalid() {
    let device = |at: &str, len: u64, block_size: u32| DeviceConfig {
        path: path(at),
        len,
        block_size,
    };
    let bad: Vec<Tweak> = vec![
        Box::new(|c| c.block_size = 1_024),
        Box::new(move |c| c.devices.push(device("/", 4_096, 4_096))),
        Box::new(move |c| c.devices.push(device("/d", 4_097, 4_096))),
        Box::new(move |c| c.devices.push(device("/d", 0, 512))),
        Box::new(move |c| c.devices.push(device("/d", (1 << 40) + 512, 512))),
        Box::new(move |c| c.devices.push(device("/d", 512, 256))),
        Box::new(move |c| {
            c.devices
                .extend([device("/d", 512, 512), device("/d", 512, 512)])
        }),
        Box::new(move |c| {
            c.devices
                .extend([device("/d", 512, 512), device("/d/e", 512, 512)])
        }),
        Box::new(move |c| {
            c.devices
                .extend((0..65).map(|i| device(&format!("/d{i}"), 512, 512)));
        }),
    ];
    let summary = run(RunConfig::new(3), move |world| async move {
        for tweak in bad {
            let boot: Boot = Arc::new(|_| Box::pin(async {}));
            let mut config = HostConfig::new(boot);
            tweak(&mut config);
            let added = world.add_host(config);
            if !matches!(added, Err(WorldError::Invalid(_))) {
                return Err(Failure(Detail::new(&format!("{added:?}"))));
            }
        }
        Ok(())
    });
    assert_eq!(summary.result, RunResult::Pass, "{:?}", summary.failure);

    let look = |end: &'static str| {
        phase(move |rt, _| async move {
            let disk = rt.disk_io().as_ref();
            let kind = disk
                .metadata(&path("/a/b/sd"))
                .await
                .map(|m| (m.kind, m.len));
            let info = match disk
                .open(&path("/a/b/sd"), options(Access::ReadWrite, Create::Never))
                .await
            {
                Ok(file) => {
                    write(file.as_ref(), 0, &[7; 512]).await.expect("write");
                    format!("{:?}", file.info().await.map(|i| i.block_size))
                }
                Err(error) => format!("{error:?}"),
            };
            vec![
                format!("{kind:?} {info} {end}"),
                format!("{:?}", names(disk, "/a").await),
            ]
        })
    };
    let tweak = |config: &mut HostConfig| {
        config.devices.push(DeviceConfig {
            path: path("/a/b/sd"),
            len: 1 << 20,
            block_size: 512,
        });
    };
    let (outs, _) = starts(
        4,
        tweak,
        Unsynced::Lost,
        look("first"),
        Some(look("second")),
    );
    let block = EntryKind::Block;
    assert_eq!(
        outs[0][0],
        format!("Ok(({block:?}, 1048576)) Ok(512) first")
    );
    assert_eq!(
        outs[1][0],
        format!("Ok(({block:?}, 1048576)) Ok(512) second")
    );
    assert_eq!(outs[1][1], r#"["b"]"#);
}

/// Collects every `disk-op` record: host, body, and data.
#[derive(Clone, Default)]
struct DiskOps(Arc<Mutex<Vec<Seen>>>);

impl Observer for DiskOps {
    fn record(&mut self, record: &Record<'_>) {
        if record.kind == DISK_OP {
            let seen = (
                record.host.map(|h| h.0),
                record.body.to_vec(),
                record.data.to_vec(),
            );
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(seen);
        }
    }
}

fn op_body(handle: u64, op: u8, range: (u64, u64), result: u8, data: Option<&[u8]>) -> Vec<u8> {
    let mut body = handle.to_le_bytes().to_vec();
    body.push(op);
    body.extend_from_slice(&range.0.to_le_bytes());
    body.extend_from_slice(&range.1.to_le_bytes());
    body.push(result);
    match data {
        Some(data) => body.extend_from_slice(&alloy_crypto::hash(data)[..8]),
        None => body.extend_from_slice(&[0; 8]),
    }
    body
}

#[test]
fn disk_ops_write_their_records_and_dead_handles_fail_with_io() {
    let old: Arc<Mutex<Option<Box<dyn DiskFile>>>> = Arc::default();
    let held = Arc::clone(&old);
    let first = phase(move |rt, _| {
        let held = Arc::clone(&held);
        async move {
            let disk = rt.disk_io().as_ref();
            let file = open(disk, "/r").await;
            write(file.as_ref(), 0, b"hi").await.expect("write");
            read(file.as_ref(), 0, 4).await.expect("read");
            let missing = disk.metadata(&path("/missing")).await;
            *held.lock().unwrap_or_else(PoisonError::into_inner) = Some(file);
            vec![format!("{missing:?}")]
        }
    });
    let held = Arc::clone(&old);
    let second = phase(move |rt, _| {
        let held = Arc::clone(&held);
        async move {
            let stale = held.lock().unwrap_or_else(PoisonError::into_inner).take();
            let stale = stale.expect("the old handle");
            let failed = write(stale.as_ref(), 0, b"x").await;
            drop(stale);
            let file = open(rt.disk_io().as_ref(), "/r").await;
            vec![
                format!("{failed:?}"),
                format!("{:?}", read(file.as_ref(), 0, 4).await),
            ]
        }
    });
    let ops = DiskOps::default();
    let seen = ops.clone();
    let observing = phase(move |rt, world| {
        let (first, seen) = (Arc::clone(&first), seen.clone());
        async move {
            world.observe(Box::new(seen)).expect("room");
            first(rt, world).await
        }
    });
    let (outs, _) = starts(5, |_| {}, Unsynced::Lost, observing, Some(second));
    assert_eq!(outs[0], ["Err(NotFound)"]);
    // The unsynced create of /r is lost.
    assert_eq!(outs[1], ["Err(Io)", "Ok([])"]);
    let records = ops.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let expected: Vec<Seen> = vec![
        (Some(0), op_body(1, 1, (0, 0), 0, None), vec![]),
        (
            Some(0),
            op_body(1, 3, (0, 2), 0, Some(b"hi")),
            b"hi".to_vec(),
        ),
        (
            Some(0),
            op_body(1, 2, (0, 4), 0, Some(b"hi")),
            b"hi".to_vec(),
        ),
        (Some(0), op_body(0, 6, (0, 0), 1, None), vec![]),
        (Some(0), op_body(1, 3, (0, 1), 5, None), vec![]),
        (Some(0), op_body(2, 1, (0, 0), 0, None), vec![]),
        (Some(0), op_body(2, 2, (0, 4), 0, Some(b"")), vec![]),
    ];
    assert_eq!(records, expected);
}
