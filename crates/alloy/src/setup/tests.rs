//! Tests of the parts of `alloy init` and `alloy join` that need no machine.

use alloy_rt::{DiskFact, HostFacts};

use super::checks::{install_command, size};
use super::join::request_facts;
use super::*;

#[test]
fn the_default_node_name_follows_section_3_3() {
    assert_eq!(default_node_name("Web-1.example.com"), "web-1");
    assert_eq!(default_node_name("db_2"), "db-2");
    assert_eq!(default_node_name("1node"), "node-1node");
    assert_eq!(default_node_name(&"a".repeat(80)).len(), 63);
    assert_eq!(default_node_name(""), "node-");
}

#[test]
fn private_addresses_follow_section_3_1_step_2() {
    for private in [
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.10",
        "100.64.0.1",
        "169.254.1.1",
        "fd00::1",
        "fc00::1",
        "fe80::1",
    ] {
        assert!(is_private(private.parse().unwrap()), "{private}");
    }
    for public in [
        "172.32.0.1",
        "100.128.0.1",
        "203.0.113.10",
        "2001:db8::1",
        "8.8.8.8",
    ] {
        assert!(!is_private(public.parse().unwrap()), "{public}");
    }
}

#[test]
fn step_lines_pad_the_status_word_and_color_it_alone() {
    let plain = Style { on: false };
    assert_eq!(
        step_line(Status::Ok, "Linux 6.8.0 on x86_64", plain),
        "  ok    Linux 6.8.0 on x86_64\n"
    );
    assert_eq!(step_line(Status::Done, "x", plain), "  done  x\n");
    assert_eq!(step_line(Status::Warn, "x", plain), "  warn  x\n");
    assert_eq!(step_line(Status::Fail, "x", plain), "  fail  x\n");
    let styled = Style { on: true };
    assert_eq!(
        step_line(Status::Ok, "x", styled),
        "  \x1b[32mok\x1b[0m    x\n"
    );
    assert_eq!(
        step_line(Status::Fail, "x", styled),
        "  \x1b[31mfail\x1b[0m  x\n"
    );
    let long = step_line(Status::Ok, &"word ".repeat(30), plain);
    assert!(long.lines().all(|line| line.len() <= 80), "{long}");
    assert!(
        long.lines()
            .skip(1)
            .all(|line| line.starts_with("        word")),
        "{long}"
    );
}

#[test]
fn sizes_use_binary_units() {
    assert_eq!(size(512), "512 bytes");
    assert_eq!(size(1_932_735_283), "1.7 GiB");
    assert_eq!(size(442_381_631_488), "412 GiB");
    assert_eq!(size(1_979_120_929_996), "1.7 TiB");
}

#[test]
fn the_install_command_names_this_release() {
    let linux = install_command(false);
    assert_eq!(
        linux,
        format!(
            "curl -fsSL https://github.com/tundrasgg/alloy/releases/download/v{}/install.sh | sh",
            crate::VERSION
        )
    );
    assert!(install_command(true).starts_with("irm https://"));
    assert!(install_command(true).ends_with("/install.ps1 | iex"));
}

#[test]
fn request_facts_follow_section_3_3_step_6() {
    let facts = HostFacts {
        platform: alloy_rt::Platform::Linux {
            release: "6.8.0-45-generic".into(),
            major: 6,
            minor: 8,
        },
        arch: alloy_rt::Arch::Aarch64,
        privileged: true,
        cores: 8,
        memory: 1 << 34,
        virtualization: true,
        disks: vec![DiskFact {
            path: "/dev/nvme0n1".into(),
            size: 1 << 40,
        }],
    };
    let request = request_facts(&facts);
    assert_eq!(request.os, "linux 6.8.0-45-generic");
    assert_eq!(request.arch, "aarch64");
    assert_eq!((request.cores, request.memory_bytes), (8, 1 << 34));
    assert!(request.virtualization);
    assert_eq!(request.disks.len(), 1);
    assert_eq!(request.disks[0].path, "/dev/nvme0n1");
    assert_eq!(request.disks[0].size_bytes, 1 << 40);
    let long = HostFacts {
        platform: alloy_rt::Platform::Linux {
            release: "é".repeat(32),
            major: 0,
            minor: 0,
        },
        ..facts.clone()
    };
    let os = request_facts(&long).os;
    assert!(os.len() <= 64 && os.starts_with("linux é"), "{os}");
    let windows = HostFacts {
        platform: alloy_rt::Platform::Windows {
            major: 10,
            minor: 0,
            build: 26_100,
        },
        disks: Vec::new(),
        ..facts
    };
    assert_eq!(request_facts(&windows).os, "windows 10.0.26100");
}

#[test]
fn roles_print_in_clause_3_1_15_order() {
    assert_eq!(roles_text(Roles::NONE), "none");
    assert_eq!(
        roles_text(Roles::of(&[Role::Storage, Role::Anchor])),
        "anchor,storage"
    );
}

#[test]
fn data_paths_name_every_directory_above() {
    let base = std::env::temp_dir().join("alloy-data");
    let data = DataPath::of(&base).unwrap();
    assert_eq!(data.native, base);
    assert_eq!(data.host_file_native, base.join("alloy-host"));
    assert_eq!(data.ancestors.first().map(|a| a.0.as_str()), Some("/"));
    assert_eq!(data.ancestors.len(), data.dir.as_str().matches('/').count());
    assert!(DataPath::of(Path::new("relative")).is_err());
}
