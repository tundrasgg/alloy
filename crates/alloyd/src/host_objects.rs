//! The lines of the host file that record host objects `alloyd` creates
//! outside the install manifest (ARFC 0017 clause 3.17.2, section 4.2).
//!
//! Alloy adds an object's line before it creates the object, and removes the
//! line when the creation fails, so that an uninstall finds every object
//! Alloy created and removes it (clause 3.16 step 6). In this release the
//! one such object `alloyd` creates is `/run/alloy`, the directory of the
//! local admin socket (ARFC 0007 clause 3.10.2), on a `run` line.
//!
//! The host file is replaced whole through [`DataDir::write`], as section
//! 4.2 says.

use alloy_id::store::HOST_FILE;
use alloy_id::{DataDir, StoreError};
use alloy_rt::{Access, Create, DiskError, DiskIo, IoBuf, OpenOptions, ReadOp, RtPath};

/// The file whose content is the machine's boot id on Linux.
pub(crate) const BOOT_ID_FILE: &str = "/proc/sys/kernel/random/boot_id";

/// The length of a boot id, without its line feed (section 4.2).
const BOOT_ID_LEN: usize = 36;

/// Why a host file line could not be added or removed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HostLineError {
    /// The boot id file could not be opened or read.
    #[error("{BOOT_ID_FILE} cannot be read: {0}")]
    BootIdRead(DiskError),
    /// The boot id file does not hold 36 characters of lower case hex digits
    /// and dashes, with or without a line feed.
    #[error("{BOOT_ID_FILE} does not hold a boot id")]
    BootId,
    /// The host file could not be read or written.
    #[error("the host file cannot be read or written: {0}")]
    Store(#[from] StoreError),
    /// There is no host file.
    #[error("there is no host file")]
    Missing,
    /// The host file is not UTF-8 text whose every line ends with a line
    /// feed.
    #[error("the host file is not lines of text, each ending with a line feed")]
    Format,
}

/// The machine's current boot id: the 36 characters of [`BOOT_ID_FILE`]
/// without the line feed, read through `disk`, a `disk-io` rooted at `/`.
///
/// # Errors
///
/// [`HostLineError::BootIdRead`] and [`HostLineError::BootId`].
pub(crate) async fn read_boot_id(disk: &dyn DiskIo) -> Result<String, HostLineError> {
    let path =
        RtPath::new(BOOT_ID_FILE).map_err(|_| HostLineError::BootIdRead(DiskError::InvalidPath))?;
    let options = OpenOptions {
        access: Access::Read,
        create: Create::Never,
        direct: false,
        exclusive: false,
    };
    let file = disk
        .open(&path, options)
        .await
        .map_err(HostLineError::BootIdRead)?;
    // The file of /proc reports length 0, so a buffer longer than the content
    // is read until the end of the file.
    let buf = IoBuf::zeroed(2 * BOOT_ID_LEN).map_err(HostLineError::BootIdRead)?;
    let mut done = file.read_at(vec![ReadOp { offset: 0, buf }]).await;
    let read = done.pop().ok_or(HostLineError::BootIdRead(DiskError::Io))?;
    let len = read.result.map_err(HostLineError::BootIdRead)?;
    let bytes = read.buf.get(..len).ok_or(HostLineError::BootId)?;
    parse_boot_id(bytes)
}

/// The boot id in `bytes`, the content of [`BOOT_ID_FILE`].
fn parse_boot_id(bytes: &[u8]) -> Result<String, HostLineError> {
    let id = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let valid = id.len() == BOOT_ID_LEN
        && id
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b) || *b == b'-');
    if !valid {
        return Err(HostLineError::BootId);
    }
    String::from_utf8(id.to_vec()).map_err(|_| HostLineError::BootId)
}

/// The `run` line of the directory `dir` under `/run`, created in the boot
/// `boot_id` (section 4.2).
pub(crate) fn run_line(boot_id: &str, dir: &RtPath) -> String {
    format!("run {boot_id} {}", dir.as_str())
}

/// Adds `line` to the host file of `data`, unless the file holds it already.
/// `true` when it was added.
///
/// # Errors
///
/// [`HostLineError::Missing`], [`HostLineError::Format`], and
/// [`HostLineError::Store`]. The host file is then as it was.
pub(crate) async fn add_line(data: &DataDir, line: &str) -> Result<bool, HostLineError> {
    let text = read_host_file(data).await?;
    match with_line(&text, line)? {
        Some(new) => {
            data.write(HOST_FILE, new.as_bytes()).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Removes every copy of `line` from the host file of `data`. Writes nothing
/// when it holds none.
///
/// # Errors
///
/// As [`add_line`].
pub(crate) async fn remove_line(data: &DataDir, line: &str) -> Result<(), HostLineError> {
    let text = read_host_file(data).await?;
    if let Some(new) = without_line(&text, line)? {
        data.write(HOST_FILE, new.as_bytes()).await?;
    }
    Ok(())
}

async fn read_host_file(data: &DataDir) -> Result<String, HostLineError> {
    let bytes = data.read(HOST_FILE).await?.ok_or(HostLineError::Missing)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| HostLineError::Format)
}

/// `text`, a host file, with `line` added at its end, or `None` when one of
/// its lines is `line` already.
fn with_line(text: &str, line: &str) -> Result<Option<String>, HostLineError> {
    if lines(text)?.any(|l| l == line) {
        return Ok(None);
    }
    Ok(Some(format!("{text}{line}\n")))
}

/// `text`, a host file, without the lines that are `line`, or `None` when it
/// has none.
fn without_line(text: &str, line: &str) -> Result<Option<String>, HostLineError> {
    if !lines(text)?.any(|l| l == line) {
        return Ok(None);
    }
    let kept: String = lines(text)?
        .filter(|l| *l != line)
        .flat_map(|l| [l, "\n"])
        .collect();
    Ok(Some(kept))
}

/// The lines of `text`, each without its line feed.
fn lines(text: &str) -> Result<impl Iterator<Item = &str>, HostLineError> {
    let body = text.strip_suffix('\n').ok_or(HostLineError::Format)?;
    Ok(body.split('\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "alloy-host 1\njoin joined\nrelease none\n";
    const BOOT: &str = "6a2b8f0e-1c3d-4e5f-8a9b-0c1d2e3f4a5b";

    #[test]
    fn a_boot_id_is_read_without_its_line_feed() {
        assert_eq!(parse_boot_id(format!("{BOOT}\n").as_bytes()).unwrap(), BOOT);
        assert_eq!(parse_boot_id(BOOT.as_bytes()).unwrap(), BOOT);
        for bad in [
            "",
            "\n",
            "6a2b8f0e-1c3d-4e5f-8a9b-0c1d2e3f4a5",
            "6A2B8F0E-1C3D-4E5F-8A9B-0C1D2E3F4A5B",
            "6a2b8f0e-1c3d-4e5f-8a9b-0c1d2e3f4a5b\n\n",
            "6a2b8f0e 1c3d-4e5f-8a9b-0c1d2e3f4a5b",
        ] {
            assert!(
                matches!(parse_boot_id(bad.as_bytes()), Err(HostLineError::BootId)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_line_is_added_once_at_the_end() {
        let line = run_line(BOOT, &RtPath::new("/run/alloy").unwrap());
        assert_eq!(line, format!("run {BOOT} /run/alloy"));
        let added = with_line(HEAD, &line).unwrap().unwrap();
        assert_eq!(added, format!("{HEAD}{line}\n"));
        assert_eq!(with_line(&added, &line).unwrap(), None);
    }

    #[test]
    fn a_line_is_removed_and_the_others_kept() {
        let line = run_line(BOOT, &RtPath::new("/run/alloy").unwrap());
        let other = "link 6a2b8f0e-1c3d-4e5f-8a9b-0c1d2e3f4a5b al0";
        let text = format!("{HEAD}{line}\n{other}\n{line}\n");
        assert_eq!(
            without_line(&text, &line).unwrap().unwrap(),
            format!("{HEAD}{other}\n")
        );
        assert_eq!(without_line(HEAD, &line).unwrap(), None);
    }

    #[test]
    fn a_host_file_without_a_final_line_feed_is_refused() {
        let text = "alloy-host 1\njoin joined\nrelease none";
        assert!(matches!(
            with_line(text, "run x /run/alloy"),
            Err(HostLineError::Format)
        ));
        assert!(matches!(
            without_line(text, "run x /run/alloy"),
            Err(HostLineError::Format)
        ));
    }
}
