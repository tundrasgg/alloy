//! The files a node keeps in its data directory (ARFC 0002 section 3.14),
//! through the `alloy-rt` `disk-io` seam.
//!
//! Every file is written whole: `<name>.new` is written and synced, then
//! renamed over `<name>` with the seam's `replace`, which returns once the
//! rename is durable (ARFC 0005 clause 3.1.16), so before the call returns
//! and before any other file is written. After a crash each file holds its
//! last durable content or the content before that write, and writes reach
//! the disk in the order they were made. Readers ignore `.new` files.
//!
//! [`DataDir::create`] creates the data directory with
//! `Protection::PrivateRoot`, and every file is created with
//! `Protection::Private` (section 3.12, ARFC 0005 clause 3.1.15), so no
//! file ever exists with more access than the data directory's own.

use std::sync::Arc;

use alloy_crypto::Zeroizing;
use alloy_crypto::aead::{self, NONCE_LEN, TAG_LEN};
use alloy_rt::{
    Access, Create, DiskError, DiskIo, EntryKind, IoBuf, OpenOptions, Protection, ReadOp, Rng,
    RngError, RtPath, SyncKind, WriteOp,
};
use prost::Message as _;

use crate::keys::AtRestKey;
use crate::proto::{NodeSecrets, NodeState};

/// The host file (ARFC 0017 section 4.2).
pub const HOST_FILE: &str = "alloy-host";
/// The at-rest key: 32 bytes, or 64 during a rotation.
pub const AT_REST_KEY_FILE: &str = "at-rest.key";
/// `NodeSecrets`, sealed under the at-rest key.
pub const SECRETS_FILE: &str = "secrets";
/// The end of the highest block of handshake counter values reserved.
pub const HANDSHAKE_COUNTER_FILE: &str = "handshake-counter";
/// `NodeState`.
pub const NODE_FILE: &str = "node";
/// The directory of the `file` disk.
pub const DISKS_DIR: &str = "disks";

/// The associated data of the `secrets` seal.
pub const SECRETS_AD: &[u8] = b"alloy/at-rest/secrets/v1";

/// The longest file this module reads, in bytes. A `NodeState` holds at most
/// 17 records and a `NodeSecrets` a join request of under 16,384 bytes, so
/// both stay far below it.
pub const MAX_FILE_LEN: usize = 1 << 20;

/// Why a stored file could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The disk failed.
    #[error("disk: {0}")]
    Disk(#[from] DiskError),
    /// The randomness trait failed.
    #[error("random generator: {0}")]
    Rng(#[from] RngError),
    /// A file is longer than [`MAX_FILE_LEN`].
    #[error("{0} is too long")]
    TooLong(&'static str),
    /// The data directory is damaged: `at-rest.key` has a length other than
    /// 32 or 64, `secrets` opens under no key it holds, or a file does not
    /// decode. `alloyd` then logs it and serves nothing.
    #[error("the data directory is damaged: {0}")]
    Damaged(&'static str),
}

/// The at-rest key or keys that `at-rest.key` holds.
#[derive(Debug)]
pub enum AtRestKeys {
    /// 32 bytes: the key.
    One(AtRestKey),
    /// 64 bytes, during a rotation: the old key, then the new.
    Rotating {
        /// The key being replaced.
        old: AtRestKey,
        /// The key replacing it.
        new: AtRestKey,
    },
}

/// The secrets of a node, opened, with the at-rest key that now seals them.
#[derive(Debug)]
pub struct Opened {
    /// The at-rest key.
    pub key: AtRestKey,
    /// The secrets.
    pub secrets: NodeSecrets,
}

/// A data directory on a `disk-io` instance.
#[derive(Debug, Clone)]
pub struct DataDir {
    disk: Arc<dyn DiskIo>,
    dir: RtPath,
}

impl DataDir {
    /// The data directory at `dir` on `disk`. Its checks (section 3.12) are
    /// the caller's, before it reads a secret.
    #[must_use]
    pub fn new(disk: Arc<dyn DiskIo>, dir: RtPath) -> Self {
        Self { disk, dir }
    }

    /// Creates the missing data directory at `dir` with
    /// [`Protection::PrivateRoot`], so it has its owner and its mode or
    /// access control list from the call that creates it, and makes the
    /// create durable by syncing its parent, as `init` and `join` do
    /// (section 3.12). The caller writes the host file in it next, before
    /// anything else.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disk`]: [`DiskError::AlreadyExists`] when something is
    /// at `dir`, [`DiskError::InvalidPath`] for the root, and on Windows
    /// [`DiskError::PermissionDenied`] for a process that may not name
    /// `Administrators` as owner. Nothing is left at `dir` when the protection
    /// cannot be given.
    pub async fn create(disk: Arc<dyn DiskIo>, dir: RtPath) -> Result<Self, StoreError> {
        let parent = dir.parent().ok_or(DiskError::InvalidPath)?;
        disk.create_dir_protected(&dir, Protection::PrivateRoot)
            .await?;
        disk.sync_dir(&parent).await?;
        Ok(Self { disk, dir })
    }

    /// Adopts an empty directory at `dir` on Linux, such as a mount point,
    /// whose checks passed with [`InitAction::Adopt`](crate::InitAction):
    /// sets its mode to `0700` and makes that durable by syncing the
    /// directory itself, before anything is written in it (section 3.12).
    ///
    /// # Errors
    ///
    /// [`StoreError::Disk`]; [`DiskError::Unsupported`] on Windows, which
    /// adopts no directory.
    pub async fn adopt(disk: Arc<dyn DiskIo>, dir: RtPath) -> Result<Self, StoreError> {
        disk.set_mode(&dir, 0o700).await?;
        disk.sync_dir(&dir).await?;
        Ok(Self { disk, dir })
    }

    /// Writes the file `name` whole, as the module documentation says.
    ///
    /// `<name>.new` is created with [`Protection::Private`], after a
    /// `<name>.new` that an interrupted write left is removed, so the file
    /// never has more access than section 3.12 allows: mode `0600` on Linux,
    /// and on Windows owner `Administrators` and the list it inherits from
    /// the data directory. The rename carries that protection to `<name>`.
    ///
    /// The rename is [`DiskIo::replace`], durable when it returns: by `fsync`
    /// of the data directory on Linux, and on Windows by `MoveFileExW` with
    /// `MOVEFILE_WRITE_THROUGH` (ARFC 0005 clause 3.1.16, ARFC 0002 section
    /// 3.14). `replace` makes only its own rename and the create of
    /// `<name>.new` durable, so the removal of a stale `<name>.new` is made
    /// durable by `sync_dir` first: else a crash could undo that removal,
    /// and with it the create and the rename that depend on the name.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disk`]; the file then holds its old content or the new.
    pub async fn write(&self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.dir.join(name)?;
        let temp = self.dir.join(&format!("{name}.new"))?;
        match self.disk.remove(&temp).await {
            Ok(()) => self.disk.sync_dir(&self.dir).await?,
            Err(DiskError::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        let file = self.disk.create_file(&temp, Protection::Private).await?;
        let buf = IoBuf::from_slice(bytes)?;
        for mut done in file.write_at(vec![WriteOp { offset: 0, buf }]).await {
            // The buffer may hold a key; erase it before it is freed.
            done.buf.fill(0);
            done.result?;
        }
        file.sync(SyncKind::All).await?;
        drop(file);
        self.disk.replace(&temp, &path).await?;
        Ok(())
    }

    /// Reads the file `name` whole. `None` when it does not exist. The
    /// bytes erase themselves when dropped.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disk`], and [`StoreError::TooLong`] above
    /// [`MAX_FILE_LEN`].
    pub async fn read(&self, name: &'static str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        let options = OpenOptions {
            access: Access::Read,
            create: Create::Never,
            direct: false,
            exclusive: false,
        };
        let file = match self.disk.open(&self.dir.join(name)?, options).await {
            Ok(file) => file,
            Err(DiskError::NotFound) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let len = usize::try_from(file.info().await?.len).map_err(|_| StoreError::TooLong(name))?;
        if len > MAX_FILE_LEN {
            return Err(StoreError::TooLong(name));
        }
        let mut done = file
            .read_at(vec![ReadOp {
                offset: 0,
                buf: IoBuf::zeroed(len)?,
            }])
            .await;
        let mut read = done.pop().ok_or(DiskError::Io)?;
        let bytes = match read.result {
            Ok(count) if count == len => Ok(Zeroizing::new(read.buf.to_vec())),
            Ok(_) => Err(DiskError::Io.into()),
            Err(e) => Err(e.into()),
        };
        // The buffer may hold a key; erase it before it is freed.
        read.buf.fill(0);
        bytes.map(Some)
    }

    /// Removes the file `name`, and makes the removal durable. A missing
    /// file is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disk`].
    pub async fn remove(&self, name: &str) -> Result<(), StoreError> {
        match self.disk.remove(&self.dir.join(name)?).await {
            Ok(()) | Err(DiskError::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        self.disk.sync_dir(&self.dir).await?;
        Ok(())
    }

    /// Reads `at-rest.key`. `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// [`StoreError::Damaged`] for a length other than 32 or 64.
    pub async fn read_at_rest_keys(&self) -> Result<Option<AtRestKeys>, StoreError> {
        let Some(bytes) = self.read(AT_REST_KEY_FILE).await? else {
            return Ok(None);
        };
        let key = |range: std::ops::Range<usize>| {
            let mut array = Zeroizing::new([0u8; aead::KEY_LEN]);
            array.copy_from_slice(&bytes[range]);
            AtRestKey::from_bytes(&array)
        };
        match bytes.len() {
            32 => Ok(Some(AtRestKeys::One(key(0..32)))),
            64 => Ok(Some(AtRestKeys::Rotating {
                old: key(0..32),
                new: key(32..64),
            })),
            _ => Err(StoreError::Damaged(
                "at-rest.key is neither 32 nor 64 bytes",
            )),
        }
    }

    /// Writes `secrets` under `key`, with a new 24-byte nonce from `rng`:
    /// the nonce, then the XChaCha20-Poly1305 encryption with associated
    /// data `alloy/at-rest/secrets/v1`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Rng`] and [`StoreError::Disk`].
    pub async fn write_secrets(
        &self,
        rng: &dyn Rng,
        key: &AtRestKey,
        secrets: &NodeSecrets,
    ) -> Result<(), StoreError> {
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill(&mut nonce)?;
        let plaintext = Zeroizing::new(secrets.encode_to_vec());
        let sealed = key
            .aead()
            .seal(&nonce, SECRETS_AD, &plaintext)
            .map_err(|_| StoreError::Damaged("secrets too long to seal"))?;
        let mut file = Vec::with_capacity(NONCE_LEN + sealed.len());
        file.extend_from_slice(&nonce);
        file.extend_from_slice(&sealed);
        self.write(SECRETS_FILE, &file).await
    }

    /// Opens `secrets` under `key`. `None` when it does not authenticate.
    async fn open_secrets(&self, key: &AtRestKey) -> Result<Option<NodeSecrets>, StoreError> {
        let file = self
            .read(SECRETS_FILE)
            .await?
            .ok_or(StoreError::Damaged("secrets is missing"))?;
        if file.len() < NONCE_LEN + TAG_LEN {
            return Ok(None);
        }
        let (nonce, sealed) = file.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce
            .try_into()
            .map_err(|_| StoreError::Damaged("secrets"))?;
        let Ok(plaintext) = key.aead().open(&nonce, SECRETS_AD, sealed) else {
            return Ok(None);
        };
        NodeSecrets::decode(&plaintext[..])
            .map(Some)
            .map_err(|_| StoreError::Damaged("secrets does not decode"))
    }

    /// Writes a node's first at-rest key and its secrets: `at-rest.key`,
    /// then `secrets` (section 3.14). Returns the new key.
    ///
    /// # Errors
    ///
    /// [`StoreError::Rng`] and [`StoreError::Disk`].
    pub async fn create_secrets(
        &self,
        rng: &dyn Rng,
        secrets: &NodeSecrets,
    ) -> Result<AtRestKey, StoreError> {
        let key = AtRestKey::generate(rng).map_err(|_| StoreError::Rng(RngError::Unavailable))?;
        self.write(AT_REST_KEY_FILE, key.as_bytes()).await?;
        self.write_secrets(rng, &key, secrets).await?;
        Ok(key)
    }

    /// Opens the node's secrets, as `alloyd` does at start, after its data
    /// directory checks passed. With 64 bytes in `at-rest.key` it opens
    /// `secrets` under whichever key authenticates it and finishes the
    /// rotation: from write 2 when the old key opened it, else from write 3.
    /// `None` when there is no `at-rest.key`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Damaged`] when `secrets` opens under no key, and the
    /// errors of reading and writing.
    pub async fn open(&self, rng: &dyn Rng) -> Result<Option<Opened>, StoreError> {
        let Some(keys) = self.read_at_rest_keys().await? else {
            return Ok(None);
        };
        let damaged = StoreError::Damaged("secrets opens under no at-rest key");
        match keys {
            AtRestKeys::One(key) => {
                let secrets = self.open_secrets(&key).await?.ok_or(damaged)?;
                Ok(Some(Opened { key, secrets }))
            }
            AtRestKeys::Rotating { old, new } => {
                if let Some(secrets) = self.open_secrets(&old).await? {
                    self.write_secrets(rng, &new, &secrets).await?;
                    self.write(AT_REST_KEY_FILE, new.as_bytes()).await?;
                    return Ok(Some(Opened { key: new, secrets }));
                }
                let secrets = self.open_secrets(&new).await?.ok_or(damaged)?;
                self.write(AT_REST_KEY_FILE, new.as_bytes()).await?;
                Ok(Some(Opened { key: new, secrets }))
            }
        }
    }

    /// Replaces the at-rest key, as `alloy node rotate` does, in three
    /// writes each durable before the next: `at-rest.key` becomes the old
    /// key then a new one, `secrets` is written under the new key, and
    /// `at-rest.key` becomes the new key. Returns the new key.
    ///
    /// # Errors
    ///
    /// [`StoreError::Rng`] and [`StoreError::Disk`]. After a failure,
    /// [`DataDir::open`] still opens the secrets and finishes the rotation.
    pub async fn rotate_at_rest_key(
        &self,
        rng: &dyn Rng,
        opened: &Opened,
    ) -> Result<AtRestKey, StoreError> {
        let new = AtRestKey::generate(rng).map_err(|_| StoreError::Rng(RngError::Unavailable))?;
        let mut both = Zeroizing::new([0u8; 2 * aead::KEY_LEN]);
        both[..aead::KEY_LEN].copy_from_slice(opened.key.as_bytes());
        both[aead::KEY_LEN..].copy_from_slice(new.as_bytes());
        self.write(AT_REST_KEY_FILE, &both[..]).await?;
        self.write_secrets(rng, &new, &opened.secrets).await?;
        self.write(AT_REST_KEY_FILE, new.as_bytes()).await?;
        Ok(new)
    }

    /// Reads `handshake-counter`. `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// [`StoreError::Damaged`] when it is not 8 bytes.
    pub async fn read_handshake_counter(&self) -> Result<Option<u64>, StoreError> {
        let Some(bytes) = self.read(HANDSHAKE_COUNTER_FILE).await? else {
            return Ok(None);
        };
        let bytes: [u8; 8] = bytes[..]
            .try_into()
            .map_err(|_| StoreError::Damaged("handshake-counter is not 8 bytes"))?;
        Ok(Some(u64::from_le_bytes(bytes)))
    }

    /// Writes `handshake-counter`: 8 bytes, little endian.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disk`].
    pub async fn write_handshake_counter(&self, end: u64) -> Result<(), StoreError> {
        self.write(HANDSHAKE_COUNTER_FILE, &end.to_le_bytes()).await
    }

    /// Reads `node`. `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// [`StoreError::Damaged`] when it does not decode.
    pub async fn read_node_state(&self) -> Result<Option<NodeState>, StoreError> {
        let Some(bytes) = self.read(NODE_FILE).await? else {
            return Ok(None);
        };
        NodeState::decode(&bytes[..])
            .map(Some)
            .map_err(|_| StoreError::Damaged("node does not decode"))
    }

    /// Writes `node`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disk`].
    pub async fn write_node_state(&self, state: &NodeState) -> Result<(), StoreError> {
        self.write(NODE_FILE, &state.encode_to_vec()).await
    }

    /// What a failed join attempt leaves (section 3.14 and section 6):
    /// `secrets` rewritten with only the node keys, and `node` removed.
    /// `at-rest.key`, `handshake-counter`, and the host file are kept.
    ///
    /// # Errors
    ///
    /// [`StoreError::Rng`] and [`StoreError::Disk`].
    pub async fn keep_only_node_keys(
        &self,
        rng: &dyn Rng,
        opened: &Opened,
    ) -> Result<(), StoreError> {
        let mut kept = NodeSecrets::default();
        kept.identity_key.clone_from(&opened.secrets.identity_key);
        kept.mesh_key.clone_from(&opened.secrets.mesh_key);
        self.write_secrets(rng, &opened.key, &kept).await?;
        self.remove(NODE_FILE).await
    }

    /// What `leave` does here: removes every file but the host file, the
    /// `disks` directory with the files in it included.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disk`].
    pub async fn remove_all_but_host_file(&self) -> Result<(), StoreError> {
        for entry in self.disk.list(&self.dir).await? {
            if entry.name == HOST_FILE {
                continue;
            }
            let path = self.dir.join(&entry.name)?;
            if entry.kind == EntryKind::Dir {
                for inner in self.disk.list(&path).await? {
                    self.disk.remove(&path.join(&inner.name)?).await?;
                }
            }
            self.disk.remove(&path).await?;
        }
        self.disk.sync_dir(&self.dir).await?;
        Ok(())
    }
}
