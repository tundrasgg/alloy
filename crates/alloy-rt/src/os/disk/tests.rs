//! Unit tests of the `os` disk-io in a scratch directory of the host.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;
use crate::os::OsHost;
use crate::os::tests::host;

/// A host and an instance rooted in a fresh scratch directory, removed by
/// [`Scratch::finish`].
struct Scratch {
    host: OsHost,
    disk: OsDiskIo,
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let host = host();
        let dir = std::env::temp_dir().join(format!("alloy-rt-disk-{name}-{}", std::process::id()));
        if fs::symlink_metadata(&dir).is_ok() {
            fs::remove_dir_all(&dir).unwrap();
        }
        fs::create_dir_all(&dir).unwrap();
        let disk = OsDiskIo::new(host.host(), dir.clone());
        Scratch { host, disk, dir }
    }

    fn finish(self) {
        let Scratch { host, disk, dir } = self;
        drop(disk);
        host.shutdown(Duration::from_secs(1));
        fs::remove_dir_all(dir).unwrap();
    }

    fn run<F: Future>(&self, future: F) -> F::Output {
        self.host.block_on(future)
    }

    fn open(
        &self,
        path: &str,
        access: Access,
        create: Create,
    ) -> Result<Box<dyn DiskFile>, DiskError> {
        let options = OpenOptions {
            access,
            create,
            direct: false,
            exclusive: false,
        };
        self.run(self.disk.open(&RtPath::new(path).unwrap(), options))
    }
}

fn path(text: &str) -> RtPath {
    RtPath::new(text).unwrap()
}

fn write(offset: u64, bytes: &[u8]) -> WriteOp {
    WriteOp {
        offset,
        buf: IoBuf::from_slice(bytes).unwrap(),
    }
}

fn read(offset: u64, len: usize) -> ReadOp {
    ReadOp {
        offset,
        buf: IoBuf::zeroed(len).unwrap(),
    }
}

#[test]
fn open_follows_the_create_modes_and_checks_its_input() {
    use Access::{Read, ReadWrite};
    let s = Scratch::new("open");
    let fails = |r: Result<Box<dyn DiskFile>, DiskError>| r.map(|_| ()).unwrap_err();
    assert_eq!(
        fails(s.open("/f", ReadWrite, Create::Never)),
        DiskError::NotFound
    );
    assert!(matches!(
        fails(s.open("/f", Read, Create::IfMissing)),
        DiskError::InvalidInput(_)
    ));
    s.open("/f", ReadWrite, Create::New).unwrap();
    assert_eq!(
        fails(s.open("/f", ReadWrite, Create::New)),
        DiskError::AlreadyExists
    );
    s.open("/f", ReadWrite, Create::IfMissing).unwrap();
    s.open("/f", Read, Create::Never).unwrap();
    assert_eq!(
        fails(s.open("/missing/f", ReadWrite, Create::IfMissing)),
        DiskError::NotFound
    );
    s.run(s.disk.create_dir(&path("/d"))).unwrap();
    assert_eq!(
        fails(s.open("/d", Read, Create::Never)),
        DiskError::IsDirectory
    );
    assert_eq!(
        fails(s.open("/", Read, Create::Never)),
        DiskError::IsDirectory
    );
    let exclusive = OpenOptions {
        access: ReadWrite,
        create: Create::Never,
        direct: false,
        exclusive: true,
    };
    assert!(matches!(
        fails(s.run(s.disk.open(&path("/f"), exclusive))),
        DiskError::InvalidInput(_)
    ));
    s.disk.open.store(MAX_OPEN, Ordering::SeqCst);
    assert_eq!(
        fails(s.open("/f", Read, Create::Never)),
        DiskError::TooManyOpen
    );
    s.disk.open.store(0, Ordering::SeqCst);
    s.finish();
}

#[test]
fn writes_extend_with_zeros_and_reads_stop_at_the_end() {
    let s = Scratch::new("io");
    let file = s.open("/f", Access::ReadWrite, Create::New).unwrap();
    let done = s.run(file.write_at(vec![write(10, b"abc"), write(0, b"xy")]));
    assert!(done.iter().all(|d| d.result.is_ok()));
    assert_eq!(&done[0].buf[..], b"abc");
    let info = s.run(file.info()).unwrap();
    assert_eq!((info.kind, info.len), (EntryKind::File, 13));
    assert!(info.block_size == 512 || info.block_size == 4_096);
    let done = s.run(file.read_at(vec![read(0, 13), read(11, 8), read(40, 4)]));
    assert_eq!(done[0].result, Ok(13));
    assert_eq!(&done[0].buf[..], b"xy\0\0\0\0\0\0\0\0abc");
    assert_eq!(done[1].result, Ok(2));
    assert_eq!(&done[1].buf[..2], b"bc");
    assert_eq!(done[2].result, Ok(0));
    // A second handle sees the completed writes.
    let other = s.open("/f", Access::Read, Create::Never).unwrap();
    let done = s.run(other.read_at(vec![read(10, 3)]));
    assert_eq!(&done[0].buf[..], b"abc");
    s.run(file.truncate(4)).unwrap();
    assert_eq!(s.run(file.info()).unwrap().len, 4);
    s.run(file.truncate(6)).unwrap();
    let done = s.run(file.read_at(vec![read(0, 8)]));
    assert_eq!(done[0].result, Ok(6));
    assert_eq!(&done[0].buf[..6], b"xy\0\0\0\0");
    s.run(file.sync(SyncKind::Data)).unwrap();
    s.run(file.sync(SyncKind::All)).unwrap();
    s.run(other.sync(SyncKind::All)).unwrap();
    assert_eq!(s.run(other.truncate(0)), Err(DiskError::ReadOnly));
    assert!(s.run(file.read_at(Vec::new())).is_empty());
    assert!(format!("{file:?}").starts_with("OsDiskFile"));
    drop((file, other));
    assert_eq!(s.disk.open.load(Ordering::SeqCst), 0);
    s.finish();
}

#[test]
fn invalid_batches_fail_whole_and_return_their_buffers() {
    let s = Scratch::new("batch");
    let file = s.open("/f", Access::ReadWrite, Create::New).unwrap();
    let reader = s.open("/f", Access::Read, Create::Never).unwrap();
    let too_many: Vec<ReadOp> = (0..=MAX_DISK_BATCH as u64).map(|i| read(i, 1)).collect();
    let done = s.run(file.read_at(too_many));
    assert_eq!(done.len(), MAX_DISK_BATCH + 1);
    assert!(
        done.iter()
            .all(|d| d.result == Err(DiskError::BatchTooLarge))
    );
    let done = s.run(reader.write_at(vec![write(0, b"a"), write(5, b"b")]));
    assert!(done.iter().all(|d| d.result == Err(DiskError::ReadOnly)));
    let done = s.run(file.write_at(vec![write(0, b"abcd"), write(3, b"z"), write(9, b"")]));
    assert!(
        done.iter()
            .all(|d| matches!(d.result, Err(DiskError::InvalidInput(_))))
    );
    assert_eq!(&done[0].buf[..], b"abcd");
    // Nothing of a failed batch was written.
    assert_eq!(s.run(file.info()).unwrap().len, 0);
    // Writes that only touch do not overlap.
    let done = s.run(file.write_at(vec![write(0, b"ab"), write(2, b"cd")]));
    assert!(done.iter().all(|d| d.result.is_ok()));
    s.finish();
}

#[test]
fn direct_handles_need_aligned_operations() {
    let s = Scratch::new("direct");
    let options = OpenOptions {
        access: Access::ReadWrite,
        create: Create::New,
        direct: true,
        exclusive: false,
    };
    let file = match s.run(s.disk.open(&path("/f"), options)) {
        Ok(file) => file,
        // A file system without direct I/O has nothing more to check.
        Err(DiskError::Unsupported) => return s.finish(),
        Err(e) => panic!("{e:?}"),
    };
    let block = s.run(file.info()).unwrap().block_size as usize;
    let unaligned = vec![
        write(0, &vec![7; block]),
        write(2 * block as u64, &[1; 100]),
    ];
    let done = s.run(file.write_at(unaligned));
    assert!(done.iter().all(|d| d.result == Err(DiskError::Unaligned)));
    let done = s.run(file.write_at(vec![write(0, &vec![7; block])]));
    assert_eq!(done[0].result, Ok(()));
    let done = s.run(file.read_at(vec![read(0, block), read(1, block)]));
    assert!(done.iter().all(|d| d.result == Err(DiskError::Unaligned)));
    let done = s.run(file.read_at(vec![read(0, block)]));
    assert_eq!(done[0].result, Ok(block));
    assert!(done[0].buf.iter().all(|b| *b == 7));
    s.run(file.sync(SyncKind::Data)).unwrap();
    drop(file);
    s.finish();
}

#[test]
fn the_namespace_lists_creates_removes_and_renames() {
    let s = Scratch::new("names");
    let disk = &s.disk;
    for name in ["b", "a", "C"] {
        s.open(&format!("/{name}"), Access::ReadWrite, Create::New)
            .unwrap();
    }
    s.run(disk.create_dir(&path("/d"))).unwrap();
    assert_eq!(
        s.run(disk.create_dir(&path("/d"))),
        Err(DiskError::AlreadyExists)
    );
    assert_eq!(
        s.run(disk.create_dir(&path("/x/y"))),
        Err(DiskError::NotFound)
    );
    s.open("/d/inner", Access::ReadWrite, Create::New).unwrap();
    let listed = s.run(disk.list(&RtPath::root())).unwrap();
    let names: Vec<_> = listed.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(
        names,
        [
            ("C", EntryKind::File),
            ("a", EntryKind::File),
            ("b", EntryKind::File),
            ("d", EntryKind::Dir)
        ]
    );
    assert_eq!(s.run(disk.list(&path("/a"))), Err(DiskError::NotDirectory));
    assert_eq!(s.run(disk.list(&path("/none"))), Err(DiskError::NotFound));
    assert_eq!(
        s.run(disk.metadata(&path("/d"))).map(|m| m.kind),
        Ok(EntryKind::Dir)
    );
    assert_eq!(
        s.run(disk.metadata(&path("/none"))),
        Err(DiskError::NotFound)
    );
    assert_eq!(s.run(disk.remove(&path("/d"))), Err(DiskError::NotEmpty));
    s.run(disk.remove(&path("/d/inner"))).unwrap();
    s.run(disk.remove(&path("/d"))).unwrap();
    assert_eq!(s.run(disk.remove(&path("/d"))), Err(DiskError::NotFound));
    assert!(matches!(
        s.run(disk.remove(&RtPath::root())),
        Err(DiskError::InvalidInput(_))
    ));
    // Rename replaces a file and refuses a directory.
    let a = s.open("/a", Access::ReadWrite, Create::Never).unwrap();
    let done = s.run(a.write_at(vec![write(0, b"from a")]));
    assert_eq!(done[0].result, Ok(()));
    drop(a);
    s.run(disk.rename(&path("/a"), &path("/b"))).unwrap();
    assert_eq!(
        s.run(disk.metadata(&path("/b"))),
        Ok(Metadata {
            kind: EntryKind::File,
            len: 6
        })
    );
    assert_eq!(s.run(disk.metadata(&path("/a"))), Err(DiskError::NotFound));
    s.run(disk.create_dir(&path("/e"))).unwrap();
    assert_eq!(
        s.run(disk.rename(&path("/b"), &path("/e"))),
        Err(DiskError::IsDirectory)
    );
    assert_eq!(
        s.run(disk.rename(&path("/none"), &path("/z"))),
        Err(DiskError::NotFound)
    );
    s.run(disk.sync_dir(&RtPath::root())).unwrap();
    s.run(disk.sync_dir(&path("/e"))).unwrap();
    assert_eq!(
        s.run(disk.sync_dir(&path("/b"))),
        Err(DiskError::NotDirectory)
    );
    assert_eq!(
        s.run(disk.sync_dir(&path("/none"))),
        Err(DiskError::NotFound)
    );
    s.finish();
}

#[test]
fn sync_waits_for_operations_issued_before_it() {
    let s = Scratch::new("sync");
    let file = s.open("/f", Access::ReadWrite, Create::New).unwrap();
    // The write is issued at the call. Dropping its future does not withdraw
    // it, and the sync waits for it.
    drop(file.write_at(vec![write(0, &[5; 4_096])]));
    s.run(file.sync(SyncKind::Data)).unwrap();
    assert_eq!(s.run(file.info()).unwrap().len, 4_096);
    drop(file);
    s.finish();
}

#[test]
fn plain_names_only_extend_the_root() {
    let s = Scratch::new("native");
    assert_eq!(s.disk.native(&RtPath::root()), Ok(s.dir.clone()));
    assert_eq!(s.disk.native(&path("/a/b")), Ok(s.dir.join("a").join("b")));
    // Names Windows would read as another file, a stream, or a device.
    let aliases = [
        "/C:",
        "/a:b",
        "/a.",
        "/b ",
        "/d/c.",
        "/CON",
        "/con",
        "/AUX.txt",
        "/nul .x",
        "/COM1",
        "/LPT9.log",
        "/COM\u{b9}",
        "/q?",
        "/s*",
        "/p|q",
        "/x<y",
        "/\"q\"",
        "/t\u{1}",
    ];
    for alias in aliases {
        let expected = if cfg!(windows) {
            Err(DiskError::InvalidPath)
        } else {
            Ok(s.dir.join(alias.trim_start_matches('/')))
        };
        assert_eq!(s.disk.native(&path(alias)), expected, "{alias}");
    }
    for plain in [
        "/COM10",
        "/CONSOLE",
        "/a.b",
        "/.a",
        "/a b",
        "/LPT",
        "/conx.txt",
    ] {
        assert_eq!(
            s.disk.native(&path(plain)),
            Ok(s.dir.join(&plain[1..])),
            "{plain}"
        );
    }
    s.finish();
}

#[test]
fn a_direct_read_across_the_end_returns_the_bytes_before_it() {
    let s = Scratch::new("direct-end");
    let direct = OpenOptions {
        access: Access::Read,
        create: Create::Never,
        direct: true,
        exclusive: false,
    };
    // The block size the platform reports for a direct handle in this
    // directory; a file system without direct I/O has nothing to check.
    drop(s.open("/probe", Access::ReadWrite, Create::New).unwrap());
    let block = match s.run(s.disk.open(&path("/probe"), direct)) {
        Ok(file) => s.run(file.info()).unwrap().block_size as usize,
        Err(DiskError::Unsupported) => return s.finish(),
        Err(e) => panic!("{e:?}"),
    };
    // The tail past the first block is shorter than a block and not a multiple
    // of 512: 904 bytes when the block is 4,096, 392 when it is 512.
    let tail = 904 % block;
    let len = block + tail;
    let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    let file = s.open("/f", Access::ReadWrite, Create::New).unwrap();
    assert_eq!(
        s.run(file.write_at(vec![write(0, &bytes)]))[0].result,
        Ok(())
    );
    s.run(file.sync(SyncKind::Data)).unwrap();
    drop(file);
    let file = s.run(s.disk.open(&path("/f"), direct)).unwrap();
    let done = s.run(file.read_at(vec![
        read(block as u64, block),
        read(0, 2 * block),
        read(2 * block as u64, block),
    ]));
    assert_eq!(done[0].result, Ok(tail));
    assert_eq!(&done[0].buf[..tail], &bytes[block..]);
    assert_eq!(done[1].result, Ok(len));
    assert_eq!(&done[1].buf[..len], &bytes[..]);
    assert_eq!(done[2].result, Ok(0));
    drop(file);
    s.finish();
}

/// A platform read over `file` that returns at most `limit(offset)` bytes.
fn scripted<'a>(
    file: &'a [u8],
    limit: impl Fn(u64) -> usize + 'a,
) -> impl FnMut(&mut [u8], u64) -> io::Result<usize> + 'a {
    move |buf, at| {
        let start = usize::try_from(at).unwrap().min(file.len());
        let n = buf.len().min(file.len() - start).min(limit(at));
        buf[..n].copy_from_slice(&file[start..start + n]);
        Ok(n)
    }
}

#[test]
fn a_direct_read_ends_only_at_the_end_of_the_file() {
    let file: Vec<u8> = (0..3 * 4_096 + 904).map(|i| (i % 251) as u8).collect();
    let len = || Ok(file.len() as u64);
    let unit = Some(4_096);

    // Short reads that stop on a 512-byte sector inside the file: the read
    // starts again at the last block boundary and fills the buffer.
    let mut buf = vec![0; 3 * 4_096];
    let sectors = scripted(&file, |at| {
        if at == 0 {
            4_096 + 1_536
        } else {
            4_096 + 2_560
        }
    });
    assert_eq!(fill(&mut buf, 0, unit, sectors, len), Ok(3 * 4_096));
    assert_eq!(buf, file[..3 * 4_096]);

    // A short read that stops at the end of the file ends the read there.
    let mut buf = vec![0; 2 * 4_096];
    let whole = scripted(&file, |_| usize::MAX);
    assert_eq!(fill(&mut buf, 2 * 4_096, unit, whole, len), Ok(4_096 + 904));
    assert_eq!(buf[..4_096 + 904], file[2 * 4_096..]);

    // A read past the end of the file returns no bytes.
    let mut buf = vec![0; 4_096];
    let whole = scripted(&file, |_| usize::MAX);
    assert_eq!(fill(&mut buf, 4 * 4_096, unit, whole, len), Ok(0));

    // A read that keeps stopping inside the file short of the next block
    // boundary makes no progress and fails.
    let mut buf = vec![0; 4_096];
    let stuck = scripted(&file, |_| 512);
    assert_eq!(fill(&mut buf, 0, unit, stuck, len), Err(DiskError::Io));

    // The length is read only when a read stops off a block boundary.
    let mut buf = vec![0; 2 * 4_096];
    let whole = scripted(&file, |_| usize::MAX);
    let no_len = || Err(io::Error::other("the length is not needed"));
    assert_eq!(fill(&mut buf, 0, unit, whole, no_len), Ok(2 * 4_096));
}

#[test]
fn a_buffered_read_goes_on_until_a_read_returns_zero() {
    let file: Vec<u8> = (0..5_000).map(|i| (i % 251) as u8).collect();
    let no_len = || Err(io::Error::other("a buffered read never needs the length"));
    let mut buf = vec![0; 6_000];
    let short = scripted(&file, |_| 333);
    assert_eq!(fill(&mut buf, 0, None, short, no_len), Ok(5_000));
    assert_eq!(buf[..5_000], file[..]);
}

#[test]
fn an_exclusive_open_creates_nothing() {
    for create in [Create::Never, Create::IfMissing, Create::New] {
        let exclusive = OpenOptions {
            access: Access::ReadWrite,
            create,
            direct: false,
            exclusive: true,
        };
        assert_eq!(platform_create(exclusive), Create::Never);
        let shared = OpenOptions {
            exclusive: false,
            ..exclusive
        };
        assert_eq!(platform_create(shared), create);
    }
}

#[test]
fn sync_on_a_read_handle_makes_the_file_durable() {
    let s = Scratch::new("sync-read");
    let writer = s.open("/f", Access::ReadWrite, Create::New).unwrap();
    let done = s.run(writer.write_at(vec![write(0, b"durable")]));
    assert_eq!(done[0].result, Ok(()));
    let reader = s.open("/f", Access::Read, Create::Never).unwrap();
    assert_eq!(s.run(reader.sync(SyncKind::Data)), Ok(()));
    assert_eq!(s.run(reader.sync(SyncKind::All)), Ok(()));
    drop(writer);
    // The handle is synced, not the name: renaming the file changes nothing.
    s.run(s.disk.rename(&path("/f"), &path("/g"))).unwrap();
    assert_eq!(s.run(reader.sync(SyncKind::All)), Ok(()));
    drop(reader);
    s.finish();
}
