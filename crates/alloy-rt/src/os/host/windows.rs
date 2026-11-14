//! `host` under `os` on Windows (ARFC 0005 clauses 3.1.18.1 to 3.1.18.4):
//! `RtlGetVersion`, `IsWow64Process2`, the process token's elevation,
//! `GetActiveProcessorCount`, `GlobalMemoryStatusEx`,
//! `IsProcessorFeaturePresent`, `GetDiskFreeSpaceExW`, and the service
//! control manager for the service `alloyd`.

use std::ptr;
use std::time::Instant;

use alloy_seam::Detail;
use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::System::Services::{
    ChangeServiceConfigW, CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW,
    QUERY_SERVICE_CONFIGW, QueryServiceConfigW, QueryServiceStatusEx, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG,
    SERVICE_CONTROL_STOP, SERVICE_DISABLED, SERVICE_NO_CHANGE, SERVICE_QUERY_CONFIG,
    SERVICE_QUERY_STATUS, SERVICE_START, SERVICE_START_PENDING, SERVICE_STATUS,
    SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED, StartServiceW,
};
use windows_sys::Win32::System::SystemInformation::{
    GlobalMemoryStatusEx, IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64,
    MEMORYSTATUSEX, OSVERSIONINFOW,
};
use windows_sys::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetCurrentProcess, IsProcessorFeaturePresent,
    IsWow64Process2, OpenProcessToken, PF_VIRT_FIRMWARE_ENABLED,
};

use super::parse::MAX_PATH;
use super::{Manager, ServiceAction};
use crate::{Arch, HostError, HostFacts, Platform, RunState, ServiceStatus};

/// The `Os` code of a call cut short because the runtime stopped:
/// `ERROR_IO_DEVICE`.
pub(super) const STOPPED: i32 = 1_117;

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_NAME: u32 = 123;
const ERROR_DIRECTORY: u32 = 267;
const ERROR_SERVICE_ALREADY_RUNNING: u32 = 1_056;
const ERROR_SERVICE_DOES_NOT_EXIST: u32 = 1_060;
const ERROR_SERVICE_NOT_ACTIVE: u32 = 1_062;

/// The service, the only one any call names: `alloyd`, NUL-terminated.
const SERVICE: [u16; 7] = [
    b'a' as u16,
    b'l' as u16,
    b'l' as u16,
    b'o' as u16,
    b'y' as u16,
    b'd' as u16,
    0,
];

fn code(error: u32) -> i32 {
    i32::from_ne_bytes(error.to_ne_bytes())
}

#[allow(unsafe_code, reason = "GetLastError has no safe binding")]
fn last_error() -> u32 {
    // SAFETY: GetLastError reads the calling thread's last error code.
    unsafe { GetLastError() }
}

/// The error of a failed service control manager call.
fn scm_error(error: u32) -> HostError {
    match error {
        ERROR_SERVICE_DOES_NOT_EXIST => HostError::NotInstalled,
        ERROR_ACCESS_DENIED => HostError::PermissionDenied,
        other => HostError::Os { code: code(other) },
    }
}

#[allow(unsafe_code, reason = "RtlGetVersion has no safe binding")]
fn version() -> Result<Platform, HostError> {
    let mut info = OSVERSIONINFOW {
        // The struct is a few hundred bytes, far below `u32::MAX`.
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..OSVERSIONINFOW::default()
    };
    // SAFETY: `info` is a valid, writable `OSVERSIONINFOW` whose size field
    // is set, as the call requires.
    let status = unsafe { RtlGetVersion(&raw mut info) };
    if status != 0 {
        return Err(HostError::Os { code: status });
    }
    Ok(Platform::Windows {
        major: info.dwMajorVersion,
        minor: info.dwMinorVersion,
        build: info.dwBuildNumber,
    })
}

#[allow(unsafe_code, reason = "IsWow64Process2 has no safe binding")]
fn native_machine() -> Result<Arch, HostError> {
    let mut process: IMAGE_FILE_MACHINE = 0;
    let mut native: IMAGE_FILE_MACHINE = 0;
    // SAFETY: the pseudo handle of the current process needs no closing, and
    // both out pointers are valid and writable for the call.
    let ok = unsafe { IsWow64Process2(GetCurrentProcess(), &raw mut process, &raw mut native) };
    if ok == 0 {
        return Err(HostError::Os {
            code: code(last_error()),
        });
    }
    Ok(match native {
        IMAGE_FILE_MACHINE_AMD64 => Arch::X86_64,
        IMAGE_FILE_MACHINE_ARM64 => Arch::Aarch64,
        other => Arch::Other(format!("0x{other:04x}")),
    })
}

/// Whether the process token is elevated, or the error of reading it.
#[allow(unsafe_code, reason = "the token calls have no safe binding")]
fn elevated() -> Result<bool, HostError> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo handle of the current process needs no closing, and
    // `token` is valid and writable for the call.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(HostError::Os {
            code: code(last_error()),
        });
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut written = 0_u32;
    // SAFETY: `token` was opened above with `TOKEN_QUERY`, and the buffer is
    // a valid, writable `TOKEN_ELEVATION` of the size passed.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut written,
        )
    };
    let error = last_error();
    // SAFETY: `token` is a handle this function opened and closes once.
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return Err(HostError::Os { code: code(error) });
    }
    Ok(elevation.TokenIsElevated != 0)
}

#[allow(
    unsafe_code,
    reason = "the system information calls have no safe binding"
)]
fn cores_memory_virtualization() -> Result<(u32, u64, bool), HostError> {
    // SAFETY: GetActiveProcessorCount reads a count and touches no memory of
    // ours.
    let cores = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }.max(1);
    let mut memory = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..MEMORYSTATUSEX::default()
    };
    // SAFETY: `memory` is a valid, writable `MEMORYSTATUSEX` whose length
    // field is set, as the call requires.
    if unsafe { GlobalMemoryStatusEx(&raw mut memory) } == 0 {
        return Err(HostError::Os {
            code: code(last_error()),
        });
    }
    // SAFETY: IsProcessorFeaturePresent reads a flag and touches no memory of
    // ours.
    let virtualization = unsafe { IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED) } != 0;
    Ok((cores, memory.ullTotalPhys, virtualization))
}

pub(super) fn facts() -> Result<HostFacts, HostError> {
    let platform = version()?;
    let arch = native_machine()?;
    let privileged = elevated()?;
    let (cores, memory, virtualization) = cores_memory_virtualization()?;
    Ok(HostFacts {
        platform,
        arch,
        privileged,
        cores,
        memory,
        virtualization,
        disks: Vec::new(),
    })
}

/// Whether `path` is a letter, `:`, and `\` followed by components separated
/// by `\`, at most 4,096 bytes.
fn drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if path.len() > MAX_PATH || bytes.len() < 3 || path.contains('\0') {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return false;
    }
    let rest = path[3..].strip_suffix('\\').unwrap_or(&path[3..]);
    rest.is_empty() || rest.split('\\').all(|c| !c.is_empty() && !c.contains('/'))
}

#[allow(unsafe_code, reason = "GetDiskFreeSpaceExW has no safe binding")]
fn disk_free(path: &str) -> Result<u64, u32> {
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut free = 0_u64;
    // SAFETY: `wide` is NUL-terminated, `free` is valid and writable, and
    // the two other out pointers may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut free,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if ok == 0 { Err(last_error()) } else { Ok(free) }
}

pub(super) fn free_space(path: &str) -> Result<u64, HostError> {
    if !drive_path(path) {
        return Err(HostError::InvalidInput(Detail::new(
            "free_space takes a path of a drive letter, `:\\`, and names separated by `\\`, at most 4,096 bytes",
        )));
    }
    let mut at = path;
    loop {
        match disk_free(at) {
            Ok(free) => return Ok(free),
            Err(
                ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_INVALID_NAME | ERROR_DIRECTORY,
            ) if super::parse::parent(at, '\\', 3).is_some() => {
                // Checked just above.
                at = super::parse::parent(at, '\\', 3).unwrap_or(at);
            }
            Err(error) => return Err(HostError::Os { code: code(error) }),
        }
    }
}

/// A service control manager handle, closed when dropped.
struct Handle(SC_HANDLE);

impl Drop for Handle {
    #[allow(unsafe_code, reason = "CloseServiceHandle has no safe binding")]
    fn drop(&mut self) {
        // SAFETY: the handle was returned by OpenSCManagerW or OpenServiceW
        // and is closed only here.
        unsafe { CloseServiceHandle(self.0) };
    }
}

/// The service `alloyd`, opened for the query rights and `extra`.
#[allow(
    unsafe_code,
    reason = "the service control manager has no safe binding"
)]
fn open(extra: u32) -> Result<(Handle, Handle), HostError> {
    // SAFETY: null machine and database names name the local service
    // control manager's active database.
    let manager = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(scm_error(last_error()));
    }
    let manager = Handle(manager);
    let rights = SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS | extra;
    // SAFETY: `manager` is open and `SERVICE` is NUL-terminated.
    let service = unsafe { OpenServiceW(manager.0, SERVICE.as_ptr(), rights) };
    if service.is_null() {
        return Err(scm_error(last_error()));
    }
    Ok((manager, Handle(service)))
}

#[allow(unsafe_code, reason = "QueryServiceConfigW has no safe binding")]
fn start_type(service: &Handle) -> Result<u32, HostError> {
    let mut needed = 0_u32;
    // SAFETY: a null buffer of size 0 asks only for the size needed.
    let ok = unsafe { QueryServiceConfigW(service.0, ptr::null_mut(), 0, &raw mut needed) };
    let error = last_error();
    if ok == 0 && error != ERROR_INSUFFICIENT_BUFFER {
        return Err(scm_error(error));
    }
    // 8-byte units, so the buffer is aligned for QUERY_SERVICE_CONFIGW.
    let units = (needed as usize)
        .div_ceil(8)
        .max(size_of::<QUERY_SERVICE_CONFIGW>() / 8 + 1);
    let mut buffer = vec![0_u64; units];
    let size = u32::try_from(units * 8).unwrap_or(u32::MAX);
    // SAFETY: the buffer is aligned and `size` bytes long.
    let ok = unsafe {
        QueryServiceConfigW(service.0, buffer.as_mut_ptr().cast(), size, &raw mut needed)
    };
    if ok == 0 {
        return Err(scm_error(last_error()));
    }
    // SAFETY: the call succeeded, so the buffer starts with an initialized
    // QUERY_SERVICE_CONFIGW, and it is aligned for it.
    let config = unsafe { &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    Ok(config.dwStartType)
}

#[allow(unsafe_code, reason = "QueryServiceStatusEx has no safe binding")]
fn process_status(service: &Handle) -> Result<SERVICE_STATUS_PROCESS, HostError> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0_u32;
    // SAFETY: `status` is a valid, writable SERVICE_STATUS_PROCESS of the
    // size passed.
    let ok = unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&raw mut status).cast(),
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &raw mut needed,
        )
    };
    if ok == 0 {
        return Err(scm_error(last_error()));
    }
    Ok(status)
}

fn run_state(state: u32) -> RunState {
    match state {
        SERVICE_STOPPED => RunState::Stopped,
        SERVICE_START_PENDING => RunState::Starting,
        SERVICE_STOP_PENDING => RunState::Stopping,
        _ => RunState::Running,
    }
}

pub(super) fn status(deadline: Instant) -> Result<ServiceStatus, HostError> {
    Service::status(deadline)
}

/// The service control manager.
pub(super) struct Service;

impl Manager for Service {
    const POLLS_START: bool = true;

    fn privileged() -> bool {
        elevated().unwrap_or(false)
    }

    fn status(_deadline: Instant) -> Result<ServiceStatus, HostError> {
        let (_manager, service) = open(0)?;
        let start = start_type(&service)?;
        let status = process_status(&service)?;
        Ok(ServiceStatus {
            enabled: start == SERVICE_AUTO_START,
            run: run_state(status.dwCurrentState),
        })
    }

    #[allow(unsafe_code, reason = "the service control calls have no safe binding")]
    fn request(action: ServiceAction, _deadline: Instant) -> Result<(), HostError> {
        let extra = match action {
            ServiceAction::Enable | ServiceAction::Disable => SERVICE_CHANGE_CONFIG,
            ServiceAction::Start => SERVICE_START,
            ServiceAction::Stop => SERVICE_STOP,
        };
        let (_manager, service) = open(extra)?;
        let ok = match action {
            ServiceAction::Enable | ServiceAction::Disable => {
                let start = if action == ServiceAction::Enable {
                    SERVICE_AUTO_START
                } else {
                    SERVICE_DISABLED
                };
                // SAFETY: `service` is open with SERVICE_CHANGE_CONFIG; every
                // other argument is SERVICE_NO_CHANGE or null, which change
                // nothing.
                unsafe {
                    ChangeServiceConfigW(
                        service.0,
                        SERVICE_NO_CHANGE,
                        start,
                        SERVICE_NO_CHANGE,
                        ptr::null(),
                        ptr::null(),
                        ptr::null_mut(),
                        ptr::null(),
                        ptr::null(),
                        ptr::null(),
                        ptr::null(),
                    )
                }
            }
            // SAFETY: `service` is open with SERVICE_START; no arguments are
            // passed.
            ServiceAction::Start => unsafe { StartServiceW(service.0, 0, ptr::null()) },
            ServiceAction::Stop => {
                let mut status = SERVICE_STATUS::default();
                // SAFETY: `service` is open with SERVICE_STOP and `status`
                // is valid and writable.
                unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &raw mut status) }
            }
        };
        if ok != 0 {
            return Ok(());
        }
        match (action, last_error()) {
            (ServiceAction::Start, ERROR_SERVICE_ALREADY_RUNNING)
            | (ServiceAction::Stop, ERROR_SERVICE_NOT_ACTIVE) => Ok(()),
            (_, error) => Err(scm_error(error)),
        }
    }

    fn stop_reason() -> Option<String> {
        let (_manager, service) = open(0).ok()?;
        let status = process_status(&service).ok()?;
        Some(format!(
            "alloyd stopped with exit code {}",
            status.dwWin32ExitCode
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_paths_follow_clause_3_1_18_2() {
        for good in [
            r"C:\",
            r"C:\ProgramData",
            r"c:\ProgramData\Alloy\",
            r"D:\a b\c",
        ] {
            assert!(drive_path(good), "{good}");
        }
        for bad in [
            "",
            "C:",
            "C:/x",
            r"\\server\share",
            r"C:\a\\b",
            r"1:\x",
            "relative",
        ] {
            assert!(!drive_path(bad), "{bad}");
        }
        assert!(!drive_path(&format!(r"C:\{}", "a".repeat(4_094))));
    }
}
