use crate::datadir::{
    Ace, DataDirFacts, DataDirProblem, FILE_ALL_ACCESS, FILE_DELETE_CHILD, InitAction, Principal,
    SID_ADMINISTRATORS, SID_SYSTEM, SID_TRUSTED_INSTALLER, UnixEntry, UnixKind, WindowsEntry,
    check_at_start, check_for_init,
};

fn unix(path: &str, kind: UnixKind, uid: u32, mode: u32) -> UnixEntry {
    UnixEntry {
        path: path.to_owned(),
        kind,
        uid,
        mode,
    }
}

fn linux(
    change: impl FnOnce(
        &mut Vec<UnixEntry>,
        &mut Option<UnixEntry>,
        &mut bool,
        &mut Option<UnixEntry>,
        &mut bool,
    ),
) -> DataDirFacts {
    let mut ancestors = vec![
        unix("/", UnixKind::Dir, 0, 0o755),
        unix("/var", UnixKind::Dir, 0, 0o755),
        unix("/var/lib", UnixKind::Dir, 0, 0o755),
    ];
    let mut data_dir = Some(unix("/var/lib/alloyd", UnixKind::Dir, 0, 0o700));
    let mut empty = false;
    let mut host = Some(unix("/var/lib/alloyd/alloy-host", UnixKind::File, 0, 0o600));
    let mut first_line_ok = true;
    change(
        &mut ancestors,
        &mut data_dir,
        &mut empty,
        &mut host,
        &mut first_line_ok,
    );
    DataDirFacts::Linux {
        ancestors,
        data_dir,
        data_dir_empty: empty,
        host_file: host,
        host_file_first_line_ok: first_line_ok,
    }
}

fn mode(path: &str, mode: u32, required: &'static str) -> DataDirProblem {
    DataDirProblem::Mode {
        path: path.to_owned(),
        mode,
        required,
    }
}

fn owner(path: &str, owner: &str) -> DataDirProblem {
    DataDirProblem::Owner {
        path: path.to_owned(),
        owner: owner.to_owned(),
    }
}

#[test]
fn linux_rules() {
    let ok = linux(|_, _, _, _, _| {});
    assert_eq!(check_for_init(&ok), Ok(InitAction::Use));
    assert_eq!(check_at_start(&ok), Ok(()));
    let missing = linux(|_, d, _, h, _| {
        *d = None;
        *h = None;
    });
    assert_eq!(check_for_init(&missing), Ok(InitAction::Create));
    assert!(matches!(
        check_at_start(&missing),
        Err(DataDirProblem::Missing { .. })
    ));
    let mount_point = linux(|_, d, e, h, _| {
        *d = Some(unix("/var/lib/alloyd", UnixKind::Dir, 0, 0o755));
        *e = true;
        *h = None;
    });
    assert_eq!(check_for_init(&mount_point), Ok(InitAction::Adopt));
    assert_eq!(
        check_at_start(&mount_point),
        Err(mode("/var/lib/alloyd", 0o755, "0700"))
    );

    let refused: Vec<(DataDirFacts, DataDirProblem)> = vec![
        (
            linux(|a, _, _, _, _| a[1].uid = 1000),
            owner("/var", "user id 1000"),
        ),
        (
            linux(|a, _, _, _, _| a[2].mode = 0o775),
            mode("/var/lib", 0o775, "no write permission for group or others"),
        ),
        (
            linux(|a, _, _, _, _| a[0].mode = 0o757),
            mode("/", 0o757, "no write permission for group or others"),
        ),
        (
            linux(|_, d, _, _, _| *d = Some(unix("/var/lib/alloyd", UnixKind::Symlink, 0, 0o777))),
            DataDirProblem::Link {
                path: "/var/lib/alloyd".to_owned(),
            },
        ),
        (
            linux(|_, d, _, _, _| *d = Some(unix("/var/lib/alloyd", UnixKind::File, 0, 0o700))),
            DataDirProblem::Kind {
                path: "/var/lib/alloyd".to_owned(),
                expected: "directory",
            },
        ),
        (
            linux(|_, d, _, _, _| *d = Some(unix("/var/lib/alloyd", UnixKind::Dir, 1000, 0o700))),
            owner("/var/lib/alloyd", "user id 1000"),
        ),
        (
            linux(|_, d, _, _, _| *d = Some(unix("/var/lib/alloyd", UnixKind::Dir, 0, 0o750))),
            mode("/var/lib/alloyd", 0o750, "0700"),
        ),
        (
            linux(|_, _, _, h, _| {
                *h = Some(unix("/var/lib/alloyd/alloy-host", UnixKind::File, 0, 0o644))
            }),
            mode("/var/lib/alloyd/alloy-host", 0o644, "0600"),
        ),
        (
            linux(|_, _, _, h, _| {
                *h = Some(unix("/var/lib/alloyd/alloy-host", UnixKind::File, 7, 0o600))
            }),
            owner("/var/lib/alloyd/alloy-host", "user id 7"),
        ),
        (
            linux(|_, _, _, h, _| {
                *h = Some(unix(
                    "/var/lib/alloyd/alloy-host",
                    UnixKind::Symlink,
                    0,
                    0o600,
                ))
            }),
            DataDirProblem::Kind {
                path: "/var/lib/alloyd/alloy-host".to_owned(),
                expected: "regular file",
            },
        ),
        (
            linux(|_, _, _, _, f| *f = false),
            DataDirProblem::HostFile {
                path: "/var/lib/alloyd/alloy-host".to_owned(),
            },
        ),
    ];
    for (facts, problem) in refused {
        assert_eq!(check_for_init(&facts), Err(problem.clone()), "{facts:?}");
        assert_eq!(check_at_start(&facts), Err(problem), "{facts:?}");
    }

    // A directory with other content and no host file is never adopted,
    // nor an empty one that another user owns.
    let crowded = linux(|_, d, _, h, _| {
        *d = Some(unix("/var/lib/alloyd", UnixKind::Dir, 0, 0o755));
        *h = None;
    });
    assert_eq!(
        check_for_init(&crowded),
        Err(DataDirProblem::Foreign {
            path: "/var/lib/alloyd".to_owned(),
            owner: "user id 0".to_owned(),
        })
    );
    let owned = linux(|_, d, e, h, _| {
        *d = Some(unix("/var/lib/alloyd", UnixKind::Dir, 1000, 0o700));
        *e = true;
        *h = None;
    });
    assert_eq!(
        check_for_init(&owned),
        Err(owner("/var/lib/alloyd", "user id 1000"))
    );
    assert!(check_at_start(&crowded).is_err() && check_at_start(&owned).is_err());
}

fn principal(sid: &str, name: &str) -> Principal {
    Principal {
        sid: sid.to_owned(),
        name: name.to_owned(),
    }
}

fn system() -> Principal {
    principal(SID_SYSTEM, "NT AUTHORITY\\SYSTEM")
}

fn admins() -> Principal {
    principal(SID_ADMINISTRATORS, "BUILTIN\\Administrators")
}

fn users() -> Principal {
    principal("S-1-5-32-545", "BUILTIN\\Users")
}

fn user() -> Principal {
    principal("S-1-5-21-1-2-3-1001", "HOST\\someone")
}

fn ace(principal: Principal, mask: u32) -> Ace {
    Ace {
        allow: true,
        principal,
        mask,
        inherit_only: false,
        inherited: false,
    }
}

fn win_dir(path: &str, owner: Principal, protected: bool, dacl: Vec<Ace>) -> WindowsEntry {
    WindowsEntry {
        path: path.to_owned(),
        is_dir: true,
        reparse_point: false,
        owner,
        dacl_protected: protected,
        dacl: Some(dacl),
    }
}

type WindowsParts = (
    Vec<WindowsEntry>,
    Option<WindowsEntry>,
    Option<WindowsEntry>,
    bool,
);

fn windows(change: impl FnOnce(&mut WindowsParts)) -> DataDirFacts {
    let read_execute = 0x0012_00a9;
    let modify = 0x0013_01bf;
    let root = win_dir(
        "C:\\",
        principal(SID_TRUSTED_INSTALLER, "NT SERVICE\\TrustedInstaller"),
        true,
        vec![
            ace(system(), FILE_ALL_ACCESS),
            ace(admins(), FILE_ALL_ACCESS),
            ace(users(), read_execute),
            // Authenticated Users may modify what they create below the
            // root, which does not reach the root itself.
            Ace {
                inherit_only: true,
                ..ace(
                    principal("S-1-5-11", "NT AUTHORITY\\Authenticated Users"),
                    modify,
                )
            },
            // ...and create folders in it.
            ace(
                principal("S-1-5-11", "NT AUTHORITY\\Authenticated Users"),
                0x0000_0004,
            ),
        ],
    );
    let program_data = win_dir(
        "C:\\ProgramData",
        system(),
        true,
        vec![
            ace(system(), FILE_ALL_ACCESS),
            ace(admins(), FILE_ALL_ACCESS),
            ace(users(), read_execute | 0x0000_0006),
            Ace {
                inherit_only: true,
                ..ace(principal("S-1-3-0", "CREATOR OWNER"), FILE_ALL_ACCESS)
            },
        ],
    );
    let data_dir = win_dir(
        "C:\\ProgramData\\Alloy",
        system(),
        true,
        vec![
            ace(system(), FILE_ALL_ACCESS),
            ace(admins(), FILE_ALL_ACCESS),
        ],
    );
    let host = WindowsEntry {
        is_dir: false,
        dacl_protected: false,
        dacl: Some(vec![
            Ace {
                inherited: true,
                ..ace(system(), FILE_ALL_ACCESS)
            },
            Ace {
                inherited: true,
                ..ace(admins(), FILE_ALL_ACCESS)
            },
        ]),
        ..win_dir(
            "C:\\ProgramData\\Alloy\\alloy-host",
            system(),
            false,
            Vec::new(),
        )
    };
    let mut parts = (vec![root, program_data], Some(data_dir), Some(host), true);
    change(&mut parts);
    DataDirFacts::Windows {
        ancestors: parts.0,
        data_dir: parts.1,
        host_file: parts.2,
        host_file_first_line_ok: parts.3,
    }
}

fn grants(path: &str, principal: &str) -> DataDirProblem {
    DataDirProblem::Grants {
        path: path.to_owned(),
        principal: principal.to_owned(),
    }
}

fn acl(path: &str) -> DataDirProblem {
    DataDirProblem::Acl {
        path: path.to_owned(),
    }
}

#[test]
fn windows_rules() {
    let ok = windows(|_| {});
    assert_eq!(check_for_init(&ok), Ok(InitAction::Use));
    assert_eq!(check_at_start(&ok), Ok(()));
    let missing = windows(|p| {
        p.1 = None;
        p.2 = None;
    });
    assert_eq!(check_for_init(&missing), Ok(InitAction::Create));

    // A local user created `Alloy` in %ProgramData%: `join` names the
    // directory and its owner, and no existing directory is adopted.
    let squatted = windows(|p| {
        p.1 = Some(win_dir(
            "C:\\ProgramData\\Alloy",
            user(),
            false,
            vec![ace(user(), FILE_ALL_ACCESS)],
        ));
        p.2 = None;
    });
    let foreign = DataDirProblem::Foreign {
        path: "C:\\ProgramData\\Alloy".to_owned(),
        owner: "HOST\\someone".to_owned(),
    };
    assert_eq!(check_for_init(&squatted), Err(foreign));
    assert_eq!(
        foreign_message(&squatted),
        "C:\\ProgramData\\Alloy, owned by HOST\\someone, is not an Alloy data directory"
    );
    let empty_by_admins = windows(|p| p.2 = None);
    assert!(matches!(
        check_for_init(&empty_by_admins),
        Err(DataDirProblem::Foreign { .. })
    ));

    let refused: Vec<(DataDirFacts, DataDirProblem)> = vec![
        // An administrator granted Users FILE_DELETE_CHILD on %ProgramData%.
        (
            windows(|p| {
                p.0[1]
                    .dacl
                    .as_mut()
                    .expect("list")
                    .push(ace(users(), FILE_DELETE_CHILD))
            }),
            grants("C:\\ProgramData", "BUILTIN\\Users"),
        ),
        (
            windows(|p| {
                p.0[0]
                    .dacl
                    .as_mut()
                    .expect("list")
                    .push(ace(user(), 0x0001_0000))
            }),
            grants("C:\\", "HOST\\someone"),
        ),
        (
            windows(|p| {
                p.0[1]
                    .dacl
                    .as_mut()
                    .expect("list")
                    .push(ace(users(), 0x1000_0000))
            }),
            grants("C:\\ProgramData", "BUILTIN\\Users"),
        ),
        (
            windows(|p| p.0[1].dacl = None),
            grants("C:\\ProgramData", "Everyone"),
        ),
        (
            windows(|p| p.0[1].reparse_point = true),
            DataDirProblem::Link {
                path: "C:\\ProgramData".to_owned(),
            },
        ),
        (
            windows(|p| p.0[1].owner = user()),
            owner("C:\\ProgramData", "HOST\\someone"),
        ),
        (
            windows(|p| p.1.as_mut().expect("dir").reparse_point = true),
            DataDirProblem::Link {
                path: "C:\\ProgramData\\Alloy".to_owned(),
            },
        ),
        (
            windows(|p| {
                p.1.as_mut().expect("dir").owner =
                    principal(SID_TRUSTED_INSTALLER, "NT SERVICE\\TrustedInstaller");
            }),
            owner("C:\\ProgramData\\Alloy", "NT SERVICE\\TrustedInstaller"),
        ),
        (
            windows(|p| p.1.as_mut().expect("dir").dacl_protected = false),
            acl("C:\\ProgramData\\Alloy"),
        ),
        (
            windows(|p| {
                p.1.as_mut().expect("dir").dacl = Some(vec![ace(system(), FILE_ALL_ACCESS)])
            }),
            acl("C:\\ProgramData\\Alloy"),
        ),
        (
            windows(|p| {
                p.1.as_mut()
                    .expect("dir")
                    .dacl
                    .as_mut()
                    .expect("list")
                    .push(ace(users(), 0x0012_0089));
            }),
            grants("C:\\ProgramData\\Alloy", "BUILTIN\\Users"),
        ),
        // T-0106 F1: an inherit-only entry reaches nothing on the data
        // directory itself but every file created in it.
        (
            windows(|p| {
                p.1.as_mut()
                    .expect("dir")
                    .dacl
                    .as_mut()
                    .expect("list")
                    .push(Ace {
                        inherit_only: true,
                        ..ace(users(), 0x0012_0089)
                    });
            }),
            grants("C:\\ProgramData\\Alloy", "BUILTIN\\Users"),
        ),
        // The same for a principal holding no right at all, and for an
        // entry on the host file.
        (
            windows(|p| {
                p.1.as_mut()
                    .expect("dir")
                    .dacl
                    .as_mut()
                    .expect("list")
                    .push(Ace {
                        inherit_only: true,
                        ..ace(principal("S-1-3-0", "CREATOR OWNER"), 0)
                    });
            }),
            grants("C:\\ProgramData\\Alloy", "CREATOR OWNER"),
        ),
        (
            windows(|p| {
                p.2.as_mut()
                    .expect("host")
                    .dacl
                    .as_mut()
                    .expect("list")
                    .push(Ace {
                        inherited: true,
                        ..ace(users(), 0x0012_0089)
                    });
            }),
            grants("C:\\ProgramData\\Alloy\\alloy-host", "BUILTIN\\Users"),
        ),
        (
            windows(|p| p.2.as_mut().expect("host").dacl_protected = true),
            acl("C:\\ProgramData\\Alloy\\alloy-host"),
        ),
        (
            windows(|p| {
                p.2.as_mut().expect("host").dacl.as_mut().expect("list")[0].inherited = false;
            }),
            acl("C:\\ProgramData\\Alloy\\alloy-host"),
        ),
        (
            windows(|p| p.2.as_mut().expect("host").owner = users()),
            owner("C:\\ProgramData\\Alloy\\alloy-host", "BUILTIN\\Users"),
        ),
        (
            windows(|p| p.3 = false),
            DataDirProblem::HostFile {
                path: "C:\\ProgramData\\Alloy\\alloy-host".to_owned(),
            },
        ),
    ];
    for (facts, problem) in refused {
        assert_eq!(check_for_init(&facts), Err(problem.clone()), "{facts:?}");
        assert_eq!(check_at_start(&facts), Err(problem), "{facts:?}");
    }
}

fn foreign_message(facts: &DataDirFacts) -> String {
    check_for_init(facts).map_or_else(|e| e.to_string(), |_| String::new())
}
