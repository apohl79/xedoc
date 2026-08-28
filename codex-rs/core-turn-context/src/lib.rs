//! Per-turn state shared by session orchestration and tool execution.

#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_config::NetworkDomainPermissionsToml;
use codex_core_config::config::Config;
use codex_core_config::config::Constrained;
use codex_core_environment::TurnEnvironment;
use codex_core_environment::TurnEnvironmentSnapshot;
use codex_core_skills::HostSkillsSnapshot;
use codex_core_turn_metadata::TurnMetadataState;
use codex_core_turn_timing::TurnTimingState;
use codex_extension_api::ExtensionData;
use codex_features::Feature;
use codex_file_system::FileSystemSandboxContext;
use codex_login::AuthManager;
use codex_model_provider::SharedModelProvider;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_network_proxy::NetworkProxy;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::config_types::Settings;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::protocol::TurnContextNetworkItem;
use codex_sandboxing::compatibility_sandbox_policy_for_permission_profile;
use codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy;
use codex_sandboxing::policy_transforms::effective_network_sandbox_policy;
use codex_tools::UnifiedExecShellMode;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;
use tokio::sync::Mutex;

/// Per-turn skills snapshot and implicit-invocation state.
#[derive(Clone, Debug)]
pub struct TurnSkillsContext {
    /// Workspace-internal snapshot used while building model input.
    pub snapshot: HostSkillsSnapshot,
    /// Workspace-internal state preventing repeated implicit invocations.
    pub implicit_invocation_seen_skills: Arc<Mutex<HashSet<String>>>,
}

impl TurnSkillsContext {
    /// Creates the skills state for a newly constructed turn.
    pub fn new(snapshot: HostSkillsSnapshot) -> Self {
        Self {
            snapshot,
            implicit_invocation_seen_skills: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

/// The context needed for a single turn of the thread.
#[derive(Debug)]
pub struct TurnContext {
    /// Workspace-internal turn identifier.
    pub sub_id: String,
    /// Workspace-internal trace identifier.
    pub trace_id: Option<String>,
    /// Workspace-internal realtime state.
    pub realtime_active: bool,
    /// Effective configuration for this turn.
    pub config: Arc<Config>,
    /// Workspace-internal authentication manager.
    pub auth_manager: Option<Arc<AuthManager>>,
    /// Workspace-internal resolved model metadata.
    pub model_info: ModelInfo,
    /// Workspace-internal telemetry handle.
    pub session_telemetry: SessionTelemetry,
    /// Workspace-internal model provider.
    pub provider: SharedModelProvider,
    /// Workspace-internal reasoning setting.
    pub reasoning_effort: Option<ReasoningEffortConfig>,
    /// Workspace-internal reasoning-summary setting.
    pub reasoning_summary: ReasoningSummaryConfig,
    /// Workspace-internal session source.
    pub session_source: SessionSource,
    /// Workspace-internal history mode.
    pub history_mode: ThreadHistoryMode,
    /// Workspace-internal parent thread identifier.
    pub parent_thread_id: Option<ThreadId>,
    /// Workspace-internal thread originator.
    pub originator: String,
    /// Workspace-internal selected environments.
    pub environments: TurnEnvironmentSnapshot,
    /// Workspace-internal compatibility working directory.
    #[deprecated(note = "use the selected turn environment cwd instead")]
    pub cwd: AbsolutePathBuf,
    /// Workspace-internal local date.
    pub current_date: Option<String>,
    /// Workspace-internal local timezone.
    pub timezone: Option<String>,
    /// Workspace-internal app-server client name.
    pub app_server_client_name: Option<String>,
    /// Workspace-internal developer instructions.
    pub developer_instructions: Option<String>,
    /// Workspace-internal collaboration mode.
    pub mode: ModeKind,
    /// Workspace-internal collaboration developer instructions.
    pub collaboration_mode_developer_instructions: Option<String>,
    /// Workspace-internal multi-agent protocol version.
    pub multi_agent_version: MultiAgentVersion,
    /// Workspace-internal personality selection.
    pub personality: Option<Personality>,
    /// Workspace-internal approval policy.
    pub approval_policy: Constrained<AskForApproval>,
    /// Workspace-internal permission profile.
    pub permission_profile: PermissionProfile,
    /// Workspace-internal network proxy.
    pub network: Option<NetworkProxy>,
    /// Workspace-internal Windows sandbox level.
    pub windows_sandbox_level: WindowsSandboxLevel,
    /// Workspace-internal available-model catalog.
    pub available_models: Vec<ModelPreset>,
    /// Workspace-internal Unified Exec shell mode.
    pub unified_exec_shell_mode: UnifiedExecShellMode,
    /// Workspace-internal final-output schema.
    pub final_output_json_schema: Option<Value>,
    /// Workspace-internal dynamic tools.
    pub dynamic_tools: Vec<DynamicToolSpec>,
    /// Workspace-internal turn metadata.
    pub turn_metadata_state: Arc<TurnMetadataState>,
    /// Workspace-internal extension state.
    pub extension_data: Arc<ExtensionData>,
    /// Workspace-internal skills state.
    pub turn_skills: TurnSkillsContext,
    /// Workspace-internal timing state.
    pub turn_timing_state: Arc<TurnTimingState>,
    /// Workspace-internal terminal error state.
    pub terminal_error: Arc<Mutex<Option<ErrorEvent>>>,
    /// Workspace-internal model warning state.
    pub server_model_warning_emitted: AtomicBool,
    /// Workspace-internal model verification state.
    pub model_verification_emitted: AtomicBool,
}

impl TurnContext {
    /// Whether model response items need stable identifiers.
    pub fn item_ids_enabled(&self) -> bool {
        self.config.features.enabled(Feature::ItemIds)
            || matches!(self.history_mode, ThreadHistoryMode::Paginated)
    }

    /// Resolves the effective collaboration settings for this turn.
    pub fn collaboration_mode(&self) -> CollaborationMode {
        CollaborationMode {
            mode: self.mode,
            settings: Settings {
                model: self.model_info.slug.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
                developer_instructions: self.collaboration_mode_developer_instructions.clone(),
            },
        }
    }

    /// Returns the active permission profile.
    pub fn permission_profile(&self) -> PermissionProfile {
        self.permission_profile.clone()
    }

    /// Returns the active filesystem sandbox policy.
    pub fn file_system_sandbox_policy(&self) -> FileSystemSandboxPolicy {
        self.permission_profile.file_system_sandbox_policy()
    }

    /// Returns the active network sandbox policy.
    pub fn network_sandbox_policy(&self) -> NetworkSandboxPolicy {
        self.permission_profile.network_sandbox_policy()
    }

    /// Returns the compatibility sandbox policy.
    pub fn sandbox_policy(&self) -> SandboxPolicy {
        compatibility_sandbox_policy_for_permission_profile(
            &self.permission_profile,
            #[allow(deprecated)]
            &self.cwd,
        )
    }

    /// Resolves reasoning effort from the explicit setting or model default.
    pub fn effective_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        self.reasoning_effort
            .clone()
            .or_else(|| self.model_info.default_reasoning_level.clone())
    }

    /// Returns a tracing-friendly reasoning effort label.
    pub fn effective_reasoning_effort_for_tracing(&self) -> String {
        self.effective_reasoning_effort()
            .map(|effort| effort.to_string())
            .unwrap_or_else(|| "default".to_string())
    }

    /// Returns the effective context window after applying the percentage cap.
    pub fn model_context_window(&self) -> Option<i64> {
        let effective_context_window_percent = self.model_info.effective_context_window_percent;
        self.model_info
            .resolved_context_window()
            .map(|context_window| {
                context_window.saturating_mul(effective_context_window_percent) / 100
            })
    }

    /// Whether Codex Apps are enabled for the current authentication state.
    pub fn apps_enabled(&self) -> bool {
        let uses_codex_backend = self
            .auth_manager
            .as_deref()
            .is_some_and(AuthManager::current_auth_uses_codex_backend);
        self.config
            .features
            .apps_enabled_for_auth(uses_codex_backend)
            && self.config.orchestrator_mcp_enabled
    }

    /// Clones this turn with model-specific settings.
    pub async fn with_model(&self, model: String, models_manager: &SharedModelsManager) -> Self {
        let mut config = (*self.config).clone();
        config.model = Some(model.clone());
        let model_info = models_manager
            .get_model_info_for_provider(
                model.as_str(),
                config.model_provider_id.as_str(),
                &config.to_models_manager_config(),
            )
            .await;
        let supported_reasoning_levels = model_info
            .supported_reasoning_levels
            .iter()
            .map(|preset| preset.effort.clone())
            .collect::<Vec<_>>();
        let reasoning_effort = if let Some(current_reasoning_effort) = self.reasoning_effort.clone()
        {
            if supported_reasoning_levels.contains(&current_reasoning_effort) {
                Some(current_reasoning_effort)
            } else {
                supported_reasoning_levels
                    .get(supported_reasoning_levels.len().saturating_sub(1) / 2)
                    .cloned()
                    .or_else(|| model_info.default_reasoning_level.clone())
            }
        } else {
            supported_reasoning_levels
                .get(supported_reasoning_levels.len().saturating_sub(1) / 2)
                .cloned()
                .or_else(|| model_info.default_reasoning_level.clone())
        };
        config.model_reasoning_effort = reasoning_effort.clone();

        let available_models = models_manager
            .list_models(
                RefreshStrategy::OnlineIfUncached,
                config.http_client_factory(),
            )
            .await;

        Self {
            sub_id: self.sub_id.clone(),
            trace_id: self.trace_id.clone(),
            realtime_active: self.realtime_active,
            config: Arc::new(config),
            auth_manager: self.auth_manager.clone(),
            model_info: model_info.clone(),
            session_telemetry: self
                .session_telemetry
                .clone()
                .with_model(model.as_str(), model_info.slug.as_str()),
            provider: self.provider.clone(),
            reasoning_effort,
            reasoning_summary: self.reasoning_summary,
            session_source: self.session_source.clone(),
            history_mode: self.history_mode,
            parent_thread_id: self.parent_thread_id,
            originator: self.originator.clone(),
            environments: self.environments.clone(),
            #[allow(deprecated)]
            cwd: self.cwd.clone(),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            app_server_client_name: self.app_server_client_name.clone(),
            developer_instructions: self.developer_instructions.clone(),
            mode: self.mode,
            collaboration_mode_developer_instructions: self
                .collaboration_mode_developer_instructions
                .clone(),
            multi_agent_version: self.multi_agent_version,
            personality: self.personality,
            approval_policy: self.approval_policy.clone(),
            permission_profile: self.permission_profile.clone(),
            network: self.network.clone(),
            windows_sandbox_level: self.windows_sandbox_level,
            available_models,
            unified_exec_shell_mode: self.unified_exec_shell_mode.clone(),
            final_output_json_schema: self.final_output_json_schema.clone(),
            dynamic_tools: self.dynamic_tools.clone(),
            turn_metadata_state: self.turn_metadata_state.clone(),
            extension_data: Arc::clone(&self.extension_data),
            turn_skills: self.turn_skills.clone(),
            turn_timing_state: Arc::clone(&self.turn_timing_state),
            terminal_error: Arc::clone(&self.terminal_error),
            server_model_warning_emitted: AtomicBool::new(
                self.server_model_warning_emitted.load(Ordering::Relaxed),
            ),
            model_verification_emitted: AtomicBool::new(
                self.model_verification_emitted.load(Ordering::Relaxed),
            ),
        }
    }

    /// Builds filesystem sandbox input for a selected turn environment.
    pub fn file_system_sandbox_context(
        &self,
        additional_permissions: Option<AdditionalPermissionProfile>,
        environment: &TurnEnvironment,
    ) -> FileSystemSandboxContext {
        let permission_profile = self.config.permissions.permission_profile();
        let (base_file_system_sandbox_policy, base_network_sandbox_policy) =
            permission_profile.to_runtime_permissions();
        let file_system_sandbox_policy = effective_file_system_sandbox_policy(
            &base_file_system_sandbox_policy,
            additional_permissions.as_ref(),
        );
        let network_sandbox_policy = effective_network_sandbox_policy(
            base_network_sandbox_policy,
            additional_permissions.as_ref(),
        );
        let permissions = PermissionProfile::from_runtime_permissions_with_enforcement(
            permission_profile.enforcement(),
            &file_system_sandbox_policy,
            network_sandbox_policy,
        );
        FileSystemSandboxContext {
            permissions: permissions.into(),
            cwd: Some(environment.cwd().clone()),
            workspace_roots: environment.workspace_roots().to_vec(),
            windows_sandbox_level: self.windows_sandbox_level,
            windows_sandbox_private_desktop: self
                .config
                .permissions
                .windows_sandbox_private_desktop,
            use_legacy_landlock: self.config.features.use_legacy_landlock(),
        }
    }

    fn non_legacy_file_system_sandbox_policy(&self) -> Option<FileSystemSandboxPolicy> {
        let legacy_file_system_sandbox_policy =
            FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
                &self.sandbox_policy(),
                #[allow(deprecated)]
                &self.cwd,
            );
        let file_system_sandbox_policy = self.file_system_sandbox_policy();
        (file_system_sandbox_policy != legacy_file_system_sandbox_policy)
            .then_some(file_system_sandbox_policy)
    }

    /// Converts this turn to its persisted model-visible context item.
    pub fn to_turn_context_item(&self) -> TurnContextItem {
        let workspace_roots = self.config.effective_workspace_roots();
        #[allow(deprecated)]
        let cwd = self.cwd.clone();
        TurnContextItem {
            turn_id: Some(self.sub_id.clone()),
            cwd,
            workspace_roots: (!workspace_roots.is_empty()).then_some(workspace_roots),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            approval_policy: self.approval_policy.value(),
            approvals_reviewer: Some(self.config.approvals_reviewer),
            sandbox_policy: self.sandbox_policy(),
            permission_profile: Some(self.permission_profile()),
            network: self.turn_context_network_item(),
            file_system_sandbox_policy: self.non_legacy_file_system_sandbox_policy(),
            model: self.model_info.slug.clone(),
            comp_hash: self.model_info.comp_hash.clone(),
            personality: self.personality,
            collaboration_mode: Some(self.collaboration_mode()),
            multi_agent_version: Some(self.multi_agent_version),
            multi_agent_mode: effective_multi_agent_mode(self),
            realtime_active: Some(self.realtime_active),
            effort: self.reasoning_effort.clone(),
            summary: ReasoningSummaryConfig::Auto,
        }
    }

    fn turn_context_network_item(&self) -> Option<TurnContextNetworkItem> {
        let network = self
            .config
            .config_layer_stack
            .requirements()
            .network
            .as_ref()?;
        Some(TurnContextNetworkItem {
            allowed_domains: network
                .domains
                .as_ref()
                .and_then(NetworkDomainPermissionsToml::allowed_domains)
                .unwrap_or_default(),
            denied_domains: network
                .domains
                .as_ref()
                .and_then(NetworkDomainPermissionsToml::denied_domains)
                .unwrap_or_default(),
        })
    }
}

/// Derives the effective multi-agent mode for a model-visible turn context.
pub fn effective_multi_agent_mode(
    turn_context: &TurnContext,
) -> Option<codex_protocol::config_types::MultiAgentMode> {
    use codex_protocol::config_types::MultiAgentMode;
    use codex_protocol::openai_models::ReasoningEffort;

    if turn_context.multi_agent_version != MultiAgentVersion::V2 {
        return None;
    }

    let multi_agent_mode = match &turn_context
        .config
        .multi_agent_v2
        .multi_agent_mode_hint_text
    {
        Some(hint_text) => MultiAgentMode::Custom(hint_text.clone()),
        None => match turn_context.effective_reasoning_effort() {
            Some(ReasoningEffort::Ultra) => MultiAgentMode::Proactive,
            _ => MultiAgentMode::ExplicitRequestOnly,
        },
    };

    match &turn_context.session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
        | SessionSource::Cli
        | SessionSource::VSCode
        | SessionSource::Exec
        | SessionSource::Mcp
        | SessionSource::Custom(_)
        | SessionSource::Unknown => Some(multi_agent_mode),
        SessionSource::Internal(_) | SessionSource::SubAgent(_) => None,
    }
}
