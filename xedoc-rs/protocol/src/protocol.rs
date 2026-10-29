//! Defines the protocol for a Xedoc session between a client and an agent.
//!
//! Uses a SQ (Submission Queue) / EQ (Event Queue) pattern to asynchronously communicate
//! between user and agent.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::ops::Mul;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use strum_macros::EnumIter;

use crate::AgentPath;
use crate::ResponseItemId;
use crate::SessionId;
use crate::ThreadId;
use crate::approvals::ElicitationRequestEvent;
use crate::capabilities::SelectedCapabilityRoot;
use crate::config_types::CollaborationMode;
use crate::config_types::ModeKind;
use crate::config_types::MultiAgentMode;
use crate::config_types::Personality;
use crate::config_types::ReasoningSummary as ReasoningSummaryConfig;
use crate::dynamic_tools::DynamicToolCallOutputContentItem;
use crate::dynamic_tools::DynamicToolCallRequest;
use crate::dynamic_tools::DynamicToolResponse;
use crate::dynamic_tools::DynamicToolSpec;
use crate::items::TurnItem;
use crate::mcp::CallToolResult;
use crate::mcp::RequestId;
use crate::models::ActivePermissionProfile;
use crate::models::AgentMessageInputContent;
use crate::models::BaseInstructions;
use crate::models::ContentItem;
use crate::models::ImageDetail;
use crate::models::InternalChatMessageMetadataPassthrough;
use crate::models::MessagePhase;
use crate::models::PermissionProfile;
use crate::models::ResponseInputItem;
use crate::models::ResponseItem;
use crate::models::SandboxEnforcement;
use crate::models::WebSearchAction;
use crate::num_format::format_with_separators;
use crate::openai_models::ReasoningEffort as ReasoningEffortConfig;
use crate::parse_command::ParsedCommand;
use crate::plan_tool::UpdatePlanArgs;
use crate::request_permissions::RequestPermissionsEvent;
use crate::request_permissions::RequestPermissionsResponse;
use crate::request_user_input::RequestUserInputResponse;
use crate::user_input::UserInput;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error as _;
use serde_json::Value;
use serde_with::serde_as;
use strum_macros::Display;
use tracing::error;
use ts_rs::TS;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_path_uri::PathUri;

pub use crate::approvals::ApplyPatchApprovalRequestEvent;
pub use crate::approvals::ElicitationAction;
pub use crate::approvals::ExecApprovalRequestEvent;
pub use crate::approvals::ExecPolicyAmendment;
pub use crate::approvals::NetworkApprovalContext;
pub use crate::approvals::NetworkApprovalProtocol;
pub use crate::approvals::NetworkPolicyAmendment;
pub use crate::approvals::NetworkPolicyRuleAction;
pub use crate::legacy_events::HasLegacyEvent;
pub use crate::permissions::FileSystemAccessMode;
pub use crate::permissions::FileSystemPath;
pub use crate::permissions::FileSystemSandboxEntry;
pub use crate::permissions::FileSystemSandboxKind;
pub use crate::permissions::FileSystemSandboxPolicy;
pub use crate::permissions::FileSystemSpecialPath;
pub use crate::permissions::NetworkSandboxPolicy;
use crate::permissions::default_read_only_subpaths_for_writable_root;
pub use crate::request_permissions::RequestPermissionsArgs;
pub use crate::request_user_input::RequestUserInputEvent;

/// Open/close tags for special context blocks. Used across crates to avoid duplicated hardcoded
/// strings.
pub const USER_INSTRUCTIONS_OPEN_TAG: &str = "<user_instructions>";
pub const USER_INSTRUCTIONS_CLOSE_TAG: &str = "</user_instructions>";
pub const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
pub const ENVIRONMENT_CONTEXT_CLOSE_TAG: &str = "</environment_context>";
pub const ENVIRONMENTS_INSTRUCTIONS_OPEN_TAG: &str = "<environments_instructions>";
pub const ENVIRONMENTS_INSTRUCTIONS_CLOSE_TAG: &str = "</environments_instructions>";
pub const SKILLS_INSTRUCTIONS_OPEN_TAG: &str = "<skills_instructions>";
pub const SKILLS_INSTRUCTIONS_CLOSE_TAG: &str = "</skills_instructions>";
pub const PLUGINS_INSTRUCTIONS_OPEN_TAG: &str = "<plugins_instructions>";
pub const PLUGINS_INSTRUCTIONS_CLOSE_TAG: &str = "</plugins_instructions>";
pub const COLLABORATION_MODE_OPEN_TAG: &str = "<collaboration_mode>";
pub const COLLABORATION_MODE_CLOSE_TAG: &str = "</collaboration_mode>";
pub const MULTI_AGENT_MODE_OPEN_TAG: &str = "<multi_agent_mode>";
pub const MULTI_AGENT_MODE_CLOSE_TAG: &str = "</multi_agent_mode>";
pub const CONTEXT_WINDOW_OPEN_TAG: &str = "<context_window>";
pub const CONTEXT_WINDOW_CLOSE_TAG: &str = "</context_window>";
pub const CONTEXT_WINDOW_GUIDANCE_OPEN_TAG: &str = "<context_window_guidance>";
pub const CONTEXT_WINDOW_GUIDANCE_CLOSE_TAG: &str = "</context_window_guidance>";
pub const USER_MESSAGE_BEGIN: &str = "## My request for Xedoc:";

/// Removes the model-context prefix from a user message before displaying it.
pub fn strip_user_message_prefix(text: &str) -> &str {
    match text.find(USER_MESSAGE_BEGIN) {
        Some(idx) => text[idx + USER_MESSAGE_BEGIN.len()..].trim(),
        None => text.trim(),
    }
}

// TODO(anp): Replace `TurnEnvironmentSelection` with `PathUri` once path URIs carry environment
// identifiers.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnEnvironmentSelection {
    pub environment_id: String,
    pub cwd: PathUri,
    pub workspace_roots: Vec<PathUri>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnEnvironmentSelections {
    pub legacy_fallback_cwd: AbsolutePathBuf,
    pub environments: Vec<TurnEnvironmentSelection>,
}

impl TurnEnvironmentSelections {
    pub fn new(
        legacy_fallback_cwd: AbsolutePathBuf,
        environments: Vec<TurnEnvironmentSelection>,
    ) -> Self {
        Self {
            legacy_fallback_cwd,
            environments,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
#[serde(transparent)]
#[ts(type = "string")]
pub struct GitSha(pub String);

impl GitSha {
    pub fn new(sha: &str) -> Self {
        Self(sha.to_string())
    }
}

/// Submission Queue Entry - requests from user
#[derive(Debug, Clone)]
pub struct Submission {
    /// Unique id for this Submission to correlate with Events
    pub id: String,
    /// Payload
    pub op: Op,
    /// Client-provided id for the user message represented by `Op::UserInput`.
    pub client_user_message_id: Option<String>,
    /// Optional W3C trace carrier propagated across async submission handoffs.
    pub trace: Option<W3cTraceContext>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct W3cTraceContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tracestate: Option<String>,
}

/// Config payload for refreshing MCP servers.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerRefreshConfig {
    /// Complete runtime server map after source and thread-scoped resolution.
    pub mcp_servers: Value,
    /// OAuth credential store mode to use with this server snapshot.
    pub mcp_oauth_credentials_store_mode: Value,
    pub auth_keyring_backend_kind: Value,
}

/// Persistent thread-settings overrides that can be applied before user input or
/// on their own.
#[derive(Clone, Default, PartialEq)]
pub struct ThreadSettingsOverrides {
    /// Updated fallback `cwd` and environments supplied together as a complete pair.
    pub environments: Option<TurnEnvironmentSelections>,

    /// Replacement environment source for subsequent tool calls.
    pub environment_variables: Option<BTreeMap<String, String>>,

    /// Updated profile-defined workspace roots for status summaries and
    /// per-turn config reconstruction.
    pub profile_workspace_roots: Option<Vec<AbsolutePathBuf>>,

    /// Updated command approval policy.
    pub approval_policy: Option<AskForApproval>,

    /// Updated sandbox policy for tool calls.
    pub sandbox_policy: Option<SandboxPolicy>,

    /// Updated permissions profile for tool calls.
    pub permission_profile: Option<PermissionProfile>,

    /// Named or built-in profile that produced `permission_profile`, if the
    /// update selected a profile rather than supplying raw permissions.
    pub active_permission_profile: Option<ActivePermissionProfile>,

    /// Updated model slug. When set, the model info is derived automatically.
    pub model: Option<String>,

    /// Updated reasoning effort (honored only for reasoning-capable models).
    ///
    /// Use `Some(Some(_))` to set a specific effort, `Some(None)` to clear the
    /// effort, or `None` to leave the existing value unchanged.
    pub effort: Option<Option<ReasoningEffortConfig>>,

    /// Updated reasoning summary preference (honored only for reasoning-capable models).
    pub summary: Option<ReasoningSummaryConfig>,

    /// Updated service tier preference for future turns.
    ///
    /// Use `Some(Some(_))` to set a specific tier, `Some(None)` to clear the
    /// preference, or `None` to leave the existing value unchanged.
    pub service_tier: Option<Option<String>>,

    /// EXPERIMENTAL - set a pre-set collaboration mode.
    /// Takes precedence over model, effort, and developer instructions if set.
    pub collaboration_mode: Option<CollaborationMode>,

    /// Updated personality preference.
    pub personality: Option<Personality>,

    /// Updated model provider id. When set, switches the active provider
    /// for subsequent turns in this thread.
    pub model_provider_id: Option<String>,
}

impl fmt::Debug for ThreadSettingsOverrides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadSettingsOverrides")
            .field("environments", &self.environments)
            .field(
                "environment_variables",
                &self.environment_variables.as_ref().map(BTreeMap::len),
            )
            .field("profile_workspace_roots", &self.profile_workspace_roots)
            .field("approval_policy", &self.approval_policy)
            .field("sandbox_policy", &self.sandbox_policy)
            .field("permission_profile", &self.permission_profile)
            .field("active_permission_profile", &self.active_permission_profile)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .field("summary", &self.summary)
            .field("service_tier", &self.service_tier)
            .field("collaboration_mode", &self.collaboration_mode)
            .field("personality", &self.personality)
            .field("model_provider_id", &self.model_provider_id)
            .finish()
    }
}

/// Source classification for client-supplied context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdditionalContextKind {
    Untrusted,
    Application,
}

/// Client-supplied context keyed by an opaque source identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionalContextEntry {
    pub value: String,
    pub kind: AdditionalContextKind,
}

/// Submission operation
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
#[non_exhaustive]
pub enum Op {
    /// Abort current task without terminating background terminal processes.
    /// This server sends [`EventMsg::TurnAborted`] in response.
    Interrupt,

    /// Terminate all running background terminal processes for this thread.
    /// Use this when callers intentionally want to stop long-lived background shells.
    CleanBackgroundTerminals,

    /// User input, optionally with thread-settings overrides applied first.
    UserInput {
        /// User input items, see `InputItem`
        items: Vec<UserInput>,
        /// Optional JSON Schema used to constrain the final assistant message for this turn.
        final_output_json_schema: Option<Value>,
        /// Optional turn-scoped Responses API `client_metadata`.
        responsesapi_client_metadata: Option<HashMap<String, String>>,
        /// Client-supplied context fragments keyed by an opaque source identifier.
        additional_context: BTreeMap<String, AdditionalContextEntry>,

        /// Persistent thread-settings overrides to apply before the input.
        thread_settings: ThreadSettingsOverrides,
    },

    /// Apply persistent thread-settings overrides without starting a turn.
    ///
    /// This uses the same submission queue as turn starts so app-server can
    /// preserve caller order between both kinds of mutation.
    ThreadSettings {
        /// Persistent thread-settings overrides to apply.
        thread_settings: ThreadSettingsOverrides,
    },

    /// Inter-agent communication that should be recorded as agent-message history
    /// while still using the normal thread submission lifecycle.
    InterAgentCommunication {
        communication: InterAgentCommunication,
    },

    /// Stage an inter-agent communication without allowing it to start a turn.
    ///
    /// A paired sender releases it only after every participant has acknowledged
    /// staging, preventing one branch from starting on a partial delivery.
    StageInterAgentCommunication {
        communication: InterAgentCommunication,
        barrier_id: String,
    },

    /// Release a previously staged inter-agent communication.
    ReleaseInterAgentCommunication { barrier_id: String },

    /// Start an idle turn after previously queued work becomes eligible.
    StartPendingWork,

    /// Approve a command execution
    ExecApproval {
        /// The id of the submission we are approving
        id: String,
        /// Turn id associated with the approval event, when available.
        turn_id: Option<String>,
        /// The user's decision in response to the request.
        decision: ReviewDecision,
    },

    /// Approve a code patch
    PatchApproval {
        /// The id of the submission we are approving
        id: String,
        /// The user's decision in response to the request.
        decision: ReviewDecision,
    },

    /// Resolve an MCP elicitation request.
    ResolveElicitation {
        /// Name of the MCP server that issued the request.
        server_name: String,
        /// Request identifier from the MCP server.
        request_id: RequestId,
        /// User's decision for the request.
        decision: ElicitationAction,
        /// Structured user input supplied for accepted elicitations.
        content: Option<Value>,
        /// Optional client metadata associated with the elicitation response.
        meta: Option<Value>,
    },

    /// Resolve a request_user_input tool call.
    UserInputAnswer {
        /// Turn id for the in-flight request.
        id: String,
        /// User-provided answers.
        response: RequestUserInputResponse,
    },

    /// Resolve a pending scripted extension interaction.
    ScriptedInteractionResponse {
        /// Identifier emitted with the pending interaction request.
        request_id: String,
        /// User-selected interaction outcome.
        response: ScriptedInteractionResponse,
    },

    /// Expire a scripted interaction from the host scheduler.
    ExpireScriptedInteraction {
        /// Identifier emitted with the pending interaction request.
        request_id: String,
    },

    /// Control the next root-turn model-router A/B experiment.
    ModelRouterAbControl {
        /// Requested A/B control action.
        action: ModelRouterAbControlAction,
    },

    /// Resolve a request_permissions tool call.
    RequestPermissionsResponse {
        /// Call id for the in-flight request.
        id: String,
        /// User-granted permissions.
        response: RequestPermissionsResponse,
    },

    /// Resolve a dynamic tool call request.
    DynamicToolResponse {
        /// Call id for the in-flight request.
        id: String,
        /// Tool output payload.
        response: DynamicToolResponse,
    },

    /// Request MCP servers to reinitialize and refresh cached tool lists.
    RefreshMcpServers { config: McpServerRefreshConfig },

    /// Reload user config layer overrides for the active session.
    ///
    /// This updates runtime config-derived behavior (for example app
    /// enable/disable state) without restarting the thread.
    ReloadUserConfig,

    /// Request the agent to summarize the current conversation context.
    /// The agent will use its existing context (either conversation history or previous response id)
    /// to generate a summary which will be returned as an AgentMessage event.
    Compact,

    /// Request Xedoc to drop the last N user turns from in-memory context.
    ///
    /// This does not attempt to revert local filesystem changes. Clients are
    /// responsible for undoing any edits on disk.
    ThreadRollback { num_turns: u32 },

    /// Request a code review from the agent.
    Review { review_request: ReviewRequest },

    /// Request to shut down xedoc instance.
    Shutdown,

    /// Execute a user-initiated one-off shell command (triggered by "!cmd").
    ///
    /// The command string is executed using the user's default shell and may
    /// include shell syntax (pipes, redirects, etc.). Output is streamed via
    /// `ExecCommand*` events and the UI regains control upon `TurnComplete`.
    RunUserShellCommand {
        /// The raw command string after '!'
        command: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum ThreadHistoryMode {
    #[default]
    Legacy,
    Paginated,
}

impl ThreadHistoryMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Paginated => "paginated",
        }
    }
}

impl FromStr for ThreadHistoryMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "legacy" => Ok(Self::Legacy),
            "paginated" => Ok(Self::Paginated),
            _ => Err(format!("unknown thread history mode `{value}`")),
        }
    }
}

impl From<Vec<UserInput>> for Op {
    fn from(value: Vec<UserInput>) -> Self {
        Op::UserInput {
            items: value,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: ThreadSettingsOverrides::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct InterAgentCommunication {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<ResponseItemId>,
    pub author: AgentPath,
    pub recipient: AgentPath,
    #[serde(default)]
    pub other_recipients: Vec<AgentPath>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub encrypted_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    /// Session cost in USD not previously reported to the parent.
    /// Only populated on subagent completion messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_cost_usd: Option<f64>,
    pub trigger_turn: bool,
}

impl InterAgentCommunication {
    pub fn new(
        author: AgentPath,
        recipient: AgentPath,
        other_recipients: Vec<AgentPath>,
        content: String,
        trigger_turn: bool,
    ) -> Self {
        Self {
            id: None,
            author,
            recipient,
            other_recipients,
            content,
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
            session_cost_usd: None,
            trigger_turn,
        }
    }

    pub fn new_encrypted(
        author: AgentPath,
        recipient: AgentPath,
        other_recipients: Vec<AgentPath>,
        encrypted_content: String,
        trigger_turn: bool,
    ) -> Self {
        Self {
            id: None,
            author,
            recipient,
            other_recipients,
            content: String::new(),
            encrypted_content: Some(encrypted_content),
            internal_chat_message_metadata_passthrough: None,
            session_cost_usd: None,
            trigger_turn,
        }
    }

    pub fn set_turn_id_if_missing(&mut self, turn_id: &str) {
        InternalChatMessageMetadataPassthrough::set_turn_id_if_missing(
            &mut self.internal_chat_message_metadata_passthrough,
            turn_id,
        );
    }

    pub fn to_response_input_item(&self) -> ResponseInputItem {
        let mut communication = self.clone();
        communication.id = None;
        communication.internal_chat_message_metadata_passthrough = None;
        ResponseInputItem::Message {
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: serde_json::to_string(&communication).unwrap_or_default(),
            }],
            phase: Some(MessagePhase::Commentary),
        }
    }

    pub fn to_model_input_item(&self) -> ResponseItem {
        let content = match &self.encrypted_content {
            Some(encrypted_content) => {
                let message_type = if self.trigger_turn {
                    "NEW_TASK"
                } else {
                    "MESSAGE"
                };
                vec![
                    AgentMessageInputContent::InputText {
                        text: format!(
                            "Message Type: {message_type}\nTask name: {}\nSender: {}\nPayload:\n",
                            self.recipient, self.author
                        ),
                    },
                    AgentMessageInputContent::EncryptedContent {
                        encrypted_content: encrypted_content.clone(),
                    },
                ]
            }
            None => vec![AgentMessageInputContent::InputText {
                text: self.content.clone(),
            }],
        };
        ResponseItem::AgentMessage {
            id: self.id.clone(),
            author: self.author.to_string(),
            recipient: self.recipient.to_string(),
            content,
            internal_chat_message_metadata_passthrough: self
                .internal_chat_message_metadata_passthrough
                .clone(),
        }
    }

    pub fn is_message_content(content: &[ContentItem]) -> bool {
        Self::from_message_content(content).is_some()
    }

    pub fn from_message_content(content: &[ContentItem]) -> Option<Self> {
        match content {
            [ContentItem::InputText { text }] | [ContentItem::OutputText { text }] => {
                serde_json::from_str(text).ok()
            }
            _ => None,
        }
    }
}

impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::CleanBackgroundTerminals => "clean_background_terminals",
            Self::UserInput { .. } => "user_input",
            Self::ThreadSettings { .. } => "thread_settings",
            Self::InterAgentCommunication { .. } => "inter_agent_communication",
            Self::StageInterAgentCommunication { .. } => "stage_inter_agent_communication",
            Self::ReleaseInterAgentCommunication { .. } => "release_inter_agent_communication",
            Self::StartPendingWork => "start_pending_work",
            Self::ExecApproval { .. } => "exec_approval",
            Self::PatchApproval { .. } => "patch_approval",
            Self::ResolveElicitation { .. } => "resolve_elicitation",
            Self::UserInputAnswer { .. } => "user_input_answer",
            Self::ScriptedInteractionResponse { .. } => "scripted_interaction_response",
            Self::ExpireScriptedInteraction { .. } => "expire_scripted_interaction",
            Self::ModelRouterAbControl { .. } => "model_router_ab_control",
            Self::RequestPermissionsResponse { .. } => "request_permissions_response",
            Self::DynamicToolResponse { .. } => "dynamic_tool_response",
            Self::RefreshMcpServers { .. } => "refresh_mcp_servers",
            Self::ReloadUserConfig => "reload_user_config",
            Self::Compact => "compact",
            Self::ThreadRollback { .. } => "thread_rollback",
            Self::Review { .. } => "review",
            Self::Shutdown => "shutdown",
            Self::RunUserShellCommand { .. } => "run_user_shell_command",
        }
    }
}

/// Root-thread A/B experiment controls exposed to interactive clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRouterAbControlAction {
    /// Arm the next eligible root turn to spawn a bounded A/B pair.
    ArmNext,
    /// Disable any armed A/B experiment.
    Disable,
}

/// Determines the conditions under which the user is consulted to approve
/// running the command proposed by Xedoc.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    JsonSchema,
    TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum AskForApproval {
    /// Under this policy, only "known safe" commands—as determined by
    /// `is_safe_command()`—that **only read files** are auto‑approved.
    /// Everything else will ask the user to approve.
    #[serde(rename = "untrusted")]
    #[strum(serialize = "untrusted")]
    UnlessTrusted,

    /// The model decides when to ask the user for approval.
    #[serde(alias = "on-failure")]
    #[default]
    OnRequest,

    /// Fine-grained controls for individual approval flows.
    ///
    /// When a field is `true`, commands in that category are allowed. When it
    /// is `false`, those requests are automatically rejected instead of shown
    /// to the user.
    #[strum(serialize = "granular")]
    Granular(GranularApprovalConfig),

    /// Never ask the user to approve commands. Failures are immediately returned
    /// to the model, and never escalated to the user for approval.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
pub struct GranularApprovalConfig {
    /// Whether to allow shell command approval requests, including inline
    /// `with_additional_permissions` and `require_escalated` requests.
    pub sandbox_approval: bool,
    /// Whether to allow prompts triggered by execpolicy `prompt` rules.
    pub rules: bool,
    /// Whether to allow approval prompts triggered by skill script execution.
    #[serde(default)]
    pub skill_approval: bool,
    /// Whether to allow prompts triggered by the `request_permissions` tool.
    #[serde(default)]
    pub request_permissions: bool,
    /// Whether to allow MCP elicitation prompts.
    pub mcp_elicitations: bool,
}

impl GranularApprovalConfig {
    pub const fn allows_sandbox_approval(self) -> bool {
        self.sandbox_approval
    }

    pub const fn allows_rules_approval(self) -> bool {
        self.rules
    }

    pub const fn allows_skill_approval(self) -> bool {
        self.skill_approval
    }

    pub const fn allows_request_permissions(self) -> bool {
        self.request_permissions
    }

    pub const fn allows_mcp_elicitations(self) -> bool {
        self.mcp_elicitations
    }
}

/// Represents whether outbound network access is available to the agent.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, Default, JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum NetworkAccess {
    #[default]
    Restricted,
    Enabled,
}

impl NetworkAccess {
    pub fn is_enabled(self) -> bool {
        matches!(self, NetworkAccess::Enabled)
    }
}

/// Determines execution restrictions for model shell commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema, TS)]
#[strum(serialize_all = "kebab-case")]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SandboxPolicy {
    /// No restrictions whatsoever. Use with caution.
    #[serde(rename = "danger-full-access")]
    DangerFullAccess,

    /// Read-only access configuration.
    #[serde(rename = "read-only")]
    ReadOnly {
        /// When set to `true`, outbound network access is allowed. `false` by
        /// default.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        network_access: bool,
    },

    /// Indicates the process is already in an external sandbox. Allows full
    /// disk access while honoring the provided network setting.
    #[serde(rename = "external-sandbox")]
    ExternalSandbox {
        /// Whether the external sandbox permits outbound network traffic.
        #[serde(default)]
        network_access: NetworkAccess,
    },

    /// Same as `ReadOnly` but additionally grants write access to the current
    /// working directory ("workspace").
    #[serde(rename = "workspace-write")]
    WorkspaceWrite {
        /// Additional folders (beyond cwd and possibly TMPDIR) that should be
        /// writable from within the sandbox.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        writable_roots: Vec<AbsolutePathBuf>,

        /// When set to `true`, outbound network access is allowed. `false` by
        /// default.
        #[serde(default)]
        network_access: bool,

        /// When set to `true`, will NOT include the per-user `TMPDIR`
        /// environment variable among the default writable roots. Defaults to
        /// `false`.
        #[serde(default)]
        exclude_tmpdir_env_var: bool,

        /// When set to `true`, will NOT include the `/tmp` among the default
        /// writable roots on UNIX. Defaults to `false`.
        #[serde(default)]
        exclude_slash_tmp: bool,
    },
}

/// A writable root path accompanied by a list of subpaths that should remain
/// read‑only even when the root is writable. This is primarily used to ensure
/// that folders containing files that could be modified to escalate the
/// privileges of the agent (e.g. `.xedoc`, `.git`, notably `.git/hooks`) under
/// a writable root are not modified by the agent.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct WritableRoot {
    pub root: AbsolutePathBuf,

    /// By construction, these subpaths are all under `root`.
    pub read_only_subpaths: Vec<AbsolutePathBuf>,

    /// Workspace metadata path names that must not be created or replaced under
    /// `root` unless the policy grants an explicit write rule for that metadata
    /// path.
    pub protected_metadata_names: Vec<String>,
}

impl WritableRoot {
    pub fn is_path_writable(&self, path: &Path) -> bool {
        // Check if the path is under the root.
        if !path.starts_with(&self.root) {
            return false;
        }

        // Check if the path is under any of the read-only subpaths.
        for subpath in &self.read_only_subpaths {
            if path.starts_with(subpath) {
                return false;
            }
        }

        if self.path_contains_protected_metadata_name(path) {
            return false;
        }

        true
    }

    fn path_contains_protected_metadata_name(&self, path: &Path) -> bool {
        let Ok(relative_path) = path.strip_prefix(&self.root) else {
            return false;
        };

        let Some(first_component) = relative_path.components().next() else {
            return false;
        };

        self.protected_metadata_names
            .iter()
            .any(|name| first_component.as_os_str() == std::ffi::OsStr::new(name))
    }
}

impl FromStr for SandboxPolicy {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

impl FromStr for FileSystemSandboxPolicy {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

impl FromStr for NetworkSandboxPolicy {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

impl SandboxPolicy {
    /// Returns a policy with read-only disk access and no network.
    pub fn new_read_only_policy() -> Self {
        SandboxPolicy::ReadOnly {
            network_access: false,
        }
    }

    /// Returns a policy that can read the entire disk, but can only write to
    /// the current working directory and the per-user tmp dir on macOS. It does
    /// not allow network access.
    pub fn new_workspace_write_policy() -> Self {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: false,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        }
    }

    pub fn has_full_disk_read_access(&self) -> bool {
        true
    }

    pub fn has_full_disk_write_access(&self) -> bool {
        match self {
            SandboxPolicy::DangerFullAccess => true,
            SandboxPolicy::ExternalSandbox { .. } => true,
            SandboxPolicy::ReadOnly { .. } => false,
            SandboxPolicy::WorkspaceWrite { .. } => false,
        }
    }

    pub fn has_full_network_access(&self) -> bool {
        match self {
            SandboxPolicy::DangerFullAccess => true,
            SandboxPolicy::ExternalSandbox { network_access } => network_access.is_enabled(),
            SandboxPolicy::ReadOnly { network_access, .. } => *network_access,
            SandboxPolicy::WorkspaceWrite { network_access, .. } => *network_access,
        }
    }

    /// Returns the list of writable roots (tailored to the current working
    /// directory) together with subpaths that should remain read‑only under
    /// each writable root.
    pub fn get_writable_roots_with_cwd(&self, cwd: &Path) -> Vec<WritableRoot> {
        match self {
            SandboxPolicy::DangerFullAccess => Vec::new(),
            SandboxPolicy::ExternalSandbox { .. } => Vec::new(),
            SandboxPolicy::ReadOnly { .. } => Vec::new(),
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
                network_access: _,
            } => {
                // Start from explicitly configured writable roots.
                let mut roots: Vec<AbsolutePathBuf> = writable_roots.clone();

                // Always include defaults: cwd, /tmp (if present on Unix), and
                // on macOS, the per-user TMPDIR unless explicitly excluded.
                // TODO(mbolin): cwd param should be AbsolutePathBuf.
                let cwd_absolute = AbsolutePathBuf::from_absolute_path(cwd);
                match cwd_absolute {
                    Ok(cwd) => {
                        roots.push(cwd);
                    }
                    Err(e) => {
                        error!(
                            "Ignoring invalid cwd {:?} for sandbox writable root: {}",
                            cwd, e
                        );
                    }
                }

                // Include /tmp on Unix unless explicitly excluded.
                if cfg!(unix) && !exclude_slash_tmp {
                    match AbsolutePathBuf::from_absolute_path("/tmp") {
                        Ok(slash_tmp) => {
                            if slash_tmp.as_path().is_dir() {
                                roots.push(slash_tmp);
                            }
                        }
                        Err(e) => {
                            error!("Ignoring invalid /tmp for sandbox writable root: {e}");
                        }
                    }
                }

                // Include $TMPDIR unless explicitly excluded. On macOS, TMPDIR
                // is per-user, so writes to TMPDIR should not be readable by
                // other users on the system.
                //
                // By comparison, TMPDIR is not guaranteed to be defined on
                // Linux or Windows, but supporting it here gives users a way to
                // provide the model with their own temporary directory without
                // having to hardcode it in the config.
                if !exclude_tmpdir_env_var
                    && let Some(tmpdir) = std::env::var_os("TMPDIR")
                    && !tmpdir.is_empty()
                {
                    match AbsolutePathBuf::from_absolute_path(PathBuf::from(&tmpdir)) {
                        Ok(tmpdir_path) => {
                            roots.push(tmpdir_path);
                        }
                        Err(e) => {
                            error!(
                                "Ignoring invalid TMPDIR value {tmpdir:?} for sandbox writable root: {e}",
                            );
                        }
                    }
                }

                // For each root, compute subpaths that should remain read-only.
                let cwd_root = AbsolutePathBuf::from_absolute_path(cwd).ok();
                roots
                    .into_iter()
                    .map(|writable_root| {
                        let protect_missing_dot_xedoc = cwd_root
                            .as_ref()
                            .is_some_and(|cwd_root| cwd_root == &writable_root);
                        WritableRoot {
                            read_only_subpaths: default_read_only_subpaths_for_writable_root(
                                &writable_root,
                                protect_missing_dot_xedoc,
                            ),
                            protected_metadata_names: Vec::new(),
                            root: writable_root,
                        }
                    })
                    .collect()
            }
        }
    }
}

/// Event Queue Entry - events from agent
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    /// Submission `id` that this event is correlated with.
    pub id: String,
    /// Payload
    pub msg: EventMsg,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct EnvironmentConnectionEvent {
    pub environment_id: String,
}

/// Prefix used for transient compaction progress messages.
pub const COMPACTION_PROGRESS_PREFIX: &str = "• Compacting";

/// Response event from the agent
/// NOTE: Make sure none of these values have optional types, as it will mess up the extension code-gen.
#[derive(Debug, Clone, Deserialize, Serialize, Display, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
#[strum(serialize_all = "snake_case")]
pub enum EventMsg {
    /// Error while executing a submission
    Error(ErrorEvent),

    /// Warning issued while processing a submission. Unlike `Error`, this
    /// indicates the turn continued but the user should still be notified.
    Warning(WarningEvent),

    /// Model routing changed from the requested model to a different model.
    ModelReroute(ModelRerouteEvent),

    /// Model-router classification has started or finished.
    ModelRouterActivity(ModelRouterActivityEvent),

    /// Model-router decision for a newly accepted root or subagent task.
    ModelRouterDecision(ModelRouterDecisionEvent),

    /// A constrained interaction requested by a scripted extension.
    ScriptedInteractionRequest(ScriptedInteractionRequestEvent),

    /// Backend recommends additional account verification for this turn.
    ModelVerification(ModelVerificationEvent),

    /// Backend moderation metadata intended for first-party turn presentation.
    TurnModerationMetadata(TurnModerationMetadataEvent),

    /// Backend indicates that response output is waiting on a safety review.
    SafetyBuffering(SafetyBufferingEvent),

    /// Conversation history was compacted (either automatically or manually).
    ContextCompacted(ContextCompactedEvent),

    /// Conversation history was rolled back by dropping the last N user turns.
    ThreadRolledBack(ThreadRolledBackEvent),

    /// Agent has started a turn.
    /// v1 wire format uses `task_started`; accept `turn_started` for v2 interop.
    #[serde(rename = "task_started", alias = "turn_started")]
    TurnStarted(TurnStartedEvent),

    /// Persistent thread-settings overrides from the correlated submission have
    /// been applied to the session configuration.
    ThreadSettingsApplied(ThreadSettingsAppliedEvent),

    /// Agent has completed all actions.
    /// v1 wire format uses `task_complete`; accept `turn_complete` for v2 interop.
    #[serde(rename = "task_complete", alias = "turn_complete")]
    TurnComplete(TurnCompleteEvent),

    /// Usage update for the current session, including totals and last turn.
    /// Optional means unknown — UIs should not display when `None`.
    TokenCount(TokenCountEvent),

    /// Agent text output message
    AgentMessage(AgentMessageEvent),

    /// User/system input message (what was sent to the model)
    UserMessage(UserMessageEvent),

    /// Reasoning event from agent.
    AgentReasoning(AgentReasoningEvent),

    /// Raw chain-of-thought from agent.
    AgentReasoningRawContent(AgentReasoningRawContentEvent),

    /// Signaled when the model begins a new reasoning summary section (e.g., a new titled block).
    AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent),

    /// Ack the client's configure message.
    SessionConfigured(SessionConfiguredEvent),

    /// A selected environment completed its connection handshake.
    EnvironmentConnected(EnvironmentConnectionEvent),

    /// A selected environment lost its established connection.
    EnvironmentDisconnected(EnvironmentConnectionEvent),

    /// Updated long-running goal metadata for the thread.
    ThreadGoalUpdated(ThreadGoalUpdatedEvent),

    /// Incremental MCP startup progress updates.
    McpStartupUpdate(McpStartupUpdateEvent),

    /// Aggregate MCP startup completion summary.
    McpStartupComplete(McpStartupCompleteEvent),

    McpToolCallBegin(McpToolCallBeginEvent),

    McpToolCallEnd(McpToolCallEndEvent),

    WebSearchBegin(WebSearchBeginEvent),

    WebSearchEnd(WebSearchEndEvent),

    ImageGenerationBegin(ImageGenerationBeginEvent),

    ImageGenerationEnd(ImageGenerationEndEvent),

    /// Notification that the server is about to execute a command.
    ExecCommandBegin(ExecCommandBeginEvent),

    /// Incremental chunk of output from a running command.
    ExecCommandOutputDelta(ExecCommandOutputDeltaEvent),

    /// Terminal interaction for an in-progress command (stdin sent and stdout observed).
    TerminalInteraction(TerminalInteractionEvent),

    ExecCommandEnd(ExecCommandEndEvent),

    /// Notification that the agent attached a local image via the view_image tool.
    ViewImageToolCall(ViewImageToolCallEvent),

    ExecApprovalRequest(ExecApprovalRequestEvent),

    RequestPermissions(RequestPermissionsEvent),

    RequestUserInput(RequestUserInputEvent),

    DynamicToolCallRequest(DynamicToolCallRequest),

    DynamicToolCallResponse(DynamicToolCallResponseEvent),

    ElicitationRequest(ElicitationRequestEvent),

    ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent),

    /// Notification advising the user that something they are using has been
    /// deprecated and should be phased out.
    DeprecationNotice(DeprecationNoticeEvent),

    /// Notification that a model stream experienced an error or disconnect
    /// and the system is handling it (e.g., retrying with backoff).
    StreamError(StreamErrorEvent),

    /// Notification that the agent is about to apply a code patch. Mirrors
    /// `ExecCommandBegin` so front‑ends can show progress indicators.
    PatchApplyBegin(PatchApplyBeginEvent),

    /// Latest model-generated structured changes for an `apply_patch` call.
    PatchApplyUpdated(PatchApplyUpdatedEvent),

    /// Notification that a patch application has finished.
    PatchApplyEnd(PatchApplyEndEvent),

    TurnDiff(TurnDiffEvent),

    PlanUpdate(UpdatePlanArgs),

    TurnAborted(TurnAbortedEvent),

    /// Notification that the agent is shutting down.
    ShutdownComplete,

    /// Entered review mode.
    EnteredReviewMode(EnteredReviewModeEvent),

    /// Exited review mode with an optional final result to apply.
    ExitedReviewMode(ExitedReviewModeEvent),

    RawResponseItem(RawResponseItemEvent),
    RawResponseCompleted(RawResponseCompletedEvent),

    ItemStarted(ItemStartedEvent),
    ItemCompleted(ItemCompletedEvent),
    HookStarted(HookStartedEvent),
    HookCompleted(HookCompletedEvent),

    AgentMessageContentDelta(AgentMessageContentDeltaEvent),
    PlanDelta(PlanDeltaEvent),
    ReasoningContentDelta(ReasoningContentDeltaEvent),
    ReasoningRawContentDelta(ReasoningRawContentDeltaEvent),

    /// Collab interaction: agent spawn begin.
    CollabAgentSpawnBegin(CollabAgentSpawnBeginEvent),
    /// Collab interaction: agent spawn end.
    CollabAgentSpawnEnd(CollabAgentSpawnEndEvent),
    /// Collab interaction: agent interaction begin.
    CollabAgentInteractionBegin(CollabAgentInteractionBeginEvent),
    /// Collab interaction: agent interaction end.
    CollabAgentInteractionEnd(CollabAgentInteractionEndEvent),
    /// Collab interaction: waiting begin.
    CollabWaitingBegin(CollabWaitingBeginEvent),
    /// Collab interaction: waiting end.
    CollabWaitingEnd(CollabWaitingEndEvent),
    /// Collab interaction: close begin.
    CollabCloseBegin(CollabCloseBeginEvent),
    /// Collab interaction: close end.
    CollabCloseEnd(CollabCloseEndEvent),
    /// Collab interaction: resume begin.
    CollabResumeBegin(CollabResumeBeginEvent),
    /// Collab interaction: resume end.
    CollabResumeEnd(CollabResumeEndEvent),

    /// Path-based v2 sub-agent activity.
    SubAgentActivity(SubAgentActivityEvent),
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, EnumIter)]
#[serde(rename_all = "snake_case")]
pub enum HookEventName {
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PreCompact,
    PostCompact,
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    SubagentStart,
    SubagentStop,
    Stop,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookHandlerType {
    Command,
    Prompt,
    Agent,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookExecutionMode {
    Sync,
    Async,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookScope {
    Thread,
    Turn,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookSource {
    System,
    User,
    Project,
    Mdm,
    SessionFlags,
    Plugin,
    CloudRequirements,
    CloudManagedConfig,
    LegacyManagedConfigFile,
    LegacyManagedConfigMdm,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookTrustStatus {
    Managed,
    Untrusted,
    Trusted,
    Modified,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookRunStatus {
    Running,
    Completed,
    Failed,
    Blocked,
    Stopped,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookOutputEntryKind {
    Warning,
    Stop,
    Feedback,
    Context,
    Error,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct HookOutputEntry {
    pub kind: HookOutputEntryKind,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct HookRunSummary {
    pub id: String,
    pub event_name: HookEventName,
    pub handler_type: HookHandlerType,
    pub execution_mode: HookExecutionMode,
    pub scope: HookScope,
    pub source_path: AbsolutePathBuf,
    #[serde(default)]
    pub source: HookSource,
    pub display_order: i64,
    pub status: HookRunStatus,
    pub status_message: Option<String>,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub completed_at: Option<i64>,
    #[ts(type = "number | null")]
    pub duration_ms: Option<i64>,
    pub entries: Vec<HookOutputEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct HookStartedEvent {
    pub turn_id: Option<String>,
    pub run: HookRunSummary,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct HookCompletedEvent {
    pub turn_id: Option<String>,
    pub run: HookRunSummary,
}

impl From<CollabAgentSpawnBeginEvent> for EventMsg {
    fn from(event: CollabAgentSpawnBeginEvent) -> Self {
        EventMsg::CollabAgentSpawnBegin(event)
    }
}

impl From<CollabAgentSpawnEndEvent> for EventMsg {
    fn from(event: CollabAgentSpawnEndEvent) -> Self {
        EventMsg::CollabAgentSpawnEnd(event)
    }
}

impl From<CollabAgentInteractionBeginEvent> for EventMsg {
    fn from(event: CollabAgentInteractionBeginEvent) -> Self {
        EventMsg::CollabAgentInteractionBegin(event)
    }
}

impl From<CollabAgentInteractionEndEvent> for EventMsg {
    fn from(event: CollabAgentInteractionEndEvent) -> Self {
        EventMsg::CollabAgentInteractionEnd(event)
    }
}

impl From<CollabWaitingBeginEvent> for EventMsg {
    fn from(event: CollabWaitingBeginEvent) -> Self {
        EventMsg::CollabWaitingBegin(event)
    }
}

impl From<CollabWaitingEndEvent> for EventMsg {
    fn from(event: CollabWaitingEndEvent) -> Self {
        EventMsg::CollabWaitingEnd(event)
    }
}

impl From<CollabCloseBeginEvent> for EventMsg {
    fn from(event: CollabCloseBeginEvent) -> Self {
        EventMsg::CollabCloseBegin(event)
    }
}

impl From<CollabCloseEndEvent> for EventMsg {
    fn from(event: CollabCloseEndEvent) -> Self {
        EventMsg::CollabCloseEnd(event)
    }
}

impl From<CollabResumeBeginEvent> for EventMsg {
    fn from(event: CollabResumeBeginEvent) -> Self {
        EventMsg::CollabResumeBegin(event)
    }
}

impl From<CollabResumeEndEvent> for EventMsg {
    fn from(event: CollabResumeEndEvent) -> Self {
        EventMsg::CollabResumeEnd(event)
    }
}

impl From<SubAgentActivityEvent> for EventMsg {
    fn from(event: SubAgentActivityEvent) -> Self {
        EventMsg::SubAgentActivity(event)
    }
}

/// Agent lifecycle status, derived from emitted events.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Agent is waiting for initialization.
    #[default]
    PendingInit,
    /// Agent is currently running.
    Running,
    /// Agent's current turn was interrupted and it may receive more input.
    Interrupted,
    /// Agent is done. Contains the final assistant message.
    Completed(Option<String>),
    /// Agent encountered an error.
    Errored(String),
    /// Agent has been shutdown.
    Shutdown,
    /// Agent is not found.
    NotFound,
}

/// Turn kinds that reject same-turn steering.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum NonSteerableTurnKind {
    Review,
    Compact,
}

/// Xedoc errors that we expose to clients. Wire name stays `CodexErrorInfo`
/// so persisted rollouts and existing clients keep working.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename = "CodexErrorInfo", rename_all = "snake_case")]
#[ts(rename = "CodexErrorInfo")]
#[ts(rename_all = "snake_case")]
pub enum XedocErrorInfo {
    ContextWindowExceeded,
    SessionBudgetExceeded,
    UsageLimitExceeded,
    ServerOverloaded,
    CyberPolicy,
    HttpConnectionFailed {
        http_status_code: Option<u16>,
    },
    /// Failed to connect to the response SSE stream.
    ResponseStreamConnectionFailed {
        http_status_code: Option<u16>,
    },
    InternalServerError,
    Unauthorized,
    BadRequest,
    SandboxError,
    /// The response SSE stream disconnected in the middle of a turnbefore completion.
    ResponseStreamDisconnected {
        http_status_code: Option<u16>,
    },
    /// Reached the retry limit for responses.
    ResponseTooManyFailedAttempts {
        http_status_code: Option<u16>,
    },
    /// Returned when `turn/start` or `turn/steer` is submitted while the current active turn
    /// cannot accept same-turn steering, for example `/review` or manual `/compact`.
    ActiveTurnNotSteerable {
        turn_kind: NonSteerableTurnKind,
    },
    ThreadRollbackFailed,
    Other,
}

impl XedocErrorInfo {
    /// Whether this error should mark the current turn as failed when replaying history.
    pub fn affects_turn_status(&self) -> bool {
        match self {
            Self::ThreadRollbackFailed | Self::ActiveTurnNotSteerable { .. } => false,
            Self::ContextWindowExceeded
            | Self::SessionBudgetExceeded
            | Self::UsageLimitExceeded
            | Self::ServerOverloaded
            | Self::CyberPolicy
            | Self::HttpConnectionFailed { .. }
            | Self::ResponseStreamConnectionFailed { .. }
            | Self::InternalServerError
            | Self::Unauthorized
            | Self::BadRequest
            | Self::SandboxError
            | Self::ResponseStreamDisconnected { .. }
            | Self::ResponseTooManyFailedAttempts { .. }
            | Self::Other => true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct RawResponseItemEvent {
    pub item: ResponseItem,
}

/// Exact usage reported by one upstream Responses API completion.
///
/// Unlike TokenCountEvent, this is not accumulated, estimated, or replayed.
#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct RawResponseCompletedEvent {
    pub response_id: String,
    pub token_usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ItemStartedEvent {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub item: TurnItem,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ItemCompletedEvent {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub item: TurnItem,
    // Old rollout files may contain ItemCompleted events for PlanItem without
    // this field. Default to 0 so those persisted rollouts still deserialize
    // after tightening the core event contract.
    #[serde(default = "default_item_completed_at_ms")]
    pub completed_at_ms: i64,
}

const fn default_item_completed_at_ms() -> i64 {
    0
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct AgentMessageContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct PlanDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ReasoningContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ReasoningRawContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub content_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct EnteredReviewModeEvent {
    pub target: ReviewTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub user_facing_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub item_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExitedReviewModeEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub item_id: Option<String>,
    pub review_output: Option<ReviewOutputEvent>,
}

// Individual event payload types matching each `EventMsg` variant.

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ErrorEvent {
    pub message: String,
    #[serde(default, rename = "codex_error_info")]
    #[ts(rename = "codex_error_info")]
    pub xedoc_error_info: Option<XedocErrorInfo>,
}

impl ErrorEvent {
    /// Whether this error should mark the current turn as failed when replaying history.
    pub fn affects_turn_status(&self) -> bool {
        self.xedoc_error_info
            .as_ref()
            .is_none_or(XedocErrorInfo::affects_turn_status)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WarningEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelRerouteReason {
    HighRiskCyberActivity,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ModelRerouteEvent {
    pub from_model: String,
    pub to_model: String,
    pub reason: ModelRerouteReason,
}

/// Transient lifecycle metadata for local model-router classification.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ModelRouterActivityEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub scope: ModelRouterScope,
    pub state: ModelRouterActivityState,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelRouterActivityState {
    Started,
    Finished,
}

/// Bounded, transcript-visible model-router decision metadata.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ModelRouterDecisionEvent {
    pub decision_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub scope: ModelRouterScope,
    pub disposition: ModelRouterDisposition,
    pub reason: ModelRouterDecisionReason,
    /// Whether interactive clients should render this decision's feedback.
    #[serde(default)]
    pub feedback_visible: bool,
    /// Prompt-free diagnostic retained when a local router dependency fails.
    pub diagnostic: Option<String>,
    /// Prompt-free multiline decision summary supplied by the router.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    pub policy_revision: String,
    #[serde(default)]
    pub classifications: BTreeMap<String, String>,
    /// Classifier confidence score from the task embedding.
    #[serde(default)]
    pub confidence_score: f32,
    /// Margin between the selected classifier head and its runner-up.
    #[serde(default)]
    pub confidence_margin: f32,
    pub ranking_score: Option<u16>,
    pub ranking_minimum_class: Option<String>,
    pub ranking_maximum_class: Option<String>,
    pub ranking_minimum_rank: Option<u16>,
    pub ranking_maximum_rank: Option<u16>,
    pub ranking_target_rank: Option<u16>,
    pub ranking_selected_rank: Option<u16>,
    pub proposed_provider_id: String,
    pub proposed_model_slug: String,
    pub proposed_reasoning_effort: String,
    pub effective_route: ModelRouterEffectiveRoute,
    pub prompt_sha256: String,
    pub prompt_original_bytes: u64,
    pub prompt_truncated: bool,
    /// Unix timestamp in whole seconds when this immutable decision was made.
    pub created_at: i64,
}

/// A bounded constrained interaction requested by an extension script.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ScriptedInteractionRequestEvent {
    /// Host-generated identifier for this pending interaction.
    pub request_id: String,
    /// Identifier of the extension requesting the interaction.
    pub extension_id: String,
    /// Opaque script-defined interaction identifier.
    pub interaction_id: String,
    /// Opaque script state returned unchanged with the response.
    pub continuation: String,
    /// Script state revision used to reject stale submissions.
    pub state_revision: Option<String>,
    /// Unix timestamp in whole seconds after which this request is invalid.
    pub expires_at: i64,
    /// Canonical camelCase script-protocol surface, retained without projection.
    pub surface: Value,
}

/// A user response to a scripted interaction.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ScriptedInteractionResponse {
    /// Identifier of the extension that requested the interaction.
    pub extension_id: String,
    /// Opaque script-defined interaction identifier.
    pub interaction_id: String,
    /// Opaque script continuation received with the request.
    pub continuation: String,
    /// State revision rendered by the host.
    pub state_revision: Option<String>,
    /// User outcome.
    pub outcome: ScriptedInteractionOutcome,
    /// Selected action, when applicable.
    pub action: Option<ScriptedInteractionSelectedAction>,
    /// Canonical script-protocol value payload.
    pub values: Value,
}

/// An opaque action selected from a scripted interaction.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ScriptedInteractionSelectedAction {
    /// Opaque script-defined action identifier.
    pub id: String,
}

/// How the user concluded a scripted interaction.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum ScriptedInteractionOutcome {
    /// The user selected an action.
    Accepted,
    /// The user cancelled the interaction.
    Cancelled,
    /// The user dismissed a notice.
    Dismissed,
}

/// The route that was actually available to execute after routing validation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type", rename_all = "snake_case")]
pub enum ModelRouterEffectiveRoute {
    Available {
        provider_id: String,
        model_slug: String,
        reasoning_effort: String,
    },
    Unavailable,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelRouterScope {
    Root,
    Subagent,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelRouterDisposition {
    Applied,
    Shadow,
    Fallback,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelRouterDecisionReason {
    Classified,
    SteeringBypass,
    LowConfidence,
    NoClass,
    EmbeddingFailed,
    RouteUnavailable,
    ExplicitOverride,
}

impl From<ModelRouterDecisionEvent> for EventMsg {
    fn from(event: ModelRouterDecisionEvent) -> Self {
        Self::ModelRouterDecision(event)
    }
}

impl From<ModelRouterActivityEvent> for EventMsg {
    fn from(event: ModelRouterActivityEvent) -> Self {
        Self::ModelRouterActivity(event)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelVerification {
    TrustedAccessForCyber,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ModelVerificationEvent {
    pub verifications: Vec<ModelVerification>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct TurnModerationMetadataEvent {
    pub metadata: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct SafetyBufferingEvent {
    pub model: String,
    pub use_cases: Vec<String>,
    pub reasons: Vec<String>,
    pub show_buffering_ui: bool,
    pub faster_model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ContextCompactedEvent;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnCompleteEvent {
    pub turn_id: String,
    pub last_agent_message: Option<String>,
    /// Terminal error details when the turn completed unsuccessfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<ErrorEvent>,
    /// Unix timestamp (in seconds) when the turn started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub started_at: Option<i64>,
    /// Unix timestamp (in seconds) when the turn completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub completed_at: Option<i64>,
    /// Duration between turn start and completion in milliseconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub duration_ms: Option<i64>,
    /// Duration between turn start and the first model token in milliseconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub time_to_first_token_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnStartedEvent {
    pub turn_id: String,
    // Persist for rollout consumers that correlate turns with telemetry traces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub trace_id: Option<String>,
    /// Unix timestamp (in seconds) when the turn started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub started_at: Option<i64>,
    // TODO(aibrahim): make this not optional
    pub model_context_window: Option<i64>,
    #[serde(default)]
    pub collaboration_mode_kind: ModeKind,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ThreadSettingsAppliedEvent {
    pub thread_settings: ThreadSettingsSnapshot,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ThreadSettingsSnapshot {
    pub model: String,
    pub model_provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub approval_policy: AskForApproval,
    pub permission_profile: PermissionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_permission_profile: Option<ActivePermissionProfile>,
    pub cwd: AbsolutePathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<ReasoningSummaryConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    pub collaboration_mode: CollaborationMode,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq, JsonSchema, TS)]
pub struct TokenUsage {
    #[ts(type = "number")]
    pub input_tokens: i64,
    #[ts(type = "number")]
    pub cached_input_tokens: i64,
    #[serde(default)]
    #[ts(type = "number")]
    pub cache_write_input_tokens: i64,
    #[ts(type = "number")]
    pub output_tokens: i64,
    #[ts(type = "number")]
    pub reasoning_output_tokens: i64,
    #[ts(type = "number")]
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct TokenUsageInfo {
    pub total_token_usage: TokenUsage,
    pub last_token_usage: TokenUsage,
    // TODO(aibrahim): make this not optional
    #[ts(type = "number | null")]
    pub model_context_window: Option<i64>,
}

impl TokenUsageInfo {
    pub fn new_or_append(
        info: &Option<TokenUsageInfo>,
        last: &Option<TokenUsage>,
        model_context_window: Option<i64>,
    ) -> Option<Self> {
        if info.is_none() && last.is_none() {
            return None;
        }

        let mut info = match info {
            Some(info) => info.clone(),
            None => Self {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window,
            },
        };
        if let Some(last) = last {
            info.append_last_usage(last);
        }
        if let Some(model_context_window) = model_context_window {
            info.model_context_window = Some(model_context_window);
        }
        Some(info)
    }

    pub fn append_last_usage(&mut self, last: &TokenUsage) {
        self.total_token_usage.add_assign(last);
        self.last_token_usage = last.clone();
    }

    pub fn fill_to_context_window(&mut self, context_window: i64) {
        let previous_total = self.total_token_usage.total_tokens;
        let delta = (context_window - previous_total).max(0);

        self.model_context_window = Some(context_window);
        self.total_token_usage = TokenUsage {
            total_tokens: context_window,
            ..TokenUsage::default()
        };
        self.last_token_usage = TokenUsage {
            total_tokens: delta,
            ..TokenUsage::default()
        };
    }

    pub fn full_context_window(context_window: i64) -> Self {
        let mut info = Self {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage::default(),
            model_context_window: Some(context_window),
        };
        info.fill_to_context_window(context_window);
        info
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TokenCountEvent {
    pub info: Option<TokenUsageInfo>,
    pub rate_limits: Option<RateLimitSnapshot>,
    /// Accumulated session cost in USD from the session's cost tracker.
    /// `None` when no pricing is configured for any model used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_cost_usd: Option<f64>,
    /// Session-scoped token savings from the in-process output reducer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub token_optimizer: Option<TokenOptimizerSessionStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct TokenOptimizerSessionStats {
    pub reductions: i64,
    pub tokens_saved: i64,
    pub cost_saved_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct RateLimitSnapshot {
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits: Option<CreditsSnapshot>,
    pub individual_limit: Option<SpendControlLimitSnapshot>,
    /// Backend-reported spend-control state. `None` is unavailable, not a sparse-update recovery.
    pub spend_control_reached: Option<bool>,
    pub plan_type: Option<crate::account::PlanType>,
    pub rate_limit_reached_type: Option<RateLimitReachedType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum RateLimitReachedType {
    RateLimitReached,
    WorkspaceOwnerCreditsDepleted,
    WorkspaceMemberCreditsDepleted,
    WorkspaceOwnerUsageLimitReached,
    WorkspaceMemberUsageLimitReached,
}

impl FromStr for RateLimitReachedType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "rate_limit_reached" => Ok(Self::RateLimitReached),
            "workspace_owner_credits_depleted" => Ok(Self::WorkspaceOwnerCreditsDepleted),
            "workspace_member_credits_depleted" => Ok(Self::WorkspaceMemberCreditsDepleted),
            "workspace_owner_usage_limit_reached" => Ok(Self::WorkspaceOwnerUsageLimitReached),
            "workspace_member_usage_limit_reached" => Ok(Self::WorkspaceMemberUsageLimitReached),
            other => Err(format!("unknown rate limit reached type: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct RateLimitWindow {
    /// Percentage (0-100) of the window that has been consumed.
    pub used_percent: f64,
    /// Rolling window duration, in minutes.
    #[ts(type = "number | null")]
    pub window_minutes: Option<i64>,
    /// Unix timestamp (seconds since epoch) when the window resets.
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct CreditsSnapshot {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct SpendControlLimitSnapshot {
    pub limit: String,
    pub used: String,
    pub remaining_percent: i32,
    pub resets_at: i64,
}

// Includes prompts, tools and space to call compact.
const BASELINE_TOKENS: i64 = 12000;

impl TokenUsage {
    pub fn is_zero(&self) -> bool {
        self.total_tokens == 0
    }

    pub fn cached_input(&self) -> i64 {
        self.cached_input_tokens.max(0)
    }

    pub fn non_cached_input(&self) -> i64 {
        (self.input_tokens - self.cached_input()).max(0)
    }

    /// Primary count for display as a single absolute value: non-cached input + output.
    pub fn blended_total(&self) -> i64 {
        (self.non_cached_input() + self.output_tokens.max(0)).max(0)
    }

    pub fn tokens_in_context_window(&self) -> i64 {
        self.total_tokens
    }

    /// Estimate the remaining user-controllable percentage of the model's context window.
    ///
    /// `context_window` is the total size of the model's context window.
    /// `BASELINE_TOKENS` should capture tokens that are always present in
    /// the context (e.g., system prompt and fixed tool instructions) so that
    /// the percentage reflects the portion the user can influence.
    ///
    /// This normalizes both the numerator and denominator by subtracting the
    /// baseline, so immediately after the first prompt the UI shows 100% left
    /// and trends toward 0% as the user fills the effective window.
    pub fn percent_of_context_window_remaining(&self, context_window: i64) -> i64 {
        if context_window <= BASELINE_TOKENS {
            return 0;
        }

        let effective_window = context_window - BASELINE_TOKENS;
        let used = (self.tokens_in_context_window() - BASELINE_TOKENS).max(0);
        let remaining = (effective_window - used).max(0);
        ((remaining as f64 / effective_window as f64) * 100.0)
            .clamp(0.0, 100.0)
            .round() as i64
    }

    /// In-place element-wise sum of token counts.
    pub fn add_assign(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.cache_write_input_tokens += other.cache_write_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FinalOutput {
    pub token_usage: TokenUsage,
}

impl From<TokenUsage> for FinalOutput {
    fn from(token_usage: TokenUsage) -> Self {
        Self { token_usage }
    }
}

impl fmt::Display for FinalOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let token_usage = &self.token_usage;

        write!(
            f,
            "Token usage: total={} input={}{} output={}{}",
            format_with_separators(token_usage.blended_total()),
            format_with_separators(token_usage.non_cached_input()),
            if token_usage.cached_input() > 0 {
                format!(
                    " (+ {} cached)",
                    format_with_separators(token_usage.cached_input())
                )
            } else {
                String::new()
            },
            format_with_separators(token_usage.output_tokens),
            if token_usage.reasoning_output_tokens > 0 {
                format!(
                    " (reasoning {})",
                    format_with_separators(token_usage.reasoning_output_tokens)
                )
            } else {
                String::new()
            }
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentMessageEvent {
    pub message: String,
    #[serde(default)]
    pub phase: Option<MessagePhase>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, TS)]
pub struct UserMessageEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub message: String,
    /// Image URLs sourced from `UserInput::Image`. These are safe
    /// to replay in legacy UI history events and correspond to images sent to
    /// the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    /// Detail hints for `images`, indexed in parallel. Missing entries imply
    /// default image detail behavior.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_details: Vec<Option<ImageDetail>>,
    /// Local file paths sourced from `UserInput::LocalImage`. These are kept so
    /// the UI can reattach images when editing history. Local image prompts may
    /// include a display form of the path, but these should not be treated as
    /// API-ready URLs.
    #[serde(default)]
    pub local_images: Vec<std::path::PathBuf>,
    /// Detail hints for `local_images`, indexed in parallel. Missing entries
    /// imply default image detail behavior.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_image_details: Vec<Option<ImageDetail>>,
    /// Audio URLs sourced from `UserInput::Audio`. These are safe to replay in
    /// legacy UI history events and correspond to audio sent to the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<Vec<String>>,
    /// Local file paths sourced from `UserInput::LocalAudio`. These are kept so
    /// clients can reattach audio when editing history and should not be
    /// treated as API-ready URLs.
    #[serde(default)]
    pub local_audio: Vec<std::path::PathBuf>,
    /// UI-defined spans within `message` used to render or persist special elements.
    #[serde(default)]
    pub text_elements: Vec<crate::user_input::TextElement>,
}

/// Returns the user-facing preview text for a user message.
pub fn user_message_preview(user: &UserMessageEvent) -> Option<String> {
    let message = strip_user_message_prefix(user.message.as_str());
    if !message.is_empty() {
        return Some(message.to_string());
    }
    if user
        .images
        .as_ref()
        .is_some_and(|images| !images.is_empty())
        || !user.local_images.is_empty()
    {
        return Some("[Image]".to_string());
    }
    if user.audio.as_ref().is_some_and(|audio| !audio.is_empty()) || !user.local_audio.is_empty() {
        return Some("[Audio]".to_string());
    }
    None
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningRawContentEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningSectionBreakEvent {
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpInvocation {
    /// Name of the MCP server as defined in the config.
    pub server: String,
    /// Name of the tool as given by the MCP server.
    pub tool: String,
    /// Arguments to the tool call.
    pub arguments: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpToolCallBeginEvent {
    /// Identifier so this can be paired with the McpToolCallEnd event.
    pub call_id: String,
    pub invocation: McpInvocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub connector_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_app_resource_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub link_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpToolCallEndEvent {
    /// Identifier for the corresponding McpToolCallBegin that finished.
    pub call_id: String,
    pub invocation: McpInvocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub connector_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_app_resource_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub link_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_id: Option<String>,
    #[ts(type = "string")]
    pub duration: Duration,
    /// Result of the tool call. Note this could be an error.
    pub result: Result<CallToolResult, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct DynamicToolCallResponseEvent {
    /// Identifier for the corresponding DynamicToolCallRequest.
    pub call_id: String,
    /// Turn ID that this dynamic tool call belongs to.
    pub turn_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    /// Dynamic tool namespace, when one was provided.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Dynamic tool name.
    pub tool: String,
    /// Dynamic tool call arguments.
    pub arguments: serde_json::Value,
    /// Dynamic tool response content items.
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
    /// Whether the tool call succeeded.
    pub success: bool,
    /// Optional error text when the tool call failed before producing a response.
    pub error: Option<String>,
    /// The duration of the dynamic tool call.
    #[ts(type = "string")]
    pub duration: Duration,
}

impl McpToolCallEndEvent {
    pub fn is_success(&self) -> bool {
        match &self.result {
            Ok(result) => !result.is_error.unwrap_or(false),
            Err(_) => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WebSearchBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WebSearchEndEvent {
    pub call_id: String,
    pub query: String,
    pub action: WebSearchAction,
    /// Structured search results returned out-of-band by standalone web search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub results: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ImageGenerationBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ImageGenerationEndEvent {
    pub call_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revised_prompt: Option<String>,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub saved_path: Option<AbsolutePathBuf>,
}

// Conversation kept for backward compatibility.
/// Response payload for `Op::GetHistory` containing the current session's
/// in-memory transcript.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ConversationPathResponseEvent {
    pub conversation_id: ThreadId,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ResumedHistory {
    pub conversation_id: ThreadId,
    pub history: Arc<Vec<RolloutItem>>,
    pub rollout_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub enum InitialHistory {
    New,
    Cleared,
    Resumed(ResumedHistory),
    Forked(Vec<RolloutItem>),
}

impl InitialHistory {
    pub fn scan_rollout_items(&self, mut predicate: impl FnMut(&RolloutItem) -> bool) -> bool {
        match self {
            InitialHistory::New | InitialHistory::Cleared => false,
            InitialHistory::Resumed(resumed) => resumed.history.iter().any(&mut predicate),
            InitialHistory::Forked(items) => items.iter().any(predicate),
        }
    }

    pub fn forked_from_id(&self) -> Option<ThreadId> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => meta_line.meta.forked_from_id,
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => Some(meta_line.meta.id),
                _ => None,
            }),
        }
    }

    pub fn session_cwd(&self) -> Option<PathBuf> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => session_cwd_from_items(&resumed.history),
            InitialHistory::Forked(items) => session_cwd_from_items(items),
        }
    }

    pub fn get_rollout_items(&self) -> &[RolloutItem] {
        match self {
            InitialHistory::New | InitialHistory::Cleared => &[],
            InitialHistory::Resumed(resumed) => &resumed.history,
            InitialHistory::Forked(items) => items,
        }
    }

    pub fn get_event_msgs(&self) -> Option<Vec<EventMsg>> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => Some(
                resumed
                    .history
                    .iter()
                    .filter_map(|ri| match ri {
                        RolloutItem::EventMsg(ev) => Some(ev.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            InitialHistory::Forked(items) => Some(
                items
                    .iter()
                    .filter_map(|ri| match ri {
                        RolloutItem::EventMsg(ev) => Some(ev.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
        }
    }

    pub fn get_base_instructions(&self) -> Option<BaseInstructions> {
        // TODO: SessionMeta should (in theory) always be first in the history, so we can probably only check the first item?
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => meta_line.meta.base_instructions.clone(),
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => meta_line.meta.base_instructions.clone(),
                _ => None,
            }),
        }
    }

    pub fn get_dynamic_tools(&self) -> Option<Vec<DynamicToolSpec>> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => meta_line.meta.dynamic_tools.clone(),
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => meta_line.meta.dynamic_tools.clone(),
                _ => None,
            }),
        }
    }

    pub fn get_selected_capability_roots(&self) -> Vec<SelectedCapabilityRoot> {
        self.get_session_meta()
            .map(|meta| meta.selected_capability_roots.clone())
            .unwrap_or_default()
    }

    pub fn get_multi_agent_version(&self) -> Option<MultiAgentVersion> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => {
                multi_agent_version_from_items(&resumed.history, Some(resumed.conversation_id))
            }
            InitialHistory::Forked(items) => {
                multi_agent_version_from_items(items, /*thread_id*/ None)
            }
        }
    }

    pub fn get_history_mode(&self, default_history_mode: ThreadHistoryMode) -> ThreadHistoryMode {
        match self {
            InitialHistory::New | InitialHistory::Cleared => default_history_mode,
            // Forks copy their source rollout items as-is, so they must keep
            // the source format instead of converting to the destination default.
            InitialHistory::Resumed(_) | InitialHistory::Forked(_) => self
                .get_session_meta()
                .map(|meta| meta.history_mode)
                .unwrap_or(default_history_mode),
        }
    }

    pub fn get_latest_effective_multi_agent_mode(&self) -> Option<MultiAgentMode> {
        let items = match self {
            InitialHistory::New | InitialHistory::Cleared => return None,
            InitialHistory::Resumed(resumed) => &resumed.history,
            InitialHistory::Forked(items) => items,
        };
        items
            .iter()
            .rev()
            .find_map(|item| match item {
                RolloutItem::TurnContext(turn_context) => Some(turn_context),
                RolloutItem::SessionMeta(_)
                | RolloutItem::ResponseItem(_)
                | RolloutItem::InterAgentCommunication(_)
                | RolloutItem::InterAgentCommunicationMetadata { .. }
                | RolloutItem::Compacted(_)
                | RolloutItem::WorldState(_)
                | RolloutItem::EventMsg(_) => None,
            })
            .and_then(|turn_context| turn_context.multi_agent_mode.clone())
    }

    pub fn get_resumed_session_sources(&self) -> Option<(SessionSource, Option<ThreadSource>)> {
        let meta = self.get_resumed_session_meta()?;
        Some((meta.source.clone(), meta.thread_source.clone()))
    }

    pub fn get_resumed_thread_source(&self) -> Option<ThreadSource> {
        self.get_resumed_session_meta()
            .and_then(|meta| meta.thread_source.clone())
    }

    pub fn get_session_originator(&self) -> Option<String> {
        self.get_session_meta()
            .map(|meta| meta.originator.clone())
            .filter(|originator| !originator.is_empty())
    }

    pub fn get_resumed_parent_thread_id(&self) -> Option<ThreadId> {
        self.get_resumed_session_meta()
            .and_then(|meta| meta.parent_thread_id)
    }

    fn get_session_meta(&self) -> Option<&SessionMeta> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => Some(&meta_line.meta),
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => Some(&meta_line.meta),
                _ => None,
            }),
        }
    }

    fn get_resumed_session_meta(&self) -> Option<&SessionMeta> {
        match self {
            InitialHistory::New | InitialHistory::Cleared | InitialHistory::Forked(_) => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => Some(&meta_line.meta),
                    _ => None,
                })
            }
        }
    }
}

fn session_cwd_from_items(items: &[RolloutItem]) -> Option<PathBuf> {
    items.iter().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => Some(meta_line.meta.cwd.clone()),
        _ => None,
    })
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum SessionSource {
    Cli,
    #[default]
    VSCode,
    Exec,
    Mcp,
    Custom(String),
    Internal(InternalSessionSource),
    SubAgent(SubAgentSource),
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
#[ts(type = "string")]
pub enum ThreadSource {
    User,
    Subagent,
    Feature(String),
    MemoryConsolidation,
}

impl ThreadSource {
    pub fn as_str(&self) -> &str {
        match self {
            ThreadSource::User => "user",
            ThreadSource::Subagent => "subagent",
            ThreadSource::Feature(feature) => feature,
            ThreadSource::MemoryConsolidation => "memory_consolidation",
        }
    }
}

impl fmt::Display for ThreadSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for ThreadSource {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ThreadSource> for String {
    fn from(value: ThreadSource) -> Self {
        value.to_string()
    }
}

impl FromStr for ThreadSource {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "user" => Ok(ThreadSource::User),
            "subagent" => Ok(ThreadSource::Subagent),
            "memory_consolidation" => Ok(ThreadSource::MemoryConsolidation),
            other => Ok(ThreadSource::Feature(other.to_string())),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum InternalSessionSource {
    MemoryConsolidation,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SubAgentSource {
    Review,
    Compact,
    ThreadSpawn {
        parent_thread_id: ThreadId,
        depth: i32,
        #[serde(default)]
        agent_path: Option<AgentPath>,
        #[serde(default)]
        agent_nickname: Option<String>,
        #[serde(default, alias = "agent_type")]
        agent_role: Option<String>,
    },
    MemoryConsolidation,
    Other(String),
}

impl fmt::Display for SessionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionSource::Cli => f.write_str("cli"),
            SessionSource::VSCode => f.write_str("vscode"),
            SessionSource::Exec => f.write_str("exec"),
            SessionSource::Mcp => f.write_str("mcp"),
            SessionSource::Custom(source) => f.write_str(source),
            SessionSource::Internal(source) => write!(f, "internal_{source}"),
            SessionSource::SubAgent(sub_source) => write!(f, "subagent_{sub_source}"),
            SessionSource::Unknown => f.write_str("unknown"),
        }
    }
}

impl SessionSource {
    pub fn from_startup_arg(value: &str) -> Result<Self, &'static str> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("session source must not be empty");
        }

        let normalized = trimmed.to_ascii_lowercase();
        Ok(match normalized.as_str() {
            "cli" => SessionSource::Cli,
            "vscode" => SessionSource::VSCode,
            "exec" => SessionSource::Exec,
            "mcp" | "appserver" | "app-server" | "app_server" => SessionSource::Mcp,
            "unknown" => SessionSource::Unknown,
            _ => SessionSource::Custom(normalized),
        })
    }

    pub fn is_internal(&self) -> bool {
        matches!(self, SessionSource::Internal(_))
    }

    pub fn is_non_root_agent(&self) -> bool {
        matches!(
            self,
            SessionSource::Internal(_) | SessionSource::SubAgent(_)
        )
    }

    pub fn get_nickname(&self) -> Option<String> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_nickname, .. }) => {
                agent_nickname.clone()
            }
            _ => None,
        }
    }

    pub fn get_agent_role(&self) -> Option<String> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_role, .. }) => {
                agent_role.clone()
            }
            _ => None,
        }
    }

    pub fn get_agent_path(&self) -> Option<AgentPath> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_path, .. }) => {
                agent_path.clone()
            }
            _ => None,
        }
    }

    pub fn restriction_product(&self) -> Option<Product> {
        match self {
            SessionSource::Custom(source) => Product::from_session_source_name(source),
            SessionSource::Cli
            | SessionSource::VSCode
            | SessionSource::Exec
            | SessionSource::Mcp
            | SessionSource::Unknown => Some(Product::Xedoc),
            SessionSource::Internal(_) | SessionSource::SubAgent(_) => None,
        }
    }

    pub fn matches_product_restriction(&self, products: &[Product]) -> bool {
        products.is_empty()
            || self
                .restriction_product()
                .is_some_and(|product| product.matches_product_restriction(products))
    }

    pub fn parent_thread_id(&self) -> Option<ThreadId> {
        match self {
            SessionSource::SubAgent(subagent_source) => subagent_source.parent_thread_id(),
            SessionSource::Cli
            | SessionSource::VSCode
            | SessionSource::Exec
            | SessionSource::Mcp
            | SessionSource::Custom(_)
            | SessionSource::Internal(_)
            | SessionSource::Unknown => None,
        }
    }
}

impl fmt::Display for SubAgentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubAgentSource::Review => f.write_str("review"),
            SubAgentSource::Compact => f.write_str("compact"),
            SubAgentSource::MemoryConsolidation => f.write_str("memory_consolidation"),
            SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth,
                ..
            } => {
                write!(f, "thread_spawn_{parent_thread_id}_d{depth}")
            }
            SubAgentSource::Other(other) => f.write_str(other),
        }
    }
}

impl SubAgentSource {
    pub fn kind(&self) -> &str {
        match self {
            SubAgentSource::Review => "review",
            SubAgentSource::Compact => "compact",
            SubAgentSource::ThreadSpawn { .. } => "thread_spawn",
            SubAgentSource::MemoryConsolidation => "memory_consolidation",
            SubAgentSource::Other(other) => other,
        }
    }

    pub fn parent_thread_id(&self) -> Option<ThreadId> {
        match self {
            SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            } => Some(*parent_thread_id),
            SubAgentSource::Review
            | SubAgentSource::Compact
            | SubAgentSource::MemoryConsolidation
            | SubAgentSource::Other(_) => None,
        }
    }
}

impl fmt::Display for InternalSessionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InternalSessionSource::MemoryConsolidation => f.write_str("memory_consolidation"),
        }
    }
}

fn multi_agent_version_from_items(
    items: &[RolloutItem],
    thread_id: Option<ThreadId>,
) -> Option<MultiAgentVersion> {
    let session_meta_version = items.iter().rev().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line)
            if thread_id.is_none_or(|thread_id| meta_line.meta.id == thread_id) =>
        {
            meta_line.meta.multi_agent_version
        }
        _ => None,
    });

    session_meta_version.or_else(|| {
        items.iter().rev().find_map(|item| match item {
            RolloutItem::TurnContext(turn_context) => turn_context.multi_agent_version,
            RolloutItem::SessionMeta(_)
            | RolloutItem::ResponseItem(_)
            | RolloutItem::InterAgentCommunication(_)
            | RolloutItem::InterAgentCommunicationMetadata { .. }
            | RolloutItem::Compacted(_)
            | RolloutItem::WorldState(_)
            | RolloutItem::EventMsg(_) => None,
        })
    })
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum MultiAgentVersion {
    Disabled,
    V1,
    V2,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
pub struct SessionContextWindow {
    /// UUIDv7 identity of this context window.
    pub window_id: String,
}

impl SessionContextWindow {
    pub fn new(window_id: String) -> Self {
        Self { window_id }
    }
}

/// Exclusive position in another thread's paginated rollout history.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema, TS)]
pub struct HistoryPosition {
    pub thread_id: ThreadId,
    /// First rollout ordinal not included from the prefix file.
    pub end_ordinal_exclusive: u64,
    /// Byte offset immediately after the last included JSONL record from the prefix file.
    pub end_byte_offset: u64,
}

/// SessionMeta contains session-level data that doesn't correspond to a specific turn.
///
/// NOTE: There used to be an `instructions` field here, which stored user_instructions, but we
/// now save that on TurnContext. base_instructions stores the base instructions for the session,
/// and should be used when there is no config override.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct SessionMeta {
    pub session_id: SessionId,
    pub id: ThreadId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<ThreadId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
    pub timestamp: String,
    pub cwd: PathBuf,
    pub originator: String,
    pub cli_version: String,
    #[serde(default)]
    pub source: SessionSource,
    /// Optional analytics source classification for this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<ThreadSource>,
    /// Optional random unique nickname assigned to an AgentControl-spawned sub-agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    /// Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.
    #[serde(default, alias = "agent_type", skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    /// Optional canonical agent path assigned to an AgentControl-spawned sub-agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    pub model_provider: Option<String>,
    /// base_instructions for the session. This *should* always be present when creating a new session,
    /// but may be missing for older sessions. If not present, fall back to rendering the base_instructions
    /// from ModelsManager.
    pub base_instructions: Option<BaseInstructions>,
    #[serde(
        default,
        deserialize_with = "crate::dynamic_tools::deserialize_dynamic_tool_specs",
        skip_serializing_if = "Option::is_none"
    )]
    pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
    /// Capability roots selected for this thread by the hosting platform.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_capability_roots: Vec<SelectedCapabilityRoot>,
    #[serde(default)]
    pub history_mode: ThreadHistoryMode,
    /// Exclusive prefix of another paginated rollout inherited by this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_base: Option<HistoryPosition>,
    /// First rollout ordinal that belongs to this subagent's own projected history.
    ///
    /// Earlier rollout records are inherited model context and stay out of child
    /// turn/item projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_history_start_ordinal: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_agent_version: Option<MultiAgentVersion>,
    /// Initial context-window identity for consumers that tail rollout JSONL before compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<SessionContextWindow>,
}

impl Default for SessionMeta {
    fn default() -> Self {
        let id = ThreadId::default();
        SessionMeta {
            session_id: id.into(),
            id,
            forked_from_id: None,
            parent_thread_id: None,
            timestamp: String::new(),
            cwd: PathBuf::new(),
            originator: String::new(),
            cli_version: String::new(),
            source: SessionSource::default(),
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            agent_path: None,
            model_provider: None,
            base_instructions: None,
            dynamic_tools: None,
            selected_capability_roots: Vec::new(),
            history_mode: ThreadHistoryMode::default(),
            history_base: None,
            subagent_history_start_ordinal: None,
            multi_agent_version: None,
            context_window: None,
        }
    }
}

#[derive(Serialize, Debug, Clone, JsonSchema, TS)]
pub struct SessionMetaLine {
    #[serde(flatten)]
    pub meta: SessionMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
}

impl<'de> Deserialize<'de> for SessionMetaLine {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SessionMetaLineFields {
            #[serde(flatten)]
            meta: SessionMeta,
            git: Option<GitInfo>,
        }

        let mut value = Value::deserialize(deserializer)?;
        let fields = value
            .as_object_mut()
            .ok_or_else(|| D::Error::custom("session metadata must be an object"))?;
        if !fields.contains_key("session_id") {
            let thread_id = fields
                .get("id")
                .cloned()
                .ok_or_else(|| D::Error::missing_field("id"))?;
            fields.insert("session_id".to_string(), thread_id);
        }
        let SessionMetaLineFields { meta, git } =
            serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Self { meta, git })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, TS)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RolloutItem {
    SessionMeta(SessionMetaLine),
    ResponseItem(ResponseItem),
    /// Legacy delivery item reconstructed as a model-visible `agent_message`.
    InterAgentCommunication(InterAgentCommunication),
    /// Local delivery metadata that is not part of the Responses API item.
    InterAgentCommunicationMetadata {
        trigger_turn: bool,
    },
    Compacted(CompactedItem),
    TurnContext(TurnContextItem),
    WorldState(WorldStateItem),
    EventMsg(EventMsg),
}

/// Persisted comparison state used to resume model-visible world-state diffing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
pub struct WorldStateItem {
    /// Full snapshots establish a new baseline; patches update the current baseline.
    pub full: bool,
    pub state: Value,
}

impl WorldStateItem {
    pub fn full(state: Value) -> Self {
        Self { full: true, state }
    }

    pub fn patch(state: Value) -> Self {
        Self { full: false, state }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, JsonSchema, TS)]
pub struct CompactedItem {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_history: Option<Vec<ResponseItem>>,
    /// Monotonic position of this context window within the thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_number: Option<u64>,
    /// UUIDv7 identity of the first context window in this thread's window chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_window_id: Option<String>,
    /// UUIDv7 identity of the context window immediately before this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_window_id: Option<String>,
    /// UUIDv7 identity of this context window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
}

impl From<CompactedItem> for ResponseItem {
    fn from(value: CompactedItem) -> Self {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: value.message,
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
pub struct TurnContextNetworkItem {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
}

/// Persist once per real user turn after computing that turn's model-visible
/// context updates, and again after mid-turn compaction when replacement
/// history re-establishes full context, so resume/fork replay can recover the
/// latest durable baseline.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
pub struct TurnContextItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub cwd: AbsolutePathBuf,
    /// Effective workspace roots used to materialize symbolic
    /// `:workspace_roots` filesystem permissions in `permission_profile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_roots: Option<Vec<AbsolutePathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub approval_policy: AskForApproval,
    pub sandbox_policy: SandboxPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_profile: Option<PermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<TurnContextNetworkItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_system_sandbox_policy: Option<FileSystemSandboxPolicy>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comp_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<CollaborationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_agent_version: Option<MultiAgentVersion>,
    /// Effective model-visible mode used as the durable context-diff baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_agent_mode: Option<MultiAgentMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffortConfig>,
    // Compatibility-only field written with a default value so older Xedoc
    // versions can deserialize turn-context rollout items. It is no longer
    // read by context reconstruction and should be removed in a future schema
    // cleanup.
    pub summary: ReasoningSummaryConfig,
}

impl TurnContextItem {
    pub fn permission_profile(&self) -> PermissionProfile {
        self.permission_profile.clone().unwrap_or_else(|| {
            let file_system_sandbox_policy =
                self.file_system_sandbox_policy.clone().unwrap_or_else(|| {
                    FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
                        &self.sandbox_policy,
                        self.cwd.as_path(),
                    )
                });
            PermissionProfile::from_runtime_permissions_with_enforcement(
                SandboxEnforcement::from_legacy_sandbox_policy(&self.sandbox_policy),
                &file_system_sandbox_policy,
                NetworkSandboxPolicy::from(&self.sandbox_policy),
            )
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "mode", content = "limit", rename_all = "snake_case")]
pub enum TruncationPolicy {
    Bytes(usize),
    Tokens(usize),
}

impl From<crate::openai_models::TruncationPolicyConfig> for TruncationPolicy {
    fn from(config: crate::openai_models::TruncationPolicyConfig) -> Self {
        match config.mode {
            crate::openai_models::TruncationMode::Bytes => Self::Bytes(config.limit as usize),
            crate::openai_models::TruncationMode::Tokens => Self::Tokens(config.limit as usize),
        }
    }
}

impl TruncationPolicy {
    pub fn token_budget(&self) -> usize {
        match self {
            TruncationPolicy::Bytes(bytes) => {
                usize::try_from(xedoc_utils_string::approx_tokens_from_byte_count(*bytes))
                    .unwrap_or(usize::MAX)
            }
            TruncationPolicy::Tokens(tokens) => *tokens,
        }
    }

    pub fn byte_budget(&self) -> usize {
        match self {
            TruncationPolicy::Bytes(bytes) => *bytes,
            TruncationPolicy::Tokens(tokens) => {
                xedoc_utils_string::approx_bytes_for_tokens(*tokens)
            }
        }
    }
}

impl Mul<f64> for TruncationPolicy {
    type Output = Self;

    fn mul(self, multiplier: f64) -> Self::Output {
        match self {
            TruncationPolicy::Bytes(bytes) => {
                TruncationPolicy::Bytes((bytes as f64 * multiplier).ceil() as usize)
            }
            TruncationPolicy::Tokens(tokens) => {
                TruncationPolicy::Tokens((tokens as f64 * multiplier).ceil() as usize)
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
pub struct RolloutLine {
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
    #[serde(flatten)]
    pub item: RolloutItem,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct GitInfo {
    /// Current commit hash (SHA)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<GitSha>,
    /// Current branch name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Repository URL (if available from remote)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDelivery {
    Inline,
    Detached,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
pub enum ReviewTarget {
    /// Review the working tree: staged, unstaged, and untracked files.
    UncommittedChanges,

    /// Review changes between the current branch and the given base branch.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    BaseBranch { branch: String },

    /// Review the changes introduced by a specific commit.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Commit {
        sha: String,
        /// Optional human-readable label (e.g., commit subject) for UIs.
        title: Option<String>,
    },

    /// Arbitrary instructions provided by the user.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Custom { instructions: String },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
/// Review request sent to the review session.
pub struct ReviewRequest {
    pub target: ReviewTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub user_facing_hint: Option<String>,
}

/// Structured review result produced by a child review session.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewOutputEvent {
    pub findings: Vec<ReviewFinding>,
    pub overall_correctness: String,
    pub overall_explanation: String,
    pub overall_confidence_score: f32,
}

impl Default for ReviewOutputEvent {
    fn default() -> Self {
        Self {
            findings: Vec::new(),
            overall_correctness: String::default(),
            overall_explanation: String::default(),
            overall_confidence_score: 0.0,
        }
    }
}

/// A single review finding describing an observed issue or recommendation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewFinding {
    pub title: String,
    pub body: String,
    pub confidence_score: f32,
    pub priority: i32,
    pub code_location: ReviewCodeLocation,
}

/// Location of the code related to a review finding.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewCodeLocation {
    pub absolute_file_path: PathBuf,
    pub line_range: ReviewLineRange,
}

/// Inclusive line range in a file associated with the finding.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewLineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(
    Debug, Clone, Copy, Display, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandSource {
    #[default]
    Agent,
    UserShell,
    UnifiedExecStartup,
    UnifiedExecInteraction,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandBeginEvent {
    /// Identifier so this can be paired with the ExecCommandEnd event.
    pub call_id: String,
    /// Identifier for the underlying PTY process (when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    /// Turn ID that this command belongs to.
    pub turn_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    /// The command to be executed.
    pub command: Vec<String>,
    /// The command's working directory if not the default cwd for the agent.
    pub cwd: PathUri,
    pub parsed_cmd: Vec<ParsedCommand>,
    /// Where the command originated. Defaults to Agent for backward compatibility.
    #[serde(default)]
    pub source: ExecCommandSource,
    /// Raw input sent to a unified exec session (if this is an interaction event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandEndEvent {
    /// Identifier for the ExecCommandBegin that finished.
    pub call_id: String,
    /// Identifier for the underlying PTY process (when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    /// Turn ID that this command belongs to.
    pub turn_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    /// The command that was executed.
    pub command: Vec<String>,
    /// The command's working directory if not the default cwd for the agent.
    pub cwd: PathUri,
    pub parsed_cmd: Vec<ParsedCommand>,
    /// Where the command originated. Defaults to Agent for backward compatibility.
    #[serde(default)]
    pub source: ExecCommandSource,
    /// Raw input sent to a unified exec session (if this is an interaction event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,

    /// Captured stdout
    pub stdout: String,
    /// Captured stderr
    pub stderr: String,
    /// Captured aggregated output
    #[serde(default)]
    pub aggregated_output: String,
    /// The command's exit code.
    pub exit_code: i32,
    /// The duration of the command execution.
    #[ts(type = "string")]
    pub duration: Duration,
    /// Formatted output from the command, as seen by the model.
    pub formatted_output: String,
    /// Completion status for this command execution.
    pub status: ExecCommandStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ViewImageToolCallEvent {
    /// Identifier for the originating tool call.
    pub call_id: String,
    /// Filesystem path resolved for the selected environment.
    ///
    /// This core event is not exposed directly in the app-server API. App-server
    /// converts the path to `LegacyAppPathString` when building its public item.
    pub path: PathUri,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutputStream {
    Stdout,
    Stderr,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ExecCommandOutputDeltaEvent {
    /// Identifier for the ExecCommandBegin that produced this chunk.
    pub call_id: String,
    /// Which stream produced this chunk.
    pub stream: ExecOutputStream,
    /// Raw bytes from the stream (may not be valid UTF-8).
    #[serde_as(as = "serde_with::base64::Base64")]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    pub chunk: Vec<u8>,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct TerminalInteractionEvent {
    /// Identifier for the ExecCommandBegin that produced this chunk.
    pub call_id: String,
    /// Process id associated with the running command.
    pub process_id: String,
    /// Stdin sent to the running session.
    pub stdin: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct DeprecationNoticeEvent {
    /// Concise summary of what is deprecated.
    pub summary: String,
    /// Optional extra guidance, such as migration steps or rationale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ThreadRolledBackEvent {
    /// Number of user turns that were removed from context.
    pub num_turns: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct StreamErrorEvent {
    pub message: String,
    #[serde(default, rename = "codex_error_info")]
    #[ts(rename = "codex_error_info")]
    pub xedoc_error_info: Option<XedocErrorInfo>,
    /// Optional details about the underlying stream failure (often the same
    /// human-readable message that is surfaced as the terminal error if retries
    /// are exhausted).
    #[serde(default)]
    pub additional_details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct StreamInfoEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyBeginEvent {
    /// Identifier so this can be paired with the PatchApplyEnd event.
    pub call_id: String,
    /// Turn ID that this patch belongs to.
    /// Uses `#[serde(default)]` for backwards compatibility.
    #[serde(default)]
    pub turn_id: String,
    /// If true, there was no ApplyPatchApprovalRequest for this patch.
    pub auto_approved: bool,
    /// The changes to be applied.
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyUpdatedEvent {
    /// Identifier for the originating `apply_patch` tool call.
    pub call_id: String,
    /// Structured file changes parsed from the model-generated patch input so far.
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyEndEvent {
    /// Identifier for the PatchApplyBegin that finished.
    pub call_id: String,
    /// Turn ID that this patch belongs to.
    /// Uses `#[serde(default)]` for backwards compatibility.
    #[serde(default)]
    pub turn_id: String,
    /// Captured stdout (summary printed by apply_patch).
    pub stdout: String,
    /// Captured stderr (parser errors, IO failures, etc.).
    pub stderr: String,
    /// Whether the patch was applied successfully.
    pub success: bool,
    /// The changes that were applied (mirrors PatchApplyBeginEvent::changes).
    #[serde(default)]
    pub changes: HashMap<PathBuf, FileChange>,
    /// Completion status for this patch application.
    pub status: PatchApplyStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum PatchApplyStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnDiffEvent {
    pub unified_diff: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct McpStartupUpdateEvent {
    /// Server name being started.
    pub server: String,
    /// Current startup status.
    pub status: McpStartupStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case", tag = "state")]
#[ts(rename_all = "snake_case", tag = "state")]
pub enum McpStartupStatus {
    Starting,
    Ready,
    Failed {
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        reason: Option<McpStartupFailureReason>,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum McpStartupFailureReason {
    ReauthenticationRequired,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, Default)]
pub struct McpStartupCompleteEvent {
    pub ready: Vec<String>,
    pub failed: Vec<McpStartupFailure>,
    pub cancelled: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct McpStartupFailure {
    pub server: String,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum McpAuthStatus {
    Unsupported,
    NotLoggedIn,
    BearerToken,
    OAuth,
}

impl fmt::Display for McpAuthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            McpAuthStatus::Unsupported => "Unsupported",
            McpAuthStatus::NotLoggedIn => "Not logged in",
            McpAuthStatus::BearerToken => "Bearer token",
            McpAuthStatus::OAuth => "OAuth",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum Product {
    #[serde(alias = "CHATGPT")]
    Chatgpt,
    // OpenAI's product identifier stays "codex" on the wire.
    #[serde(rename = "codex", alias = "CODEX")]
    #[ts(rename = "codex")]
    Xedoc,
    #[serde(alias = "ATLAS")]
    Atlas,
}
impl Product {
    pub fn to_app_platform(self) -> &'static str {
        match self {
            Self::Chatgpt => "chat",
            Self::Xedoc => "codex",
            Self::Atlas => "atlas",
        }
    }

    pub fn from_session_source_name(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "chatgpt" => Some(Self::Chatgpt),
            "xedoc" => Some(Self::Xedoc),
            "atlas" => Some(Self::Atlas),
            _ => None,
        }
    }

    pub fn matches_product_restriction(&self, products: &[Product]) -> bool {
        products.is_empty() || products.contains(self)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SkillScope {
    User,
    Repo,
    System,
    Admin,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Legacy short_description from SKILL.md. Prefer SKILL.json interface.short_description.
    pub short_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interface: Option<SkillInterface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dependencies: Option<SkillDependencies>,
    pub path: AbsolutePathBuf,
    pub scope: SkillScope,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillInterface {
    #[ts(optional)]
    pub display_name: Option<String>,
    #[ts(optional)]
    pub short_description: Option<String>,
    #[ts(optional)]
    pub icon_small: Option<AbsolutePathBuf>,
    #[ts(optional)]
    pub icon_large: Option<AbsolutePathBuf>,
    #[ts(optional)]
    pub brand_color: Option<String>,
    #[ts(optional)]
    pub default_prompt: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillDependencies {
    pub tools: Vec<SkillToolDependency>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillToolDependency {
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub r#type: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SessionNetworkProxyRuntime {
    pub http_addr: String,
    pub socks_addr: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, TS)]
pub struct SessionConfiguredEvent {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<ThreadId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
    /// Optional analytics source classification for this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<ThreadSource>,

    /// Optional user-facing thread name (may be unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_name: Option<String>,

    /// Tell the client what model is being queried.
    pub model: String,

    pub model_provider_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    /// When to escalate for approval for execution
    pub approval_policy: AskForApproval,

    /// Canonical effective permissions for commands executed in the session.
    pub permission_profile: PermissionProfile,

    /// Named or implicit built-in profile that produced `permission_profile`,
    /// when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_permission_profile: Option<ActivePermissionProfile>,

    /// Working directory that should be treated as the *root* of the
    /// session.
    pub cwd: AbsolutePathBuf,

    /// The effort the model is putting into reasoning about the user's request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortConfig>,

    /// Optional initial messages (as events) for resumed sessions.
    /// When present, UIs can use these to seed the history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_messages: Option<Vec<EventMsg>>,

    /// Runtime proxy bind addresses, when the managed proxy was started for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub network_proxy: Option<SessionNetworkProxyRuntime>,

    /// Path in which the rollout is stored. Can be `None` for ephemeral threads
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollout_path: Option<PathBuf>,
}

impl<'de> Deserialize<'de> for SessionConfiguredEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            session_id: SessionId,
            #[serde(default)]
            thread_id: Option<ThreadId>,
            forked_from_id: Option<ThreadId>,
            parent_thread_id: Option<ThreadId>,
            #[serde(default)]
            thread_source: Option<ThreadSource>,
            #[serde(default)]
            thread_name: Option<String>,
            model: String,
            model_provider_id: String,
            service_tier: Option<String>,
            approval_policy: AskForApproval,
            // `SessionConfiguredEvent` is persisted into rollout history. Older
            // rollouts only have `sandbox_policy`, so accept it on deserialize
            // and immediately project it into the canonical `permission_profile`.
            sandbox_policy: Option<SandboxPolicy>,
            permission_profile: Option<PermissionProfile>,
            #[serde(default)]
            active_permission_profile: Option<ActivePermissionProfile>,
            cwd: AbsolutePathBuf,
            reasoning_effort: Option<ReasoningEffortConfig>,
            initial_messages: Option<Vec<EventMsg>>,
            network_proxy: Option<SessionNetworkProxyRuntime>,
            rollout_path: Option<PathBuf>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let permission_profile = match (wire.permission_profile, wire.sandbox_policy) {
            (Some(permission_profile), _) => permission_profile,
            (None, Some(sandbox_policy)) => PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                &sandbox_policy,
                wire.cwd.as_path(),
            ),
            (None, None) => {
                return Err(serde::de::Error::missing_field("permission_profile"));
            }
        };

        Ok(Self {
            session_id: wire.session_id,
            thread_id: wire.thread_id.unwrap_or_else(|| wire.session_id.into()),
            forked_from_id: wire.forked_from_id,
            parent_thread_id: wire.parent_thread_id,
            thread_source: wire.thread_source,
            thread_name: wire.thread_name,
            model: wire.model,
            model_provider_id: wire.model_provider_id,
            service_tier: wire.service_tier,
            approval_policy: wire.approval_policy,
            permission_profile,
            active_permission_profile: wire.active_permission_profile,
            cwd: wire.cwd,
            reasoning_effort: wire.reasoning_effort,
            initial_messages: wire.initial_messages,
            network_proxy: wire.network_proxy,
            rollout_path: wire.rollout_path,
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub enum ThreadGoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

pub const MAX_THREAD_GOAL_OBJECTIVE_CHARS: usize = 4_000;

pub fn validate_thread_goal_objective(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("goal objective must not be empty".to_string());
    }
    if value.chars().count() > MAX_THREAD_GOAL_OBJECTIVE_CHARS {
        return Err(format!(
            "goal objective must be at most {MAX_THREAD_GOAL_OBJECTIVE_CHARS} characters"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub struct ThreadGoal {
    pub thread_id: ThreadId,
    pub objective: String,
    pub status: ThreadGoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub struct ThreadGoalUpdatedEvent {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    pub goal: ThreadGoal,
}

/// User's decision in response to an ExecApprovalRequest.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Display, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// User has approved this command and the agent should execute it.
    Approved,

    /// User has approved this command and wants to apply the proposed execpolicy
    /// amendment so future matching commands are permitted.
    ApprovedExecpolicyAmendment {
        proposed_execpolicy_amendment: ExecPolicyAmendment,
    },

    /// User has approved this request and wants future prompts in the same
    /// session-scoped approval cache to be automatically approved for the
    /// remainder of the session.
    ApprovedForSession,

    /// User chose to persist a network policy rule (allow/deny) for future
    /// requests to the same host.
    NetworkPolicyAmendment {
        network_policy_amendment: NetworkPolicyAmendment,
    },

    /// User has denied this command and the agent should not execute it, but
    /// it should continue the session and try something else.
    Denied { rejection: String },

    /// Automatic approval review timed out before reaching a decision.
    TimedOut,

    /// User has denied this command and the agent should not do anything until
    /// the user's next command.
    Abort,
}

impl Default for ReviewDecision {
    fn default() -> Self {
        Self::Denied {
            rejection: "denied".to_string(),
        }
    }
}

impl ReviewDecision {
    pub fn denied(rejection: impl Into<String>) -> Self {
        Self::Denied {
            rejection: rejection.into(),
        }
    }

    /// Returns an opaque version of the decision without PII. We can't use an ignored flag
    /// on `serde` because the serialization is required by some surfaces.
    pub fn to_opaque_string(&self) -> &'static str {
        match self {
            ReviewDecision::Approved => "approved",
            ReviewDecision::ApprovedExecpolicyAmendment { .. } => "approved_with_amendment",
            ReviewDecision::ApprovedForSession => "approved_for_session",
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } => match network_policy_amendment.action {
                NetworkPolicyRuleAction::Allow => "approved_with_network_policy_allow",
                NetworkPolicyRuleAction::Deny => "denied_with_network_policy_deny",
            },
            ReviewDecision::Denied { .. } => "denied",
            ReviewDecision::TimedOut => "timed_out",
            ReviewDecision::Abort => "abort",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum FileChange {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct Chunk {
    /// 1-based line index of the first line in the original file
    pub orig_index: u32,
    pub deleted_lines: Vec<String>,
    pub inserted_lines: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnAbortedEvent {
    pub turn_id: Option<String>,
    pub reason: TurnAbortReason,
    /// Unix timestamp (in seconds) when the turn started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub started_at: Option<i64>,
    /// Unix timestamp (in seconds) when the turn was aborted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub completed_at: Option<i64>,
    /// Duration between turn start and abort in milliseconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnAbortReason {
    Interrupted,
    Replaced,
    ReviewEnded,
    BudgetLimited,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentSpawnBeginEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// Initial prompt sent to the agent. Can be empty to prevent CoT leaking at the
    /// beginning.
    pub prompt: String,
    pub model: String,
    pub reasoning_effort: ReasoningEffortConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct CollabAgentRef {
    /// Thread ID of the receiver/new agent.
    pub thread_id: ThreadId,
    /// Optional nickname assigned to an AgentControl-spawned sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    /// Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.
    #[serde(default, alias = "agent_type", skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct CollabAgentStatusEntry {
    /// Thread ID of the receiver/new agent.
    pub thread_id: ThreadId,
    /// Optional nickname assigned to an AgentControl-spawned sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    /// Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.
    #[serde(default, alias = "agent_type", skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    /// Last known status of the agent.
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentSpawnEndEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// Thread ID of the newly spawned agent, if it was created.
    pub new_thread_id: Option<ThreadId>,
    /// Optional nickname assigned to the new agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_agent_nickname: Option<String>,
    /// Optional role assigned to the new agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_agent_role: Option<String>,
    /// Initial prompt sent to the agent. Can be empty to prevent CoT leaking at the
    /// beginning.
    pub prompt: String,
    /// Effective model used by the spawned agent after inheritance and role overrides.
    pub model: String,
    /// Effective reasoning effort used by the spawned agent after inheritance and role overrides.
    pub reasoning_effort: ReasoningEffortConfig,
    /// Last known status of the new agent reported to the sender agent.
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentInteractionBeginEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// Thread ID of the receiver.
    pub receiver_thread_id: ThreadId,
    /// Prompt sent from the sender to the receiver. Can be empty to prevent CoT
    /// leaking at the beginning.
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentInteractionEndEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// Thread ID of the receiver.
    pub receiver_thread_id: ThreadId,
    /// Optional nickname assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    /// Optional role assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
    /// Prompt sent from the sender to the receiver. Can be empty to prevent CoT
    /// leaking at the beginning.
    pub prompt: String,
    /// Last known status of the receiver agent reported to the sender agent.
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SubAgentActivityKind {
    Started,
    Interacted,
    Interrupted,
    Completed,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default)]
pub struct SubAgentChangeTotals {
    pub files_edited: usize,
    pub total_added: usize,
    pub total_removed: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct SubAgentActivityEvent {
    pub event_id: String,
    #[serde(default)]
    pub occurred_at_ms: i64,
    /// Thread ID of the affected sub-agent.
    pub agent_thread_id: ThreadId,
    /// Canonical v2 path of the affected sub-agent.
    pub agent_path: AgentPath,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffortConfig>,
    pub kind: SubAgentActivityKind,
    /// Human-readable summary of what the sub-agent is currently doing.
    /// Spawn and periodic heartbeat events report a non-empty working state;
    /// generated summaries replace that state when available.
    /// Max 64 characters.
    #[serde(default)]
    pub current_activity: Option<String>,
    #[serde(default)]
    pub change_totals: Option<SubAgentChangeTotals>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabWaitingBeginEvent {
    #[serde(default)]
    pub started_at_ms: i64,
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// Thread ID of the receivers.
    pub receiver_thread_ids: Vec<ThreadId>,
    /// Optional nicknames/roles for receivers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receiver_agents: Vec<CollabAgentRef>,
    /// ID of the waiting call.
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabWaitingEndEvent {
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// ID of the waiting call.
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    /// Optional receiver metadata paired with final statuses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_statuses: Vec<CollabAgentStatusEntry>,
    /// Last known status of the receiver agents reported to the sender agent.
    pub statuses: HashMap<ThreadId, AgentStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabCloseBeginEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// Thread ID of the receiver.
    pub receiver_thread_id: ThreadId,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabCloseEndEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// Thread ID of the receiver.
    pub receiver_thread_id: ThreadId,
    /// Optional nickname assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    /// Optional role assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
    /// Last known status of the receiver agent reported to the sender agent before
    /// the close.
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabResumeBeginEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// Thread ID of the receiver.
    pub receiver_thread_id: ThreadId,
    /// Optional nickname assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    /// Optional role assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabResumeEndEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    /// Thread ID of the sender.
    pub sender_thread_id: ThreadId,
    /// Thread ID of the receiver.
    pub receiver_thread_id: ThreadId,
    /// Optional nickname assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    /// Optional role assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
    /// Last known status of the receiver agent reported to the sender agent after
    /// resume.
    pub status: AgentStatus,
}
