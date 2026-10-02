mod pid;

use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;

use serde::Serialize;

pub(crate) use pid::PidBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendKind {
    Pid,
}

#[derive(Debug, Clone)]
pub(crate) struct BackendPaths {
    pub(crate) xedoc_bin: PathBuf,
    pub(crate) socket_path: Option<PathBuf>,
    pub(crate) pid_file: PathBuf,
    pub(crate) update_pid_file: PathBuf,
    pub(crate) environment: Vec<(OsString, OsString)>,
}

pub(crate) fn pid_backend(paths: BackendPaths) -> PidBackend {
    let BackendPaths {
        xedoc_bin,
        socket_path,
        pid_file,
        environment,
        ..
    } = paths;
    match socket_path {
        Some(socket_path) => PidBackend::new_with_socket_and_environment(
            xedoc_bin,
            pid_file,
            socket_path,
            environment,
        ),
        None => PidBackend::new_with_environment(xedoc_bin, pid_file, environment),
    }
}

pub(crate) fn pid_update_loop_backend(paths: BackendPaths) -> PidBackend {
    PidBackend::new_update_loop(paths.xedoc_bin, paths.update_pid_file)
}

pub(crate) async fn append_stderr_log_tail_context(pid_file: &Path, context: &mut String) {
    match pid::read_stderr_log_tail(pid_file).await {
        Ok(Some(tail)) => tail.append_to_context(context),
        Ok(None) => {}
        Err(err) => {
            context.push_str(&format!(
                "\n\nFailed to read managed app-server stderr log: {err:#}"
            ));
        }
    }
}
