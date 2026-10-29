//! Protect the inline viewport from unmanaged macOS writes to stderr.
//!
//! Some macOS frameworks and runtime diagnostics write directly to file
//! descriptor 2. While the inline TUI is active, those writes paint into the
//! same terminal region as the composer without going through the renderer.
//! Keep them off the terminal until the TUI releases terminal ownership.

use std::io;

#[cfg(target_os = "macos")]
use std::fs::OpenOptions;
#[cfg(target_os = "macos")]
use std::io::IsTerminal;
#[cfg(target_os = "macos")]
use std::mem::MaybeUninit;
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "macos")]
use std::os::fd::FromRawFd;
#[cfg(target_os = "macos")]
use std::os::fd::OwnedFd;
#[cfg(target_os = "macos")]
use std::sync::Mutex;
#[cfg(target_os = "macos")]
use std::sync::MutexGuard;

#[cfg(target_os = "macos")]
static STDERR_STATE: Mutex<StderrState> = Mutex::new(StderrState {
    owner_active: false,
    saved_stderr: None,
});

#[cfg(target_os = "macos")]
struct StderrState {
    owner_active: bool,
    saved_stderr: Option<OwnedFd>,
}

/// Keeps unmanaged stderr output away from the terminal while the TUI owns it.
pub(crate) struct TerminalStderrGuard {
    active: bool,
}

impl TerminalStderrGuard {
    pub(super) fn install() -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            if stderr_targets_stdout_terminal() {
                return Self::install_suppression();
            }
        }

        Ok(Self { active: false })
    }

    #[cfg(target_os = "macos")]
    fn install_suppression() -> io::Result<Self> {
        let mut state = lock_state()?;
        if state.owner_active {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "terminal stderr suppression is already active",
            ));
        }
        suppress_locked(&mut state)?;
        state.owner_active = true;
        Ok(Self { active: true })
    }
}

impl Drop for TerminalStderrGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = finish();
            self.active = false;
        }
    }
}

/// Restores stderr while terminal ownership is temporarily released.
pub(super) fn pause() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut state = lock_state()?;
        if state.owner_active {
            restore_locked(&mut state)?;
        }
    }

    Ok(())
}

/// Suppresses stderr again when terminal ownership returns to the TUI.
pub(super) fn resume() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut state = lock_state()?;
        if state.owner_active {
            suppress_locked(&mut state)?;
        }
    }

    Ok(())
}

/// Restores stderr permanently when the TUI session ends.
pub(super) fn finish() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut state = lock_state()?;
        if state.owner_active {
            restore_locked(&mut state)?;
            state.owner_active = false;
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn lock_state() -> io::Result<MutexGuard<'static, StderrState>> {
    STDERR_STATE
        .lock()
        .map_err(|_| io::Error::other("terminal stderr suppression lock poisoned"))
}

#[cfg(target_os = "macos")]
fn stderr_targets_stdout_terminal() -> bool {
    if !io::stdout().is_terminal() || !io::stderr().is_terminal() {
        return false;
    }

    let mut stdout_stat = MaybeUninit::<libc::stat>::uninit();
    let mut stderr_stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: both output pointers reference valid storage for libc to initialize.
    if unsafe {
        libc::fstat(libc::STDOUT_FILENO, stdout_stat.as_mut_ptr()) != 0
            || libc::fstat(libc::STDERR_FILENO, stderr_stat.as_mut_ptr()) != 0
    } {
        return false;
    }
    // SAFETY: both fstat calls above returned successfully.
    let (stdout_stat, stderr_stat) =
        unsafe { (stdout_stat.assume_init(), stderr_stat.assume_init()) };
    stdout_stat.st_dev == stderr_stat.st_dev && stdout_stat.st_ino == stderr_stat.st_ino
}

#[cfg(target_os = "macos")]
fn suppress_locked(state: &mut StderrState) -> io::Result<()> {
    if state.saved_stderr.is_some() {
        return Ok(());
    }

    // SAFETY: dup returns a newly owned file descriptor on success.
    let saved_stderr = unsafe { libc::dup(libc::STDERR_FILENO) };
    if saved_stderr == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: saved_stderr is a fresh descriptor returned by dup above.
    let saved_stderr = unsafe { OwnedFd::from_raw_fd(saved_stderr) };
    let devnull = OpenOptions::new().write(true).open("/dev/null")?;
    // SAFETY: both descriptors are valid for the duration of this call.
    if unsafe { libc::dup2(devnull.as_raw_fd(), libc::STDERR_FILENO) } == -1 {
        return Err(io::Error::last_os_error());
    }
    state.saved_stderr = Some(saved_stderr);
    Ok(())
}

#[cfg(target_os = "macos")]
fn restore_locked(state: &mut StderrState) -> io::Result<()> {
    let Some(saved_stderr) = state.saved_stderr.as_ref() else {
        return Ok(());
    };

    // SAFETY: saved_stderr was duplicated from stderr and remains owned here.
    if unsafe { libc::dup2(saved_stderr.as_raw_fd(), libc::STDERR_FILENO) } == -1 {
        return Err(io::Error::last_os_error());
    }
    state.saved_stderr = None;
    Ok(())
}
