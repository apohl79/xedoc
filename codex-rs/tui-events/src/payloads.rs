use std::collections::HashMap;
use std::path::PathBuf;

use codex_app_server_protocol::AdditionalPermissionProfile;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::NetworkApprovalContext;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::Turn;
use codex_protocol::ThreadId;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::user_input::TextElement;
use codex_tui_input::LocalImageAttachment;
use codex_tui_render::diff_model::FileChange;
use codex_tui_transcript::session_state::ThreadSessionState;
use codex_utils_absolute_path::AbsolutePathBuf;

#[derive(Debug)]
pub struct AppServerStartedThread {
    pub session: ThreadSessionState,
    pub turns: Vec<Turn>,
    pub blocks_direct_input: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GoalDraft {
    pub objective: String,
    pub text_elements: Vec<TextElement>,
    pub pending_pastes: Vec<(String, String)>,
    pub local_images: Vec<LocalImageAttachment>,
    pub remote_image_urls: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookTrustUpdate {
    pub key: String,
    pub current_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAppServerRequest {
    ExecApproval {
        id: String,
    },
    FileChangeApproval {
        id: String,
    },
    PermissionsApproval {
        id: String,
    },
    UserInput {
        call_id: String,
    },
    McpElicitation {
        server_name: String,
        request_id: RequestId,
    },
}

/// Request coming from the agent that needs user approval.
#[derive(Clone, Debug)]
pub enum ApprovalRequest {
    Exec(ExecApprovalRequest),
    Permissions(PermissionsApprovalRequest),
    ApplyPatch(ApplyPatchApprovalRequest),
    McpElicitation(McpElicitationApprovalRequest),
}

#[derive(Clone, Debug)]
pub struct ExecApprovalRequest {
    pub thread_id: ThreadId,
    pub thread_label: Option<String>,
    pub id: String,
    pub environment_id: Option<String>,
    pub command: Vec<String>,
    pub reason: Option<String>,
    pub available_decisions: Vec<CommandExecutionApprovalDecision>,
    pub network_approval_context: Option<NetworkApprovalContext>,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
}

#[derive(Clone, Debug)]
pub struct PermissionsApprovalRequest {
    pub thread_id: ThreadId,
    pub thread_label: Option<String>,
    pub call_id: String,
    pub environment_id: Option<String>,
    pub reason: Option<String>,
    pub permissions: RequestPermissionProfile,
}

#[derive(Clone, Debug)]
pub struct ApplyPatchApprovalRequest {
    pub thread_id: ThreadId,
    pub thread_label: Option<String>,
    pub id: String,
    pub reason: Option<String>,
    pub cwd: AbsolutePathBuf,
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Clone, Debug)]
pub struct McpElicitationApprovalRequest {
    pub thread_id: ThreadId,
    pub thread_label: Option<String>,
    pub server_name: String,
    pub request_id: RequestId,
    pub message: String,
}

/// Additions and deletions between `HEAD` and a branch comparison base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitBranchDiffStats {
    pub additions: u64,
    pub deletions: u64,
}

/// Combined git metadata cached by the status line for one working directory.
#[derive(Clone, Debug, Default)]
pub struct StatusLineGitSummary {
    pub pull_request: Option<StatusLinePullRequest>,
    pub branch_change_stats: Option<GitBranchDiffStats>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusLinePullRequest {
    pub number: u64,
    pub url: String,
}
