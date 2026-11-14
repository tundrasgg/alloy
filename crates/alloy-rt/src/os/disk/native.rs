//! `rt_path` of the `os` disk-io (ARFC 0005 clause 3.1.15): the `RtPath` that
//! names an absolute native path below the configured root. It reads nothing
//! from the disk, so both platforms' rules are plain functions of text and
//! are tested on every host.

use crate::{DiskError, RtPath};

/// How a platform writes native paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Style {
    /// Linux and macOS: absolute paths start with `/`, components are
    /// separated by `/` and compared byte for byte.
    Unix,
    /// Windows: absolute paths start with a drive letter, `:`, and a
    /// separator; components are separated by `\` or `/` and compared
    /// ignoring ASCII case.
    Windows,
}

impl Style {
    /// The style of the platform this is built for.
    pub(super) const HOST: Style = if cfg!(windows) {
        Style::Windows
    } else {
        Style::Unix
    };

    fn split(self, text: &str) -> impl Iterator<Item = &str> {
        let separators: &[char] = match self {
            Style::Unix => &['/'],
            Style::Windows => &['/', '\\'],
        };
        text.split(separators).filter(|c| !c.is_empty())
    }

    /// Whether `native` is absolute as `rt_path` requires.
    fn is_absolute(self, native: &str) -> bool {
        match self {
            Style::Unix => native.starts_with('/'),
            Style::Windows => matches!(
                native.as_bytes(),
                [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic()
            ),
        }
    }

    fn same(self, a: &str, b: &str) -> bool {
        match self {
            Style::Unix => a == b,
            Style::Windows => a.eq_ignore_ascii_case(b),
        }
    }
}

/// The `RtPath` of the native path `native` in an instance whose root is the
/// native path `root`.
///
/// # Errors
///
/// [`DiskError::InvalidPath`] when `native` is not absolute in `style`, holds
/// a component `.` or `..`, does not begin with every component of `root`, or
/// leaves a path that [`RtPath::new`] refuses.
pub(super) fn rt_path(root: &str, native: &str, style: Style) -> Result<RtPath, DiskError> {
    if !style.is_absolute(native) {
        return Err(DiskError::InvalidPath);
    }
    let mut parts = style.split(native);
    for base in style.split(root) {
        match parts.next() {
            Some(part) if style.same(part, base) && part != "." && part != ".." => {}
            _ => return Err(DiskError::InvalidPath),
        }
    }
    let rest: Vec<&str> = parts.collect();
    if rest.iter().any(|c| *c == "." || *c == "..") {
        return Err(DiskError::InvalidPath);
    }
    if rest.is_empty() {
        return Ok(RtPath::root());
    }
    RtPath::new(&format!("/{}", rest.join("/"))).map_err(|_| DiskError::InvalidPath)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(root: &str, native: &str, style: Style) -> String {
        rt_path(root, native, style)
            .map(|p| p.as_str().to_owned())
            .unwrap_or_else(|e| format!("{e:?}"))
    }

    #[test]
    fn unix_paths_follow_the_root_byte_for_byte() {
        use Style::Unix;
        assert_eq!(ok("/var/lib", "/var/lib/alloy/keys", Unix), "/alloy/keys");
        assert_eq!(ok("/var/lib/", "//var//lib///alloy/", Unix), "/alloy");
        assert_eq!(ok("/var/lib", "/var/lib", Unix), "/");
        assert_eq!(ok("/", "/etc/x", Unix), "/etc/x");
        assert_eq!(ok("/", "/", Unix), "/");
        for refused in [
            "var/lib/a",
            "",
            "/var/LIB/a",
            "/var/li/a",
            "/var",
            "/var/lib/./a",
            "/var/lib/../a",
            "/var/lib/a\\b",
            "/var/lib/a\0b",
        ] {
            assert_eq!(ok("/var/lib", refused, Unix), "InvalidPath", "{refused:?}");
        }
        let long = format!("/r/{}", "c".repeat(256));
        assert_eq!(ok("/r", &long, Unix), "InvalidPath");
    }

    #[test]
    fn windows_paths_start_with_a_drive_and_ignore_ascii_case() {
        use Style::Windows;
        let root = r"C:\ProgramData\Alloy";
        assert_eq!(ok(root, r"c:\programdata\alloy\data\k", Windows), "/data/k");
        assert_eq!(ok(root, "C:/ProgramData/Alloy/data/", Windows), "/data");
        assert_eq!(ok(root, r"C:\ProgramData\\Alloy", Windows), "/");
        assert_eq!(ok(r"C:\", r"C:\x", Windows), "/x");
        assert_eq!(ok("C:", r"C:\x\y", Windows), "/x/y");
        for refused in [
            r"\ProgramData\Alloy\x",
            r"C:ProgramData\Alloy\x",
            r"D:\ProgramData\Alloy\x",
            r"1:\ProgramData\Alloy\x",
            r"\\server\share\x",
            r"C:\ProgramData\x",
            r"C:\ProgramData\Alloy\.\x",
            r"C:\ProgramData\Alloy\..\x",
            r"C:\ProgramData\Älloy\x",
        ] {
            assert_eq!(ok(root, refused, Windows), "InvalidPath", "{refused:?}");
        }
    }
}
