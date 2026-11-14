//! The data directory (ARFC 0002 sections 3.3 and 3.12) and the local join
//! state in its host file (ARFC 0017 section 4.2).

use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use alloy_id::datadir::{self, Ace, DataDirFacts, Principal, UnixEntry, UnixKind, WindowsEntry};
use alloy_rt::{
    Access, Create, DiskError, DiskIo, EntryKind, IoBuf, OpenOptions, ReadOp, RtPath, Security, Sid,
};

/// The default data directory on Linux (ARFC 0002 section 3.3).
pub const LINUX_DATA_DIR: &str = "/var/lib/alloyd";

/// The name of the default data directory on Windows, in the folder that
/// Windows reports as `%ProgramData%` (ARFC 0002 section 3.3).
pub const WINDOWS_DATA_DIR_NAME: &str = "Alloy";

/// The name of the host file in the data directory (ARFC 0017 section 4.2).
pub const HOST_FILE: &str = "alloy-host";

/// The most bytes of the host file read to find its first two lines.
const HEAD_LEN: usize = 4_096;

/// A node's local join state (ARFC 0002 sections 2 and 3.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinState {
    /// No `init` or `join` has made the machine a node.
    Unjoined,
    /// A `join` is in progress.
    Joining,
    /// The machine is a node of a cluster.
    Joined,
}

impl fmt::Display for JoinState {
    /// Writes the state as the host file's `join` line spells it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            JoinState::Unjoined => "unjoined",
            JoinState::Joining => "joining",
            JoinState::Joined => "joined",
        })
    }
}

/// Why the data directory cannot be found or used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataDirError {
    /// On Windows, `%ProgramData%` is unset, empty, or not absolute.
    #[error(
        "the folder %ProgramData% is not set to an absolute path, so the data directory cannot be found"
    )]
    NoProgramData,
    /// The path is not absolute.
    #[error("the data directory {path} is not an absolute path")]
    NotAbsolute {
        /// The path, as the platform displays it.
        path: String,
    },
    /// A component of the path is `..` or not UTF-8.
    #[error("the data directory {path} has a component that is `..` or not UTF-8")]
    Unsupported {
        /// The path, as the platform displays it.
        path: String,
    },
}

/// Why the host file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostFileError {
    /// The file exists but reading it failed.
    #[error("reading it failed: {0}")]
    Disk(DiskError),
    /// The first line is not `alloy-host 1`.
    #[error("its first line is not `alloy-host 1`")]
    Version,
    /// The second line is not `join` followed by a local join state.
    #[error("its second line is not `join unjoined`, `join joining`, or `join joined`")]
    Join,
    /// The file ends, or its first 4,096 bytes end, before its second line
    /// ends.
    #[error("it ends before its second line does, within its first 4096 bytes")]
    Short,
}

/// The default data directory: `/var/lib/alloyd`, and on Windows `Alloy` in
/// `program_data`, the value of `%ProgramData%` (ARFC 0002 section 3.3).
/// `program_data` is ignored on other platforms.
///
/// # Errors
///
/// [`DataDirError::NoProgramData`] on Windows when `program_data` is missing,
/// empty, or not an absolute path.
pub fn default_data_dir(program_data: Option<&OsStr>) -> Result<PathBuf, DataDirError> {
    if cfg!(windows) {
        windows_data_dir(program_data)
    } else {
        Ok(PathBuf::from(LINUX_DATA_DIR))
    }
}

fn windows_data_dir(program_data: Option<&OsStr>) -> Result<PathBuf, DataDirError> {
    program_data
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(WINDOWS_DATA_DIR_NAME))
        .ok_or(DataDirError::NoProgramData)
}

/// Parses the start of a host file: its first two lines, `alloy-host 1` and
/// `join <state>`, each ending with a line feed (ARFC 0017 sections 4.1 and
/// 4.2). Total over any input.
///
/// # Errors
///
/// [`HostFileError::Short`] when `head` holds fewer than two line feeds,
/// [`HostFileError::Version`] and [`HostFileError::Join`] when a line is not
/// the one expected.
pub fn parse_host_file(head: &[u8]) -> Result<JoinState, HostFileError> {
    let mut lines = head.split(|&byte| byte == b'\n');
    let (Some(first), Some(second), Some(_)) = (lines.next(), lines.next(), lines.next()) else {
        return Err(HostFileError::Short);
    };
    if first != b"alloy-host 1" {
        return Err(HostFileError::Version);
    }
    match second {
        b"join unjoined" => Ok(JoinState::Unjoined),
        b"join joining" => Ok(JoinState::Joining),
        b"join joined" => Ok(JoinState::Joined),
        _ => Err(HostFileError::Join),
    }
}

/// Where the data directory is, as the `os` `disk-io` seam names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Location {
    /// The root of the data directory's file system: `/`, or a Windows
    /// prefix and root such as `C:\`.
    native_root: String,
    /// Every directory above the data directory, from the root down to its
    /// parent, each with its native path for messages.
    ancestors: Vec<(RtPath, PathBuf)>,
    /// The data directory, below `native_root`.
    dir: RtPath,
    /// The data directory's native path, for messages.
    dir_native: PathBuf,
    /// The host file, below `native_root`.
    host_file: RtPath,
    /// The host file's native path, for messages.
    host_file_native: PathBuf,
}

impl Location {
    /// The location of the data directory `data_dir`.
    ///
    /// # Errors
    ///
    /// [`DataDirError::NotAbsolute`] and [`DataDirError::Unsupported`].
    pub(crate) fn of(data_dir: &Path) -> Result<Location, DataDirError> {
        let shown = || data_dir.display().to_string();
        if !data_dir.is_absolute() {
            return Err(DataDirError::NotAbsolute { path: shown() });
        }
        let unsupported = || DataDirError::Unsupported { path: shown() };
        let mut root = PathBuf::new();
        let mut names = Vec::new();
        for component in data_dir.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => root.push(component.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => return Err(unsupported()),
                Component::Normal(name) => names.push(name.to_str().ok_or_else(unsupported)?),
            }
        }
        let mut ancestors = Vec::with_capacity(names.len());
        let mut path = RtPath::root();
        let mut native = root.clone();
        for name in &names {
            ancestors.push((path.clone(), native.clone()));
            path = path.join(name).map_err(|_| unsupported())?;
            native.push(name);
        }
        let native_root = root
            .into_os_string()
            .into_string()
            .map_err(|_| unsupported())?;
        let host_file = path.join(HOST_FILE).map_err(|_| unsupported())?;
        Ok(Location {
            native_root,
            ancestors,
            dir: path,
            host_file,
            host_file_native: native.join(HOST_FILE),
            dir_native: native,
        })
    }

    /// The root for the `os` `disk-io` configuration.
    pub(crate) fn native_root(&self) -> &str {
        &self.native_root
    }

    /// The host file's native path, for messages.
    pub(crate) fn host_file_display(&self) -> std::path::Display<'_> {
        self.host_file_native.display()
    }

    /// The data directory, as the `disk-io` seam names it.
    pub(crate) fn dir(&self) -> &RtPath {
        &self.dir
    }

    /// The host file, as the `disk-io` seam names it.
    #[cfg(test)]
    pub(crate) fn host_file(&self) -> &RtPath {
        &self.host_file
    }
}

/// What the check of the data directory at start found (ARFC 0002 section
/// 3.12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartCheck {
    /// It is an Alloy data directory, so its host file's join line is the
    /// local join state.
    Alloy,
    /// Nothing is at the path: the local join state is `unjoined`.
    Missing,
    /// Something is at the path and it is not an Alloy data directory: the
    /// reason, naming the entry at fault. `alloyd` reads no secret.
    NotAlloy(String),
}

/// Checks that the data directory at `location` is an Alloy data directory
/// (ARFC 0002 section 3.12), before anything in it is read.
///
/// It reads the owner, mode or access control list, and link or reparse
/// point facts of the data directory, each directory above it up to the
/// root, and its host file, with one `security` call per entry, and then the
/// first line of the host file. A `security` call that fails for an entry
/// fails the rules for that entry, with its owner `unknown`.
pub(crate) async fn check_at_start(disk: &dyn DiskIo, location: &Location) -> StartCheck {
    let dir = match disk.security(&location.dir).await {
        Ok(security) => security,
        Err(DiskError::NotFound) => return StartCheck::Missing,
        Err(e) => return unreadable(&location.dir_native, &e),
    };
    let mut ancestors = Vec::with_capacity(location.ancestors.len());
    for (path, native) in &location.ancestors {
        match disk.security(path).await {
            Ok(security) => ancestors.push((security, native.as_path())),
            Err(e) => return unreadable(native, &e),
        }
    }
    let host = match disk.security(&location.host_file).await {
        Ok(security) => Some(security),
        Err(DiskError::NotFound) => None,
        Err(e) => return unreadable(&location.host_file_native, &e),
    };
    let first_line_ok = match &host {
        Some(_) => host_file_head(disk, location)
            .await
            .is_ok_and(|head| head.starts_with(b"alloy-host 1\n")),
        None => false,
    };
    let host = host.map(|h| (h, location.host_file_native.as_path()));
    let facts = match facts(ancestors, (dir, &location.dir_native), host, first_line_ok) {
        Some(facts) => facts,
        None => {
            return StartCheck::NotAlloy(format!(
                "{} cannot be checked: its file system reports the security facts of two platforms",
                location.dir_native.display()
            ));
        }
    };
    match datadir::check_at_start(&facts) {
        Ok(()) => StartCheck::Alloy,
        Err(problem) => StartCheck::NotAlloy(problem.to_string()),
    }
}

fn unreadable(path: &Path, error: &DiskError) -> StartCheck {
    StartCheck::NotAlloy(format!(
        "{}, owned by unknown, cannot be checked: reading its security facts failed: {error}",
        path.display()
    ))
}

/// The facts of [`datadir::check_at_start`], from what `security` returned,
/// or `None` when the entries mix the facts of two platforms.
fn facts(
    ancestors: Vec<(Security, &Path)>,
    dir: (Security, &Path),
    host: Option<(Security, &Path)>,
    host_file_first_line_ok: bool,
) -> Option<DataDirFacts> {
    match dir.0 {
        Security::Unix { .. } => {
            let unix = |(security, path): (Security, &Path)| unix_entry(security, path);
            let host_file = match host {
                Some(host) => Some(unix(host)?),
                None => None,
            };
            Some(DataDirFacts::Linux {
                ancestors: ancestors.into_iter().map(unix).collect::<Option<_>>()?,
                data_dir: Some(unix(dir)?),
                // `check_at_start` does not read it: only `init` and `join`
                // adopt an empty directory.
                data_dir_empty: false,
                host_file,
                host_file_first_line_ok,
            })
        }
        Security::Windows { .. } => {
            let windows = |(security, path): (Security, &Path)| windows_entry(security, path);
            let host_file = match host {
                Some(host) => Some(windows(host)?),
                None => None,
            };
            Some(DataDirFacts::Windows {
                ancestors: ancestors.into_iter().map(windows).collect::<Option<_>>()?,
                data_dir: Some(windows(dir)?),
                host_file,
                host_file_first_line_ok,
            })
        }
    }
}

fn unix_entry(security: Security, path: &Path) -> Option<UnixEntry> {
    let Security::Unix {
        kind,
        symlink,
        uid,
        mode,
    } = security
    else {
        return None;
    };
    let kind = match kind {
        _ if symlink => UnixKind::Symlink,
        EntryKind::Dir => UnixKind::Dir,
        EntryKind::File => UnixKind::File,
        EntryKind::Block | EntryKind::Other => UnixKind::Other,
    };
    Some(UnixEntry {
        path: path.display().to_string(),
        kind,
        uid,
        mode,
    })
}

fn windows_entry(security: Security, path: &Path) -> Option<WindowsEntry> {
    let Security::Windows {
        kind,
        reparse_point,
        owner,
        dacl_protected,
        dacl,
    } = security
    else {
        return None;
    };
    let principal = |sid: Sid| Principal {
        sid: sid.sid,
        name: sid.name,
    };
    Some(WindowsEntry {
        path: path.display().to_string(),
        is_dir: kind == EntryKind::Dir,
        reparse_point,
        owner: principal(owner),
        dacl_protected,
        dacl: dacl.map(|aces| {
            aces.into_iter()
                .map(|ace| Ace {
                    allow: ace.allow,
                    principal: principal(ace.trustee),
                    mask: ace.mask,
                    inherit_only: ace.inherit_only,
                    inherited: ace.inherited,
                })
                .collect()
        }),
    })
}

/// The local join state that the host file at `location` holds: `Unjoined`
/// when there is no host file. Reads at most the first 4,096 bytes of the
/// file. Callers call it only for an Alloy data directory
/// ([`check_at_start`]).
///
/// # Errors
///
/// As [`parse_host_file`], and [`HostFileError::Disk`] when the file exists
/// but cannot be opened or read.
pub(crate) async fn read_join_state(
    disk: &dyn DiskIo,
    location: &Location,
) -> Result<JoinState, HostFileError> {
    match host_file_head(disk, location).await {
        Ok(head) => parse_host_file(&head),
        Err(HostFileError::Disk(DiskError::NotFound)) => Ok(JoinState::Unjoined),
        Err(e) => Err(e),
    }
}

/// The first 4,096 bytes of the host file, or all of it when it is shorter.
async fn host_file_head(disk: &dyn DiskIo, location: &Location) -> Result<Vec<u8>, HostFileError> {
    let options = OpenOptions {
        access: Access::Read,
        create: Create::Never,
        direct: false,
        exclusive: false,
    };
    let file = disk
        .open(&location.host_file, options)
        .await
        .map_err(HostFileError::Disk)?;
    let buf = IoBuf::zeroed(HEAD_LEN).map_err(HostFileError::Disk)?;
    let mut done = file.read_at(vec![ReadOp { offset: 0, buf }]).await;
    let read = done.pop().ok_or(HostFileError::Disk(DiskError::Io))?;
    let len = read.result.map_err(HostFileError::Disk)?;
    read.buf
        .get(..len)
        .map(<[u8]>::to_vec)
        .ok_or(HostFileError::Disk(DiskError::Io))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_file_reads_the_join_line() {
        let cases: [(&[u8], Result<JoinState, HostFileError>); 11] = [
            (b"alloy-host 1\njoin unjoined\n", Ok(JoinState::Unjoined)),
            (
                b"alloy-host 1\njoin joining\nrelease none\n",
                Ok(JoinState::Joining),
            ),
            (
                b"alloy-host 1\njoin joined\nrelease 0.0.1\nlink x\n",
                Ok(JoinState::Joined),
            ),
            (b"", Err(HostFileError::Short)),
            (b"alloy-host 1\n", Err(HostFileError::Short)),
            (b"alloy-host 1\njoin joined", Err(HostFileError::Short)),
            (b"alloy-host 2\njoin joined\n", Err(HostFileError::Version)),
            (b"alloy-host 1 \njoin joined\n", Err(HostFileError::Version)),
            (b"alloy-host 1\njoin  joined\n", Err(HostFileError::Join)),
            (b"alloy-host 1\njoin joined\r\n", Err(HostFileError::Join)),
            (b"alloy-host 1\nrelease none\n", Err(HostFileError::Join)),
        ];
        for (head, expected) in cases {
            assert_eq!(
                parse_host_file(head),
                expected,
                "{}",
                String::from_utf8_lossy(head)
            );
        }
    }

    #[test]
    fn parse_host_file_is_total() {
        let mut bytes = Vec::new();
        for byte in [b'\n', b'a', b' ', 0, 0xFF] {
            bytes.push(byte);
            let _ = parse_host_file(&bytes);
        }
    }

    #[test]
    fn default_data_dir_follows_the_platform() {
        if cfg!(windows) {
            assert_eq!(
                default_data_dir(Some(OsStr::new(r"C:\ProgramData"))),
                Ok(PathBuf::from(r"C:\ProgramData\Alloy"))
            );
            for unusable in [None, Some(""), Some("ProgramData"), Some(r"\ProgramData")] {
                assert_eq!(
                    default_data_dir(unusable.map(OsStr::new)),
                    Err(DataDirError::NoProgramData),
                    "{unusable:?}"
                );
            }
        } else {
            assert_eq!(
                default_data_dir(Some(OsStr::new("ignored"))),
                Ok(PathBuf::from("/var/lib/alloyd"))
            );
            assert_eq!(default_data_dir(None), Ok(PathBuf::from("/var/lib/alloyd")));
        }
    }

    #[test]
    fn location_splits_the_root_from_the_path() {
        let data_dir = if cfg!(windows) {
            PathBuf::from(r"C:\ProgramData\Alloy")
        } else {
            PathBuf::from("/var/lib/alloyd")
        };
        let location = Location::of(&data_dir).unwrap();
        if cfg!(windows) {
            assert_eq!(location.native_root(), r"C:\");
            assert_eq!(location.host_file.as_str(), "/ProgramData/Alloy/alloy-host");
        } else {
            assert_eq!(location.native_root(), "/");
            assert_eq!(location.host_file.as_str(), "/var/lib/alloyd/alloy-host");
        }
        assert_eq!(
            location.host_file_display().to_string(),
            data_dir.join("alloy-host").display().to_string()
        );
    }

    #[test]
    fn location_refuses_relative_and_parent_paths() {
        assert!(matches!(
            Location::of(Path::new("alloyd")),
            Err(DataDirError::NotAbsolute { .. })
        ));
        let parent = if cfg!(windows) {
            r"C:\ProgramData\..\Alloy"
        } else {
            "/var/lib/../alloyd"
        };
        assert!(matches!(
            Location::of(Path::new(parent)),
            Err(DataDirError::Unsupported { .. })
        ));
    }
}
