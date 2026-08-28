use crate::shell::Shell;
use crate::shell_snapshot::ShellSnapshotFile;
use codex_exec_server::Environment;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;
use std::sync::Arc;

pub type ShellSnapshotTask = Shared<BoxFuture<'static, Option<Arc<ShellSnapshotFile>>>>;

#[derive(Clone)]
pub struct TurnEnvironment {
    pub environment_id: String,
    pub environment: Arc<Environment>,
    cwd: PathUri,
    workspace_roots: Vec<PathUri>,
    pub shell: Option<Shell>,
    pub shell_snapshot: ShellSnapshotTask,
}

impl TurnEnvironment {
    pub fn new(
        environment_id: String,
        environment: Arc<Environment>,
        cwd: PathUri,
        workspace_roots: Vec<PathUri>,
        shell: Option<Shell>,
    ) -> Self {
        Self {
            environment_id,
            environment,
            cwd,
            workspace_roots,
            shell,
            shell_snapshot: futures::future::ready(None).boxed().shared(),
        }
    }

    pub fn shell_snapshot(&self, cwd: &AbsolutePathBuf) -> Option<AbsolutePathBuf> {
        if self.cwd != PathUri::from_abs_path(cwd) {
            return None;
        }
        self.shell_snapshot
            .peek()?
            .as_deref()
            .map(ShellSnapshotFile::path)
    }

    pub fn cwd(&self) -> &PathUri {
        &self.cwd
    }

    pub fn workspace_roots(&self) -> &[PathUri] {
        &self.workspace_roots
    }

    pub fn selection(&self) -> TurnEnvironmentSelection {
        TurnEnvironmentSelection {
            environment_id: self.environment_id.clone(),
            cwd: self.cwd.clone(),
            workspace_roots: self.workspace_roots.clone(),
        }
    }
}

impl std::fmt::Debug for TurnEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnEnvironment")
            .field("environment_id", &self.environment_id)
            .field("environment", &self.environment)
            .field("cwd", &self.cwd)
            .field("workspace_roots", &self.workspace_roots)
            .field("shell", &self.shell)
            .finish_non_exhaustive()
    }
}
