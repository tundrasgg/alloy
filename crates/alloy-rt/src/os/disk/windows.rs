//! Ownership and access control lists of the `os` disk-io on Windows (ARFC
//! 0005 clause 3.1.15), and its write-through `replace` (clause 3.1.16).
//!
//! `Private` and `PrivateRoot` pass a security descriptor to the call that
//! creates the entry, so the entry never exists without it. `security` reads
//! the owner and the discretionary list of the entry, opened without
//! following a reparse point, and decodes the list from its bytes.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetLengthSid, GetSecurityDescriptorControl, IsValidSid,
    LookupAccountSidW, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, SID_NAME_USE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, READ_CONTROL,
};

use super::{errors, invalid};
use crate::{Ace, DiskError, EntryKind, Protection, Security, Sid};

/// The SDDL alias of `Administrators` (`S-1-5-32-544`), the owner of every
/// `Private` and `PrivateRoot` entry and the second grantee of a
/// `PrivateRoot` list.
pub(super) const ADMINISTRATORS: &str = "BA";
/// The most entries `security` decodes from one list (ARFC 0005 clause
/// 3.1.15).
const MAX_ACES: usize = 4_096;
/// `ERROR_INVALID_OWNER`: the token may not name that owner.
const ERROR_INVALID_OWNER: i32 = 1_307;
/// `ERROR_PRIVILEGE_NOT_HELD`.
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1_314;
/// The access-denied ACE types: `ACCESS_DENIED_ACE_TYPE`,
/// `ACCESS_DENIED_OBJECT_ACE_TYPE`, `ACCESS_DENIED_CALLBACK_ACE_TYPE`, and
/// `ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE`.
const DENIED_TYPES: [u8; 4] = [1, 6, 10, 12];
/// The ACE types laid out as an object ACE: mask, flags, up to two GUIDs,
/// then the trustee.
const OBJECT_TYPES: [u8; 8] = [5, 6, 7, 8, 11, 12, 15, 16];
/// `ACE_OBJECT_TYPE_PRESENT` and `ACE_INHERITED_OBJECT_TYPE_PRESENT`.
const OBJECT_GUID_FLAGS: [u32; 2] = [1, 2];
/// The `ACE_HEADER` flags `Ace` reports.
const OBJECT_INHERIT: u8 = 0x01;
const CONTAINER_INHERIT: u8 = 0x02;
const INHERIT_ONLY: u8 = 0x08;
const INHERITED: u8 = 0x10;

/// The descriptor that `protection` gives an entry, in SDDL, with `owner` as
/// owner and `admins` as the second grantee of a `PrivateRoot` list. `None`
/// for `Default`, which passes none.
fn sddl(protection: Protection, dir: bool, owner: &str, admins: &str) -> Option<String> {
    let inherit = if dir { "OICI" } else { "" };
    match protection {
        Protection::Default => None,
        // No list at all (`SE_DACL_PRESENT` clear), so the entry gets what
        // the parent passes on. An empty list would grant no one anything.
        Protection::Private => Some(format!("O:{owner}")),
        Protection::PrivateRoot => Some(format!(
            "O:{owner}D:P(A;{inherit};FA;;;SY)(A;{inherit};FA;;;{admins})"
        )),
    }
}

/// A security descriptor that `LocalFree` releases.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    #[allow(unsafe_code, reason = "the SDDL conversion has no safe binding")]
    fn from_sddl(sddl: &str) -> io::Result<Descriptor> {
        let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `text` is NUL-terminated and outlives the call, and
        // `descriptor` is a valid place for the result. The size is not
        // asked for, which a null pointer says.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Descriptor(descriptor))
        }
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            // The structure is 24 bytes, far below `u32::MAX`.
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for Descriptor {
    #[allow(unsafe_code, reason = "LocalFree has no safe binding")]
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the descriptor was allocated by the system for the
            // caller to release with LocalFree, and is released once, here.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// The error of a create that passed a descriptor: a token that may not
/// name the owner is `PermissionDenied`.
fn create_error(error: &io::Error) -> DiskError {
    match error.raw_os_error() {
        Some(ERROR_INVALID_OWNER | ERROR_PRIVILEGE_NOT_HELD) => DiskError::PermissionDenied,
        _ => errors::disk(error),
    }
}

/// `path` as a NUL-terminated wide string in the form that has no length
/// limit, `\\?\` before a drive path and `\\?\UNC\` before a share.
fn wide(path: &Path) -> Result<Vec<u16>, DiskError> {
    let absolute = std::path::absolute(path).map_err(|e| errors::disk(&e))?;
    let text: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if text.contains(&0) {
        return Err(DiskError::InvalidPath);
    }
    let unit = |c: char| c as u16;
    let slash = unit('\\');
    let mut out: Vec<u16> = Vec::with_capacity(text.len() + 9);
    match text.as_slice() {
        [a, b, c, d, ..]
            if *a == slash && *b == slash && *d == slash && [unit('?'), unit('.')].contains(c) =>
        {
            out.extend_from_slice(&text);
        }
        [a, b, rest @ ..] if *a == slash && *b == slash => {
            out.extend(r"\\?\UNC\".encode_utf16());
            out.extend_from_slice(rest);
        }
        [_, colon, ..] if *colon == unit(':') => {
            out.extend(r"\\?\".encode_utf16());
            out.extend_from_slice(&text);
        }
        _ => out.extend_from_slice(&text),
    }
    out.push(0);
    Ok(out)
}

/// Creates the file `path`, which the caller has found missing, with
/// `std_options` (which say `create_new`) for `Default`, and otherwise with
/// the descriptor of `protection` passed to `CreateFileW`.
pub(super) fn create_file(
    std_options: &mut fs::OpenOptions,
    path: &Path,
    protection: Protection,
) -> Result<File, DiskError> {
    if protection == Protection::Default {
        return std_options.open(path).map_err(|e| errors::disk(&e));
    }
    create_file_owned(path, protection, ADMINISTRATORS, ADMINISTRATORS)
}

/// `create_file` with `owner` and `admins` in place of `Administrators`.
#[allow(
    unsafe_code,
    reason = "CreateFileW with a descriptor has no safe binding"
)]
pub(super) fn create_file_owned(
    path: &Path,
    protection: Protection,
    owner: &str,
    admins: &str,
) -> Result<File, DiskError> {
    let Some(sddl) = sddl(protection, false, owner, admins) else {
        return Err(invalid("Default passes no descriptor"));
    };
    let descriptor = Descriptor::from_sddl(&sddl).map_err(|e| errors::disk(&e))?;
    let name = wide(path)?;
    let attributes = descriptor.attributes();
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    // SAFETY: `name` is NUL-terminated and `attributes` points at a live
    // descriptor; both outlive the call, which reads them only. The template
    // handle is null, which CreateFileW allows.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            share,
            &raw const attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(create_error(&io::Error::last_os_error()));
    }
    // SAFETY: `handle` is a new open handle that nothing else owns, so the
    // `OwnedHandle` is its only owner and closes it once.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
}

/// Creates the directory `path` as `create_dir` does for `Default`, and
/// otherwise with the descriptor of `protection` passed to
/// `CreateDirectoryW`.
pub(super) fn create_dir(path: &Path, protection: Protection) -> Result<(), DiskError> {
    if protection == Protection::Default {
        return fs::create_dir(path).map_err(|e| errors::disk(&e));
    }
    create_dir_owned(path, protection, ADMINISTRATORS, ADMINISTRATORS)
}

/// `create_dir` with `owner` and `admins` in place of `Administrators`.
#[allow(
    unsafe_code,
    reason = "CreateDirectoryW with a descriptor has no safe binding"
)]
pub(super) fn create_dir_owned(
    path: &Path,
    protection: Protection,
    owner: &str,
    admins: &str,
) -> Result<(), DiskError> {
    let Some(sddl) = sddl(protection, true, owner, admins) else {
        return Err(invalid("Default passes no descriptor"));
    };
    let descriptor = Descriptor::from_sddl(&sddl).map_err(|e| errors::disk(&e))?;
    let name = wide(path)?;
    let attributes = descriptor.attributes();
    // SAFETY: `name` is NUL-terminated and `attributes` points at a live
    // descriptor; both outlive the call, which reads them only.
    let ok = unsafe { CreateDirectoryW(name.as_ptr(), &raw const attributes) };
    if ok == 0 {
        return Err(create_error(&io::Error::last_os_error()));
    }
    Ok(())
}

/// Moves the file `source` over `target` with one `MoveFileExW` and the
/// flags `MOVEFILE_REPLACE_EXISTING` and `MOVEFILE_WRITE_THROUGH`, which
/// returns only once the file is moved on the disk (ARFC 0005 clause
/// 3.1.16, <https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw>).
#[allow(unsafe_code, reason = "MoveFileExW has no safe binding")]
pub(super) fn replace(source: &Path, target: &Path) -> Result<(), DiskError> {
    let from = wide(source)?;
    let to = wide(target)?;
    // SAFETY: `from` and `to` are NUL-terminated and outlive the call, which
    // reads them only.
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(errors::disk(&io::Error::last_os_error()));
    }
    Ok(())
}

/// `Security::Windows` of the entry at `path`, opened without following a
/// reparse point at the last component.
#[allow(unsafe_code, reason = "GetSecurityInfo has no safe binding")]
pub(super) fn security(path: &Path) -> Result<Security, DiskError> {
    let file = fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| errors::disk(&e))?;
    let attributes = file
        .metadata()
        .map_err(|e| errors::disk(&e))?
        .file_attributes();
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut raw: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `file` owns an open handle for the whole call, and every out
    // pointer is either a valid place for the result or null for a part not
    // asked for.
    let rc = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut raw,
        )
    };
    if rc != 0 {
        // A Win32 error code is below 2^16, so it fits an `i32`.
        return Err(errors::disk(&io::Error::from_raw_os_error(rc as i32)));
    }
    let descriptor = Descriptor(raw);
    let mut control: u16 = 0;
    let mut revision: u32 = 0;
    // SAFETY: `descriptor` is the live descriptor GetSecurityInfo returned,
    // and both out pointers are valid places for the results.
    let ok =
        unsafe { GetSecurityDescriptorControl(descriptor.0, &raw mut control, &raw mut revision) };
    if ok == 0 {
        return Err(errors::disk(&io::Error::last_os_error()));
    }
    // SAFETY: `owner` and `dacl` point into `descriptor`, which is alive
    // until the end of this function.
    let (owner, dacl) = unsafe { (sid_bytes(owner)?, acl_bytes(dacl)) };
    let mut names = Names::default();
    let dacl = match dacl {
        None => None,
        Some(bytes) => Some(
            parse_acl(&bytes)?
                .into_iter()
                .map(|raw| names.ace(&raw))
                .collect::<Result<Vec<Ace>, DiskError>>()?,
        ),
    };
    let kind = if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    Ok(Security::Windows {
        kind,
        reparse_point: attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        owner: names.sid(&owner)?,
        dacl_protected: control & SE_DACL_PROTECTED != 0,
        dacl,
    })
}

/// The bytes of the security identifier at `sid`.
///
/// # Safety
///
/// `sid` is null or points at a security identifier that stays alive for
/// the call.
#[allow(unsafe_code, reason = "reads a SID the system returned")]
unsafe fn sid_bytes(sid: PSID) -> Result<Vec<u8>, DiskError> {
    // SAFETY: the caller guarantees `sid` is null or a live SID; IsValidSid
    // checks its structure before its length is read.
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(DiskError::Io);
    }
    // SAFETY: as above; the SID is valid, so its length is.
    let len = unsafe { GetLengthSid(sid) } as usize;
    // SAFETY: a valid SID spans `len` readable bytes from `sid`.
    Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), len) }.to_vec())
}

/// The bytes of the access control list at `acl`, `None` for a null list.
///
/// # Safety
///
/// `acl` is null or points at an access control list that stays alive for
/// the call.
#[allow(unsafe_code, reason = "reads an ACL the system returned")]
unsafe fn acl_bytes(acl: *mut ACL) -> Option<Vec<u8>> {
    if acl.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees `acl` points at a live ACL, whose header
    // is readable; the read does not assume alignment.
    let size = unsafe { ptr::addr_of!((*acl).AclSize).read_unaligned() } as usize;
    // SAFETY: an ACL spans `AclSize` readable bytes from its start.
    Some(unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), size) }.to_vec())
}

/// One entry of a list as stored: type, flags, mask, and trustee bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RawAce {
    kind: u8,
    flags: u8,
    mask: u32,
    sid: Vec<u8>,
}

fn le_u16(bytes: &[u8], at: usize) -> Option<usize> {
    let b = bytes.get(at..at + 2)?;
    Some(usize::from(u16::from_le_bytes([b[0], b[1]])))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The security identifier that starts at `at` in `bytes`, if it fits.
fn sid_at(bytes: &[u8], at: usize) -> Option<&[u8]> {
    let count = usize::from(*bytes.get(at.checked_add(1)?)?);
    bytes.get(at..at.checked_add(8 + 4 * count)?)
}

/// Decodes the entries of the access control list `acl`. A list of more than
/// 4,096 entries is `InvalidInput`; a list whose bytes do not hold the
/// entries its header counts is `Io`.
fn parse_acl(acl: &[u8]) -> Result<Vec<RawAce>, DiskError> {
    let count = le_u16(acl, 4).ok_or(DiskError::Io)?;
    if count > MAX_ACES {
        return Err(invalid(
            "the access control list has more than 4096 entries",
        ));
    }
    let mut aces = Vec::with_capacity(count);
    let mut at = 8;
    for _ in 0..count {
        let size = le_u16(acl, at + 2).ok_or(DiskError::Io)?;
        let ace = acl.get(at..at + size).ok_or(DiskError::Io)?;
        let mask = le_u32(ace, 4).ok_or(DiskError::Io)?;
        let trustee = if OBJECT_TYPES.contains(&ace[0]) {
            let flags = le_u32(ace, 8).ok_or(DiskError::Io)?;
            let guids = OBJECT_GUID_FLAGS
                .iter()
                .filter(|flag| flags & **flag != 0)
                .count();
            12 + 16 * guids
        } else {
            8
        };
        let sid = sid_at(ace, trustee).ok_or(DiskError::Io)?;
        aces.push(RawAce {
            kind: ace[0],
            flags: ace[1],
            mask,
            sid: sid.to_vec(),
        });
        at += size;
    }
    Ok(aces)
}

/// The string form of a security identifier, such as `S-1-5-18`, as
/// `ConvertSidToStringSidW` writes it: an authority of 2^32 or more in
/// hexadecimal.
fn sid_string(sid: &[u8]) -> Option<String> {
    let (&revision, rest) = sid.split_first()?;
    let (&count, rest) = rest.split_first()?;
    let authority = rest.get(..6)?;
    let subs = rest.get(6..)?;
    if subs.len() != 4 * usize::from(count) {
        return None;
    }
    let authority = authority
        .iter()
        .fold(0u64, |value, byte| value << 8 | u64::from(*byte));
    let mut text = if authority >> 32 == 0 {
        format!("S-{revision}-{authority}")
    } else {
        format!("S-{revision}-0x{authority:012X}")
    };
    for sub in subs.as_chunks::<4>().0 {
        let _ = write!(text, "-{}", u32::from_le_bytes(*sub));
    }
    Some(text)
}

/// Account names already looked up during one `security` call.
#[derive(Default)]
struct Names(BTreeMap<Vec<u8>, String>);

impl Names {
    fn sid(&mut self, sid: &[u8]) -> Result<Sid, DiskError> {
        let text = sid_string(sid).ok_or(DiskError::Io)?;
        let name = self
            .0
            .entry(sid.to_vec())
            .or_insert_with(|| account_name(sid))
            .clone();
        Ok(Sid { sid: text, name })
    }

    fn ace(&mut self, raw: &RawAce) -> Result<Ace, DiskError> {
        Ok(Ace {
            allow: !DENIED_TYPES.contains(&raw.kind),
            trustee: self.sid(&raw.sid)?,
            mask: raw.mask,
            inherit_only: raw.flags & INHERIT_ONLY != 0,
            inherited: raw.flags & INHERITED != 0,
            object_inherit: raw.flags & OBJECT_INHERIT != 0,
            container_inherit: raw.flags & CONTAINER_INHERIT != 0,
        })
    }
}

/// `DOMAIN\name` of the security identifier `sid` as `LookupAccountSidW`
/// gives it, or empty when it gives none.
#[allow(unsafe_code, reason = "LookupAccountSidW has no safe binding")]
fn account_name(sid: &[u8]) -> String {
    // A copy aligned for the identifier's 32-bit sub-authorities.
    let mut words = vec![0u32; sid.len().div_ceil(4)];
    for (word, chunk) in words.iter_mut().zip(sid.chunks(4)) {
        let mut bytes = [0u8; 4];
        bytes[..chunk.len()].copy_from_slice(chunk);
        *word = u32::from_ne_bytes(bytes);
    }
    let psid: PSID = words.as_mut_ptr().cast::<c_void>();
    let (mut name_len, mut domain_len): (u32, u32) = (0, 0);
    let mut use_kind: SID_NAME_USE = 0;
    // SAFETY: `psid` points at a whole security identifier in `words`. With
    // null buffers the call only writes the sizes it needs to the two
    // counters, which are valid places.
    unsafe {
        LookupAccountSidW(
            ptr::null(),
            psid,
            ptr::null_mut(),
            &raw mut name_len,
            ptr::null_mut(),
            &raw mut domain_len,
            &raw mut use_kind,
        )
    };
    if name_len == 0 {
        return String::new();
    }
    let mut name = vec![0u16; name_len as usize];
    let mut domain = vec![0u16; domain_len.max(1) as usize];
    domain_len = domain_len.max(1);
    // SAFETY: each buffer holds as many units as its counter says, and the
    // call writes at most that many; the rest is as above.
    let ok = unsafe {
        LookupAccountSidW(
            ptr::null(),
            psid,
            name.as_mut_ptr(),
            &raw mut name_len,
            domain.as_mut_ptr(),
            &raw mut domain_len,
            &raw mut use_kind,
        )
    };
    if ok == 0 {
        return String::new();
    }
    name.truncate(name_len as usize);
    domain.truncate(domain_len as usize);
    let name = String::from_utf16_lossy(&name);
    let domain = String::from_utf16_lossy(&domain);
    if domain.is_empty() {
        name
    } else {
        format!("{domain}\\{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ACE of the basic layout: type, flags, size, mask, trustee.
    fn ace(kind: u8, flags: u8, mask: u32, sid: &[u8]) -> Vec<u8> {
        let size = (8 + sid.len()) as u16;
        let mut bytes = vec![kind, flags];
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&mask.to_le_bytes());
        bytes.extend_from_slice(sid);
        bytes
    }

    fn acl(aces: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = aces.concat();
        let size = (8 + body.len()) as u16;
        let mut bytes = vec![2, 0];
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&(aces.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&body);
        bytes
    }

    /// `S-1-5-18`.
    const SYSTEM: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
    /// `S-1-5-32-544`.
    const ADMINS: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 2, 0, 0];

    #[test]
    fn sids_print_as_windows_writes_them() {
        assert_eq!(sid_string(&SYSTEM).as_deref(), Some("S-1-5-18"));
        assert_eq!(sid_string(&ADMINS).as_deref(), Some("S-1-5-32-544"));
        let big = [1, 0, 1, 0, 0, 0, 0, 0];
        assert_eq!(sid_string(&big).as_deref(), Some("S-1-0x010000000000"));
        assert_eq!(sid_string(&SYSTEM[..11]), None);
        assert_eq!(sid_string(&[1]), None);
    }

    #[test]
    fn lists_decode_types_flags_and_layouts() {
        let mut object = vec![5, 0];
        let size: u16 = 4 + 4 + 4 + 16 + SYSTEM.len() as u16;
        object.extend_from_slice(&size.to_le_bytes());
        object.extend_from_slice(&0x10u32.to_le_bytes());
        object.extend_from_slice(&1u32.to_le_bytes());
        object.extend_from_slice(&[0xAB; 16]);
        object.extend_from_slice(&SYSTEM);
        let list = acl(&[
            ace(0, 0x13, 0x001F_01FF, &SYSTEM),
            ace(1, 0x08, 0x1, &ADMINS),
            object,
        ]);
        let aces = parse_acl(&list).expect("a valid list");
        assert_eq!(
            aces,
            [
                RawAce {
                    kind: 0,
                    flags: 0x13,
                    mask: 0x001F_01FF,
                    sid: SYSTEM.to_vec()
                },
                RawAce {
                    kind: 1,
                    flags: 0x08,
                    mask: 1,
                    sid: ADMINS.to_vec()
                },
                RawAce {
                    kind: 5,
                    flags: 0,
                    mask: 0x10,
                    sid: SYSTEM.to_vec()
                },
            ]
        );
        let mut names = Names::default();
        let decoded: Vec<Ace> = aces.iter().map(|a| names.ace(a).unwrap()).collect();
        assert!(decoded[0].allow && decoded[0].inherited && decoded[0].object_inherit);
        assert!(decoded[0].container_inherit && !decoded[0].inherit_only);
        assert!(!decoded[1].allow && decoded[1].inherit_only && !decoded[1].inherited);
        assert!(decoded[2].allow);
        assert_eq!(decoded[1].trustee.sid, "S-1-5-32-544");
        assert_eq!(decoded[1].trustee.name, r"BUILTIN\Administrators");
    }

    #[test]
    fn broken_and_oversized_lists_are_refused() {
        let good = acl(&[ace(0, 0, 1, &SYSTEM)]);
        assert_eq!(parse_acl(&good[..good.len() - 1]), Err(DiskError::Io));
        assert_eq!(parse_acl(&good[..6]), Err(DiskError::Io));
        let mut short = good.clone();
        short[10] = 9;
        assert_eq!(parse_acl(&short), Err(DiskError::Io));
        let mut many = acl(&[]);
        many[4..6].copy_from_slice(&4_097u16.to_le_bytes());
        assert!(matches!(parse_acl(&many), Err(DiskError::InvalidInput(_))));
        let exactly = acl(&vec![ace(0, 0, 1, &SYSTEM); 4_096]);
        assert_eq!(parse_acl(&exactly).map(|a| a.len()), Ok(4_096));
    }

    #[test]
    fn descriptors_are_the_ones_the_clause_names() {
        let root_dir = sddl(Protection::PrivateRoot, true, "BA", "BA");
        assert_eq!(
            root_dir.as_deref(),
            Some("O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
        );
        let root_file = sddl(Protection::PrivateRoot, false, "BA", "BA");
        assert_eq!(
            root_file.as_deref(),
            Some("O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)")
        );
        assert_eq!(
            sddl(Protection::Private, true, "BA", "BA").as_deref(),
            Some("O:BA")
        );
        assert_eq!(sddl(Protection::Default, false, "BA", "BA"), None);
        for text in [root_dir, root_file] {
            assert!(Descriptor::from_sddl(&text.unwrap()).is_ok());
        }
    }
}
