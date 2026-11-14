//! Ownership and permissions of the `os` disk-io on Linux and macOS (ARFC
//! 0005 clause 3.1.15).

use std::fs::{self, File, Permissions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use super::{errors, invalid, kind_of};
use crate::{DiskError, EntryKind, Protection, Security};

/// The mode of a `Private` or `PrivateRoot` file.
const FILE_MODE: u32 = 0o600;
/// The mode of a `Private` or `PrivateRoot` directory.
const DIR_MODE: u32 = 0o700;
/// The permission bits `security` reports and `set_mode` sets.
const PERMISSION_BITS: u32 = 0o7777;

/// Creates the file `path`, which the caller has found missing, with the
/// options `std_options` (which say `create_new`) and mode `0o600` for any
/// protection but `Default`. The mode is passed to `open`, so the file never
/// has more access bits than it; bits the umask removed are then set through
/// the new descriptor. When that fails the file is removed.
pub(super) fn create_file(
    std_options: &mut fs::OpenOptions,
    path: &Path,
    protection: Protection,
) -> Result<File, DiskError> {
    if protection != Protection::Default {
        std_options.mode(FILE_MODE);
    }
    let file = std_options.open(path).map_err(|e| errors::disk(&e))?;
    if protection == Protection::Default {
        return Ok(file);
    }
    // `O_CREAT | O_EXCL` made this file, so it is the process's own.
    match settle(&file, FILE_MODE) {
        Ok(()) => Ok(file),
        Err(e) => {
            drop(file);
            let _ = fs::remove_file(path);
            Err(e)
        }
    }
}

/// Creates the directory `path` with mode `0o700` for any protection but
/// `Default`, which creates as `create_dir` does. The mode is passed to
/// `mkdir`; the new directory is then opened without following a symbolic
/// link, checked to be the process's own, and its mode set through that
/// descriptor when the umask removed bits or a set-group-ID parent added
/// one. When any of that fails the directory is removed.
pub(super) fn create_dir(path: &Path, protection: Protection) -> Result<(), DiskError> {
    if protection == Protection::Default {
        return fs::create_dir(path).map_err(|e| errors::disk(&e));
    }
    fs::DirBuilder::new()
        .mode(DIR_MODE)
        .create(path)
        .map_err(|e| errors::disk(&e))?;
    let settled = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| errors::disk(&e))
        .and_then(|dir| {
            // Another user who may write the parent could have put their own
            // directory at `path` between `mkdir` and `open`.
            let owner = dir.metadata().map_err(|e| errors::disk(&e))?.uid();
            if owner != effective_uid() {
                return Err(DiskError::PermissionDenied);
            }
            settle(&dir, DIR_MODE)
        });
    if settled.is_err() {
        let _ = fs::remove_dir(path);
    }
    settled
}

/// Makes the permission bits of the open entry `file` exactly `mode`. A
/// change is flushed through the same descriptor, so that the mode that
/// survives a crash is the one the create gave (ARFC 0005 clause 3.1.15).
fn settle(file: &File, mode: u32) -> Result<(), DiskError> {
    let meta = file.metadata().map_err(|e| errors::disk(&e))?;
    if meta.mode() & PERMISSION_BITS == mode {
        return Ok(());
    }
    file.set_permissions(Permissions::from_mode(mode))
        .and_then(|()| file.sync_all())
        .map_err(|e| errors::disk(&e))
}

#[allow(unsafe_code, reason = "geteuid has no safe binding")]
fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory of ours, and
    // always succeeds.
    unsafe { libc::geteuid() }
}

/// `Security::Unix` from `lstat` of `path`.
pub(super) fn security(path: &Path) -> Result<Security, DiskError> {
    let meta = fs::symlink_metadata(path).map_err(|e| errors::disk(&e))?;
    let symlink = meta.file_type().is_symlink();
    let kind = if symlink {
        EntryKind::Other
    } else {
        kind_of(&meta.file_type())
    };
    Ok(Security::Unix {
        kind,
        symlink,
        uid: meta.uid(),
        mode: meta.mode() & PERMISSION_BITS,
    })
}

/// `chmod` of `path`, which follows a symbolic link and keeps the owner.
pub(super) fn set_mode(path: &Path, mode: u32) -> Result<(), DiskError> {
    if mode > PERMISSION_BITS {
        return Err(invalid("a mode is at most 0o7777"));
    }
    fs::set_permissions(path, Permissions::from_mode(mode)).map_err(|e| errors::disk(&e))
}
