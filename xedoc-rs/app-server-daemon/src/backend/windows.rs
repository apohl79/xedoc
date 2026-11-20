//! Win32 process and file-lock primitives for the pid-managed backend.

use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER;
use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Foundation::STILL_ACTIVE;
use windows_sys::Win32::Foundation::SetHandleInformation;
use windows_sys::Win32::Storage::FileSystem::LOCKFILE_EXCLUSIVE_LOCK;
use windows_sys::Win32::Storage::FileSystem::LOCKFILE_FAIL_IMMEDIATELY;
use windows_sys::Win32::Storage::FileSystem::LockFileEx;
use windows_sys::Win32::System::Console::GetStdHandle;
use windows_sys::Win32::System::Console::STD_ERROR_HANDLE;
use windows_sys::Win32::System::Console::STD_INPUT_HANDLE;
use windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE;
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::System::Threading::GetExitCodeProcess;
use windows_sys::Win32::System::Threading::GetProcessTimes;
use windows_sys::Win32::System::Threading::OpenProcess;
use windows_sys::Win32::System::Threading::PROCESS_ACCESS_RIGHTS;
use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
use windows_sys::Win32::System::Threading::PROCESS_TERMINATE;
use windows_sys::Win32::System::Threading::TerminateProcess;

/// Creation flags for background processes: a hidden console that console
/// children inherit (so tool shells do not open windows), detached from the
/// launching console's Ctrl+C.
pub(crate) const DETACHED_CREATION_FLAGS: u32 = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;

/// Stops this process's own standard handles from leaking into detached
/// children. CreateProcess inherits every inheritable handle, so a background
/// server would otherwise keep the caller's stdout pipe open and block
/// whoever is reading this command's output.
pub(crate) fn disinherit_std_handles() {
    for std_handle in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = unsafe { GetStdHandle(std_handle) };
        if handle != 0 && handle != INVALID_HANDLE_VALUE {
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, /*dwflags*/ 0)
            };
        }
    }
}

/// Takes a non-blocking exclusive lock that is released when the handle closes.
pub(crate) fn try_lock_file(file: &impl AsRawHandle) -> std::io::Result<bool> {
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    let locked = unsafe {
        LockFileEx(
            file.as_raw_handle() as HANDLE,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            /*dwreserved*/ 0,
            /*nnumberofbytestolocklow*/ u32::MAX,
            /*nnumberofbytestolockhigh*/ u32::MAX,
            &mut overlapped,
        )
    };
    if locked != 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        return Ok(false);
    }
    Err(err)
}

struct ProcessHandle(HANDLE);

impl ProcessHandle {
    /// Returns `None` when no process with this pid exists.
    fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> std::io::Result<Option<Self>> {
        let handle = unsafe {
            OpenProcess(access, /*binherithandle*/ 0, pid)
        };
        if handle != 0 {
            return Ok(Some(Self(handle)));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(None);
        }
        Err(err)
    }

    fn is_running(&self) -> std::io::Result<bool> {
        let mut exit_code = 0u32;
        if unsafe { GetExitCodeProcess(self.0, &mut exit_code) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(exit_code == STILL_ACTIVE as u32)
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Returns the creation time of a running process in FILETIME ticks, or `None`
/// when the process does not exist or has exited.
pub(crate) fn process_start_time(pid: u32) -> std::io::Result<Option<String>> {
    let Some(process) = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)? else {
        return Ok(None);
    };
    if !process.is_running()? {
        return Ok(None);
    }
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    Ok(Some(ticks.to_string()))
}

/// Terminates a process immediately. Windows has no SIGTERM equivalent for a
/// process that does not share the caller's console.
pub(crate) fn terminate_process(pid: u32) -> std::io::Result<()> {
    let Some(process) = ProcessHandle::open(pid, PROCESS_TERMINATE)? else {
        return Ok(());
    };
    if unsafe {
        TerminateProcess(process.0, /*uexitcode*/ 1)
    } != 0
    {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // Access is denied when the process is already exiting.
    if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
        return Ok(());
    }
    Err(err)
}

/// Terminates a process only if its creation time still matches the pid record.
pub(crate) fn terminate_process_if_matches(
    pid: u32,
    expected_start_time: &str,
) -> std::io::Result<()> {
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE;
    let Some(process) = ProcessHandle::open(pid, access)? else {
        return Ok(());
    };
    if !process.is_running()? {
        return Ok(());
    }
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    if ticks.to_string() != expected_start_time {
        return Ok(());
    }
    if unsafe {
        TerminateProcess(process.0, /*uexitcode*/ 1)
    } != 0
    {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
        return Ok(());
    }
    Err(err)
}
