//! The `os` disk-io (ARFC 0005 clauses 3.1.10 to 3.1.13): paths mapped below
//! a native root directory, every blocking call on the runtime's blocking
//! pool, one blocking call per batch.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::future::{Future, ready};
use std::io::{self, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::pin::pin;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, PoisonError};

use alloy_seam::Detail;
use tokio::sync::Notify;

use super::errors;
use super::{Host, Permit, Stopped};
use crate::{
    Access, BoxFuture, Create, DirEntry, DiskError, DiskFile, DiskIo, EntryKind, FileInfo, IoBuf,
    MAX_DISK_BATCH, Metadata, OpenOptions, ReadDone, ReadOp, RtPath, SyncKind, WriteDone, WriteOp,
};

/// The most open handles of one instance (ARFC 0005 section 7).
const MAX_OPEN: usize = 65_536;
/// The most entries one `list` returns (ARFC 0005 section 7).
const MAX_ENTRIES: usize = 1_048_576;
/// The block size when the platform does not tell (ARFC 0005 clause 3.1.11).
const DEFAULT_BLOCK_SIZE: u32 = 4_096;

/// Seam `disk-io`, implementation `os`.
pub(super) struct OsDiskIo {
    host: Host,
    /// The native directory that `RtPath` `/` names.
    root: PathBuf,
    /// Open handles, counted until each handle's file is closed.
    open: Arc<AtomicUsize>,
}

/// Writes the root directory and the count of open handles.
impl fmt::Debug for OsDiskIo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsDiskIo")
            .field("root", &self.root)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

fn invalid(detail: &str) -> DiskError {
    DiskError::InvalidInput(Detail::new(detail))
}

fn fail<'a, T: Send + 'a>(error: DiskError) -> BoxFuture<'a, Result<T, DiskError>> {
    Box::pin(ready(Err(error)))
}

impl OsDiskIo {
    pub(super) fn new(host: Host, root: PathBuf) -> OsDiskIo {
        OsDiskIo {
            host,
            root,
            open: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The native path of `path`: the root, then each component.
    ///
    /// # Errors
    ///
    /// [`DiskError::InvalidPath`] for a component the platform would not
    /// read as one plain name of its own: on Windows `C:`, which would replace
    /// the root rather than extend it, `a.`, which names `a`, or `CON`, which
    /// names a device.
    fn native(&self, path: &RtPath) -> Result<PathBuf, DiskError> {
        let mut native = self.root.clone();
        for component in path.as_str().split('/').filter(|c| !c.is_empty()) {
            let mut parts = Path::new(component).components();
            match (parts.next(), parts.next()) {
                (Some(Component::Normal(name)), None)
                    if name == OsStr::new(component) && platform::is_plain_name(component) =>
                {
                    native.push(component);
                }
                _ => return Err(DiskError::InvalidPath),
            }
        }
        Ok(native)
    }

    /// Runs `work` on the blocking pool with the native path of `path`.
    fn on_path<'a, T, F>(&'a self, path: &RtPath, work: F) -> BoxFuture<'a, Result<T, DiskError>>
    where
        T: Send + 'static,
        F: FnOnce(PathBuf) -> Result<T, DiskError> + Send + 'static,
    {
        match self.native(path) {
            Ok(native) => {
                let task = self.host.blocking(move || work(native));
                Box::pin(async move { task.await.unwrap_or(Err(DiskError::Io)) })
            }
            Err(e) => fail(e),
        }
    }
}

impl DiskIo for OsDiskIo {
    fn open<'a>(
        &'a self,
        path: &'a RtPath,
        options: OpenOptions,
    ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
        if options.create != Create::Never && options.access == Access::Read {
            return fail(invalid("creating a file needs Access::ReadWrite"));
        }
        let native = match self.native(path) {
            Ok(native) => native,
            Err(e) => return fail(e),
        };
        let Some(permit) = Permit::take(&self.open, MAX_OPEN) else {
            return fail(DiskError::TooManyOpen);
        };
        let host = self.host.clone();
        let task = self
            .host
            .blocking(move || open_native(&native, options).map(|opened| (opened, permit)));
        Box::pin(async move {
            let (opened, permit) = task.await.unwrap_or(Err(DiskError::Io))?;
            let file: Box<dyn DiskFile> = Box::new(OsDiskFile::new(host, opened, permit, options));
            Ok(file)
        })
    }

    fn metadata<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<Metadata, DiskError>> {
        self.on_path(path, |native| {
            let meta = fs::metadata(&native).map_err(|e| errors::disk(&e))?;
            let kind = kind_of(&meta.file_type());
            let len = if kind == EntryKind::Block {
                File::open(&native)
                    .and_then(|file| device_len(&file))
                    .unwrap_or(meta.len())
            } else {
                meta.len()
            };
            Ok(Metadata { kind, len })
        })
    }

    fn list<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<Vec<DirEntry>, DiskError>> {
        self.on_path(dir, |native| {
            let mut entries = Vec::new();
            for entry in fs::read_dir(&native).map_err(|e| errors::disk(&e))? {
                let entry = entry.map_err(|e| errors::disk(&e))?;
                if entries.len() == MAX_ENTRIES {
                    return Err(DiskError::TooManyEntries);
                }
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid("the directory holds a name that is not UTF-8"))?;
                let kind = match entry.file_type() {
                    // The `os` namespace follows symbolic links (clause 3.1.10).
                    Ok(t) if t.is_symlink() => fs::metadata(entry.path())
                        .map_or(EntryKind::Other, |meta| kind_of(&meta.file_type())),
                    Ok(t) => kind_of(&t),
                    Err(e) => return Err(errors::disk(&e)),
                };
                entries.push(DirEntry { name, kind });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        })
    }

    fn create_dir<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.on_path(path, |native| {
            fs::create_dir(native).map_err(|e| errors::disk(&e))
        })
    }

    fn remove<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        if path.parent().is_none() {
            return fail(invalid("the root cannot be removed"));
        }
        self.on_path(path, |native| {
            let meta = fs::symlink_metadata(&native).map_err(|e| errors::disk(&e))?;
            let removed = if meta.is_dir() {
                fs::remove_dir(&native)
            } else {
                fs::remove_file(&native)
            };
            removed.map_err(|e| errors::disk(&e))
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a RtPath,
        to: &'a RtPath,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        if from.parent().is_none() || to.parent().is_none() {
            return fail(invalid("the root cannot be renamed or replaced"));
        }
        let target = match self.native(to) {
            Ok(target) => target,
            Err(e) => return fail(e),
        };
        self.on_path(from, move |source| {
            if fs::metadata(&target).is_ok_and(|meta| meta.is_dir()) {
                return Err(DiskError::IsDirectory);
            }
            fs::rename(&source, &target).map_err(|e| errors::disk(&e))
        })
    }

    fn sync_dir<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        self.on_path(dir, |native| {
            let meta = fs::metadata(&native).map_err(|e| errors::disk(&e))?;
            if !meta.is_dir() {
                return Err(DiskError::NotDirectory);
            }
            platform::open_dir(&native)
                .and_then(|file| file.sync_all())
                .map_err(|e| errors::disk(&e))
        })
    }
}

fn kind_of(file_type: &fs::FileType) -> EntryKind {
    if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if platform::is_block_device(file_type) {
        EntryKind::Block
    } else {
        EntryKind::Other
    }
}

/// The length of a block device: its end, since its metadata length is 0 on
/// Linux.
fn device_len(file: &File) -> io::Result<u64> {
    let mut file = file;
    file.seek(SeekFrom::End(0))
}

/// What [`open_native`] found and opened.
#[derive(Debug)]
struct Opened {
    file: File,
    kind: EntryKind,
    /// The length of a block device, 0 for other kinds.
    device_len: u64,
    block_size: u32,
}

/// Opens `path` with `options` (ARFC 0005 clause 3.1.10).
fn open_native(path: &Path, options: OpenOptions) -> Result<Opened, DiskError> {
    let existing = match fs::metadata(path) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(errors::disk(&e)),
    };
    let kind = match existing {
        Some(meta) if meta.is_dir() => return Err(DiskError::IsDirectory),
        Some(_) if options.create == Create::New => return Err(DiskError::AlreadyExists),
        Some(meta) => kind_of(&meta.file_type()),
        None if options.create == Create::Never => return Err(DiskError::NotFound),
        None => EntryKind::File,
    };
    if options.exclusive && kind != EntryKind::Block {
        return Err(invalid("exclusive applies to block devices only"));
    }
    let mut std_options = fs::OpenOptions::new();
    std_options
        .read(true)
        .write(options.access == Access::ReadWrite);
    match platform_create(options) {
        Create::Never => {}
        Create::IfMissing => {
            std_options.create(true);
        }
        Create::New => {
            std_options.create_new(true);
        }
    }
    platform::set_flags(&mut std_options, options)?;
    let file = std_options.open(path).map_err(|e| {
        if options.direct && errors::invalid_argument(&e) {
            DiskError::Unsupported
        } else {
            errors::disk(&e)
        }
    })?;
    if options.direct {
        platform::bypass_cache(&file).map_err(|e| errors::disk(&e))?;
    }
    let meta = file.metadata().map_err(|e| errors::disk(&e))?;
    if meta.is_dir() {
        return Err(DiskError::IsDirectory);
    }
    let kind = kind_of(&meta.file_type());
    let device_len = if kind == EntryKind::Block {
        device_len(&file).map_err(|e| errors::disk(&e))?
    } else {
        0
    };
    let block_size = platform::block_size(&meta, kind);
    Ok(Opened {
        file,
        kind,
        device_len,
        block_size,
    })
}

/// The create mode to hand the platform for `options`. An exclusive open is
/// of a block device that exists, as `open_native` checks first, so it creates
/// nothing: with a create flag, Linux reads `O_EXCL` as "fail if it exists"
/// rather than "fail while another holder has the device".
fn platform_create(options: OpenOptions) -> Create {
    if options.exclusive {
        Create::Never
    } else {
        options.create
    }
}

/// An open file or block device, implementation `os`.
struct OsDiskFile {
    host: Host,
    /// The file, closed when the handle and every operation issued on it have
    /// been dropped.
    shared: Arc<SharedFile>,
    kind: EntryKind,
    access: Access,
    direct: bool,
    device_len: u64,
    block_size: u32,
    inflight: Arc<Inflight>,
}

/// The file and its place in the instance's count of open handles.
#[derive(Debug)]
struct SharedFile {
    file: File,
    _permit: Permit,
}

/// Writes the kind, access, and block size, never data.
impl fmt::Debug for OsDiskFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsDiskFile")
            .field("kind", &self.kind)
            .field("access", &self.access)
            .field("direct", &self.direct)
            .field("block_size", &self.block_size)
            .finish_non_exhaustive()
    }
}

impl OsDiskFile {
    fn new(host: Host, opened: Opened, permit: Permit, options: OpenOptions) -> OsDiskFile {
        OsDiskFile {
            host,
            shared: Arc::new(SharedFile {
                file: opened.file,
                _permit: permit,
            }),
            kind: opened.kind,
            access: options.access,
            direct: options.direct,
            device_len: opened.device_len,
            block_size: opened.block_size,
            inflight: Arc::new(Inflight::default()),
        }
    }

    /// The error that fails a whole batch of operations at `ranges`, as
    /// `(offset, length)`, or `None` (ARFC 0005 clause 3.1.11).
    fn reject(&self, ranges: &[(u64, usize)], write: bool) -> Option<DiskError> {
        if ranges.len() > MAX_DISK_BATCH {
            return Some(DiskError::BatchTooLarge);
        }
        let block = u64::from(self.block_size);
        let unaligned = |&(offset, len): &(u64, usize)| {
            !offset.is_multiple_of(block) || !(len as u64).is_multiple_of(block)
        };
        if self.direct && ranges.iter().any(unaligned) {
            return Some(DiskError::Unaligned);
        }
        if write && self.access == Access::Read {
            return Some(DiskError::ReadOnly);
        }
        let end = |&(offset, len): &(u64, usize)| u128::from(offset) + len as u128;
        if write {
            let mut spans: Vec<(u128, u128)> = ranges
                .iter()
                .filter(|(_, len)| *len > 0)
                .map(|range| (u128::from(range.0), end(range)))
                .collect();
            spans.sort_unstable();
            if spans.windows(2).any(|pair| pair[0].1 > pair[1].0) {
                return Some(invalid("two writes of the batch overlap"));
            }
        }
        if self.kind == EntryKind::Block
            && ranges
                .iter()
                .any(|range| end(range) > u128::from(self.device_len))
        {
            return Some(invalid("an operation passes the end of the block device"));
        }
        None
    }
}

/// Results for operations whose buffers were lost because the host stopped:
/// a zeroed buffer of each length and [`DiskError::Io`].
fn lost<T>(lens: &[usize], done: impl Fn(IoBuf) -> T) -> Vec<T> {
    lens.iter()
        .map(|&len| {
            // Each length is that of an existing `IoBuf`, so it is at most
            // `IoBuf::MAX_LEN` and `zeroed` accepts it.
            let buf = IoBuf::zeroed(len).expect("the length of an IoBuf is valid");
            done(buf)
        })
        .collect()
}

impl DiskFile for OsDiskFile {
    fn info(&self) -> BoxFuture<'_, Result<FileInfo, DiskError>> {
        let shared = Arc::clone(&self.shared);
        let (kind, device_len, block_size) = (self.kind, self.device_len, self.block_size);
        let task = self.host.blocking(move || {
            let len = if kind == EntryKind::Block {
                device_len
            } else {
                shared.file.metadata().map_err(|e| errors::disk(&e))?.len()
            };
            Ok(FileInfo {
                kind,
                len,
                block_size,
            })
        });
        Box::pin(async move { task.await.unwrap_or(Err(DiskError::Io)) })
    }

    fn read_at(&self, ops: Vec<ReadOp>) -> BoxFuture<'_, Vec<ReadDone>> {
        let ranges: Vec<(u64, usize)> = ops.iter().map(|op| (op.offset, op.buf.len())).collect();
        if let Some(error) = self.reject(&ranges, false) {
            let done = ops
                .into_iter()
                .map(|op| ReadDone {
                    buf: op.buf,
                    result: Err(error.clone()),
                })
                .collect();
            return Box::pin(ready(done));
        }
        if ops.is_empty() {
            return Box::pin(ready(Vec::new()));
        }
        let lens: Vec<usize> = ranges.iter().map(|range| range.1).collect();
        let shared = Arc::clone(&self.shared);
        let ticket = self.inflight.issue();
        let unit = self.direct.then_some(self.block_size as usize);
        let device_len = (self.kind == EntryKind::Block).then_some(self.device_len);
        let task = self.host.blocking(move || {
            let _ticket = ticket;
            ops.into_iter()
                .map(|op| read_one(&shared.file, op, unit, device_len))
                .collect::<Vec<_>>()
        });
        Box::pin(async move {
            task.await.unwrap_or_else(|Stopped| {
                lost(&lens, |buf| ReadDone {
                    buf,
                    result: Err(DiskError::Io),
                })
            })
        })
    }

    fn write_at(&self, ops: Vec<WriteOp>) -> BoxFuture<'_, Vec<WriteDone>> {
        let ranges: Vec<(u64, usize)> = ops.iter().map(|op| (op.offset, op.buf.len())).collect();
        if let Some(error) = self.reject(&ranges, true) {
            let done = ops
                .into_iter()
                .map(|op| WriteDone {
                    buf: op.buf,
                    result: Err(error.clone()),
                })
                .collect();
            return Box::pin(ready(done));
        }
        if ops.is_empty() {
            return Box::pin(ready(Vec::new()));
        }
        let lens: Vec<usize> = ranges.iter().map(|range| range.1).collect();
        let shared = Arc::clone(&self.shared);
        let ticket = self.inflight.issue();
        let task = self.host.blocking(move || {
            let _ticket = ticket;
            ops.into_iter()
                .map(|op| write_one(&shared.file, op))
                .collect::<Vec<_>>()
        });
        Box::pin(async move {
            task.await.unwrap_or_else(|Stopped| {
                lost(&lens, |buf| WriteDone {
                    buf,
                    result: Err(DiskError::Io),
                })
            })
        })
    }

    fn sync(&self, kind: SyncKind) -> BoxFuture<'_, Result<(), DiskError>> {
        let barrier = self.inflight.barrier();
        let shared = Arc::clone(&self.shared);
        let host = self.host.clone();
        let access = self.access;
        Box::pin(async move {
            barrier.await;
            let task = host.blocking(move || {
                platform::sync(&shared.file, access, kind).map_err(|e| errors::disk(&e))
            });
            task.await.unwrap_or(Err(DiskError::Io))
        })
    }

    fn truncate(&self, len: u64) -> BoxFuture<'_, Result<(), DiskError>> {
        if self.kind == EntryKind::Block {
            return fail(DiskError::Unsupported);
        }
        if self.access == Access::Read {
            return fail(DiskError::ReadOnly);
        }
        let shared = Arc::clone(&self.shared);
        let ticket = self.inflight.issue();
        let task = self.host.blocking(move || {
            let _ticket = ticket;
            shared.file.set_len(len).map_err(|e| errors::disk(&e))
        });
        Box::pin(async move { task.await.unwrap_or(Err(DiskError::Io)) })
    }
}

/// Reads `op` whole, or up to the end of the file. On a direct handle `unit`
/// is the block size; `device_len` is the length of a block device, and
/// `None` for a regular file, whose length is read when it is needed.
fn read_one(file: &File, op: ReadOp, unit: Option<usize>, device_len: Option<u64>) -> ReadDone {
    let ReadOp { offset, mut buf } = op;
    let len = || match device_len {
        Some(len) => Ok(len),
        None => file.metadata().map(|meta| meta.len()),
    };
    let result = fill(
        &mut buf,
        offset,
        unit,
        |b, at| platform::read_at(file, b, at),
        len,
    );
    ReadDone { buf, result }
}

/// Fills `buf` from `offset` with `read`, until it is full, a read returns 0,
/// or the file of length `len` ends (ARFC 0005 clause 3.1.11).
///
/// On a direct handle (`unit` is the block size) a read may stop off a block
/// boundary. That is the end of the file only if `len` says so. Reading on
/// from there would hand the platform an unaligned offset and buffer, which
/// it may refuse, so inside the file the read starts again at the last block
/// boundary. A read that stops inside the file short of the next boundary
/// makes no progress, and fails with [`DiskError::Io`] rather than loop.
fn fill(
    buf: &mut [u8],
    offset: u64,
    unit: Option<usize>,
    mut read: impl FnMut(&mut [u8], u64) -> io::Result<usize>,
    len: impl Fn() -> io::Result<u64>,
) -> Result<usize, DiskError> {
    let mut done = 0;
    loop {
        if done == buf.len() {
            return Ok(done);
        }
        let Some(at) = offset.checked_add(done as u64) else {
            return Err(invalid("the operation passes offset 2^64"));
        };
        match read(&mut buf[done..], at) {
            Ok(0) => return Ok(done),
            Ok(n) => {
                let before = done;
                done += n;
                let Some(unit) = unit else { continue };
                if done.is_multiple_of(unit) {
                    continue;
                }
                let end = u128::from(offset) + done as u128;
                if end >= u128::from(len().map_err(|e| errors::disk(&e))?) {
                    return Ok(done);
                }
                done -= done % unit;
                if done == before {
                    return Err(DiskError::Io);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(errors::disk(&e)),
        }
    }
}

fn write_one(file: &File, op: WriteOp) -> WriteDone {
    let WriteOp { offset, buf } = op;
    let mut done = 0;
    let result = loop {
        if done == buf.len() {
            break Ok(());
        }
        let Some(at) = offset.checked_add(done as u64) else {
            break Err(invalid("the operation passes offset 2^64"));
        };
        match platform::write_at(file, &buf[done..], at) {
            Ok(0) => break Err(DiskError::Io),
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Err(errors::disk(&e)),
        }
    };
    WriteDone { buf, result }
}

/// The operations issued on one handle and not finished yet, so that `sync`
/// can wait for those issued before it (ARFC 0005 clause 3.1.11).
#[derive(Debug, Default)]
struct Inflight {
    state: Mutex<InflightState>,
    finished: Notify,
}

#[derive(Debug, Default)]
struct InflightState {
    /// The number the next operation gets.
    next: u64,
    /// The numbers of the unfinished operations.
    pending: BTreeSet<u64>,
}

/// One unfinished operation. Dropping it marks the operation finished.
#[derive(Debug)]
struct Ticket {
    inflight: Arc<Inflight>,
    number: u64,
}

impl Inflight {
    fn lock(&self) -> std::sync::MutexGuard<'_, InflightState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn issue(self: &Arc<Self>) -> Ticket {
        let mut state = self.lock();
        let number = state.next;
        state.next += 1;
        state.pending.insert(number);
        Ticket {
            inflight: Arc::clone(self),
            number,
        }
    }

    /// Completes once every operation issued before this call has finished.
    fn barrier(self: &Arc<Self>) -> impl Future<Output = ()> + Send + 'static {
        let limit = self.lock().next;
        let inflight = Arc::clone(self);
        async move {
            loop {
                let mut finished = pin!(inflight.finished.notified());
                finished.as_mut().enable();
                let drained = inflight
                    .lock()
                    .pending
                    .first()
                    .is_none_or(|first| *first >= limit);
                if drained {
                    return;
                }
                finished.await;
            }
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.inflight.lock().pending.remove(&self.number);
        self.inflight.finished.notify_waiters();
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs::{self, File, Metadata};
    use std::io;
    use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt};
    use std::path::Path;

    use super::DEFAULT_BLOCK_SIZE;
    use crate::{Access, DiskError, EntryKind, OpenOptions, SyncKind};

    /// Makes the file's completed writes durable, and with `SyncKind::All`
    /// its metadata, through any handle of it, a read handle included.
    pub(super) fn sync(file: &File, _access: Access, kind: SyncKind) -> io::Result<()> {
        match kind {
            SyncKind::Data => file.sync_data(),
            SyncKind::All => file.sync_all(),
        }
    }

    pub(super) fn is_block_device(file_type: &fs::FileType) -> bool {
        file_type.is_block_device()
    }

    /// Every component `RtPath` accepts names itself here.
    pub(super) fn is_plain_name(_name: &str) -> bool {
        true
    }

    pub(super) fn set_flags(
        std_options: &mut fs::OpenOptions,
        options: OpenOptions,
    ) -> Result<(), DiskError> {
        let mut flags = 0;
        if options.direct {
            flags |= libc::O_DIRECT;
        }
        if options.exclusive {
            // On a block device without O_CREAT, O_EXCL fails with EBUSY while
            // the device is mounted or held open exclusively.
            flags |= libc::O_EXCL;
        }
        std_options.custom_flags(flags);
        Ok(())
    }

    pub(super) fn bypass_cache(_file: &File) -> io::Result<()> {
        Ok(())
    }

    /// The logical block size of the device that holds the file, or of the
    /// block device itself, from sysfs.
    pub(super) fn block_size(meta: &Metadata, kind: EntryKind) -> u32 {
        let device = if kind == EntryKind::Block {
            meta.rdev()
        } else {
            meta.dev()
        };
        let major = ((device >> 8) & 0xfff) | ((device >> 32) & !0xfff);
        let minor = (device & 0xff) | ((device >> 12) & !0xff);
        let base = format!("/sys/dev/block/{major}:{minor}");
        // A partition has no queue of its own; its disk, one level up, does.
        ["queue", "../queue"]
            .iter()
            .find_map(|dir| {
                fs::read_to_string(Path::new(&base).join(dir).join("logical_block_size")).ok()
            })
            .and_then(|text| text.trim().parse::<u32>().ok())
            .map_or(DEFAULT_BLOCK_SIZE, |size| {
                if size == 512 { 512 } else { DEFAULT_BLOCK_SIZE }
            })
    }

    pub(super) fn open_dir(path: &Path) -> io::Result<File> {
        File::open(path)
    }

    pub(super) fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        file.read_at(buf, offset)
    }

    pub(super) fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
        file.write_at(buf, offset)
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod platform {
    use std::fs::{self, File, Metadata};
    use std::io;
    use std::os::unix::fs::{FileExt, FileTypeExt};
    use std::path::Path;

    use super::DEFAULT_BLOCK_SIZE;
    use crate::{Access, DiskError, EntryKind, OpenOptions, SyncKind};

    /// Makes the file's completed writes durable, and with `SyncKind::All`
    /// its metadata, through any handle of it, a read handle included.
    pub(super) fn sync(file: &File, _access: Access, kind: SyncKind) -> io::Result<()> {
        match kind {
            SyncKind::Data => file.sync_data(),
            SyncKind::All => file.sync_all(),
        }
    }

    pub(super) fn is_block_device(file_type: &fs::FileType) -> bool {
        file_type.is_block_device()
    }

    /// Every component `RtPath` accepts names itself here.
    pub(super) fn is_plain_name(_name: &str) -> bool {
        true
    }

    pub(super) fn set_flags(
        _std_options: &mut fs::OpenOptions,
        options: OpenOptions,
    ) -> Result<(), DiskError> {
        if options.exclusive {
            // Only Linux reports a held block device at open.
            return Err(DiskError::Unsupported);
        }
        if options.direct && !cfg!(target_vendor = "apple") {
            return Err(DiskError::Unsupported);
        }
        Ok(())
    }

    #[cfg(target_vendor = "apple")]
    #[allow(unsafe_code, reason = "fcntl(F_NOCACHE) has no safe binding")]
    pub(super) fn bypass_cache(file: &File) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: `file` owns an open descriptor for the whole call, and
        // F_NOCACHE takes an integer argument and touches no memory of ours.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(not(target_vendor = "apple"))]
    pub(super) fn bypass_cache(_file: &File) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn block_size(_meta: &Metadata, _kind: EntryKind) -> u32 {
        DEFAULT_BLOCK_SIZE
    }

    pub(super) fn open_dir(path: &Path) -> io::Result<File> {
        File::open(path)
    }

    pub(super) fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        file.read_at(buf, offset)
    }

    pub(super) fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
        file.write_at(buf, offset)
    }
}

#[cfg(windows)]
mod platform {
    use std::fs::{self, File, Metadata};
    use std::io;
    use std::os::windows::fs::{FileExt, OpenOptionsExt};
    use std::path::Path;

    use super::DEFAULT_BLOCK_SIZE;
    use crate::{Access, DiskError, EntryKind, OpenOptions, SyncKind};

    /// `FILE_FLAG_NO_BUFFERING`.
    const NO_BUFFERING: u32 = 0x2000_0000;
    /// `FILE_FLAG_BACKUP_SEMANTICS`, which lets a directory be opened.
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;

    /// Makes the file's completed writes durable, and with `SyncKind::All`
    /// its metadata. Windows refuses to flush a handle without write access,
    /// so a read handle is flushed through a write handle of the same open
    /// file, which fails as the platform decides, never silently.
    pub(super) fn sync(file: &File, access: Access, kind: SyncKind) -> io::Result<()> {
        let writable;
        let file = if access == Access::Read {
            writable = reopen_for_write(file)?;
            &writable
        } else {
            file
        };
        match kind {
            SyncKind::Data => file.sync_data(),
            SyncKind::All => file.sync_all(),
        }
    }

    /// A new handle with write access to the file `file` has open, whatever
    /// its path is now.
    #[allow(unsafe_code, reason = "ReOpenFile has no safe binding")]
    fn reopen_for_write(file: &File) -> io::Result<File> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, ReOpenFile,
        };

        let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        // SAFETY: `file` owns an open handle for the whole call. ReOpenFile
        // reads only the handle value and returns a new handle or
        // INVALID_HANDLE_VALUE.
        let handle = unsafe { ReOpenFile(file.as_raw_handle(), GENERIC_WRITE, share, 0) };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handle` is a new open handle that nothing else owns, so
        // the `OwnedHandle` is its only owner and closes it once.
        Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    pub(super) fn is_block_device(_file_type: &fs::FileType) -> bool {
        false
    }

    /// The device names Windows reserves in every directory, with or without
    /// an extension.
    const RESERVED: [&str; 30] = [
        "CON",
        "PRN",
        "AUX",
        "NUL",
        "CONIN$",
        "CONOUT$",
        "COM1",
        "COM2",
        "COM3",
        "COM4",
        "COM5",
        "COM6",
        "COM7",
        "COM8",
        "COM9",
        "COM\u{b9}",
        "COM\u{b2}",
        "COM\u{b3}",
        "LPT1",
        "LPT2",
        "LPT3",
        "LPT4",
        "LPT5",
        "LPT6",
        "LPT7",
        "LPT8",
        "LPT9",
        "LPT\u{b9}",
        "LPT\u{b2}",
        "LPT\u{b3}",
    ];

    /// Whether Windows reads `name` as a file of exactly that name: no
    /// character it forbids or reads as a stream separator, no trailing dot
    /// or space, which it strips, and no reserved device name.
    pub(super) fn is_plain_name(name: &str) -> bool {
        const FORBIDDEN: &[char] = &['<', '>', ':', '"', '|', '?', '*'];
        if name
            .chars()
            .any(|c| c.is_ascii_control() || FORBIDDEN.contains(&c))
            || name.ends_with(['.', ' '])
        {
            return false;
        }
        let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
        !RESERVED
            .iter()
            .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    }

    pub(super) fn set_flags(
        std_options: &mut fs::OpenOptions,
        options: OpenOptions,
    ) -> Result<(), DiskError> {
        if options.direct {
            std_options.custom_flags(NO_BUFFERING);
        }
        Ok(())
    }

    pub(super) fn bypass_cache(_file: &File) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn block_size(_meta: &Metadata, _kind: EntryKind) -> u32 {
        DEFAULT_BLOCK_SIZE
    }

    pub(super) fn open_dir(path: &Path) -> io::Result<File> {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(BACKUP_SEMANTICS)
            .open(path)
    }

    pub(super) fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        file.seek_read(buf, offset)
    }

    pub(super) fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
        file.seek_write(buf, offset)
    }
}

#[cfg(test)]
mod tests;
