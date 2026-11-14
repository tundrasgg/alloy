//! The `mem` disk of a host: its file namespace and block devices (ARFC 0005
//! clauses 3.1.10 to 3.1.12), their owners and modes (clause 3.1.15), what a
//! crash keeps of them (clause 3.3.4), and the capacity and read faults of
//! clause 3.3.7.
//!
//! Every disk call completes when it is made, at the current world instant,
//! and writes its `disk-op` records then. A host's disk holds two trees: the
//! current one, which calls read and change, and the durable base, the state
//! after the longest prefix of changes that completed syncs cover. Every
//! change after that prefix stays in a log, in completion order. A crash
//! replays the log on the base: it keeps each durable change, and keeps or
//! drops each unsynced one as the crash's `Unsynced` mode says. A change whose
//! target the replay does not hold (a write to a file whose create was
//! dropped, a rename of a missing entry, a `set_mode` of an entry whose
//! create was dropped) is dropped with it. A `replace` makes its own rename,
//! and the create of the file it moves, durable on their own, wherever they
//! sit in the log (clause 3.1.16).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::future::ready;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alloy_rt::{
    Access, BoxFuture, Create, DirEntry, DiskError, DiskFile, DiskIo, EntryKind, FileInfo,
    MAX_DISK_BATCH, Metadata, OpenOptions, Protection, ReadDone, ReadOp, RtPath, Security,
    SyncKind, WriteDone, WriteOp,
};
use alloy_seam::Detail;

use crate::exec::lock;
use crate::fault::{DiskTarget, Ppm, Unsynced};
use crate::host::{DeviceConfig, Place};
use crate::stream::Stream;
use crate::trace::KIND_DISK_OP;
use crate::world::WorldError;

/// Bytes per stored chunk. Chunks never written are zeros and take no memory.
const CHUNK: u64 = 4_096;
/// Open handles per `disk-io` instance (section 7).
const MAX_HANDLES: usize = 65_536;
/// Entries `list` returns at most (section 7).
const MAX_ENTRIES: usize = 1_048_576;
/// Block devices per host (section 7).
const MAX_DEVICES: usize = 64;
/// The longest block device, 2^40 bytes (section 7).
const MAX_DEVICE_LEN: u64 = 1 << 40;
/// The longest regular file, 2^63 - 1 bytes, the limit of `os` platforms.
const MAX_FILE_LEN: u64 = i64::MAX as u64;
/// The inode of the root directory.
const ROOT: u64 = 0;
/// The mode of the root and of a directory `create_dir` creates (clause
/// 3.1.15).
const DIR_MODE: u32 = 0o755;
/// The mode of a file `open` creates.
const FILE_MODE: u32 = 0o644;
/// The mode of a block device the scenario attaches.
const DEVICE_MODE: u32 = 0o660;
/// The mode of a `Private` or `PrivateRoot` directory.
const PRIVATE_DIR_MODE: u32 = 0o700;
/// The mode of a `Private` or `PrivateRoot` file.
const PRIVATE_FILE_MODE: u32 = 0o600;
/// The permission bits `set_mode` sets.
const PERMISSION_BITS: u32 = 0o7777;

/// The operation numbers of `disk-op` records (section 4.1).
const OP_OPEN: u8 = 1;
const OP_READ: u8 = 2;
const OP_WRITE: u8 = 3;
const OP_SYNC: u8 = 4;
const OP_TRUNCATE: u8 = 5;
const OP_METADATA: u8 = 6;
const OP_LIST: u8 = 7;
const OP_CREATE_DIR: u8 = 8;
const OP_REMOVE: u8 = 9;
const OP_RENAME: u8 = 10;
const OP_SYNC_DIR: u8 = 11;
const OP_INFO: u8 = 12;
const OP_CREATE_FILE: u8 = 13;
const OP_CREATE_DIR_PROTECTED: u8 = 14;
const OP_SECURITY: u8 = 15;
const OP_SET_MODE: u8 = 16;
const OP_REPLACE: u8 = 17;

fn invalid(text: &str) -> DiskError {
    DiskError::InvalidInput(Detail::new(text))
}

/// The bytes of a file or device, stored by chunk. Bytes at or past `len`
/// are zero in every stored chunk.
#[derive(Clone, Debug, Default)]
struct Data {
    len: u64,
    chunks: BTreeMap<u64, Box<[u8]>>,
}

impl Data {
    /// Reads at `offset` into `buf` up to the end, and returns the count.
    fn read(&self, offset: u64, buf: &mut [u8]) -> usize {
        let left = usize::try_from(self.len.saturating_sub(offset)).unwrap_or(usize::MAX);
        let count = left.min(buf.len());
        let mut done = 0;
        while done < count {
            // `offset + count` is at most `len`, so this does not overflow.
            let at = offset + done as u64;
            let skip = (at % CHUNK) as usize;
            let n = (CHUNK as usize - skip).min(count - done);
            let dest = &mut buf[done..done + n];
            match self.chunks.get(&(at / CHUNK)) {
                Some(chunk) => dest.copy_from_slice(&chunk[skip..skip + n]),
                None => dest.fill(0),
            }
            done += n;
        }
        count
    }

    /// Writes `bytes` at `offset`, extending the length when they end past
    /// it. The caller keeps `offset + bytes.len()` within `u64`.
    fn write(&mut self, offset: u64, bytes: &[u8]) {
        let mut done = 0;
        while done < bytes.len() {
            let at = offset + done as u64;
            let skip = (at % CHUNK) as usize;
            let n = (CHUNK as usize - skip).min(bytes.len() - done);
            let chunk = self
                .chunks
                .entry(at / CHUNK)
                .or_insert_with(|| vec![0; CHUNK as usize].into_boxed_slice());
            chunk[skip..skip + n].copy_from_slice(&bytes[done..done + n]);
            done += n;
        }
        if !bytes.is_empty() {
            self.len = self.len.max(offset + bytes.len() as u64);
        }
    }

    /// Sets the length; bytes a growth adds are zeros.
    fn set_len(&mut self, len: u64) {
        if len < self.len {
            drop(self.chunks.split_off(&len.div_ceil(CHUNK)));
            let tail = (len % CHUNK) as usize;
            if let Some(chunk) = self.chunks.get_mut(&(len / CHUNK))
                && tail > 0
            {
                chunk[tail..].fill(0);
            }
        }
        self.len = len;
    }
}

#[derive(Clone, Debug)]
enum Body {
    Dir(BTreeMap<String, u64>),
    File(Data),
    Block { data: Data, block_size: u32 },
}

/// One file, directory, or block device. `parent` is `None` for the root
/// and for a file whose name has been removed while a handle holds it. Every
/// node is owned by user id 0.
#[derive(Clone, Debug)]
struct Node {
    body: Body,
    parent: Option<u64>,
    /// The permission bits, `st_mode & 0o7777`.
    mode: u32,
}

impl Node {
    fn kind(&self) -> EntryKind {
        match self.body {
            Body::Dir(_) => EntryKind::Dir,
            Body::File(_) => EntryKind::File,
            Body::Block { .. } => EntryKind::Block,
        }
    }

    fn data(&self) -> Option<&Data> {
        match &self.body {
            Body::Dir(_) => None,
            Body::File(data) | Body::Block { data, .. } => Some(data),
        }
    }

    fn len(&self) -> u64 {
        self.data().map_or(0, |data| data.len)
    }
}

/// A change to the namespace or to the bytes of a file or device.
#[derive(Clone, Debug)]
enum Op {
    Create {
        parent: u64,
        name: String,
        ino: u64,
        dir: bool,
        mode: u32,
    },
    Remove {
        parent: u64,
        name: String,
        ino: u64,
    },
    Rename {
        from: u64,
        from_name: String,
        to: u64,
        to_name: String,
        ino: u64,
    },
    Write {
        ino: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    Truncate {
        ino: u64,
        len: u64,
    },
    /// A `set_mode`, of a directory when `dir`.
    SetMode {
        ino: u64,
        dir: bool,
        mode: u32,
    },
}

/// The components of `path`, from the root down.
fn components(path: &RtPath) -> impl Iterator<Item = &str> {
    path.as_str().split('/').filter(|c| !c.is_empty())
}

/// What [`Tree::find`] gives: the parent directory and name, and the inode.
type Found = (Option<(u64, String)>, Option<u64>);

/// A file namespace with its devices, by inode.
#[derive(Clone, Debug)]
struct Tree {
    nodes: BTreeMap<u64, Node>,
}

impl Tree {
    fn new() -> Tree {
        let root = Node {
            body: Body::Dir(BTreeMap::new()),
            parent: None,
            mode: DIR_MODE,
        };
        Tree {
            nodes: BTreeMap::from([(ROOT, root)]),
        }
    }

    /// The node `ino`. Every entry names a node, so `Io` only reports a
    /// broken tree.
    fn node(&self, ino: u64) -> Result<&Node, DiskError> {
        self.nodes.get(&ino).ok_or(DiskError::Io)
    }

    fn entries(&self, ino: u64) -> Option<&BTreeMap<String, u64>> {
        match &self.nodes.get(&ino)?.body {
            Body::Dir(entries) => Some(entries),
            _ => None,
        }
    }

    fn entries_mut(&mut self, ino: u64) -> Option<&mut BTreeMap<String, u64>> {
        match &mut self.nodes.get_mut(&ino)?.body {
            Body::Dir(entries) => Some(entries),
            _ => None,
        }
    }

    /// The directory at `path`.
    fn dir(&self, path: &RtPath) -> Result<u64, DiskError> {
        let mut at = ROOT;
        for name in components(path) {
            let entries = self.entries(at).ok_or(DiskError::NotDirectory)?;
            at = *entries.get(name).ok_or(DiskError::NotFound)?;
        }
        self.entries(at).map(|_| at).ok_or(DiskError::NotDirectory)
    }

    /// The parent directory and name of `path`, `None` for the root, and the
    /// inode at `path` if there is one.
    fn find(&self, path: &RtPath) -> Result<Found, DiskError> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Ok((None, Some(ROOT)));
        };
        let parent = self.dir(&parent)?;
        let ino = self.entries(parent).and_then(|e| e.get(name)).copied();
        Ok((Some((parent, name.to_owned())), ino))
    }

    /// Whether `at` is the directory `ino` or below it.
    fn within(&self, mut at: u64, ino: u64) -> bool {
        loop {
            if at == ino {
                return true;
            }
            match self.nodes.get(&at).and_then(|node| node.parent) {
                Some(parent) => at = parent,
                None => return false,
            }
        }
    }

    fn write(&mut self, ino: u64, offset: u64, bytes: &[u8]) -> bool {
        match self.nodes.get_mut(&ino).map(|node| &mut node.body) {
            Some(Body::File(data) | Body::Block { data, .. }) => {
                data.write(offset, bytes);
                true
            }
            _ => false,
        }
    }

    /// Applies `op` when what it needs is there. `None` when it is not;
    /// otherwise the inode whose name `op` removed, if any, which the caller
    /// drops once nothing holds it.
    fn apply(&mut self, op: &Op) -> Option<Option<u64>> {
        match op {
            Op::Create {
                parent,
                name,
                ino,
                dir,
                mode,
            } => {
                let entries = self.entries_mut(*parent)?;
                if entries.contains_key(name) {
                    return None;
                }
                entries.insert(name.clone(), *ino);
                let body = if *dir {
                    Body::Dir(BTreeMap::new())
                } else {
                    Body::File(Data::default())
                };
                let node = Node {
                    body,
                    parent: Some(*parent),
                    mode: *mode,
                };
                self.nodes.insert(*ino, node);
                Some(None)
            }
            Op::Remove { parent, name, ino } => {
                if self.entries(*parent)?.get(name) != Some(ino)
                    || self.entries(*ino).is_some_and(|e| !e.is_empty())
                {
                    return None;
                }
                self.entries_mut(*parent)?.remove(name);
                self.nodes.get_mut(ino)?.parent = None;
                Some(Some(*ino))
            }
            Op::Rename {
                from,
                from_name,
                to,
                to_name,
                ino,
            } => {
                if self.entries(*from)?.get(from_name) != Some(ino) {
                    return None;
                }
                let replaced = self.entries(*to)?.get(to_name).copied();
                if replaced == Some(*ino) {
                    return Some(None);
                }
                if replaced.is_some_and(|r| self.entries(r).is_some()) || self.within(*to, *ino) {
                    return None;
                }
                self.entries_mut(*from)?.remove(from_name);
                self.entries_mut(*to)?.insert(to_name.clone(), *ino);
                self.nodes.get_mut(ino)?.parent = Some(*to);
                if let Some(old) = replaced.and_then(|r| self.nodes.get_mut(&r)) {
                    old.parent = None;
                }
                Some(replaced)
            }
            Op::Write { ino, offset, bytes } => self.write(*ino, *offset, bytes).then_some(None),
            Op::Truncate { ino, len } => match &mut self.nodes.get_mut(ino)?.body {
                Body::File(data) => {
                    data.set_len(*len);
                    Some(None)
                }
                _ => None,
            },
            Op::SetMode { ino, mode, .. } => {
                self.nodes.get_mut(ino)?.mode = *mode;
                Some(None)
            }
        }
    }
}

/// The read fault rates of one target (clause 3.3.7).
#[derive(Clone, Copy, Debug)]
struct Rates {
    read_error: Ppm,
    corrupt: Ppm,
}

const NO_FAULTS: Rates = Rates {
    read_error: Ppm::ZERO,
    corrupt: Ppm::ZERO,
};

/// What `open` found or made.
struct Opened {
    ino: u64,
    kind: EntryKind,
    block_size: u32,
    number: u64,
}

/// The disk of one host. It persists across crashes; what a crash keeps of
/// it is clause 3.3.4.
#[derive(Debug)]
pub(crate) struct Disk {
    block_size: u32,
    capacity: u64,
    /// Bytes the current regular files count against `capacity`.
    used: u64,
    base: Tree,
    current: Tree,
    /// Changes after the durable prefix, with their sequence numbers.
    log: VecDeque<(u64, Op)>,
    /// The last sequence number given to a change or a sync.
    seq: u64,
    /// Per inode, the sequence number of the last completed sync of it.
    synced: BTreeMap<u64, u64>,
    /// Per inode, the sequence number of the last completed `sync(All)` of
    /// it, which alone makes a `set_mode` of a file durable.
    synced_all: BTreeMap<u64, u64>,
    /// Per directory, the sequence number of its last completed `sync_dir`.
    dir_synced: BTreeMap<u64, u64>,
    /// Logged changes durable on their own: the rename of each completed
    /// `replace` and the create of the file it moved (clause 3.1.16).
    forced: BTreeSet<u64>,
    next_ino: u64,
    /// The next handle number, continuing across restarts (section 4.1).
    next_handle: u64,
    /// Open handles of the current start, per inode.
    open: BTreeMap<u64, usize>,
    /// Devices an exclusive handle holds.
    held: BTreeSet<u64>,
    devices: BTreeMap<RtPath, u64>,
    files: Rates,
    device_rates: BTreeMap<u64, Rates>,
    /// The draws of read faults.
    reads: Stream,
}

fn round_up(len: u64, block_size: u32) -> u64 {
    len.div_ceil(u64::from(block_size))
        .saturating_mul(u64::from(block_size))
}

fn valid_block_size(block_size: u32) -> bool {
    block_size == 512 || block_size == 4_096
}

impl Disk {
    /// A disk with an empty namespace and `devices`, all durable.
    ///
    /// # Errors
    ///
    /// `Invalid` for a block size other than 512 or 4,096, more than 64
    /// devices, a device length out of range, and a device path that is the
    /// root, another device's, or below one (clause 3.3.7).
    pub(crate) fn new(
        block_size: u32,
        capacity: u64,
        devices: &[DeviceConfig],
        reads: Stream,
    ) -> Result<Disk, WorldError> {
        let refuse = |text: &str| Err(WorldError::Invalid(Detail::new(text)));
        if !valid_block_size(block_size) {
            return refuse("block_size must be 512 or 4096");
        }
        if devices.len() > MAX_DEVICES {
            return refuse("at most 64 devices");
        }
        let mut tree = Tree::new();
        let mut next_ino = ROOT + 1;
        let mut paths = BTreeMap::new();
        for device in devices {
            let size = u64::from(device.block_size);
            if !valid_block_size(device.block_size)
                || device.len < size
                || device.len > MAX_DEVICE_LEN
                || !device.len.is_multiple_of(size)
            {
                return refuse(
                    "a device needs block_size 512 or 4096 and a len of whole blocks up to 2^40",
                );
            }
            let in_use = format!("device path {} is in use", device.path.as_str());
            let (Some(parent), Some(name)) = (device.path.parent(), device.path.file_name()) else {
                return refuse(&in_use);
            };
            let mut at = ROOT;
            for component in components(&parent) {
                let Some(entries) = tree.entries(at) else {
                    return refuse(&in_use);
                };
                at = match entries.get(component) {
                    Some(ino) => *ino,
                    None => {
                        let ino = next_ino;
                        next_ino += 1;
                        let name = component.to_owned();
                        tree.apply(&Op::Create {
                            parent: at,
                            name,
                            ino,
                            dir: true,
                            mode: DIR_MODE,
                        });
                        ino
                    }
                };
            }
            match tree.entries_mut(at) {
                Some(entries) if !entries.contains_key(name) => {
                    entries.insert(name.to_owned(), next_ino);
                }
                _ => return refuse(&in_use),
            }
            let data = Data {
                len: device.len,
                chunks: BTreeMap::new(),
            };
            let body = Body::Block {
                data,
                block_size: device.block_size,
            };
            let node = Node {
                body,
                parent: Some(at),
                mode: DEVICE_MODE,
            };
            tree.nodes.insert(next_ino, node);
            paths.insert(device.path.clone(), next_ino);
            next_ino += 1;
        }
        Ok(Disk {
            block_size,
            capacity,
            used: 0,
            base: tree.clone(),
            current: tree,
            log: VecDeque::new(),
            seq: 0,
            synced: BTreeMap::new(),
            synced_all: BTreeMap::new(),
            dir_synced: BTreeMap::new(),
            forced: BTreeSet::new(),
            next_ino,
            next_handle: 1,
            open: BTreeMap::new(),
            held: BTreeSet::new(),
            devices: paths,
            files: NO_FAULTS,
            device_rates: BTreeMap::new(),
            reads,
        })
    }

    pub(crate) fn has_device(&self, path: &RtPath) -> bool {
        self.devices.contains_key(path)
    }

    /// Replaces the read fault rates of `target` (clause 3.3.7). A device
    /// path that names no device changes nothing.
    pub(crate) fn set_rates(&mut self, target: &DiskTarget, read_error: Ppm, corrupt: Ppm) {
        let rates = Rates {
            read_error,
            corrupt,
        };
        match target {
            DiskTarget::Files => self.files = rates,
            DiskTarget::Device(path) => {
                if let Some(ino) = self.devices.get(path) {
                    self.device_rates.insert(*ino, rates);
                }
            }
        }
    }

    pub(crate) fn set_capacity(&mut self, bytes: u64) {
        self.capacity = bytes;
    }

    /// The capacity less the bytes counted against it, 0 at or above it
    /// (clause 3.1.18.2).
    pub(crate) fn free(&self) -> u64 {
        self.capacity.saturating_sub(self.used)
    }

    /// Whether the logged change `op` of sequence number `seq` is durable.
    fn durable(&self, seq: u64, op: &Op) -> bool {
        if self.forced.contains(&seq) {
            return true;
        }
        let after = |map: &BTreeMap<u64, u64>, ino: &u64| map.get(ino).is_some_and(|s| *s > seq);
        match op {
            Op::Create { parent, .. } | Op::Remove { parent, .. } => {
                after(&self.dir_synced, parent)
            }
            Op::Rename { from, to, .. } => {
                after(&self.dir_synced, from) && after(&self.dir_synced, to)
            }
            Op::Write { ino, .. } | Op::Truncate { ino, .. } => after(&self.synced, ino),
            // Only a `sync(All)` of the file or a `sync_dir` of the
            // directory itself keeps a mode (clause 3.1.15).
            Op::SetMode { ino, dir: true, .. } => after(&self.dir_synced, ino),
            Op::SetMode {
                ino, dir: false, ..
            } => after(&self.synced_all, ino),
        }
    }

    /// Applies `op`, whose needs the caller has checked, to the current tree
    /// and logs it.
    fn commit(&mut self, op: Op) {
        self.seq += 1;
        if let Some(Some(orphan)) = self.current.apply(&op)
            && !self.open.contains_key(&orphan)
        {
            self.reap(orphan);
        }
        self.log.push_back((self.seq, op));
    }

    /// Drops the nameless node `ino` from the current tree.
    fn reap(&mut self, ino: u64) {
        if let Some(Node {
            body: Body::File(data),
            ..
        }) = self.current.nodes.remove(&ino)
        {
            self.used = self
                .used
                .saturating_sub(round_up(data.len, self.block_size));
        }
    }

    /// Moves every durable change at the front of the log into the base.
    fn compact(&mut self) {
        while let Some((seq, op)) = self.log.front()
            && self.durable(*seq, op)
        {
            if let Some((seq, op)) = self.log.pop_front() {
                self.forced.remove(&seq);
                if let Some(Some(orphan)) = self.base.apply(&op) {
                    self.base.nodes.remove(&orphan);
                }
            }
        }
        if self.log.is_empty() {
            self.synced.clear();
            self.synced_all.clear();
            self.dir_synced.clear();
            self.forced.clear();
        }
    }

    /// The bytes counted after a regular file goes from `old` to `new`
    /// bytes. `NoSpace` when that grows the count above the capacity.
    fn room(&self, old: u64, new: u64) -> Result<u64, DiskError> {
        let used = self
            .used
            .saturating_sub(round_up(old, self.block_size))
            .saturating_add(round_up(new, self.block_size));
        if used > self.used && used > self.capacity {
            Err(DiskError::NoSpace)
        } else {
            Ok(used)
        }
    }

    /// The crash of clause 3.3.4: replays the log on the base under `mode`,
    /// with `draws` deciding what `Seeded` and `Torn` keep, and makes the
    /// result both trees. Open handles and exclusive holds end.
    pub(crate) fn crash(&mut self, mode: Unsynced, draws: &mut Stream) {
        let mut tree = std::mem::replace(&mut self.base, Tree::new());
        for (seq, op) in std::mem::take(&mut self.log) {
            let durable = self.durable(seq, &op);
            match op {
                Op::Write { ino, offset, bytes } if !durable && mode == Unsynced::Torn => {
                    let unit = match tree.nodes.get(&ino).map(|node| &node.body) {
                        Some(Body::Block { block_size, .. }) => u64::from(*block_size),
                        _ => u64::from(self.block_size),
                    };
                    let end = offset + bytes.len() as u64;
                    let mut at = offset;
                    while at < end {
                        let next = ((at / unit + 1) * unit).min(end);
                        if draws.below(2) == 1 {
                            let piece = &bytes[(at - offset) as usize..(next - offset) as usize];
                            tree.write(ino, at, piece);
                        }
                        at = next;
                    }
                }
                op => {
                    let keep = durable || (mode != Unsynced::Lost && draws.below(2) == 1);
                    if keep && let Some(Some(orphan)) = tree.apply(&op) {
                        tree.nodes.remove(&orphan);
                    }
                }
            }
        }
        self.used = tree
            .nodes
            .values()
            .filter(|node| matches!(node.body, Body::File(_)))
            .map(|node| round_up(node.len(), self.block_size))
            .fold(0, u64::saturating_add);
        self.base = tree.clone();
        self.current = tree;
        self.synced.clear();
        self.synced_all.clear();
        self.dir_synced.clear();
        self.forced.clear();
        self.open.clear();
        self.held.clear();
    }

    /// Opens `path`; a file it creates gets `mode`.
    fn open(
        &mut self,
        path: &RtPath,
        options: OpenOptions,
        mode: u32,
    ) -> Result<Opened, DiskError> {
        let (place, found) = self.current.find(path)?;
        let ino = match (found, place) {
            (Some(ino), _) => {
                if options.create == Create::New {
                    return Err(DiskError::AlreadyExists);
                }
                match self.current.node(ino)?.body {
                    Body::Dir(_) => return Err(DiskError::IsDirectory),
                    Body::File(_) if options.exclusive => {
                        return Err(invalid("exclusive applies to block devices only"));
                    }
                    Body::Block { .. } if options.exclusive && self.held.contains(&ino) => {
                        return Err(DiskError::Busy);
                    }
                    _ => ino,
                }
            }
            (None, Some((parent, name))) if options.create != Create::Never => {
                if options.exclusive {
                    return Err(invalid("exclusive applies to block devices only"));
                }
                let ino = self.next_ino;
                self.next_ino += 1;
                self.commit(Op::Create {
                    parent,
                    name,
                    ino,
                    dir: false,
                    mode,
                });
                ino
            }
            (None, _) => return Err(DiskError::NotFound),
        };
        let node = self.current.node(ino)?;
        let block_size = match node.body {
            Body::Block { block_size, .. } => block_size,
            _ => self.block_size,
        };
        let kind = node.kind();
        *self.open.entry(ino).or_default() += 1;
        if options.exclusive {
            self.held.insert(ino);
        }
        let number = self.next_handle;
        self.next_handle += 1;
        Ok(Opened {
            ino,
            kind,
            block_size,
            number,
        })
    }

    /// Ends one open handle of `ino`.
    fn close(&mut self, ino: u64, exclusive: bool) {
        if exclusive {
            self.held.remove(&ino);
        }
        let Some(count) = self.open.get_mut(&ino) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            self.open.remove(&ino);
            if self
                .current
                .nodes
                .get(&ino)
                .is_some_and(|node| node.parent.is_none())
            {
                self.reap(ino);
            }
        }
    }

    fn metadata(&self, path: &RtPath) -> Result<Metadata, DiskError> {
        let ino = self.current.find(path)?.1.ok_or(DiskError::NotFound)?;
        let node = self.current.node(ino)?;
        Ok(Metadata {
            kind: node.kind(),
            len: node.len(),
        })
    }

    fn list(&self, dir: &RtPath) -> Result<Vec<DirEntry>, DiskError> {
        let ino = self.current.dir(dir)?;
        let entries = self.current.entries(ino).ok_or(DiskError::NotDirectory)?;
        if entries.len() > MAX_ENTRIES {
            return Err(DiskError::TooManyEntries);
        }
        Ok(entries
            .iter()
            .map(|(name, ino)| DirEntry {
                name: name.clone(),
                kind: self.current.node(*ino).map_or(EntryKind::Other, Node::kind),
            })
            .collect())
    }

    /// Creates the directory `path` with `mode`.
    fn create_dir(&mut self, path: &RtPath, mode: u32) -> Result<(), DiskError> {
        let (place, found) = self.current.find(path)?;
        let (Some((parent, name)), None) = (place, found) else {
            return Err(DiskError::AlreadyExists);
        };
        let ino = self.next_ino;
        self.next_ino += 1;
        self.commit(Op::Create {
            parent,
            name,
            ino,
            dir: true,
            mode,
        });
        Ok(())
    }

    /// What `lstat` would report: `mem` has no symbolic links, and user id 0
    /// owns every entry (clause 3.1.15).
    fn security(&self, path: &RtPath) -> Result<Security, DiskError> {
        let ino = self.current.find(path)?.1.ok_or(DiskError::NotFound)?;
        let node = self.current.node(ino)?;
        Ok(Security::Unix {
            kind: node.kind(),
            symlink: false,
            uid: 0,
            mode: node.mode,
        })
    }

    /// Sets the mode of the entry at `path`. User id 0 may set any mode on
    /// any entry, so nothing is refused for its mode.
    fn set_mode(&mut self, path: &RtPath, mode: u32) -> Result<(), DiskError> {
        if mode > PERMISSION_BITS {
            return Err(invalid("a mode is at most 0o7777"));
        }
        let ino = self.current.find(path)?.1.ok_or(DiskError::NotFound)?;
        let dir = self.current.node(ino)?.kind() == EntryKind::Dir;
        self.commit(Op::SetMode { ino, dir, mode });
        Ok(())
    }

    fn remove(&mut self, path: &RtPath) -> Result<(), DiskError> {
        let (place, found) = self.current.find(path)?;
        let ino = found.ok_or(DiskError::NotFound)?;
        let Some((parent, name)) = place else {
            return Err(DiskError::PermissionDenied);
        };
        match &self.current.node(ino)?.body {
            Body::Block { .. } => return Err(DiskError::PermissionDenied),
            Body::Dir(entries) if !entries.is_empty() => return Err(DiskError::NotEmpty),
            _ => {}
        }
        self.commit(Op::Remove { parent, name, ino });
        Ok(())
    }

    fn rename(&mut self, from_path: &RtPath, to_path: &RtPath) -> Result<(), DiskError> {
        let (from_place, found) = self.current.find(from_path)?;
        let ino = found.ok_or(DiskError::NotFound)?;
        let (to_place, target) = self.current.find(to_path)?;
        let (Some((from, from_name)), Some((to, to_name))) = (from_place, to_place) else {
            return Err(if to_path.parent().is_none() {
                DiskError::IsDirectory
            } else {
                invalid("the root cannot be renamed")
            });
        };
        let kind = self.current.node(ino)?.kind();
        let target_kind = match target {
            Some(target) => Some(self.current.node(target)?.kind()),
            None => None,
        };
        if target == Some(ino) {
            return Ok(());
        }
        if target_kind == Some(EntryKind::Dir) {
            return Err(DiskError::IsDirectory);
        }
        if kind == EntryKind::Block || target_kind == Some(EntryKind::Block) {
            return Err(DiskError::PermissionDenied);
        }
        if kind == EntryKind::Dir && target.is_some() {
            return Err(DiskError::NotDirectory);
        }
        if self.current.within(to, ino) {
            return Err(invalid("a directory cannot move below itself"));
        }
        self.commit(Op::Rename {
            from,
            from_name,
            to,
            to_name,
            ino,
        });
        Ok(())
    }

    /// The durable replace of clause 3.1.16: `rename` of a regular file
    /// within one directory, which with the create of the file, when that
    /// is still unsynced, is durable at once and makes nothing else durable.
    fn replace(&mut self, from_path: &RtPath, to_path: &RtPath) -> Result<(), DiskError> {
        match (from_path.parent(), to_path.parent()) {
            (Some(from), Some(to)) if from == to => {}
            _ => return Err(invalid("replace needs from and to in one directory")),
        }
        let ino = self.current.find(from_path)?.1.ok_or(DiskError::NotFound)?;
        match self.current.node(ino)?.kind() {
            EntryKind::File => {}
            EntryKind::Dir => return Err(DiskError::IsDirectory),
            EntryKind::Block | EntryKind::Other => {
                return Err(invalid("replace moves a regular file"));
            }
        }
        let created = self.log.iter().find_map(|(seq, op)| match op {
            Op::Create { ino: made, .. } if *made == ino => Some(*seq),
            _ => None,
        });
        let before = self.seq;
        self.rename(from_path, to_path)?;
        if self.seq > before {
            self.forced.insert(self.seq);
        }
        if let Some(seq) = created {
            self.forced.insert(seq);
        }
        self.compact();
        Ok(())
    }

    fn sync_dir(&mut self, dir: &RtPath) -> Result<(), DiskError> {
        let ino = self.current.dir(dir)?;
        self.seq += 1;
        self.dir_synced.insert(ino, self.seq);
        self.compact();
        Ok(())
    }

    fn sync(&mut self, ino: u64, kind: SyncKind) {
        self.seq += 1;
        self.synced.insert(ino, self.seq);
        if kind == SyncKind::All {
            self.synced_all.insert(ino, self.seq);
        }
        self.compact();
    }

    fn len(&self, ino: u64) -> u64 {
        self.current.nodes.get(&ino).map_or(0, Node::len)
    }

    /// One read at `offset`, with the read faults of the inode's target.
    fn read(&mut self, ino: u64, offset: u64, buf: &mut [u8]) -> Result<usize, DiskError> {
        let Some(node) = self.current.nodes.get(&ino) else {
            return Err(DiskError::Io);
        };
        let rates = match node.body {
            Body::Block { .. } => self.device_rates.get(&ino).copied().unwrap_or(NO_FAULTS),
            _ => self.files,
        };
        let mut draw = |ppm: Ppm| ppm.get() > 0 && self.reads.below(1_000_000) < ppm.get() as usize;
        if draw(rates.read_error) {
            return Err(DiskError::Io);
        }
        let count = node.data().map_or(0, |data| data.read(offset, buf));
        if count > 0 && draw(rates.corrupt) {
            let bit = self.reads.below(count.saturating_mul(8));
            buf[bit / 8] ^= 1 << (bit % 8);
        }
        Ok(count)
    }

    /// One write of `bytes` at `offset`.
    fn write(&mut self, ino: u64, offset: u64, bytes: &[u8]) -> Result<(), DiskError> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .filter(|end| *end <= MAX_FILE_LEN)
            .ok_or_else(|| invalid("the write ends past 2^63 - 1"))?;
        if bytes.is_empty() {
            return Ok(());
        }
        let file = match self.current.nodes.get(&ino).map(|node| &node.body) {
            Some(Body::File(data)) => Some(data.len),
            _ => None,
        };
        if let Some(old) = file {
            self.used = self.room(old, old.max(end))?;
        }
        self.commit(Op::Write {
            ino,
            offset,
            bytes: bytes.to_vec(),
        });
        Ok(())
    }

    fn truncate(&mut self, ino: u64, len: u64) -> Result<(), DiskError> {
        if len > MAX_FILE_LEN {
            return Err(invalid("the length is above 2^63 - 1"));
        }
        self.used = self.room(self.len(ino), len)?;
        self.commit(Op::Truncate { ino, len });
        Ok(())
    }
}

/// The `disk-op` result byte of an error: its number in section 2.4.
fn code(error: &DiskError) -> u8 {
    match error {
        DiskError::NotFound => 1,
        DiskError::AlreadyExists => 2,
        DiskError::PermissionDenied => 3,
        DiskError::NoSpace => 4,
        DiskError::Io => 5,
        DiskError::Unaligned => 6,
        DiskError::InvalidPath => 7,
        DiskError::InvalidInput(_) => 8,
        DiskError::BatchTooLarge => 9,
        DiskError::Busy => 10,
        DiskError::ReadOnly => 11,
        DiskError::IsDirectory => 12,
        DiskError::NotDirectory => 13,
        DiskError::NotEmpty => 14,
        DiskError::TooManyOpen => 15,
        DiskError::TooManyEntries => 16,
        DiskError::Unsupported => 17,
        DiskError::Os { .. } => 255,
    }
}

/// The body of a `disk-op` record (section 4.1), and the bytes its digest
/// covers: `data` for a success, nothing for an error.
fn disk_op(
    handle: u64,
    op: u8,
    range: (u64, u64),
    error: Option<&DiskError>,
    data: Option<&[u8]>,
) -> (Vec<u8>, Vec<u8>) {
    let mut body = Vec::with_capacity(34);
    body.extend_from_slice(&handle.to_le_bytes());
    body.push(op);
    body.extend_from_slice(&range.0.to_le_bytes());
    body.extend_from_slice(&range.1.to_le_bytes());
    body.push(error.map_or(0, code));
    match (error, data) {
        (None, Some(data)) => {
            body.extend_from_slice(&alloy_crypto::hash(data)[..8]);
            (body, data.to_vec())
        }
        _ => {
            body.extend_from_slice(&[0; 8]);
            (body, Vec::new())
        }
    }
}

/// Writes one `disk-op` record: handle, operation, offset and length, the
/// error, and the bytes read or written.
type RecordFn<'r> = dyn Fn(u64, u8, (u64, u64), Option<&DiskError>, Option<&[u8]>) + 'r;

/// Runs `f` with the disk of `at`'s host while its start runs and the run
/// has not ended, and with `None` otherwise, when calls fail with `Io`
/// (clause 3.3.4). The records are written once `core` is released. The
/// caller holds no lock.
fn with_disk<T>(at: &Place, f: impl FnOnce(Option<&mut Disk>, &RecordFn<'_>) -> T) -> T {
    let shared = &at.shared;
    let record =
        |handle: u64, op: u8, range: (u64, u64), error: Option<&DiskError>, data: Option<&[u8]>| {
            let (body, data) = disk_op(handle, op, range, error, data);
            shared.push_data(Some(at.host), KIND_DISK_OP, &body, data);
        };
    let result = {
        let mut core = lock(&shared.core);
        let disk = match core.host_mut(at.host) {
            Ok(host) if host.live(at.start) && !shared.has_ended() => Some(&mut host.disk),
            _ => None,
        };
        f(disk, &record)
    };
    shared.flush();
    result
}

/// A call with one record, of handle `handle`.
fn one_call<T: Send + 'static>(
    at: &Place,
    handle: u64,
    op: u8,
    range: (u64, u64),
    f: impl FnOnce(&mut Disk) -> Result<T, DiskError>,
) -> BoxFuture<'static, Result<T, DiskError>> {
    let result = with_disk(at, |disk, record| {
        let result = disk.map_or(Err(DiskError::Io), f);
        record(handle, op, range, result.as_ref().err(), None);
        result
    });
    Box::pin(ready(result))
}

/// A path call: one record with handle 0.
fn path_call<T: Send + 'static>(
    at: &Place,
    op: u8,
    f: impl FnOnce(&mut Disk) -> Result<T, DiskError>,
) -> BoxFuture<'static, Result<T, DiskError>> {
    one_call(at, 0, op, (0, 0), f)
}

/// The number `disk-op` records give `protection` (section 4.1).
fn protection_code(protection: Protection) -> u64 {
    match protection {
        Protection::Default => 0,
        Protection::Private => 1,
        Protection::PrivateRoot => 2,
    }
}

/// The `mem` disk-io instance of one start (clauses 3.1.10 to 3.1.12 and
/// 3.1.15). Every instance of a host shares the host's disk and counts its
/// own handles.
#[derive(Debug)]
pub(crate) struct MemDiskIo {
    at: Place,
    handles: Arc<AtomicUsize>,
}

impl MemDiskIo {
    pub(crate) fn new(at: Place) -> MemDiskIo {
        MemDiskIo {
            at,
            handles: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl MemDiskIo {
    /// `open`, writing one record of operation `op` with `range`. A file it
    /// creates gets `mode`.
    fn open_with(
        &self,
        path: &RtPath,
        options: OpenOptions,
        (op, range): (u8, (u64, u64)),
        mode: u32,
    ) -> BoxFuture<'static, Result<Box<dyn DiskFile>, DiskError>> {
        let opened = with_disk(&self.at, |disk, record| {
            let opened = match disk {
                None => Err(DiskError::Io),
                Some(_) if options.create != Create::Never && options.access == Access::Read => {
                    Err(invalid("creating a file needs Access::ReadWrite"))
                }
                Some(_) if self.handles.load(Ordering::Acquire) >= MAX_HANDLES => {
                    Err(DiskError::TooManyOpen)
                }
                Some(disk) => disk.open(path, options, mode),
            };
            if opened.is_ok() {
                self.handles.fetch_add(1, Ordering::AcqRel);
            }
            let number = opened.as_ref().map_or(0, |o| o.number);
            record(number, op, range, opened.as_ref().err(), None);
            opened
        });
        let result = opened.map(|opened| -> Box<dyn DiskFile> {
            Box::new(MemFile {
                at: self.at.clone(),
                handles: Arc::clone(&self.handles),
                ino: opened.ino,
                number: opened.number,
                kind: opened.kind,
                options,
                block_size: opened.block_size,
            })
        });
        Box::pin(ready(result))
    }
}

impl DiskIo for MemDiskIo {
    fn open<'a>(
        &'a self,
        path: &'a RtPath,
        options: OpenOptions,
    ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
        self.open_with(path, options, (OP_OPEN, (0, 0)), FILE_MODE)
    }

    fn metadata<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<Metadata, DiskError>> {
        path_call(&self.at, OP_METADATA, |disk| disk.metadata(path))
    }

    fn list<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<Vec<DirEntry>, DiskError>> {
        path_call(&self.at, OP_LIST, |disk| disk.list(dir))
    }

    fn create_dir<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        path_call(&self.at, OP_CREATE_DIR, |disk| {
            disk.create_dir(path, DIR_MODE)
        })
    }

    fn remove<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        path_call(&self.at, OP_REMOVE, |disk| disk.remove(path))
    }

    fn rename<'a>(
        &'a self,
        from: &'a RtPath,
        to: &'a RtPath,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        path_call(&self.at, OP_RENAME, |disk| disk.rename(from, to))
    }

    fn replace<'a>(
        &'a self,
        from: &'a RtPath,
        to: &'a RtPath,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        path_call(&self.at, OP_REPLACE, |disk| disk.replace(from, to))
    }

    fn sync_dir<'a>(&'a self, dir: &'a RtPath) -> BoxFuture<'a, Result<(), DiskError>> {
        path_call(&self.at, OP_SYNC_DIR, |disk| disk.sync_dir(dir))
    }

    fn create_file<'a>(
        &'a self,
        path: &'a RtPath,
        protection: Protection,
    ) -> BoxFuture<'a, Result<Box<dyn DiskFile>, DiskError>> {
        let options = OpenOptions {
            access: Access::ReadWrite,
            create: Create::New,
            direct: false,
            exclusive: false,
        };
        let mode = match protection {
            Protection::Default => FILE_MODE,
            Protection::Private | Protection::PrivateRoot => PRIVATE_FILE_MODE,
        };
        let record = (OP_CREATE_FILE, (0, protection_code(protection)));
        self.open_with(path, options, record, mode)
    }

    fn create_dir_protected<'a>(
        &'a self,
        path: &'a RtPath,
        protection: Protection,
    ) -> BoxFuture<'a, Result<(), DiskError>> {
        let mode = match protection {
            Protection::Default => DIR_MODE,
            Protection::Private | Protection::PrivateRoot => PRIVATE_DIR_MODE,
        };
        let range = (0, protection_code(protection));
        one_call(&self.at, 0, OP_CREATE_DIR_PROTECTED, range, |disk| {
            disk.create_dir(path, mode)
        })
    }

    fn security<'a>(&'a self, path: &'a RtPath) -> BoxFuture<'a, Result<Security, DiskError>> {
        path_call(&self.at, OP_SECURITY, |disk| disk.security(path))
    }

    fn set_mode<'a>(&'a self, path: &'a RtPath, mode: u32) -> BoxFuture<'a, Result<(), DiskError>> {
        let range = (0, u64::from(mode));
        one_call(&self.at, 0, OP_SET_MODE, range, |disk| {
            disk.set_mode(path, mode)
        })
    }

    /// `mem` has no native paths (clause 3.1.15). Writes no record.
    fn rt_path(&self, _native: &str) -> Result<RtPath, DiskError> {
        Err(DiskError::Unsupported)
    }
}

/// An open `mem` file or block device.
struct MemFile {
    at: Place,
    /// The open handles of the instance that opened it.
    handles: Arc<AtomicUsize>,
    ino: u64,
    number: u64,
    kind: EntryKind,
    options: OpenOptions,
    block_size: u32,
}

impl fmt::Debug for MemFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemFile")
            .field("number", &self.number)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl MemFile {
    /// The error that fails a whole batch of these ranges, if any (clause
    /// 3.1.11).
    fn refuse(&self, disk: &Disk, ranges: &[(u64, usize)], write: bool) -> Option<DiskError> {
        let size = u64::from(self.block_size);
        if ranges.len() > MAX_DISK_BATCH {
            return Some(DiskError::BatchTooLarge);
        }
        if self.options.direct
            && ranges.iter().any(|(offset, len)| {
                !offset.is_multiple_of(size) || !(*len as u64).is_multiple_of(size)
            })
        {
            return Some(DiskError::Unaligned);
        }
        if write && self.options.access == Access::Read {
            return Some(DiskError::ReadOnly);
        }
        if write {
            let mut sorted: Vec<(u64, usize)> =
                ranges.iter().copied().filter(|r| r.1 > 0).collect();
            sorted.sort_unstable();
            if sorted
                .windows(2)
                .any(|pair| pair[0].0.saturating_add(pair[0].1 as u64) > pair[1].0)
            {
                return Some(invalid("two writes of the batch overlap"));
            }
        }
        if self.kind == EntryKind::Block {
            let len = disk.len(self.ino);
            if ranges
                .iter()
                .any(|(offset, n)| offset.checked_add(*n as u64).is_none_or(|end| end > len))
            {
                return Some(invalid("an operation ends past the end of the device"));
            }
        }
        None
    }

    /// A call that writes one record with this handle.
    fn call<T: Send + 'static>(
        &self,
        op: u8,
        range: (u64, u64),
        f: impl FnOnce(&mut Disk) -> Result<T, DiskError>,
    ) -> BoxFuture<'static, Result<T, DiskError>> {
        one_call(&self.at, self.number, op, range, f)
    }
}

impl DiskFile for MemFile {
    fn info(&self) -> BoxFuture<'_, Result<FileInfo, DiskError>> {
        self.call(OP_INFO, (0, 0), |disk| {
            Ok(FileInfo {
                kind: self.kind,
                len: disk.len(self.ino),
                block_size: self.block_size,
            })
        })
    }

    fn read_at(&self, ops: Vec<ReadOp>) -> BoxFuture<'_, Vec<ReadDone>> {
        let ranges: Vec<(u64, usize)> = ops.iter().map(|op| (op.offset, op.buf.len())).collect();
        let done = with_disk(&self.at, |mut disk, record| {
            let refused = match disk.as_deref() {
                Some(disk) => self.refuse(disk, &ranges, false),
                None => Some(DiskError::Io),
            };
            let mut done = Vec::with_capacity(ops.len());
            for ReadOp { offset, mut buf } in ops {
                let result = match (&refused, disk.as_deref_mut()) {
                    (None, Some(disk)) => disk.read(self.ino, offset, &mut buf),
                    (error, _) => Err(error.clone().unwrap_or(DiskError::Io)),
                };
                let returned = result.as_ref().ok().map(|count| &buf[..*count]);
                let range = (offset, buf.len() as u64);
                record(self.number, OP_READ, range, result.as_ref().err(), returned);
                done.push(ReadDone { buf, result });
            }
            done
        });
        Box::pin(ready(done))
    }

    fn write_at(&self, ops: Vec<WriteOp>) -> BoxFuture<'_, Vec<WriteDone>> {
        let ranges: Vec<(u64, usize)> = ops.iter().map(|op| (op.offset, op.buf.len())).collect();
        let done = with_disk(&self.at, |mut disk, record| {
            let refused = match disk.as_deref() {
                Some(disk) => self.refuse(disk, &ranges, true),
                None => Some(DiskError::Io),
            };
            let mut done = Vec::with_capacity(ops.len());
            for WriteOp { offset, buf } in ops {
                let result = match (&refused, disk.as_deref_mut()) {
                    (None, Some(disk)) => disk.write(self.ino, offset, &buf),
                    (error, _) => Err(error.clone().unwrap_or(DiskError::Io)),
                };
                let range = (offset, buf.len() as u64);
                record(
                    self.number,
                    OP_WRITE,
                    range,
                    result.as_ref().err(),
                    Some(&buf),
                );
                done.push(WriteDone { buf, result });
            }
            done
        });
        Box::pin(ready(done))
    }

    /// Every call completes when it is made, so nothing is pending: the sync
    /// makes every change of the file so far durable (clause 3.3.4).
    fn sync(&self, kind: SyncKind) -> BoxFuture<'_, Result<(), DiskError>> {
        self.call(OP_SYNC, (0, 0), |disk| {
            disk.sync(self.ino, kind);
            Ok(())
        })
    }

    fn truncate(&self, len: u64) -> BoxFuture<'_, Result<(), DiskError>> {
        self.call(OP_TRUNCATE, (0, len), |disk| {
            if self.kind == EntryKind::Block {
                Err(DiskError::Unsupported)
            } else if self.options.access == Access::Read {
                Err(DiskError::ReadOnly)
            } else {
                disk.truncate(self.ino, len)
            }
        })
    }
}

impl Drop for MemFile {
    fn drop(&mut self) {
        self.handles.fetch_sub(1, Ordering::AcqRel);
        let mut core = lock(&self.at.shared.core);
        if let Ok(host) = core.host_mut(self.at.host)
            && host.live(self.at.start)
        {
            host.disk.close(self.ino, self.options.exclusive);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk() -> Disk {
        let reads = Stream::new(1, b"test-reads", &[]);
        Disk::new(4_096, 1 << 30, &[], reads).expect("a valid disk")
    }

    fn create(disk: &mut Disk, at: &str) -> u64 {
        let how = OpenOptions {
            access: Access::ReadWrite,
            create: Create::IfMissing,
            direct: false,
            exclusive: false,
        };
        let path = RtPath::new(at).expect("a valid path");
        disk.open(&path, how, FILE_MODE).expect("open").ino
    }

    fn bytes(disk: &mut Disk, ino: u64) -> Vec<u8> {
        let mut buf = vec![0; 64];
        let count = disk.read(ino, 0, &mut buf).expect("read");
        buf.truncate(count);
        buf
    }

    #[test]
    fn data_is_sparse_and_zero_past_its_length() {
        let mut data = Data::default();
        data.write(10_000, b"xy");
        assert_eq!((data.len, data.chunks.len()), (10_002, 1));
        let mut buf = [9u8; 8];
        assert_eq!(data.read(9_996, &mut buf), 6);
        assert_eq!(buf, [0, 0, 0, 0, b'x', b'y', 9, 9]);
        data.set_len(10_001);
        data.set_len(20_000);
        assert_eq!(data.read(10_000, &mut buf), 8);
        assert_eq!(buf, [b'x', 0, 0, 0, 0, 0, 0, 0]);
        data.set_len(0);
        assert!(data.chunks.is_empty());
        assert_eq!(data.read(0, &mut buf), 0);
    }

    #[test]
    fn the_log_folds_its_durable_prefix_into_the_base() {
        let mut disk = disk();
        let ino = create(&mut disk, "/f");
        disk.write(ino, 0, b"abc").expect("write");
        disk.sync(ino, SyncKind::Data);
        assert_eq!(disk.log.len(), 2, "the create is not durable yet");
        disk.sync_dir(&RtPath::root()).expect("sync_dir");
        assert!(disk.log.is_empty() && disk.synced.is_empty());
        disk.write(ino, 1, b"Z").expect("write");
        assert_eq!(disk.log.len(), 1);
        disk.crash(Unsynced::Lost, &mut Stream::new(1, b"t", &[]));
        let ino = create(&mut disk, "/f");
        assert_eq!(bytes(&mut disk, ino), b"abc");
    }

    #[test]
    fn a_seeded_crash_drops_what_depends_on_a_dropped_change() {
        let mut seen = BTreeSet::new();
        for seed in 0..32 {
            let mut disk = disk();
            let ino = create(&mut disk, "/f");
            disk.write(ino, 0, b"data").expect("write");
            disk.sync(ino, SyncKind::Data);
            disk.rename(
                &RtPath::new("/f").expect("path"),
                &RtPath::new("/g").expect("path"),
            )
            .expect("rename");
            disk.crash(Unsynced::Seeded, &mut Stream::new(seed, b"t", &[]));
            let names: Vec<String> = disk
                .list(&RtPath::root())
                .expect("list")
                .into_iter()
                .map(|e| e.name)
                .collect();
            let ino = disk
                .current
                .find(&RtPath::new("/f").expect("path"))
                .expect("find")
                .1;
            let ino = ino.or(disk
                .current
                .find(&RtPath::new("/g").expect("path"))
                .expect("find")
                .1);
            let content = ino.map(|ino| bytes(&mut disk, ino));
            seen.insert((names, content));
        }
        let some = |names: &[&str]| {
            (
                names.iter().map(|n| (*n).to_owned()).collect(),
                Some(b"data".to_vec()),
            )
        };
        let expected = BTreeSet::from([(Vec::new(), None), some(&["f"]), some(&["g"])]);
        assert_eq!(seen, expected);
    }

    fn at(text: &str) -> RtPath {
        RtPath::new(text).expect("a valid path")
    }

    /// The mode of the entry at `path`, or `None` when it is missing.
    fn mode(disk: &Disk, path: &str) -> Option<u32> {
        match disk.security(&at(path)) {
            Ok(Security::Unix {
                symlink: false,
                uid: 0,
                mode,
                ..
            }) => Some(mode),
            Err(DiskError::NotFound) => None,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn entries_start_with_the_modes_of_their_kind() {
        let device = DeviceConfig {
            path: at("/dev/sda"),
            len: 4_096,
            block_size: 512,
        };
        let reads = Stream::new(1, b"test-reads", &[]);
        let mut disk = Disk::new(4_096, 1 << 30, &[device], reads).expect("a valid disk");
        create(&mut disk, "/f");
        disk.create_dir(&at("/d"), DIR_MODE).expect("create_dir");
        disk.create_dir(&at("/p"), PRIVATE_DIR_MODE)
            .expect("create_dir");
        let how = OpenOptions {
            access: Access::ReadWrite,
            create: Create::New,
            direct: false,
            exclusive: false,
        };
        disk.open(&at("/p/k"), how, PRIVATE_FILE_MODE)
            .expect("open");
        let modes: Vec<Option<u32>> = ["/", "/f", "/d", "/dev", "/dev/sda", "/p", "/p/k", "/x"]
            .iter()
            .map(|path| mode(&disk, path))
            .collect();
        let expected = [0o755, 0o644, 0o755, 0o755, 0o660, 0o700, 0o600];
        let expected: Vec<Option<u32>> = expected.into_iter().map(Some).chain([None]).collect();
        assert_eq!(modes, expected);
        assert_eq!(
            disk.security(&at("/dev/sda")),
            Ok(Security::Unix {
                kind: EntryKind::Block,
                symlink: false,
                uid: 0,
                mode: 0o660
            })
        );
        assert_eq!(disk.security(&at("/f/x")), Err(DiskError::NotDirectory));
        assert!(matches!(
            disk.set_mode(&at("/f"), 0o10_000),
            Err(DiskError::InvalidInput(_))
        ));
        assert_eq!(disk.set_mode(&at("/x"), 0o600), Err(DiskError::NotFound));
        // User id 0 may set any mode, and nothing is refused for one.
        disk.set_mode(&at("/d"), 0).expect("set_mode");
        disk.set_mode(&at("/dev/sda"), 0o7777).expect("set_mode");
        create(&mut disk, "/d/inner");
        assert_eq!(mode(&disk, "/d"), Some(0));
        assert_eq!(mode(&disk, "/dev/sda"), Some(0o7777));
    }

    #[test]
    fn a_mode_is_kept_by_a_full_sync_of_its_file_or_a_sync_of_its_directory() {
        let mut disk = disk();
        let ino = create(&mut disk, "/f");
        disk.create_dir(&at("/d"), DIR_MODE).expect("create_dir");
        disk.sync_dir(&RtPath::root()).expect("sync_dir");
        let lost = |disk: &mut Disk| disk.crash(Unsynced::Lost, &mut Stream::new(1, b"t", &[]));
        // Neither `sync(Data)` nor a `sync_dir` of the parent keeps a mode.
        disk.set_mode(&at("/f"), 0o600).expect("set_mode");
        disk.set_mode(&at("/d"), 0o700).expect("set_mode");
        disk.sync(ino, SyncKind::Data);
        disk.sync_dir(&RtPath::root()).expect("sync_dir");
        lost(&mut disk);
        assert_eq!(
            (mode(&disk, "/f"), mode(&disk, "/d")),
            (Some(0o644), Some(0o755))
        );
        // `sync(All)` of the file and a `sync_dir` of the directory itself do.
        disk.set_mode(&at("/f"), 0o600).expect("set_mode");
        disk.set_mode(&at("/d"), 0o700).expect("set_mode");
        disk.sync(ino, SyncKind::All);
        disk.sync_dir(&at("/d")).expect("sync_dir");
        lost(&mut disk);
        assert_eq!(
            (mode(&disk, "/f"), mode(&disk, "/d")),
            (Some(0o600), Some(0o700))
        );
        assert!(disk.log.is_empty());
        // A mode given at creation is kept or undone with the create.
        disk.create_dir(&at("/d/p"), PRIVATE_DIR_MODE)
            .expect("create_dir");
        lost(&mut disk);
        assert_eq!(mode(&disk, "/d/p"), None);
        disk.create_dir(&at("/d/p"), PRIVATE_DIR_MODE)
            .expect("create_dir");
        disk.sync_dir(&at("/d")).expect("sync_dir");
        lost(&mut disk);
        assert_eq!(mode(&disk, "/d/p"), Some(0o700));
    }

    #[test]
    fn a_seeded_crash_drops_a_set_mode_whose_create_it_drops() {
        let mut seen = BTreeSet::new();
        for seed in 0..32 {
            let mut disk = disk();
            create(&mut disk, "/f");
            disk.set_mode(&at("/f"), 0o600).expect("set_mode");
            disk.crash(Unsynced::Seeded, &mut Stream::new(seed, b"t", &[]));
            seen.insert(mode(&disk, "/f"));
        }
        assert_eq!(seen, BTreeSet::from([None, Some(0o644), Some(0o600)]));
    }
}
