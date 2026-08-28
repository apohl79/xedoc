//! Canonical TUI session state shared across app-server routing, chat display, and status UI.
//!
//! The app-server API is the boundary for session lifecycle events. Once those responses enter
//! TUI, this module holds the small internal state shape used by app orchestration and widgets.

use std::path::PathBuf;

use codex_app_server_protocol::AskForApproval;
use codex_protocol::ThreadId;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::Personality;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::PermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;

#[derive(Debug, Clone, PartialEq)]
pub struct SessionNetworkProxyRuntime {
    pub http_addr: String,
    pub socks_addr: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MessageHistoryMetadata {
    pub log_id: u64,
    pub entry_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSessionState {
    pub thread_id: ThreadId,
    pub forked_from_id: Option<ThreadId>,
    pub fork_parent_title: Option<String>,
    pub thread_name: Option<String>,
    pub model: String,
    pub model_provider_id: String,
    pub service_tier: Option<String>,
    pub approval_policy: AskForApproval,
    pub approvals_reviewer: codex_protocol::config_types::ApprovalsReviewer,
    /// Permission snapshot used by TUI display surfaces. Legacy app-server
    /// responses are converted to a profile at ingestion time using the
    /// response cwd so cached sessions do not reinterpret cwd-bound grants.
    /// Turn requests must not treat this snapshot as a local permission
    /// override unless the user explicitly changed permissions in the TUI.
    pub permission_profile: PermissionProfile,
    /// Named or implicit built-in profile that produced `permission_profile`,
    /// when the server knows it.
    pub active_permission_profile: Option<ActivePermissionProfile>,
    pub cwd: AbsolutePathBuf,
    pub runtime_workspace_roots: Vec<AbsolutePathBuf>,
    pub instruction_source_paths: Vec<PathUri>,
    pub reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    pub collaboration_mode: Option<Box<CollaborationMode>>,
    pub personality: Option<Personality>,
    pub message_history: Option<MessageHistoryMetadata>,
    pub network_proxy: Option<SessionNetworkProxyRuntime>,
    pub rollout_path: Option<PathBuf>,
}

impl ThreadSessionState {
    pub fn set_cwd_retargeting_implicit_runtime_workspace_root(&mut self, cwd: AbsolutePathBuf) {
        let previous_cwd = std::mem::replace(&mut self.cwd, cwd.clone());
        if !self.runtime_workspace_roots.contains(&previous_cwd) {
            return;
        }

        let previous_roots = std::mem::take(&mut self.runtime_workspace_roots);
        self.runtime_workspace_roots.push(cwd);
        for root in previous_roots {
            if root != previous_cwd && !self.runtime_workspace_roots.contains(&root) {
                self.runtime_workspace_roots.push(root);
            }
        }
    }
}
