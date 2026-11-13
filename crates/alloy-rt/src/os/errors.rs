//! Maps operating system errors to the seam error types (ARFC 0005 sections
//! 2.3 and 2.4). A code with no variant of its own becomes `Os`.

use std::io::{self, ErrorKind};

use alloy_seam::Detail;

use crate::{DiskError, NetError};

/// Error codes that `std::io::ErrorKind` does not name.
#[cfg(unix)]
mod code {
    pub(super) const TOO_MANY_OPEN: [i32; 2] = [libc::EMFILE, libc::ENFILE];
    pub(super) const MESSAGE_TOO_LONG: i32 = libc::EMSGSIZE;
    pub(super) const IO: i32 = libc::EIO;
    pub(super) const INVALID: i32 = libc::EINVAL;
}

/// Error codes that `std::io::ErrorKind` does not name.
#[cfg(windows)]
mod code {
    /// `ERROR_TOO_MANY_OPEN_FILES` and `WSAEMFILE`.
    pub(super) const TOO_MANY_OPEN: [i32; 2] = [4, 10_024];
    /// `WSAEMSGSIZE`.
    pub(super) const MESSAGE_TOO_LONG: i32 = 10_040;
    /// `ERROR_IO_DEVICE`.
    pub(super) const IO: i32 = 1_117;
    /// `ERROR_INVALID_PARAMETER`; `WSAEINVAL` is checked separately.
    pub(super) const INVALID: i32 = 87;
}

/// True when the process or the system has no free descriptor.
pub(super) fn too_many_open(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|c| code::TOO_MANY_OPEN.contains(&c))
}

/// True for the error of a datagram longer than the path allows.
pub(super) fn message_too_long(error: &io::Error) -> bool {
    error.raw_os_error() == Some(code::MESSAGE_TOO_LONG)
}

/// True for the platform's invalid argument error.
pub(super) fn invalid_argument(error: &io::Error) -> bool {
    #[cfg(windows)]
    const WSAEINVAL: i32 = 10_022;
    #[cfg(not(windows))]
    const WSAEINVAL: i32 = code::INVALID;
    error.kind() == ErrorKind::InvalidInput
        || matches!(error.raw_os_error(), Some(c) if c == code::INVALID || c == WSAEINVAL)
}

/// `Os` with the error's code, or `fallback` for an error that carries none.
fn os_or<E>(error: &io::Error, fallback: E, os: impl FnOnce(i32) -> E) -> E {
    error.raw_os_error().map_or(fallback, os)
}

/// The [`NetError`] of a socket operation that failed with `error`. An error
/// with no operating system code, such as the one `tokio` returns once its
/// runtime has stopped, is [`NetError::Closed`].
pub(super) fn net(error: &io::Error) -> NetError {
    if too_many_open(error) {
        return NetError::TooManyOpen;
    }
    match error.kind() {
        ErrorKind::AddrInUse => NetError::AddrInUse,
        ErrorKind::AddrNotAvailable => NetError::AddrNotAvailable,
        ErrorKind::PermissionDenied => NetError::PermissionDenied,
        ErrorKind::ConnectionRefused => NetError::ConnectionRefused,
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::BrokenPipe => {
            NetError::ConnectionReset
        }
        ErrorKind::TimedOut => NetError::TimedOut,
        ErrorKind::HostUnreachable => NetError::HostUnreachable,
        ErrorKind::NetworkUnreachable | ErrorKind::NetworkDown => NetError::NetworkUnreachable,
        ErrorKind::Unsupported => NetError::Unsupported,
        _ if invalid_argument(error) => NetError::InvalidInput(Detail::new(&format!(
            "the platform rejected an argument: {error}"
        ))),
        _ => os_or(error, NetError::Closed, |code| NetError::Os { code }),
    }
}

/// The [`DiskError`] of a file system operation that failed with `error`. An
/// error with no operating system code is [`DiskError::Io`].
pub(super) fn disk(error: &io::Error) -> DiskError {
    if too_many_open(error) {
        return DiskError::TooManyOpen;
    }
    match error.kind() {
        ErrorKind::NotFound => DiskError::NotFound,
        ErrorKind::AlreadyExists => DiskError::AlreadyExists,
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => DiskError::PermissionDenied,
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => DiskError::NoSpace,
        ErrorKind::IsADirectory => DiskError::IsDirectory,
        ErrorKind::NotADirectory => DiskError::NotDirectory,
        ErrorKind::DirectoryNotEmpty => DiskError::NotEmpty,
        ErrorKind::ResourceBusy => DiskError::Busy,
        ErrorKind::InvalidFilename => DiskError::InvalidPath,
        ErrorKind::Unsupported => DiskError::Unsupported,
        _ if error.raw_os_error() == Some(code::IO) => DiskError::Io,
        _ if invalid_argument(error) => DiskError::InvalidInput(Detail::new(&format!(
            "the platform rejected an argument: {error}"
        ))),
        _ => os_or(error, DiskError::Io, |code| DiskError::Os { code }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_map_to_their_variants() {
        let e = |kind| io::Error::from(kind);
        assert_eq!(net(&e(ErrorKind::AddrInUse)), NetError::AddrInUse);
        assert_eq!(net(&e(ErrorKind::BrokenPipe)), NetError::ConnectionReset);
        assert_eq!(
            net(&e(ErrorKind::HostUnreachable)),
            NetError::HostUnreachable
        );
        assert_eq!(net(&e(ErrorKind::Other)), NetError::Closed);
        assert_eq!(disk(&e(ErrorKind::DirectoryNotEmpty)), DiskError::NotEmpty);
        assert_eq!(disk(&e(ErrorKind::StorageFull)), DiskError::NoSpace);
        assert_eq!(disk(&e(ErrorKind::Other)), DiskError::Io);
    }

    #[test]
    fn codes_without_a_kind_map_to_their_variants() {
        let [first, second] = code::TOO_MANY_OPEN;
        for c in [first, second] {
            assert_eq!(net(&io::Error::from_raw_os_error(c)), NetError::TooManyOpen);
            assert_eq!(
                disk(&io::Error::from_raw_os_error(c)),
                DiskError::TooManyOpen
            );
        }
        assert!(message_too_long(&io::Error::from_raw_os_error(
            code::MESSAGE_TOO_LONG
        )));
        assert_eq!(disk(&io::Error::from_raw_os_error(code::IO)), DiskError::Io);
        assert!(matches!(
            net(&io::Error::from_raw_os_error(code::INVALID)),
            NetError::InvalidInput(_)
        ));
        // A code with no variant of its own keeps its value.
        #[cfg(unix)]
        let other = libc::ELOOP;
        #[cfg(windows)]
        let other = 1_234;
        assert_eq!(
            net(&io::Error::from_raw_os_error(other)),
            NetError::Os { code: other }
        );
    }
}
