//! The `disk-io` seam (ARFC 0005 section 2.4 and clauses 3.1.10 to 3.1.13).

use std::fmt;
use std::ops::{Deref, DerefMut};

use alloy_seam::Detail;

use crate::BoxFuture;

/// The most operations in one [`DiskFile::read_at`] or [`DiskFile::write_at`]
/// batch.
pub const MAX_DISK_BATCH: usize = 256;

/// A path below a [`DiskIo`] instance's root (ARFC 0005 clause 3.1.10).
///
/// Either `/` alone, or `/` followed by components of 1 to 255 bytes separated
/// by single `/`, with no trailing `/`, no component `.` or `..`, no NUL and no
/// `\`, and at most 4,096 bytes in all. A value of this type always satisfies
/// these rules.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RtPath(String);

impl RtPath {
    /// The longest path, in bytes.
    const MAX_LEN: usize = 4_096;
    /// The longest component, in bytes.
    const MAX_COMPONENT: usize = 255;

    /// The path `/`, which names the instance's root.
    pub fn root() -> RtPath {
        RtPath("/".to_owned())
    }

    /// Accepts exactly the strings that satisfy the rules of [`RtPath`].
    ///
    /// # Errors
    ///
    /// [`DiskError::InvalidPath`] for any other string.
    pub fn new(path: &str) -> Result<RtPath, DiskError> {
        if Self::is_valid(path) {
            Ok(RtPath(path.to_owned()))
        } else {
            Err(DiskError::InvalidPath)
        }
    }

    /// The path of `component` inside this directory.
    ///
    /// # Errors
    ///
    /// [`DiskError::InvalidPath`] when `component` is not one valid component
    /// (it is empty, longer than 255 bytes, `.`, `..`, or holds `/`, `\`, or
    /// NUL), or when the result is longer than 4,096 bytes.
    pub fn join(&self, component: &str) -> Result<RtPath, DiskError> {
        if !Self::is_component(component) {
            return Err(DiskError::InvalidPath);
        }
        let joined = if self.0 == "/" {
            format!("/{component}")
        } else {
            format!("{}/{component}", self.0)
        };
        RtPath::new(&joined)
    }

    /// The directory holding this path. `None` for the root.
    pub fn parent(&self) -> Option<RtPath> {
        if self.0 == "/" {
            return None;
        }
        match self.0.rfind('/')? {
            0 => Some(RtPath::root()),
            index => Some(RtPath(self.0[..index].to_owned())),
        }
    }

    /// The last component. `None` for the root.
    pub fn file_name(&self) -> Option<&str> {
        if self.0 == "/" {
            return None;
        }
        self.0.rsplit('/').next()
    }

    /// The path as a string, starting with `/`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_component(component: &str) -> bool {
        (1..=Self::MAX_COMPONENT).contains(&component.len())
            && component != "."
            && component != ".."
            && !component.contains(['/', '\\', '\0'])
    }

    fn is_valid(path: &str) -> bool {
        if path == "/" {
            return true;
        }
        match path.strip_prefix('/') {
            Some(rest) if path.len() <= Self::MAX_LEN => rest.split('/').all(Self::is_component),
            _ => false,
        }
    }
}

/// Seam `disk-io`: files, directories, and block devices below a root (ARFC
/// 0005 clauses 3.1.10 to 3.1.12).
///
/// Methods take `&self`, may be called concurrently from any task, and never
/// panic: invalid input returns an error. `mem` never returns
/// [`DiskError::Os`]. The root is not a security boundary for `os`, which
/// follows symbolic links.
pub trait DiskIo: Send + Sync + fmt::Debug + 'static {
    /// Opens a file or block device.
    ///
    /// # Errors
    ///
    /// [`DiskError::InvalidInput`] for `Create::IfMissing` or `Create::New`
    /// with `Access::Read`, and for `exclusive` on anything but a block device.
    /// [`DiskError::NotFound`] for a missing path with `Create::Never` or a
    /// missing parent, [`DiskError::AlreadyExists`] for an existing path with
    /// `Create::New`, [`DiskError::IsDirectory`] for a directory,
    /// [`DiskError::Busy`] for an exclusive open of a held device, and
    /// [`DiskError::TooManyOpen`] at 65,536 open handles.
    fn open<'a>(
        &'a self,
        path: &'a RtPath,
        options: OpenOptions,
    ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>>;

    /// The kind and length of the entry at `path`.
    ///
    /// # Errors
    ///
    /// [`DiskError::NotFound`] when there is none.
    fn metadata<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<Metadata, DiskError>>;

    /// Every entry of `dir` except `.` and `..`, sorted by name, ascending
    /// bytewise.
    ///
    /// # Errors
    ///
    /// [`DiskError::NotFound`], [`DiskError::NotDirectory`], and
    /// [`DiskError::TooManyEntries`] above 1,048,576 entries.
    fn list<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<Vec<DirEntry>, DiskError>>;

    /// Creates the directory `path` in an existing parent.
    ///
    /// # Errors
    ///
    /// [`DiskError::NotFound`] for a missing parent and
    /// [`DiskError::AlreadyExists`] for an existing path.
    fn create_dir<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>>;

    /// Removes a file or an empty directory.
    ///
    /// # Errors
    ///
    /// [`DiskError::NotFound`], and [`DiskError::NotEmpty`] for a directory
    /// with entries.
    fn remove<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>>;

    /// Renames `from` to `to`, atomically replacing a file at `to`. Never half
    /// done, also across a crash.
    ///
    /// # Errors
    ///
    /// [`DiskError::NotFound`], and [`DiskError::IsDirectory`] when `to` is a
    /// directory.
    fn rename<'a>(
        &'a self,
        from: &'a RtPath,
        to: &'a RtPath,
    ) -> BoxFuture<'a, Result<(), DiskError>>;

    /// Makes the creates, removes, and renames of entries of `dir` durable
    /// (ARFC 0005 clause 3.1.12).
    ///
    /// # Errors
    ///
    /// [`DiskError::NotFound`], [`DiskError::NotDirectory`], and
    /// [`DiskError::Io`].
    fn sync_dir<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>>;
}

/// Whether a handle may write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Reads only. Writes and truncation fail with [`DiskError::ReadOnly`].
    Read,
    /// Reads and writes.
    ReadWrite,
}

/// What [`DiskIo::open`] does when the path is missing or present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Create {
    /// Fail with `NotFound` when missing.
    Never,
    /// Create the file when missing, open it when present.
    IfMissing,
    /// Fail with `AlreadyExists` when present.
    New,
}

/// Options of [`DiskIo::open`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenOptions {
    /// Whether the handle may write.
    pub access: Access,
    /// What to do when the path is missing or present.
    pub create: Create,
    /// Bypass the host's cache. Offsets and lengths must be multiples of the
    /// file's `block_size`.
    pub direct: bool,
    /// Block devices only: fail with `Busy` while another holder has it.
    pub exclusive: bool,
}

/// The kind of a directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A block device.
    Block,
    /// Anything else, such as a socket or a character device.
    Other,
}

/// What [`DiskIo::metadata`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    /// The entry's kind.
    pub kind: EntryKind,
    /// The length in bytes: of the file, of the device, or as the platform
    /// reports for other kinds.
    pub len: u64,
}

/// One entry of a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's name: one path component.
    pub name: String,
    /// The entry's kind.
    pub kind: EntryKind,
}

/// An open file or block device (ARFC 0005 clause 3.1.11). Dropping it closes
/// it once its pending operations have finished.
pub trait DiskFile: Send + Sync + fmt::Debug {
    /// The kind, length, and logical block size.
    ///
    /// # Errors
    ///
    /// [`DiskError::Io`] or a platform error.
    fn info(&self) -> BoxFuture<'_, Result<FileInfo, DiskError>>;

    /// Reads each operation's buffer length at its offset. One result per
    /// operation, in the order given, each with its buffer. A read returns
    /// fewer bytes than its buffer only at end of file.
    ///
    /// A batch longer than [`MAX_DISK_BATCH`], an unaligned operation on a
    /// direct handle, or an operation past the end of a block device fails
    /// every operation of the batch, with `BatchTooLarge`, `Unaligned`, or
    /// `InvalidInput`, and performs none. A dropped future's operations may
    /// still run, and their buffers are lost.
    fn read_at(&self, ops: Vec<ReadOp>) -> BoxFuture<'_, Vec<ReadDone>>;

    /// Writes each operation's whole buffer at its offset, extending a regular
    /// file and filling any gap with zeros. One result per operation, in the
    /// order given, each with its buffer. A completed write is seen by every
    /// read that starts after it completes.
    ///
    /// A batch longer than [`MAX_DISK_BATCH`], an unaligned operation on a
    /// direct handle, a write on an `Access::Read` handle, two writes with
    /// overlapping byte ranges, or an operation past the end of a block device
    /// fails every operation of the batch, with `BatchTooLarge`, `Unaligned`,
    /// `ReadOnly`, or `InvalidInput`, and performs none. A dropped future's
    /// writes may still run, with the outcome of an unsynced write at a crash
    /// until a later `sync` completes.
    fn write_at(&self, ops: Vec<WriteOp>) -> BoxFuture<'_, Vec<WriteDone>>;

    /// Waits for every operation issued on the handle before the call,
    /// including those of dropped futures, then makes durable every write that
    /// completed on the handle before the call and the length those writes
    /// need; with [`SyncKind::All`] also the file's metadata.
    ///
    /// # Errors
    ///
    /// [`DiskError::Io`] or a platform error.
    fn sync(&self, kind: SyncKind) -> BoxFuture<'_, Result<(), DiskError>>;

    /// Sets the length of a regular file, filling with zeros when it grows.
    ///
    /// # Errors
    ///
    /// [`DiskError::Unsupported`] on a block device, [`DiskError::ReadOnly`] on
    /// an `Access::Read` handle.
    fn truncate(&self, len: u64) -> BoxFuture<'_, Result<(), DiskError>>;
}

/// What [`DiskFile::info`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileInfo {
    /// The kind of the open entry.
    pub kind: EntryKind,
    /// The length in bytes.
    pub len: u64,
    /// The logical block size of the device that holds the file: 512 or 4,096.
    pub block_size: u32,
}

/// What [`DiskFile::sync`] makes durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncKind {
    /// Data and the length needed to read it back.
    Data,
    /// Data and all metadata.
    All,
}

/// Reads `buf.len()` bytes at `offset` into `buf`.
#[derive(Debug)]
pub struct ReadOp {
    /// The byte offset in the file.
    pub offset: u64,
    /// The buffer to fill.
    pub buf: IoBuf,
}

/// The result of one [`ReadOp`].
#[derive(Debug)]
pub struct ReadDone {
    /// The operation's buffer, returned.
    pub buf: IoBuf,
    /// Bytes read. Fewer than `buf.len()` only at end of file.
    pub result: Result<usize, DiskError>,
}

/// Writes all of `buf` at `offset`.
#[derive(Debug)]
pub struct WriteOp {
    /// The byte offset in the file.
    pub offset: u64,
    /// The bytes to write.
    pub buf: IoBuf,
}

/// The result of one [`WriteOp`].
#[derive(Debug)]
pub struct WriteDone {
    /// The operation's buffer, returned.
    pub buf: IoBuf,
    /// Whether all of the buffer was written.
    pub result: Result<(), DiskError>,
}

/// A zero-initialized byte buffer whose first byte is at an address that is a
/// multiple of 4,096, so it serves direct I/O on any block size (ARFC 0005
/// clause 3.1.13). At most 67,108,864 bytes.
///
/// Its `Debug` output holds its length only, never its bytes (I-22).
pub struct IoBuf {
    /// `len + ALIGN - 1` bytes, of which `offset..offset + len` are the
    /// buffer, or empty when `len` is 0. A boxed slice never moves its bytes,
    /// so the alignment computed at allocation holds for its lifetime.
    storage: Box<[u8]>,
    offset: usize,
    len: usize,
}

impl IoBuf {
    /// The alignment of the first byte, in bytes.
    pub const ALIGN: usize = 4_096;
    /// The largest buffer, in bytes.
    pub const MAX_LEN: usize = 67_108_864;

    /// A buffer of `len` zero bytes.
    ///
    /// # Errors
    ///
    /// [`DiskError::InvalidInput`] when `len` is above [`IoBuf::MAX_LEN`].
    pub fn zeroed(len: usize) -> Result<IoBuf, DiskError> {
        if len > Self::MAX_LEN {
            return Err(DiskError::InvalidInput(Detail::new(&format!(
                "buffer of {len} bytes is above 67108864"
            ))));
        }
        Ok(IoBuf::allocate(len))
    }

    /// A buffer holding a copy of `bytes`.
    ///
    /// # Errors
    ///
    /// [`DiskError::InvalidInput`] when `bytes` is longer than
    /// [`IoBuf::MAX_LEN`].
    pub fn from_slice(bytes: &[u8]) -> Result<IoBuf, DiskError> {
        let mut buf = IoBuf::zeroed(bytes.len())?;
        buf.copy_from_slice(bytes);
        Ok(buf)
    }

    /// The length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the length is 0.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Allocates an aligned zeroed buffer. `len` is at most `MAX_LEN`, so the
    /// padded size cannot overflow.
    fn allocate(len: usize) -> IoBuf {
        if len == 0 {
            return IoBuf {
                storage: Box::default(),
                offset: 0,
                len: 0,
            };
        }
        let storage = vec![0u8; len + Self::ALIGN - 1].into_boxed_slice();
        let misalignment = storage.as_ptr().addr() % Self::ALIGN;
        let offset = (Self::ALIGN - misalignment) % Self::ALIGN;
        IoBuf {
            storage,
            offset,
            len,
        }
    }
}

impl Clone for IoBuf {
    /// A new aligned buffer with the same bytes.
    fn clone(&self) -> IoBuf {
        let mut copy = IoBuf::allocate(self.len);
        copy.copy_from_slice(self);
        copy
    }
}

/// Writes `IoBuf { len: <len> }`, never the bytes.
impl fmt::Debug for IoBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IoBuf").field("len", &self.len).finish()
    }
}

impl Deref for IoBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.storage[self.offset..self.offset + self.len]
    }
}

impl DerefMut for IoBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.storage[self.offset..self.offset + self.len]
    }
}

/// Why a disk operation failed. Variants are numbered from 1 in this order for
/// the event trace (ARFC 0005 section 4.1); `Os` is 255.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiskError {
    /// The path, or its parent, does not exist.
    #[error("not found")]
    NotFound,
    /// The path exists and the operation needs it not to.
    #[error("already exists")]
    AlreadyExists,
    /// The platform refused the operation.
    #[error("permission denied")]
    PermissionDenied,
    /// The device or the simulated disk is full.
    #[error("no space left")]
    NoSpace,
    /// The device reported an I/O error.
    #[error("device I/O error")]
    Io,
    /// An offset, length, or buffer is not aligned for direct I/O.
    #[error("offset, length, or buffer not aligned for direct I/O")]
    Unaligned,
    /// A path broke the rules of [`RtPath`].
    #[error("invalid path")]
    InvalidPath,
    /// An argument is outside its allowed range.
    #[error("invalid input: {0}")]
    InvalidInput(Detail),
    /// A batch held more than [`MAX_DISK_BATCH`] operations.
    #[error("batch longer than 256")]
    BatchTooLarge,
    /// The block device is held exclusively by another user.
    #[error("device is held by another user")]
    Busy,
    /// The handle was opened with [`Access::Read`].
    #[error("handle is read only")]
    ReadOnly,
    /// The path is a directory and the operation needs something else.
    #[error("is a directory")]
    IsDirectory,
    /// A directory was expected.
    #[error("not a directory")]
    NotDirectory,
    /// The directory has entries.
    #[error("directory not empty")]
    NotEmpty,
    /// The instance holds 65,536 open handles.
    #[error("too many open files")]
    TooManyOpen,
    /// The directory has more than 1,048,576 entries.
    #[error("directory has more than 1048576 entries")]
    TooManyEntries,
    /// The operation is not supported on this file or platform.
    #[error("not supported on this file or platform")]
    Unsupported,
    /// An operating system error with no variant above. `os` only.
    #[error("operating system error {code}")]
    Os {
        /// The platform's error code.
        code: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exactly_the_valid_paths() {
        let long_component = format!("/{}", "c".repeat(255));
        let longest = format!("/{}", vec!["d".repeat(255); 16].join("/"));
        assert_eq!(longest.len(), 4_096);
        for ok in [
            "/",
            "/a",
            "/a/b",
            "/.hidden",
            "/a.b/c..d",
            "/...",
            &long_component,
            &longest,
        ] {
            assert_eq!(
                RtPath::new(ok).map(|p| p.as_str().to_owned()),
                Ok(ok.to_owned())
            );
        }
        let too_long = format!("{longest}e");
        let long_component = format!("/{}", "c".repeat(256));
        for bad in [
            "",
            "a",
            "a/b",
            "//",
            "/a/",
            "/a//b",
            "/.",
            "/..",
            "/a/./b",
            "/a/../b",
            "/a\0",
            "/a\\b",
            &too_long,
            &long_component,
        ] {
            assert_eq!(RtPath::new(bad), Err(DiskError::InvalidPath), "{bad:?}");
        }
    }

    #[test]
    fn join_parent_and_file_name() {
        let root = RtPath::root();
        assert_eq!(root.as_str(), "/");
        assert_eq!(root.parent(), None);
        assert_eq!(root.file_name(), None);
        let a = root.join("a").unwrap();
        assert_eq!(a.as_str(), "/a");
        assert_eq!(a.parent(), Some(RtPath::root()));
        assert_eq!(a.file_name(), Some("a"));
        let ab = a.join("b.log").unwrap();
        assert_eq!(ab.as_str(), "/a/b.log");
        assert_eq!(ab.parent(), Some(a.clone()));
        assert_eq!(ab.file_name(), Some("b.log"));
        for bad in ["", ".", "..", "x/y", "x\\y", "x\0", &"c".repeat(256)] {
            assert_eq!(a.join(bad), Err(DiskError::InvalidPath), "{bad:?}");
        }
        let near_limit = RtPath::new(&format!("/{}", vec!["d".repeat(255); 16].join("/"))).unwrap();
        assert_eq!(near_limit.join("e"), Err(DiskError::InvalidPath));
    }

    #[test]
    fn io_buf_is_aligned_zeroed_and_bounded() {
        for len in [0, 1, 511, 512, 4_096, 4_097, 1 << 20] {
            let buf = IoBuf::zeroed(len).unwrap();
            assert_eq!(buf.len(), len);
            assert_eq!(buf.is_empty(), len == 0);
            assert!(buf.iter().all(|b| *b == 0));
            if len > 0 {
                assert_eq!(buf.as_ptr().addr() % IoBuf::ALIGN, 0, "len {len}");
            }
        }
        assert_eq!(IoBuf::zeroed(IoBuf::MAX_LEN).unwrap().len(), 67_108_864);
        assert!(matches!(
            IoBuf::zeroed(IoBuf::MAX_LEN + 1),
            Err(DiskError::InvalidInput(_))
        ));
    }

    #[test]
    fn io_buf_copies_and_clones_keep_alignment() {
        let mut buf = IoBuf::from_slice(b"block").unwrap();
        assert_eq!(&buf[..], b"block");
        buf[0] = b'B';
        let copy = buf.clone();
        assert_eq!(&copy[..], b"Block");
        assert_eq!(copy.as_ptr().addr() % IoBuf::ALIGN, 0);
        assert_eq!(format!("{copy:?}"), "IoBuf { len: 5 }");
        let op = WriteOp {
            offset: 7,
            buf: copy,
        };
        assert_eq!(
            format!("{op:?}"),
            "WriteOp { offset: 7, buf: IoBuf { len: 5 } }"
        );
    }
}
