//! The text the Linux `os` implementation of `host` reads, parsed without
//! calling the operating system, so that every platform tests it (ARFC 0005
//! clauses 3.1.18.1 and 3.1.18.3).

#![cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "only the Linux implementation reads this text; the tests run everywhere"
    )
)]

use crate::{RunState, ServiceStatus};

/// The most bytes of a Linux release in `Platform::Linux`.
pub(super) const MAX_RELEASE: usize = 64;
/// The most bytes of an `Arch::Other` text.
pub(super) const MAX_ARCH: usize = 32;
/// The most disks `HostFacts` holds.
pub(super) const MAX_DISKS: usize = 32;
/// The most bytes of a `free_space` path under `os`.
pub(super) const MAX_PATH: usize = 4_096;

/// `text` cut to at most `max` bytes at a character boundary.
pub(super) fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The leading `<digits>.<digits>` of a kernel release, each at most
/// `u32::MAX`, or 0 and 0 when the release does not start that way.
pub(super) fn release_numbers(release: &str) -> (u32, u32) {
    let digits = |text: &str| {
        let end = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        (end > 0)
            .then(|| text[..end].parse::<u32>().ok().map(|n| (n, end)))
            .flatten()
    };
    let Some((major, end)) = digits(release) else {
        return (0, 0);
    };
    let Some(rest) = release[end..].strip_prefix('.') else {
        return (0, 0);
    };
    match digits(rest) {
        Some((minor, _)) => (major, minor),
        None => (0, 0),
    }
}

/// `MemTotal` of `/proc/meminfo`, in bytes, or `None` when no line gives it.
pub(super) fn mem_total(meminfo: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let value = line.strip_prefix("MemTotal:")?.trim();
        let kib = value.strip_suffix("kB").unwrap_or(value).trim();
        kib.parse::<u64>().ok().map(|kib| kib.saturating_mul(1_024))
    })
}

/// Whether a `/sys/block` entry is left out of `disks` by its name.
pub(super) fn skipped_block_name(name: &str) -> bool {
    ["loop", "ram", "zram", "nbd", "dm-", "md"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The disk size of a `/sys/block/<name>/size` file, which counts 512-byte
/// sectors, or `None` when it is not a number above 0.
pub(super) fn sectors_to_bytes(size: &str) -> Option<u64> {
    let sectors = size.trim().parse::<u64>().ok()?;
    (sectors > 0).then(|| sectors.saturating_mul(512))
}

/// What `systemctl show --property=LoadState --property=UnitFileState
/// --property=ActiveState alloyd.service` printed, or `None` when the unit
/// is not loaded.
pub(super) fn systemctl_show(output: &str) -> Option<ServiceStatus> {
    let mut load = "";
    let mut file = "";
    let mut active = "";
    for line in output.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        match name {
            "LoadState" => load = value.trim(),
            "UnitFileState" => file = value.trim(),
            "ActiveState" => active = value.trim(),
            _ => {}
        }
    }
    if load != "loaded" {
        return None;
    }
    let run = match active {
        "active" | "reloading" => RunState::Running,
        "activating" => RunState::Starting,
        "deactivating" => RunState::Stopping,
        _ => RunState::Stopped,
    };
    Some(ServiceStatus {
        enabled: file == "enabled",
        run,
    })
}

/// The first line of `stderr`, cut to 1,024 bytes, for `Refused`.
pub(super) fn first_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().next().unwrap_or("").trim_end();
    cut(line, 1_024).to_owned()
}

/// The nearest ancestor of the absolute path `path`, or `None` at the root.
/// `separator` is `/` or `\`, and `min` the length of the root.
pub(super) fn parent(path: &str, separator: char, min: usize) -> Option<&str> {
    let trimmed = path.trim_end_matches(separator);
    if trimmed.len() < min {
        return None;
    }
    let at = trimmed.rfind(separator)?;
    let parent = &trimmed[..at];
    if parent.len() < min {
        (trimmed.len() > min).then(|| &path[..min])
    } else {
        Some(parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_give_their_leading_numbers() {
        assert_eq!(release_numbers("6.1.0-18-amd64"), (6, 1));
        assert_eq!(release_numbers("6.12"), (6, 12));
        assert_eq!(
            release_numbers("5.15.153.1-microsoft-standard-WSL2"),
            (5, 15)
        );
        assert_eq!(release_numbers("6"), (0, 0));
        assert_eq!(release_numbers("6."), (0, 0));
        assert_eq!(release_numbers("v6.1"), (0, 0));
        assert_eq!(release_numbers("4294967296.1"), (0, 0));
        assert_eq!(
            release_numbers("4294967295.4294967295"),
            (u32::MAX, u32::MAX)
        );
        assert_eq!(release_numbers(""), (0, 0));
    }

    #[test]
    fn cut_keeps_character_boundaries() {
        assert_eq!(cut("abc", 2), "ab");
        assert_eq!(cut("abc", 64), "abc");
        assert_eq!(cut("aé", 2), "a");
    }

    #[test]
    fn mem_total_reads_kibibytes() {
        let meminfo = "MemFree:  100 kB\nMemTotal:       16303428 kB\n";
        assert_eq!(mem_total(meminfo), Some(16_303_428 * 1_024));
        assert_eq!(mem_total("MemFree: 1 kB\n"), None);
    }

    #[test]
    fn block_devices_follow_the_disk_rule() {
        for name in ["loop0", "ram1", "zram0", "nbd3", "dm-0", "md127"] {
            assert!(skipped_block_name(name), "{name}");
        }
        for name in ["sda", "nvme0n1", "vda", "mmcblk0"] {
            assert!(!skipped_block_name(name), "{name}");
        }
        assert_eq!(sectors_to_bytes("2048\n"), Some(1_048_576));
        assert_eq!(sectors_to_bytes("0\n"), None);
        assert_eq!(sectors_to_bytes("x"), None);
    }

    #[test]
    fn systemctl_show_maps_the_states() {
        let show = |file: &str, active: &str| {
            systemctl_show(&format!(
                "LoadState=loaded\nUnitFileState={file}\nActiveState={active}\n"
            ))
        };
        let status = |enabled, run| Some(ServiceStatus { enabled, run });
        assert_eq!(show("enabled", "active"), status(true, RunState::Running));
        assert_eq!(
            show("disabled", "reloading"),
            status(false, RunState::Running)
        );
        assert_eq!(
            show("enabled-runtime", "activating"),
            status(false, RunState::Starting)
        );
        assert_eq!(
            show("enabled", "deactivating"),
            status(true, RunState::Stopping)
        );
        assert_eq!(show("enabled", "failed"), status(true, RunState::Stopped));
        assert_eq!(
            show("disabled", "inactive"),
            status(false, RunState::Stopped)
        );
        let missing = "LoadState=not-found\nUnitFileState=\nActiveState=inactive\n";
        assert_eq!(systemctl_show(missing), None);
        assert_eq!(systemctl_show(""), None);
    }

    #[test]
    fn first_line_is_cut() {
        assert_eq!(
            first_line(b"Failed to start: x\nmore\n"),
            "Failed to start: x"
        );
        assert_eq!(first_line(&[b'a'; 2_000]).len(), 1_024);
        assert_eq!(first_line(b""), "");
    }

    #[test]
    fn parents_walk_up_to_the_root() {
        assert_eq!(parent("/var/lib/alloyd", '/', 1), Some("/var/lib"));
        assert_eq!(parent("/var/lib/", '/', 1), Some("/var"));
        assert_eq!(parent("/var", '/', 1), Some("/"));
        assert_eq!(parent("/", '/', 1), None);
        assert_eq!(
            parent(r"C:\ProgramData\Alloy", '\\', 3),
            Some(r"C:\ProgramData")
        );
        assert_eq!(parent(r"C:\ProgramData", '\\', 3), Some(r"C:\"));
        assert_eq!(parent(r"C:\", '\\', 3), None);
    }
}
