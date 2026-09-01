use std::path::PathBuf;

use xedoc_utils_absolute_path::AbsolutePathBuf;

/// Runtime paths needed by exec-server child processes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecServerRuntimePaths {
    /// Stable path to the Xedoc executable used to launch hidden helper modes.
    pub xedoc_self_exe: AbsolutePathBuf,
    /// Path to the Linux sandbox helper alias used when the platform sandbox
    /// needs to re-enter Xedoc by argv0.
    pub xedoc_linux_sandbox_exe: Option<AbsolutePathBuf>,
}

impl ExecServerRuntimePaths {
    pub fn from_optional_paths(
        xedoc_self_exe: Option<PathBuf>,
        xedoc_linux_sandbox_exe: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        let xedoc_self_exe = xedoc_self_exe.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Xedoc executable path is not configured",
            )
        })?;
        Self::new(xedoc_self_exe, xedoc_linux_sandbox_exe)
    }

    pub fn new(
        xedoc_self_exe: PathBuf,
        xedoc_linux_sandbox_exe: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            xedoc_self_exe: absolute_path(xedoc_self_exe)?,
            xedoc_linux_sandbox_exe: xedoc_linux_sandbox_exe.map(absolute_path).transpose()?,
        })
    }
}

fn absolute_path(path: PathBuf) -> std::io::Result<AbsolutePathBuf> {
    AbsolutePathBuf::from_absolute_path(path.as_path())
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))
}
