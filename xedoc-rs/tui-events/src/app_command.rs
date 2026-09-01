use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;
use xedoc_app_server_protocol::AskForApproval;
use xedoc_app_server_protocol::CommandExecutionApprovalDecision;
use xedoc_app_server_protocol::FileChangeApprovalDecision;
use xedoc_app_server_protocol::McpServerElicitationAction;
use xedoc_app_server_protocol::RequestId as AppServerRequestId;
use xedoc_app_server_protocol::ReviewTarget;
use xedoc_app_server_protocol::ToolRequestUserInputResponse;
use xedoc_app_server_protocol::UserInput;
use xedoc_protocol::config_types::CollaborationMode;
use xedoc_protocol::config_types::Personality;
use xedoc_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use xedoc_protocol::models::ActivePermissionProfile;
use xedoc_protocol::models::PermissionProfile;
use xedoc_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use xedoc_protocol::request_permissions::RequestPermissionsResponse;

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AppCommand {
    Interrupt,
    CleanBackgroundTerminals,
    RunUserShellCommand {
        command: String,
    },
    UserTurn {
        items: Vec<UserInput>,
        pending_steer_id: Option<u64>,
        cwd: PathBuf,
        approval_policy: AskForApproval,
        active_permission_profile: Option<ActivePermissionProfile>,
        model: String,
        effort: Option<ReasoningEffortConfig>,
        summary: Option<ReasoningSummaryConfig>,
        service_tier: Option<Option<String>>,
        final_output_json_schema: Option<Value>,
        collaboration_mode: Option<CollaborationMode>,
        personality: Option<Personality>,
    },
    CancelPendingSteer {
        pending_steer_id: u64,
    },
    OverrideTurnContext {
        cwd: Option<PathBuf>,
        approval_policy: Option<AskForApproval>,
        permission_profile: Option<PermissionProfile>,
        active_permission_profile: Option<ActivePermissionProfile>,
        model: Option<String>,
        effort: Option<Option<ReasoningEffortConfig>>,
        summary: Option<ReasoningSummaryConfig>,
        service_tier: Option<Option<String>>,
        collaboration_mode: Option<CollaborationMode>,
        personality: Option<Personality>,
    },
    ExecApproval {
        id: String,
        turn_id: Option<String>,
        decision: CommandExecutionApprovalDecision,
    },
    PatchApproval {
        id: String,
        decision: FileChangeApprovalDecision,
    },
    ResolveElicitation {
        server_name: String,
        request_id: AppServerRequestId,
        decision: McpServerElicitationAction,
        content: Option<Value>,
        meta: Option<Value>,
    },
    UserInputAnswer {
        id: String,
        response: ToolRequestUserInputResponse,
    },
    RequestPermissionsResponse {
        id: String,
        response: RequestPermissionsResponse,
    },
    ReloadUserConfig,
    ListSkills {
        cwds: Vec<PathBuf>,
        force_reload: bool,
    },
    Compact,
    SetThreadName {
        name: String,
    },
    Shutdown,
    Review {
        target: ReviewTarget,
    },
}

impl AppCommand {
    pub fn interrupt() -> Self {
        Self::Interrupt
    }

    pub fn clean_background_terminals() -> Self {
        Self::CleanBackgroundTerminals
    }

    pub fn run_user_shell_command(command: String) -> Self {
        Self::RunUserShellCommand { command }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn user_turn(
        items: Vec<UserInput>,
        cwd: PathBuf,
        approval_policy: AskForApproval,
        active_permission_profile: Option<ActivePermissionProfile>,
        model: String,
        effort: Option<ReasoningEffortConfig>,
        summary: Option<ReasoningSummaryConfig>,
        service_tier: Option<Option<String>>,
        final_output_json_schema: Option<Value>,
        collaboration_mode: Option<CollaborationMode>,
        personality: Option<Personality>,
    ) -> Self {
        Self::UserTurn {
            items,
            pending_steer_id: None,
            cwd,
            approval_policy,
            active_permission_profile,
            model,
            effort,
            summary,
            service_tier,
            final_output_json_schema,
            collaboration_mode,
            personality,
        }
    }

    pub fn with_pending_steer_id(mut self, id: u64) -> Self {
        if let Self::UserTurn {
            pending_steer_id, ..
        } = &mut self
        {
            *pending_steer_id = Some(id);
        }
        self
    }

    pub fn cancel_pending_steer(pending_steer_id: u64) -> Self {
        Self::CancelPendingSteer { pending_steer_id }
    }

    pub fn pending_steer_id(&self) -> Option<u64> {
        match self {
            Self::UserTurn {
                pending_steer_id, ..
            } => *pending_steer_id,
            Self::Interrupt { .. }
            | Self::CancelPendingSteer { .. }
            | Self::CleanBackgroundTerminals
            | Self::RunUserShellCommand { .. }
            | Self::OverrideTurnContext { .. }
            | Self::ExecApproval { .. }
            | Self::PatchApproval { .. }
            | Self::ResolveElicitation { .. }
            | Self::UserInputAnswer { .. }
            | Self::RequestPermissionsResponse { .. }
            | Self::ReloadUserConfig
            | Self::ListSkills { .. }
            | Self::Compact
            | Self::SetThreadName { .. }
            | Self::Shutdown
            | Self::Review { .. } => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn override_turn_context(
        cwd: Option<PathBuf>,
        approval_policy: Option<AskForApproval>,
        permission_profile: Option<PermissionProfile>,
        active_permission_profile: Option<ActivePermissionProfile>,
        model: Option<String>,
        effort: Option<Option<ReasoningEffortConfig>>,
        summary: Option<ReasoningSummaryConfig>,
        service_tier: Option<Option<String>>,
        collaboration_mode: Option<CollaborationMode>,
        personality: Option<Personality>,
    ) -> Self {
        Self::OverrideTurnContext {
            cwd,
            approval_policy,
            permission_profile,
            active_permission_profile,
            model,
            effort,
            summary,
            service_tier,
            collaboration_mode,
            personality,
        }
    }

    pub fn exec_approval(
        id: String,
        turn_id: Option<String>,
        decision: CommandExecutionApprovalDecision,
    ) -> Self {
        Self::ExecApproval {
            id,
            turn_id,
            decision,
        }
    }

    pub fn patch_approval(id: String, decision: FileChangeApprovalDecision) -> Self {
        Self::PatchApproval { id, decision }
    }

    pub fn resolve_elicitation(
        server_name: String,
        request_id: AppServerRequestId,
        decision: McpServerElicitationAction,
        content: Option<Value>,
        meta: Option<Value>,
    ) -> Self {
        Self::ResolveElicitation {
            server_name,
            request_id,
            decision,
            content,
            meta,
        }
    }

    pub fn user_input_answer(id: String, response: ToolRequestUserInputResponse) -> Self {
        Self::UserInputAnswer { id, response }
    }

    pub fn request_permissions_response(id: String, response: RequestPermissionsResponse) -> Self {
        Self::RequestPermissionsResponse { id, response }
    }

    pub fn reload_user_config() -> Self {
        Self::ReloadUserConfig
    }

    pub fn list_skills(cwds: Vec<PathBuf>, force_reload: bool) -> Self {
        Self::ListSkills { cwds, force_reload }
    }

    pub fn compact() -> Self {
        Self::Compact
    }

    pub fn set_thread_name(name: String) -> Self {
        Self::SetThreadName { name }
    }

    #[allow(dead_code)]
    pub fn shutdown() -> Self {
        Self::Shutdown
    }

    pub fn review(target: ReviewTarget) -> Self {
        Self::Review { target }
    }

    pub fn is_review(&self) -> bool {
        matches!(self, Self::Review { .. })
    }
}

impl From<&AppCommand> for AppCommand {
    fn from(value: &AppCommand) -> Self {
        value.clone()
    }
}
