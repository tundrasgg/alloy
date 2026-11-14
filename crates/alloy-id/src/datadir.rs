//! The rules of an Alloy data directory (ARFC 0002 section 3.12).
//!
//! The checks are pure functions over [`DataDirFacts`]: the owner, mode or
//! access control list, and kind of the data directory, every directory
//! above it, and its host file, as the operating system reports them. They
//! decide what `init` and `join` may do with the path ([`check_for_init`])
//! and whether `alloyd` may read a secret ([`check_at_start`]). Gathering the
//! facts is the `disk-io` seam's `security` call. Creating or adopting the
//! directory with its mode or access control list is
//! [`DataDir::create`](crate::DataDir::create) or
//! [`DataDir::adopt`](crate::DataDir::adopt).

/// The first line of every host file.
pub const HOST_FILE_FIRST_LINE: &str = "alloy-host 1";

/// What a path is, as `lstat` sees it: a symbolic link is not followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnixKind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link.
    Symlink,
    /// Anything else.
    Other,
}

/// What Linux reports for one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnixEntry {
    /// The path, for messages.
    pub path: String,
    /// Its kind.
    pub kind: UnixKind,
    /// Its owner's user id.
    pub uid: u32,
    /// Its permission bits, `st_mode & 0o7777`.
    pub mode: u32,
}

/// A Windows security principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    /// Its security identifier in string form, such as `S-1-5-18`.
    pub sid: String,
    /// Its account name, for messages, such as `BUILTIN\Users`.
    pub name: String,
}

/// `SYSTEM`.
pub const SID_SYSTEM: &str = "S-1-5-18";
/// The `Administrators` group.
pub const SID_ADMINISTRATORS: &str = "S-1-5-32-544";
/// `TrustedInstaller`.
pub const SID_TRUSTED_INSTALLER: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

/// `DELETE`.
pub const DELETE: u32 = 0x0001_0000;
/// `WRITE_DAC`.
pub const WRITE_DAC: u32 = 0x0004_0000;
/// `WRITE_OWNER`.
pub const WRITE_OWNER: u32 = 0x0008_0000;
/// `FILE_DELETE_CHILD`.
pub const FILE_DELETE_CHILD: u32 = 0x0000_0040;
/// `FILE_ALL_ACCESS`, full control.
pub const FILE_ALL_ACCESS: u32 = 0x001f_01ff;
/// `GENERIC_ALL`.
pub const GENERIC_ALL: u32 = 0x1000_0000;
/// `MAXIMUM_ALLOWED`, which grants whatever the rest allows, so it is read
/// as granting everything.
pub const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

/// The rights that let a principal delete, rename, or replace a directory,
/// or take it over, including the generic rights that hold them.
const TAKEOVER: u32 =
    DELETE | WRITE_DAC | WRITE_OWNER | FILE_DELETE_CHILD | GENERIC_ALL | MAXIMUM_ALLOWED;

impl Principal {
    fn is(&self, sid: &str) -> bool {
        self.sid == sid
    }

    fn is_admin(&self) -> bool {
        self.is(SID_SYSTEM) || self.is(SID_ADMINISTRATORS)
    }

    fn is_trusted(&self) -> bool {
        self.is_admin() || self.is(SID_TRUSTED_INSTALLER)
    }
}

/// One access control entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ace {
    /// Allow or deny.
    pub allow: bool,
    /// Whom it names.
    pub principal: Principal,
    /// Its access mask.
    pub mask: u32,
    /// `INHERIT_ONLY_ACE`: it applies only to what the object holds, not to
    /// the object itself.
    pub inherit_only: bool,
    /// `INHERITED_ACE`: it came from the parent.
    pub inherited: bool,
}

impl Ace {
    /// Whether it grants `principal` one of `rights` on the object itself.
    fn grants(&self, rights: u32) -> bool {
        self.allow && !self.inherit_only && self.mask & rights != 0
    }
}

/// What Windows reports for one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsEntry {
    /// The path, for messages.
    pub path: String,
    /// Whether it is a directory.
    pub is_dir: bool,
    /// Whether it is a reparse point: a symbolic link, junction, or mount.
    pub reparse_point: bool,
    /// Its owner.
    pub owner: Principal,
    /// `SE_DACL_PROTECTED`: its list does not inherit from its parent.
    pub dacl_protected: bool,
    /// Its discretionary access control list. `None` is a null list, which
    /// grants everyone everything.
    pub dacl: Option<Vec<Ace>>,
}

/// The facts the checks need, for one platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataDirFacts {
    /// Linux.
    Linux {
        /// Every directory above the data directory, from `/` down to its
        /// parent.
        ancestors: Vec<UnixEntry>,
        /// The data directory, `None` when nothing is at the path.
        data_dir: Option<UnixEntry>,
        /// Whether the data directory has no entries.
        data_dir_empty: bool,
        /// The host file, `None` when missing.
        host_file: Option<UnixEntry>,
        /// Whether the host file's first line is `alloy-host 1`.
        host_file_first_line_ok: bool,
    },
    /// Windows.
    Windows {
        /// Every directory above the data directory, from the volume root
        /// down to `%ProgramData%`.
        ancestors: Vec<WindowsEntry>,
        /// The data directory, `None` when nothing is at the path.
        data_dir: Option<WindowsEntry>,
        /// The host file, `None` when missing.
        host_file: Option<WindowsEntry>,
        /// Whether the host file's first line is `alloy-host 1`.
        host_file_first_line_ok: bool,
    },
}

/// What `init` and `join` may do with the data directory path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitAction {
    /// It is an Alloy data directory: use it.
    Use,
    /// Nothing is there: create it with its owner and mode or access control
    /// list set at creation, then write the host file first.
    Create,
    /// Linux only: an empty, safe directory such as a mount point. Set its
    /// mode to `0700` first, then write the host file.
    Adopt,
}

/// Why a path is not an Alloy data directory, or may not become one. Each
/// fails the `data directory` check of section 3.8 and names the path and,
/// where one is at fault, the principal.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DataDirProblem {
    /// A symbolic link or reparse point where a directory must be.
    #[error("{path} is a symbolic link or reparse point")]
    Link {
        /// The path.
        path: String,
    },
    /// Not a directory, or for the host file not a regular file.
    #[error("{path} is not a {expected}")]
    Kind {
        /// The path.
        path: String,
        /// `directory` or `regular file`.
        expected: &'static str,
    },
    /// Its owner is not one the rule allows.
    #[error("{path} is owned by {owner}, which may not own it")]
    Owner {
        /// The path.
        path: String,
        /// The owner: a user id on Linux, an account name on Windows.
        owner: String,
    },
    /// Its Linux mode is not the one the rule requires.
    #[error("{path} has mode {mode:04o}, and must have {required}")]
    Mode {
        /// The path.
        path: String,
        /// Its mode.
        mode: u32,
        /// What the rule requires.
        required: &'static str,
    },
    /// Its Windows access control list grants a principal it may not.
    #[error("{path} grants {principal} access it may not have")]
    Grants {
        /// The path.
        path: String,
        /// The principal.
        principal: String,
    },
    /// Its Windows access control list is not as the rule requires:
    /// inherited where it must be protected, or the reverse, or without
    /// full control for `SYSTEM` or `Administrators`.
    #[error("{path} does not have the access control list an Alloy data directory needs")]
    Acl {
        /// The path.
        path: String,
    },
    /// The directory exists and is not an Alloy data directory: it has no
    /// host file, or on Linux it is not empty, or on Windows it was not
    /// created by `init` or `join`.
    #[error("{path}, owned by {owner}, is not an Alloy data directory")]
    Foreign {
        /// The path.
        path: String,
        /// Its owner.
        owner: String,
    },
    /// The host file is missing or its first line is not `alloy-host 1`.
    #[error("{path} is not a host file of version 1")]
    HostFile {
        /// The path.
        path: String,
    },
    /// Nothing is at the path, so there is no Alloy data directory.
    #[error("{path} does not exist")]
    Missing {
        /// The path.
        path: String,
    },
}

/// What `init` and `join` may do with the data directory (section 3.12).
///
/// # Errors
///
/// The first [`DataDirProblem`] found, checking the directories above the
/// data directory from the top first, then the data directory, then its host
/// file. Nothing may be written in the path then.
pub fn check_for_init(facts: &DataDirFacts) -> Result<InitAction, DataDirProblem> {
    match facts {
        DataDirFacts::Linux {
            ancestors,
            data_dir,
            data_dir_empty,
            host_file,
            host_file_first_line_ok,
        } => {
            linux_ancestors(ancestors)?;
            let Some(dir) = data_dir else {
                return Ok(InitAction::Create);
            };
            if host_file.is_none() {
                linux_safe_dir(dir, "no write permission for group or others", |mode| {
                    mode & 0o022 == 0
                })?;
                if !*data_dir_empty {
                    return Err(DataDirProblem::Foreign {
                        path: dir.path.clone(),
                        owner: uid_name(dir.uid),
                    });
                }
                return Ok(InitAction::Adopt);
            }
            linux_data_dir(dir, host_file.as_ref(), *host_file_first_line_ok)?;
            Ok(InitAction::Use)
        }
        DataDirFacts::Windows {
            ancestors,
            data_dir,
            host_file,
            host_file_first_line_ok,
        } => {
            windows_ancestors(ancestors)?;
            let Some(dir) = data_dir else {
                return Ok(InitAction::Create);
            };
            if dir.reparse_point {
                return Err(DataDirProblem::Link {
                    path: dir.path.clone(),
                });
            }
            if host_file.is_none() {
                return Err(DataDirProblem::Foreign {
                    path: dir.path.clone(),
                    owner: dir.owner.name.clone(),
                });
            }
            windows_data_dir(dir, host_file.as_ref(), *host_file_first_line_ok)?;
            Ok(InitAction::Use)
        }
    }
}

/// Whether `alloyd` may read a secret: the path must be an Alloy data
/// directory (section 3.12). On failure `alloyd` reads no secret, serves
/// nothing, starts no process, writes nothing, and logs the problem every 60
/// seconds.
///
/// # Errors
///
/// The first [`DataDirProblem`] found, in the order of [`check_for_init`],
/// and [`DataDirProblem::Missing`] when nothing is at the path.
pub fn check_at_start(facts: &DataDirFacts) -> Result<(), DataDirProblem> {
    match facts {
        DataDirFacts::Linux {
            ancestors,
            data_dir,
            host_file,
            host_file_first_line_ok,
            ..
        } => {
            linux_ancestors(ancestors)?;
            let dir = data_dir
                .as_ref()
                .ok_or_else(|| missing(ancestors.last().map(|a| &a.path)))?;
            linux_data_dir(dir, host_file.as_ref(), *host_file_first_line_ok)
        }
        DataDirFacts::Windows {
            ancestors,
            data_dir,
            host_file,
            host_file_first_line_ok,
        } => {
            windows_ancestors(ancestors)?;
            let dir = data_dir
                .as_ref()
                .ok_or_else(|| missing(ancestors.last().map(|a| &a.path)))?;
            windows_data_dir(dir, host_file.as_ref(), *host_file_first_line_ok)
        }
    }
}

fn missing(parent: Option<&String>) -> DataDirProblem {
    DataDirProblem::Missing {
        path: parent.map_or_else(
            || "the data directory".to_owned(),
            |p| format!("the data directory in {p}"),
        ),
    }
}

fn uid_name(uid: u32) -> String {
    format!("user id {uid}")
}

/// Every directory above: owned by user id 0, not writable by group or
/// others.
fn linux_ancestors(ancestors: &[UnixEntry]) -> Result<(), DataDirProblem> {
    for entry in ancestors {
        if entry.uid != 0 {
            return Err(DataDirProblem::Owner {
                path: entry.path.clone(),
                owner: uid_name(entry.uid),
            });
        }
        if entry.mode & 0o022 != 0 {
            return Err(DataDirProblem::Mode {
                path: entry.path.clone(),
                mode: entry.mode,
                required: "no write permission for group or others",
            });
        }
    }
    Ok(())
}

/// A directory that is not a link, owned by user id 0, with a mode `ok`
/// accepts.
fn linux_safe_dir(
    dir: &UnixEntry,
    required: &'static str,
    ok: impl Fn(u32) -> bool,
) -> Result<(), DataDirProblem> {
    match dir.kind {
        UnixKind::Symlink => {
            return Err(DataDirProblem::Link {
                path: dir.path.clone(),
            });
        }
        UnixKind::Dir => {}
        UnixKind::File | UnixKind::Other => {
            return Err(DataDirProblem::Kind {
                path: dir.path.clone(),
                expected: "directory",
            });
        }
    }
    if dir.uid != 0 {
        return Err(DataDirProblem::Owner {
            path: dir.path.clone(),
            owner: uid_name(dir.uid),
        });
    }
    if !ok(dir.mode) {
        return Err(DataDirProblem::Mode {
            path: dir.path.clone(),
            mode: dir.mode,
            required,
        });
    }
    Ok(())
}

fn linux_data_dir(
    dir: &UnixEntry,
    host_file: Option<&UnixEntry>,
    first_line_ok: bool,
) -> Result<(), DataDirProblem> {
    linux_safe_dir(dir, "0700", |mode| mode == 0o700)?;
    let host = host_file.ok_or_else(|| DataDirProblem::HostFile {
        path: format!("{}/alloy-host", dir.path),
    })?;
    if host.kind != UnixKind::File {
        return Err(DataDirProblem::Kind {
            path: host.path.clone(),
            expected: "regular file",
        });
    }
    if host.uid != 0 {
        return Err(DataDirProblem::Owner {
            path: host.path.clone(),
            owner: uid_name(host.uid),
        });
    }
    if host.mode != 0o600 {
        return Err(DataDirProblem::Mode {
            path: host.path.clone(),
            mode: host.mode,
            required: "0600",
        });
    }
    if !first_line_ok {
        return Err(DataDirProblem::HostFile {
            path: host.path.clone(),
        });
    }
    Ok(())
}

/// Every directory above: no reparse point, owned by `SYSTEM`,
/// `Administrators`, or `TrustedInstaller`, and no entry that applies to it
/// grants anyone else a right to delete, rename, or take it over, or
/// `FILE_DELETE_CHILD`, which reaches the directory below.
fn windows_ancestors(ancestors: &[WindowsEntry]) -> Result<(), DataDirProblem> {
    for entry in ancestors {
        if entry.reparse_point {
            return Err(DataDirProblem::Link {
                path: entry.path.clone(),
            });
        }
        if !entry.owner.is_trusted() {
            return Err(DataDirProblem::Owner {
                path: entry.path.clone(),
                owner: entry.owner.name.clone(),
            });
        }
        let dacl = entry.dacl.as_ref().ok_or_else(|| DataDirProblem::Grants {
            path: entry.path.clone(),
            principal: "Everyone".to_owned(),
        })?;
        if let Some(ace) = dacl
            .iter()
            .find(|ace| !ace.principal.is_trusted() && ace.grants(TAKEOVER))
        {
            return Err(DataDirProblem::Grants {
                path: entry.path.clone(),
                principal: ace.principal.name.clone(),
            });
        }
    }
    Ok(())
}

/// No reparse point, owned by `SYSTEM` or `Administrators`, a protected
/// list granting full control to both and nothing to anyone else, and a host
/// file with such an owner whose list is all inherited.
fn windows_data_dir(
    dir: &WindowsEntry,
    host_file: Option<&WindowsEntry>,
    first_line_ok: bool,
) -> Result<(), DataDirProblem> {
    if dir.reparse_point {
        return Err(DataDirProblem::Link {
            path: dir.path.clone(),
        });
    }
    if !dir.is_dir {
        return Err(DataDirProblem::Kind {
            path: dir.path.clone(),
            expected: "directory",
        });
    }
    if !dir.owner.is_admin() {
        return Err(DataDirProblem::Owner {
            path: dir.path.clone(),
            owner: dir.owner.name.clone(),
        });
    }
    let dacl = only_admins(dir)?;
    for sid in [SID_SYSTEM, SID_ADMINISTRATORS] {
        let full = dacl.iter().any(|ace| {
            ace.principal.is(sid)
                && (ace.grants(GENERIC_ALL)
                    || (ace.allow
                        && !ace.inherit_only
                        && ace.mask & FILE_ALL_ACCESS == FILE_ALL_ACCESS))
        });
        if !dir.dacl_protected || !full {
            return Err(DataDirProblem::Acl {
                path: dir.path.clone(),
            });
        }
    }
    let host = host_file.ok_or_else(|| DataDirProblem::HostFile {
        path: format!("{}\\alloy-host", dir.path),
    })?;
    if host.reparse_point || host.is_dir {
        return Err(DataDirProblem::Kind {
            path: host.path.clone(),
            expected: "regular file",
        });
    }
    if !host.owner.is_admin() {
        return Err(DataDirProblem::Owner {
            path: host.path.clone(),
            owner: host.owner.name.clone(),
        });
    }
    let host_dacl = only_admins(host)?;
    if host.dacl_protected || host_dacl.iter().any(|ace| !ace.inherited) {
        return Err(DataDirProblem::Acl {
            path: host.path.clone(),
        });
    }
    if !first_line_ok {
        return Err(DataDirProblem::HostFile {
            path: host.path.clone(),
        });
    }
    Ok(())
}

/// The entry's list, when it grants no access to anyone but `SYSTEM` and
/// `Administrators`.
///
/// Every allow entry counts, inherit-only ones included. An inherit-only
/// entry on the data directory is copied onto every file and directory
/// created in it, `secrets` and `at-rest.key` among them, and section 3.12
/// lets those grant "no access beyond the data directory's own". Only the
/// ancestor rule of section 3.12 is limited to entries that apply to a
/// directory itself.
fn only_admins(entry: &WindowsEntry) -> Result<&[Ace], DataDirProblem> {
    let dacl = entry.dacl.as_ref().ok_or_else(|| DataDirProblem::Grants {
        path: entry.path.clone(),
        principal: "Everyone".to_owned(),
    })?;
    if let Some(ace) = dacl
        .iter()
        .find(|ace| ace.allow && !ace.principal.is_admin())
    {
        return Err(DataDirProblem::Grants {
            path: entry.path.clone(),
            principal: ace.principal.name.clone(),
        });
    }
    Ok(dacl)
}
