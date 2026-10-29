//! App-server session facade used by the TUI event loop.
//!
//! This module owns the typed JSON-RPC calls needed by the TUI and keeps
//! request/response plumbing out of `App` and `ChatWidget`.

mod fs;
mod permission_compat;
mod terminal_visualization_instructions;

use color_eyre::eyre::ContextCompat;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use permission_compat::legacy_compatible_permission_profile;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use terminal_visualization_instructions::with_terminal_visualization_instructions;
use uuid::Uuid;
use xedoc_app_server_client::AppServerClient;
use xedoc_app_server_client::AppServerEvent;
use xedoc_app_server_client::AppServerPath;
use xedoc_app_server_client::AppServerRequestHandle;
use xedoc_app_server_client::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
use xedoc_app_server_client::RemoteAppServerClient;
use xedoc_app_server_client::RemoteAppServerConnectArgs;
use xedoc_app_server_client::RemoteAppServerEndpoint;
use xedoc_app_server_client::TypedRequestError;
use xedoc_app_server_protocol::Account;
use xedoc_app_server_protocol::AskForApproval;
use xedoc_app_server_protocol::AuthMode;
use xedoc_app_server_protocol::ClientRequest;
use xedoc_app_server_protocol::ConfigBatchWriteParams;
use xedoc_app_server_protocol::ConfigRequirementsReadResponse;
use xedoc_app_server_protocol::ConfigWriteResponse;
use xedoc_app_server_protocol::GetAccountParams;
use xedoc_app_server_protocol::GetAccountResponse;
use xedoc_app_server_protocol::JSONRPCErrorError;
use xedoc_app_server_protocol::LogoutAccountResponse;
use xedoc_app_server_protocol::Model as ApiModel;
use xedoc_app_server_protocol::ModelListParams;
use xedoc_app_server_protocol::ModelListResponse;
use xedoc_app_server_protocol::ModelManagerReadParams;
use xedoc_app_server_protocol::ModelManagerReadResponse;
use xedoc_app_server_protocol::NewThreadModelDefaults;
use xedoc_app_server_protocol::RemoteSessionAttachParams;
use xedoc_app_server_protocol::RemoteSessionAttachResponse;
use xedoc_app_server_protocol::RemoteSessionCancelParams;
use xedoc_app_server_protocol::RemoteSessionCancelResponse;
use xedoc_app_server_protocol::RemoteSessionInputParams;
use xedoc_app_server_protocol::RemoteSessionInputResponse;
use xedoc_app_server_protocol::RemoteSessionListParams;
use xedoc_app_server_protocol::RemoteSessionListResponse;
use xedoc_app_server_protocol::RemoteSessionReadParams;
use xedoc_app_server_protocol::RemoteSessionReadResponse;
use xedoc_app_server_protocol::RequestId;
use xedoc_app_server_protocol::ReviewDelivery;
use xedoc_app_server_protocol::ReviewStartParams;
use xedoc_app_server_protocol::ReviewStartResponse;
use xedoc_app_server_protocol::ReviewTarget;
use xedoc_app_server_protocol::SessionExtensionListParams;
use xedoc_app_server_protocol::SessionExtensionListResponse;
use xedoc_app_server_protocol::SessionSource;
use xedoc_app_server_protocol::SkillsListParams;
use xedoc_app_server_protocol::SkillsListResponse;
use xedoc_app_server_protocol::SortDirection;
use xedoc_app_server_protocol::Thread;
use xedoc_app_server_protocol::ThreadArchiveParams;
use xedoc_app_server_protocol::ThreadArchiveResponse;
use xedoc_app_server_protocol::ThreadBackgroundTerminalsCleanParams;
use xedoc_app_server_protocol::ThreadBackgroundTerminalsCleanResponse;
use xedoc_app_server_protocol::ThreadCompactStartParams;
use xedoc_app_server_protocol::ThreadCompactStartResponse;
use xedoc_app_server_protocol::ThreadDeleteParams;
use xedoc_app_server_protocol::ThreadDeleteResponse;
use xedoc_app_server_protocol::ThreadForkParams;
use xedoc_app_server_protocol::ThreadForkResponse;
use xedoc_app_server_protocol::ThreadGoalClearParams;
use xedoc_app_server_protocol::ThreadGoalClearResponse;
use xedoc_app_server_protocol::ThreadGoalGetParams;
use xedoc_app_server_protocol::ThreadGoalGetResponse;
use xedoc_app_server_protocol::ThreadGoalSetParams;
use xedoc_app_server_protocol::ThreadGoalSetResponse;
use xedoc_app_server_protocol::ThreadGoalStatus;
use xedoc_app_server_protocol::ThreadHistoryMode;
use xedoc_app_server_protocol::ThreadInjectItemsParams;
use xedoc_app_server_protocol::ThreadInjectItemsResponse;
use xedoc_app_server_protocol::ThreadListParams;
use xedoc_app_server_protocol::ThreadListResponse;
use xedoc_app_server_protocol::ThreadLoadedListParams;
use xedoc_app_server_protocol::ThreadLoadedListResponse;
use xedoc_app_server_protocol::ThreadMetadataGitInfoUpdateParams;
use xedoc_app_server_protocol::ThreadMetadataUpdateParams;
use xedoc_app_server_protocol::ThreadMetadataUpdateResponse;
use xedoc_app_server_protocol::ThreadReadParams;
use xedoc_app_server_protocol::ThreadReadResponse;
use xedoc_app_server_protocol::ThreadResumeInitialTurnsPageParams;
use xedoc_app_server_protocol::ThreadResumeParams;
use xedoc_app_server_protocol::ThreadResumeResponse;
use xedoc_app_server_protocol::ThreadSetNameParams;
use xedoc_app_server_protocol::ThreadSetNameResponse;
use xedoc_app_server_protocol::ThreadSettingsUpdateParams;
use xedoc_app_server_protocol::ThreadSettingsUpdateResponse;
use xedoc_app_server_protocol::ThreadShellCommandParams;
use xedoc_app_server_protocol::ThreadShellCommandResponse;
use xedoc_app_server_protocol::ThreadSource;
use xedoc_app_server_protocol::ThreadStartParams;
use xedoc_app_server_protocol::ThreadStartResponse;
use xedoc_app_server_protocol::ThreadStartSource;
use xedoc_app_server_protocol::ThreadTurnsListParams;
use xedoc_app_server_protocol::ThreadTurnsListResponse;
use xedoc_app_server_protocol::ThreadUnarchiveParams;
use xedoc_app_server_protocol::ThreadUnarchiveResponse;
use xedoc_app_server_protocol::ThreadUnsubscribeParams;
use xedoc_app_server_protocol::ThreadUnsubscribeResponse;
use xedoc_app_server_protocol::Turn;
use xedoc_app_server_protocol::TurnInterruptParams;
use xedoc_app_server_protocol::TurnInterruptResponse;
use xedoc_app_server_protocol::TurnItemsView;
use xedoc_app_server_protocol::TurnStartParams;
use xedoc_app_server_protocol::TurnStartResponse;
use xedoc_app_server_protocol::TurnSteerCancelParams;
use xedoc_app_server_protocol::TurnSteerCancelResponse;
use xedoc_app_server_protocol::TurnSteerParams;
use xedoc_app_server_protocol::TurnSteerResponse;
use xedoc_app_server_protocol::TurnsPage;
use xedoc_app_server_protocol::UserInput;
use xedoc_core_config::config::Config;
use xedoc_otel::TelemetryAuthMode;
use xedoc_protocol::ThreadId;
use xedoc_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use xedoc_protocol::models::ActivePermissionProfile;
use xedoc_protocol::models::PermissionProfile;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::openai_models::ModelAvailabilityNux;
use xedoc_protocol::openai_models::ModelPreset;
use xedoc_protocol::openai_models::ModelServiceTier;
use xedoc_protocol::openai_models::ModelUpgrade;
use xedoc_protocol::openai_models::ReasoningEffortPreset;
use xedoc_protocol::protocol::SubAgentSource;
pub use xedoc_tui_events::AppServerStartedThread;
use xedoc_tui_settings::service_tier_resolution;
use xedoc_tui_status::status::StatusAccountDisplay;
use xedoc_tui_status::status::plan_type_display_name;
use xedoc_tui_transcript::session_state::MessageHistoryMetadata;
use xedoc_tui_transcript::session_state::ThreadSessionState;
use xedoc_tui_transcript::version::xedoc_cli_version;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_path_uri::PathUri;

const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const THREAD_SETTINGS_UPDATE_METHOD: &str = "thread/settings/update";
const REMOTE_INITIAL_TURNS_PAGE_LIMIT: u32 = 25;
pub async fn connect_remote_app_server(
    endpoint: RemoteAppServerEndpoint,
) -> color_eyre::Result<AppServerClient> {
    let app_server = RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
        endpoint,
        client_name: "xedoc-tui".to_string(),
        client_version: xedoc_cli_version().to_string(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    })
    .await
    .wrap_err("failed to connect to remote app server")?;
    Ok(AppServerClient::Remote(app_server))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForkGoalContinuation {
    StartIfIdle,
    DeferUntilNextTurn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkPresentation {
    Regular,
    SideConversation,
}

fn bootstrap_request_error(context: &'static str, err: TypedRequestError) -> color_eyre::Report {
    color_eyre::eyre::eyre!("{context}: {err}")
}

fn is_thread_settings_update_unsupported(source: &JSONRPCErrorError) -> bool {
    source.code == JSONRPC_METHOD_NOT_FOUND
        || (source.code == JSONRPC_INVALID_REQUEST
            && source.message.contains(THREAD_SETTINGS_UPDATE_METHOD))
}

/// Data collected during the TUI bootstrap phase that the main event loop
/// needs to configure the UI, telemetry, and initial rate-limit prefetch.
///
/// Rate-limit snapshots are intentionally **not** included here; they are
/// fetched asynchronously after bootstrap returns so that the TUI can render
/// its first frame without waiting for the rate-limit round-trip.
pub struct AppServerBootstrap {
    pub duration: Duration,
    pub account_email: Option<String>,
    pub auth_mode: Option<TelemetryAuthMode>,
    pub status_account_display: Option<StatusAccountDisplay>,
    pub plan_type: Option<xedoc_protocol::account::PlanType>,
    pub default_model: String,
    pub has_chatgpt_account: bool,
    pub available_models: Vec<ModelPreset>,
}

pub struct AppServerSession {
    client: AppServerClient,
    next_request_id: i64,
    remote_cwd_override: Option<PathBuf>,
    thread_params_mode: ThreadParamsMode,
    thread_settings_update_supported: bool,
    synced_environment_variables: HashMap<String, BTreeMap<String, String>>,
    default_model: Option<String>,
    available_models: Vec<ModelPreset>,
    managed_new_thread_defaults: Option<NewThreadModelDefaults>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadParamsMode {
    Embedded,
    Remote,
}

/// Determines where model settings come from when resuming a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeModelSettings {
    /// Sends the current config's model, provider, and reasoning effort as explicit overrides.
    OverrideFromCurrentConfig,
    /// Omits those overrides so app-server restores the settings saved with the thread.
    RestoreFromThread,
}

impl ThreadParamsMode {
    fn model_provider_from_config(self, config: &Config) -> Option<String> {
        match self {
            Self::Embedded => Some(config.model_provider_id.clone()),
            Self::Remote => None,
        }
    }
}

pub fn source_agent_path(source: &SessionSource) -> Option<String> {
    match source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_path, .. }) => {
            agent_path.clone().map(String::from)
        }
        _ => None,
    }
}

/// Uses the server capability when available and preserves compatibility with older servers.
pub fn thread_blocks_direct_input(thread: &Thread) -> bool {
    thread
        .can_accept_direct_input
        .map(|can_accept| !can_accept)
        .unwrap_or_else(|| source_agent_path(&thread.source).is_some())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnPermissionsOverride {
    /// Leave the app-server thread's sticky permission profile unchanged.
    Preserve,
    /// Select a named or built-in profile by id.
    ActiveProfile(ActivePermissionProfile),
    /// Apply a user-selected legacy/custom permission profile.
    LegacySandbox(PermissionProfile),
}

impl AppServerSession {
    pub fn new(client: AppServerClient, thread_params_mode: ThreadParamsMode) -> Self {
        Self {
            client,
            next_request_id: 1,
            remote_cwd_override: None,
            thread_params_mode,
            thread_settings_update_supported: true,
            synced_environment_variables: HashMap::new(),
            default_model: None,
            available_models: Vec::new(),
            managed_new_thread_defaults: None,
        }
    }

    pub fn with_remote_cwd_override(mut self, remote_cwd_override: Option<PathBuf>) -> Self {
        self.remote_cwd_override = remote_cwd_override;
        self
    }

    pub fn remote_cwd_override(&self) -> Option<&std::path::Path> {
        self.remote_cwd_override.as_deref()
    }

    pub fn uses_remote_workspace(&self) -> bool {
        matches!(self.thread_params_mode, ThreadParamsMode::Remote)
    }

    pub fn uses_embedded_app_server(&self) -> bool {
        matches!(&self.client, AppServerClient::InProcess(_))
    }

    pub async fn reconnect_remote(&mut self, endpoint: RemoteAppServerEndpoint) -> Result<()> {
        let client = connect_remote_app_server(endpoint).await?;
        let disconnected_client = std::mem::replace(&mut self.client, client);
        if let Err(err) = disconnected_client.shutdown().await {
            tracing::debug!(%err, "failed to shut down disconnected remote app server client");
        }
        Ok(())
    }

    pub fn xedoc_home_path(&self, local_xedoc_home: &AbsolutePathBuf) -> Option<AppServerPath> {
        self.client.xedoc_home(local_xedoc_home)
    }

    pub fn server_version(&self) -> Option<&str> {
        let AppServerClient::Remote(client) = &self.client else {
            return None;
        };
        client.server_version()
    }

    #[tracing::instrument(level = "info", skip_all)]
    pub async fn bootstrap(&mut self, config: &Config) -> Result<AppServerBootstrap> {
        let started_at = Instant::now();
        let account_started_at = Instant::now();
        let account = self.read_account().await?;
        tracing::info!(
            duration_ms = %account_started_at.elapsed().as_millis(),
            authenticated = account.account.is_some(),
            "tui bootstrap account loaded"
        );
        // `hooks/list` holds the global config queue during startup. Submit models and config
        // requirements together so an uncached model fetch can overlap both config requests.
        let catalog_started_at = Instant::now();
        let model_request_id = self.next_request_id();
        let requirements_request_id = self.next_request_id();
        let (models, requirements) = tokio::try_join!(
            async {
                self.client
                    .request_typed::<ModelListResponse>(ClientRequest::ModelList {
                        request_id: model_request_id,
                        params: ModelListParams {
                            cursor: None,
                            limit: None,
                            include_hidden: Some(true),
                        },
                    })
                    .await
                    .map_err(|err| {
                        bootstrap_request_error("model/list failed during TUI bootstrap", err)
                    })
            },
            async {
                self.client
                    .request_typed::<ConfigRequirementsReadResponse>(
                        ClientRequest::ConfigRequirementsRead {
                            request_id: requirements_request_id,
                            params: None,
                        },
                    )
                    .await
                    .map_err(|err| {
                        bootstrap_request_error(
                            "configRequirements/read failed during TUI bootstrap",
                            err,
                        )
                    })
            },
        )?;
        tracing::info!(
            duration_ms = %catalog_started_at.elapsed().as_millis(),
            model_count = models.data.len(),
            has_model_requirements = requirements.requirements.is_some(),
            "tui bootstrap model catalog and requirements loaded"
        );
        self.managed_new_thread_defaults = requirements
            .requirements
            .and_then(|requirements| requirements.models)
            .and_then(|models| models.new_thread);
        let available_models = models
            .data
            .into_iter()
            .map(model_preset_from_api_model)
            .collect::<Vec<_>>();
        let default_model = config
            .model
            .clone()
            .or_else(|| {
                available_models
                    .iter()
                    .find(|model| model.is_default)
                    .map(|model| model.model.clone())
            })
            .or_else(|| available_models.first().map(|model| model.model.clone()))
            .wrap_err("model/list returned no models for TUI bootstrap")?;
        self.default_model = Some(default_model.clone());
        self.available_models = available_models.clone();
        tracing::info!(
            duration_ms = %started_at.elapsed().as_millis(),
            available_model_count = available_models.len(),
            default_model = %default_model,
            "tui bootstrap completed"
        );

        let (account_email, auth_mode, status_account_display, plan_type, has_chatgpt_account) =
            match account.account {
                Some(Account::ApiKey {}) => (
                    None,
                    Some(TelemetryAuthMode::ApiKey),
                    Some(StatusAccountDisplay::ApiKey),
                    None,
                    false,
                ),
                Some(Account::Chatgpt { email, plan_type }) => (
                    email.clone(),
                    Some(TelemetryAuthMode::Chatgpt),
                    Some(StatusAccountDisplay::ChatGpt {
                        email,
                        plan: Some(plan_type_display_name(plan_type)),
                    }),
                    Some(plan_type),
                    true,
                ),
                Some(Account::AmazonBedrock { .. }) => (None, None, None, None, false),
                None => (None, None, None, None, false),
            };
        Ok(AppServerBootstrap {
            duration: started_at.elapsed(),
            account_email,
            auth_mode,
            status_account_display,
            plan_type,
            default_model,
            has_chatgpt_account,
            available_models,
        })
    }

    pub async fn refresh_models(&mut self) -> Result<Vec<ModelPreset>> {
        let request_id = self.next_request_id();
        let response = self
            .client
            .request_typed::<ModelListResponse>(ClientRequest::ModelList {
                request_id,
                params: ModelListParams {
                    cursor: None,
                    limit: None,
                    include_hidden: Some(true),
                },
            })
            .await
            .map_err(|err| {
                bootstrap_request_error("model/list failed while refreshing models", err)
            })?;
        let models = response
            .data
            .into_iter()
            .map(model_preset_from_api_model)
            .collect::<Vec<_>>();
        self.available_models = models.clone();
        if let Some(default_model) = models
            .iter()
            .find(|model| model.is_default)
            .map(|model| model.model.clone())
            .or_else(|| models.first().map(|model| model.model.clone()))
        {
            self.default_model = Some(default_model);
        }
        Ok(models)
    }

    pub fn managed_new_thread_defaults(&self) -> Option<&NewThreadModelDefaults> {
        self.managed_new_thread_defaults.as_ref()
    }

    /// Fetches the current account info without refreshing the auth token.
    ///
    /// Used by both `bootstrap` (to populate the initial UI) and `get_login_status`
    /// (to check auth mode without the overhead of a full bootstrap).
    pub async fn read_account(&mut self) -> Result<GetAccountResponse> {
        let account_request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::GetAccount {
                request_id: account_request_id,
                params: GetAccountParams {
                    refresh_token: false,
                },
            })
            .await
            .map_err(|err| bootstrap_request_error("account/read failed during TUI bootstrap", err))
    }

    pub async fn read_model_manager(&mut self) -> Result<ModelManagerReadResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ModelManagerRead {
                request_id,
                params: ModelManagerReadParams {},
            })
            .await
            .map_err(|err| {
                bootstrap_request_error("modelManager/read failed during TUI bootstrap", err)
            })
    }

    pub async fn next_event(&mut self) -> Option<AppServerEvent> {
        self.client.next_event().await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn start_thread(&mut self, config: &Config) -> Result<AppServerStartedThread> {
        self.start_thread_with_session_start_source(config, /*session_start_source*/ None)
            .await
    }

    pub async fn start_thread_with_session_start_source(
        &mut self,
        config: &Config,
        session_start_source: Option<ThreadStartSource>,
    ) -> Result<AppServerStartedThread> {
        let request_id = self.next_request_id();
        let session_config = self.session_config_with_effective_service_tier(config);
        let response: ThreadStartResponse = self
            .client
            .request_typed(ClientRequest::ThreadStart {
                request_id,
                params: thread_start_params_from_config(
                    &session_config,
                    self.thread_params_mode(),
                    self.remote_cwd_override.as_deref(),
                    session_start_source,
                ),
            })
            .await
            .map_err(|err| {
                bootstrap_request_error("thread/start failed during TUI bootstrap", err)
            })?;
        self.sync_environment_variables(&response.thread.id).await?;
        started_thread_from_start_response(response, config, self.thread_params_mode()).await
    }

    pub async fn resume_thread(
        &mut self,
        config: Config,
        thread_id: ThreadId,
        model_settings: ResumeModelSettings,
    ) -> Result<AppServerStartedThread> {
        let request_id = self.next_request_id();
        let session_config = self.session_config_with_effective_service_tier(&config);
        let mut params = thread_resume_params_from_config(
            session_config,
            thread_id,
            self.thread_params_mode(),
            self.remote_cwd_override.as_deref(),
            model_settings,
        );
        if matches!(&self.client, AppServerClient::Remote(_)) {
            params.exclude_turns = true;
            params.initial_turns_page = Some(ThreadResumeInitialTurnsPageParams {
                limit: Some(REMOTE_INITIAL_TURNS_PAGE_LIMIT),
                sort_direction: Some(SortDirection::Desc),
                items_view: Some(TurnItemsView::Summary),
            });
        }
        let response: ThreadResumeResponse = self
            .client
            .request_typed(ClientRequest::ThreadResume { request_id, params })
            .await
            .map_err(|err| {
                bootstrap_request_error("thread/resume failed during TUI bootstrap", err)
            })?;
        self.sync_environment_variables(&response.thread.id).await?;
        let fork_parent_title = self
            .fork_parent_title_from_app_server(response.thread.forked_from_id.as_deref())
            .await;
        let mut started =
            started_thread_from_resume_response(response, &config, self.thread_params_mode())
                .await?;
        started.session.fork_parent_title = fork_parent_title;
        Ok(started)
    }

    pub async fn fork_thread(
        &mut self,
        config: Config,
        thread_id: ThreadId,
    ) -> Result<AppServerStartedThread> {
        self.fork_thread_at(
            config,
            thread_id,
            /*last_turn_id*/ None,
            /*before_turn_id*/ None,
            ForkGoalContinuation::StartIfIdle,
        )
        .await
    }

    pub async fn fork_thread_at(
        &mut self,
        config: Config,
        thread_id: ThreadId,
        last_turn_id: Option<String>,
        before_turn_id: Option<String>,
        goal_continuation: ForkGoalContinuation,
    ) -> Result<AppServerStartedThread> {
        self.fork_thread_at_with_presentation(
            config,
            thread_id,
            last_turn_id,
            before_turn_id,
            goal_continuation,
            ForkPresentation::Regular,
        )
        .await
    }

    pub async fn fork_side_thread(
        &mut self,
        config: Config,
        thread_id: ThreadId,
    ) -> Result<AppServerStartedThread> {
        self.fork_thread_at_with_presentation(
            config,
            thread_id,
            /*last_turn_id*/ None,
            /*before_turn_id*/ None,
            ForkGoalContinuation::StartIfIdle,
            ForkPresentation::SideConversation,
        )
        .await
    }

    async fn fork_thread_at_with_presentation(
        &mut self,
        config: Config,
        thread_id: ThreadId,
        last_turn_id: Option<String>,
        before_turn_id: Option<String>,
        goal_continuation: ForkGoalContinuation,
        presentation: ForkPresentation,
    ) -> Result<AppServerStartedThread> {
        let request_id = self.next_request_id();
        let session_config = self.session_config_with_effective_service_tier(&config);
        let response: ThreadForkResponse = self
            .client
            .request_typed(ClientRequest::ThreadFork {
                request_id,
                params: ThreadForkParams {
                    last_turn_id,
                    before_turn_id,
                    defer_goal_continuation: goal_continuation
                        == ForkGoalContinuation::DeferUntilNextTurn,
                    exclude_turns: presentation == ForkPresentation::SideConversation,
                    ..thread_fork_params_from_config(
                        session_config,
                        thread_id,
                        self.thread_params_mode(),
                        self.remote_cwd_override.as_deref(),
                    )
                },
            })
            .await
            .map_err(|err| {
                bootstrap_request_error("thread/fork failed during TUI bootstrap", err)
            })?;
        let fork_parent_title = if presentation == ForkPresentation::SideConversation {
            None
        } else {
            self.fork_parent_title_from_app_server(response.thread.forked_from_id.as_deref())
                .await
        };
        let mut started =
            started_thread_from_fork_response(response, &config, self.thread_params_mode()).await?;
        started.session.fork_parent_title = fork_parent_title;
        Ok(started)
    }

    pub fn thread_params_mode(&self) -> ThreadParamsMode {
        self.thread_params_mode
    }

    pub fn session_config_with_effective_service_tier(&self, config: &Config) -> Config {
        let Some(model) = config.model.as_deref().or(self.default_model.as_deref()) else {
            return config.clone();
        };
        let mut session_config = config.clone();
        match service_tier_resolution::service_tier_update_for_core(
            config,
            model,
            &self.available_models,
        ) {
            Some(Some(service_tier)) => {
                session_config.service_tier = Some(service_tier);
                session_config.notices.fast_default_opt_out = None;
            }
            Some(None) => {
                session_config.service_tier = Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string());
                session_config.notices.fast_default_opt_out = None;
            }
            None => {
                session_config.service_tier = None;
                session_config.notices.fast_default_opt_out = None;
            }
        }
        session_config
    }

    async fn fork_parent_title_from_app_server(
        &mut self,
        forked_from_id: Option<&str>,
    ) -> Option<String> {
        let forked_from_id = forked_from_id?;
        let forked_from_id = match ThreadId::from_string(forked_from_id) {
            Ok(thread_id) => thread_id,
            Err(err) => {
                tracing::warn!("Failed to parse fork parent thread id from app server: {err}");
                return None;
            }
        };

        match self
            .thread_read(forked_from_id, /*include_turns*/ false)
            .await
        {
            Ok(thread) => thread.name,
            Err(err) => {
                tracing::warn!("Failed to read fork parent metadata from app server: {err}");
                None
            }
        }
    }

    pub async fn thread_list(&mut self, params: ThreadListParams) -> Result<ThreadListResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ThreadList { request_id, params })
            .await
            .wrap_err("thread/list failed during TUI session lookup")
    }

    /// Lists remote session projections registered for one local root thread.
    pub async fn remote_session_list(
        &mut self,
        thread_id: ThreadId,
        cursor: Option<String>,
    ) -> Result<RemoteSessionListResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::RemoteSessionList {
                request_id,
                params: RemoteSessionListParams {
                    thread_id: thread_id.to_string(),
                    cursor,
                    limit: None,
                },
            })
            .await
            .wrap_err("remoteSession/list failed during TUI session lookup")
    }

    /// Attaches the local projection to one registered remote session.
    pub async fn remote_session_attach(
        &mut self,
        thread_id: ThreadId,
        remote_session_id: String,
    ) -> Result<RemoteSessionAttachResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::RemoteSessionAttach {
                request_id,
                params: RemoteSessionAttachParams {
                    thread_id: thread_id.to_string(),
                    remote_session_id,
                },
            })
            .await
            .wrap_err("remoteSession/attach failed during TUI session lookup")
    }

    /// Reads one incremental output page from one registered remote session.
    pub async fn remote_session_read(
        &mut self,
        thread_id: ThreadId,
        remote_session_id: String,
        cursor: Option<String>,
    ) -> Result<RemoteSessionReadResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::RemoteSessionRead {
                request_id,
                params: RemoteSessionReadParams {
                    thread_id: thread_id.to_string(),
                    remote_session_id,
                    cursor,
                    limit: None,
                },
            })
            .await
            .wrap_err("remoteSession/read failed during TUI session lookup")
    }

    /// Sends a new message or steering input to a remote session.
    pub async fn remote_session_input(
        &mut self,
        thread_id: ThreadId,
        remote_session_id: String,
        message: String,
        expected_turn_id: Option<String>,
    ) -> Result<RemoteSessionInputResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::RemoteSessionInput {
                request_id,
                params: RemoteSessionInputParams {
                    thread_id: thread_id.to_string(),
                    remote_session_id,
                    message,
                    expected_turn_id,
                },
            })
            .await
            .wrap_err("remoteSession/input failed during TUI session lookup")
    }

    /// Cancels an active remote turn.
    pub async fn remote_session_cancel(
        &mut self,
        thread_id: ThreadId,
        remote_session_id: String,
        expected_turn_id: Option<String>,
    ) -> Result<RemoteSessionCancelResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::RemoteSessionCancel {
                request_id,
                params: RemoteSessionCancelParams {
                    thread_id: thread_id.to_string(),
                    remote_session_id,
                    expected_turn_id,
                },
            })
            .await
            .wrap_err("remoteSession/cancel failed during TUI session lookup")
    }

    /// Lists thread ids that the app server currently holds in memory.
    ///
    /// Used by `App::backfill_loaded_subagent_threads` to discover subagent threads that were
    /// spawned before the TUI connected. The caller then fetches full metadata per thread via
    /// `thread_read` and walks the spawn tree.
    pub async fn thread_loaded_list(
        &mut self,
        params: ThreadLoadedListParams,
    ) -> Result<ThreadLoadedListResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ThreadLoadedList { request_id, params })
            .await
            .wrap_err("failed to list loaded threads from app server")
    }

    pub async fn thread_read(
        &mut self,
        thread_id: ThreadId,
        include_turns: bool,
    ) -> Result<Thread> {
        let request_id = self.next_request_id();
        let response: ThreadReadResponse = self
            .client
            .request_typed(ClientRequest::ThreadRead {
                request_id,
                params: ThreadReadParams {
                    thread_id: thread_id.to_string(),
                    include_turns,
                },
            })
            .await
            .wrap_err("thread/read failed during TUI session lookup")?;
        Ok(response.thread)
    }

    /// Reads thread metadata and a bounded, chronological suffix of full turns.
    pub async fn thread_read_latest_turns(
        &mut self,
        thread_id: ThreadId,
        turn_limit: u32,
    ) -> Result<Thread> {
        let mut thread = self.thread_read(thread_id, /*include_turns*/ false).await?;
        if !matches!(thread.history_mode, ThreadHistoryMode::Paginated) {
            return self.thread_read(thread_id, /*include_turns*/ true).await;
        }

        let request_id = self.next_request_id();
        let response: ThreadTurnsListResponse = self
            .client
            .request_typed(ClientRequest::ThreadTurnsList {
                request_id,
                params: ThreadTurnsListParams {
                    thread_id: thread_id.to_string(),
                    cursor: None,
                    limit: Some(turn_limit),
                    sort_direction: Some(SortDirection::Desc),
                    items_view: Some(TurnItemsView::Full),
                },
            })
            .await
            .wrap_err("thread/turns/list failed during TUI session lookup")?;
        thread.turns = response.data;
        thread.turns.reverse();
        Ok(thread)
    }

    /// Reads thread metadata and every full turn, paging paginated history as needed.
    pub async fn thread_read_all_turns(&mut self, thread_id: ThreadId) -> Result<Thread> {
        let mut thread = self.thread_read(thread_id, /*include_turns*/ false).await?;
        if !matches!(thread.history_mode, ThreadHistoryMode::Paginated) {
            return self.thread_read(thread_id, /*include_turns*/ true).await;
        }

        let mut cursor = None;
        let mut turns = Vec::new();
        loop {
            let request_id = self.next_request_id();
            let response: ThreadTurnsListResponse = self
                .client
                .request_typed(ClientRequest::ThreadTurnsList {
                    request_id,
                    params: ThreadTurnsListParams {
                        thread_id: thread_id.to_string(),
                        cursor: cursor.clone(),
                        limit: None,
                        sort_direction: Some(SortDirection::Asc),
                        items_view: Some(TurnItemsView::Full),
                    },
                })
                .await
                .wrap_err("thread/turns/list failed during TUI session lookup")?;
            turns.extend(response.data);
            let Some(next_cursor) = response.next_cursor else {
                break;
            };
            cursor = Some(next_cursor);
        }
        thread.turns = turns;
        Ok(thread)
    }

    pub async fn thread_archive(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadArchiveResponse = self
            .client
            .request_typed(ClientRequest::ThreadArchive {
                request_id,
                params: ThreadArchiveParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("failed to archive session")?;
        Ok(())
    }

    pub async fn thread_delete(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadDeleteResponse = self
            .client
            .request_typed(ClientRequest::ThreadDelete {
                request_id,
                params: ThreadDeleteParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("failed to delete session")?;
        Ok(())
    }

    pub async fn thread_unarchive(&mut self, thread_id: ThreadId) -> Result<Thread> {
        let request_id = self.next_request_id();
        let response: ThreadUnarchiveResponse = self
            .client
            .request_typed(ClientRequest::ThreadUnarchive {
                request_id,
                params: ThreadUnarchiveParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("failed to unarchive session")?;
        Ok(response.thread)
    }

    pub async fn thread_metadata_update_branch(
        &mut self,
        thread_id: ThreadId,
        branch: String,
    ) -> Result<ThreadMetadataUpdateResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ThreadMetadataUpdate {
                request_id,
                params: ThreadMetadataUpdateParams {
                    thread_id: thread_id.to_string(),
                    git_info: Some(ThreadMetadataGitInfoUpdateParams {
                        sha: None,
                        branch: Some(Some(branch)),
                        origin_url: None,
                    }),
                },
            })
            .await
            .wrap_err("thread/metadata/update failed while syncing git branch")
    }

    pub async fn thread_settings_update(
        &mut self,
        params: ThreadSettingsUpdateParams,
    ) -> Result<()> {
        if !self.thread_settings_update_supported {
            return Ok(());
        }
        let request_id = self.next_request_id();
        match self
            .client
            .request_typed::<ThreadSettingsUpdateResponse>(ClientRequest::ThreadSettingsUpdate {
                request_id,
                params,
            })
            .await
        {
            Ok(_) => Ok(()),
            Err(TypedRequestError::Server { source, .. })
                if is_thread_settings_update_unsupported(&source) =>
            {
                // Older remote app servers can reject this experimental method as
                // method-not-found, experimental-capability-gated, or an unknown
                // request variant. Treat those as a session-level capability
                // downgrade so local TUI setting changes stay best-effort instead
                // of showing an error every time the user changes model, effort,
                // personality, or mode.
                self.thread_settings_update_supported = false;
                Ok(())
            }
            Err(err) => Err(err).wrap_err("thread/settings/update failed in TUI"),
        }
    }

    async fn sync_environment_variables(&mut self, thread_id: &str) -> Result<()> {
        let Some(environment_variables) = environment_variables_for_mode(self.thread_params_mode)
        else {
            return Ok(());
        };
        self.synced_environment_variables.remove(thread_id);
        self.thread_settings_update(ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            environment_variables: Some(environment_variables),
            ..ThreadSettingsUpdateParams::default()
        })
        .await
    }

    fn changed_environment_variables(
        &self,
        thread_id: &ThreadId,
    ) -> Option<BTreeMap<String, String>> {
        let environment_variables = environment_variables_for_mode(self.thread_params_mode)?;
        (self
            .synced_environment_variables
            .get(&thread_id.to_string())
            != Some(&environment_variables))
        .then_some(environment_variables)
    }

    pub async fn thread_inject_items(
        &mut self,
        thread_id: ThreadId,
        items: Vec<ResponseItem>,
    ) -> Result<ThreadInjectItemsResponse> {
        let items = items
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to encode thread/inject_items payload")?;
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ThreadInjectItems {
                request_id,
                params: ThreadInjectItemsParams {
                    thread_id: thread_id.to_string(),
                    items,
                },
            })
            .await
            .wrap_err("thread/inject_items failed during TUI side conversation setup")
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn turn_start(
        &mut self,
        thread_id: ThreadId,
        items: Vec<UserInput>,
        cwd: PathBuf,
        approval_policy: AskForApproval,
        permissions_override: TurnPermissionsOverride,
        workspace_roots: &[AbsolutePathBuf],
        model: String,
        effort: Option<xedoc_protocol::openai_models::ReasoningEffort>,
        summary: Option<xedoc_protocol::config_types::ReasoningSummary>,
        service_tier: Option<Option<String>>,
        collaboration_mode: Option<xedoc_protocol::config_types::CollaborationMode>,
        personality: Option<xedoc_protocol::config_types::Personality>,
        output_schema: Option<serde_json::Value>,
    ) -> Result<TurnStartResponse> {
        let request_id = self.next_request_id();
        let (sandbox_policy, permissions) =
            turn_permissions_overrides(permissions_override, cwd.as_path());
        let environment_variables = self.changed_environment_variables(&thread_id);
        let response = self
            .client
            .request_typed(ClientRequest::TurnStart {
                request_id,
                params: TurnStartParams {
                    thread_id: thread_id.to_string(),
                    client_user_message_id: None,
                    input: items,
                    responsesapi_client_metadata: None,
                    additional_context: None,
                    environments: None,
                    environment_variables: environment_variables.clone(),
                    cwd: Some(cwd),
                    runtime_workspace_roots: Some(workspace_roots.to_vec()),
                    approval_policy: Some(approval_policy),
                    sandbox_policy,
                    permissions,
                    model: Some(model),
                    service_tier,
                    effort,
                    summary,
                    personality,
                    output_schema,
                    collaboration_mode,
                    multi_agent_mode: None,
                    model_provider: None,
                },
            })
            .await
            .wrap_err("turn/start failed in TUI")?;
        if let Some(environment_variables) = environment_variables {
            self.synced_environment_variables
                .insert(thread_id.to_string(), environment_variables);
        }
        Ok(response)
    }

    pub async fn turn_interrupt(
        &mut self,
        thread_id: ThreadId,
        turn_id: String,
    ) -> std::result::Result<(), TypedRequestError> {
        let request_id = self.next_request_id();
        let _: TurnInterruptResponse = self
            .client
            .request_typed(ClientRequest::TurnInterrupt {
                request_id,
                params: TurnInterruptParams {
                    thread_id: thread_id.to_string(),
                    turn_id,
                },
            })
            .await?;
        Ok(())
    }

    pub async fn startup_interrupt(
        &mut self,
        thread_id: ThreadId,
    ) -> std::result::Result<(), TypedRequestError> {
        self.turn_interrupt(thread_id, String::new()).await
    }

    pub async fn turn_steer(
        &mut self,
        thread_id: ThreadId,
        turn_id: String,
        items: Vec<UserInput>,
        client_user_message_id: Option<String>,
    ) -> std::result::Result<TurnSteerResponse, TypedRequestError> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::TurnSteer {
                request_id,
                params: TurnSteerParams {
                    thread_id: thread_id.to_string(),
                    client_user_message_id,
                    input: items,
                    responsesapi_client_metadata: None,
                    additional_context: None,
                    expected_turn_id: turn_id,
                },
            })
            .await
    }

    pub async fn turn_steer_cancel(
        &mut self,
        thread_id: ThreadId,
        turn_id: String,
        client_user_message_id: String,
    ) -> std::result::Result<TurnSteerCancelResponse, TypedRequestError> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::TurnSteerCancel {
                request_id,
                params: TurnSteerCancelParams {
                    thread_id: thread_id.to_string(),
                    client_user_message_id,
                    expected_turn_id: turn_id,
                },
            })
            .await
    }

    pub async fn thread_set_name(&mut self, thread_id: ThreadId, name: String) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadSetNameResponse = self
            .client
            .request_typed(ClientRequest::ThreadSetName {
                request_id,
                params: ThreadSetNameParams {
                    thread_id: thread_id.to_string(),
                    name,
                },
            })
            .await
            .wrap_err("thread/name/set failed in TUI")?;
        Ok(())
    }

    pub async fn thread_goal_get(&mut self, thread_id: ThreadId) -> Result<ThreadGoalGetResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ThreadGoalGet {
                request_id,
                params: ThreadGoalGetParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/goal/get failed in TUI")
    }

    pub async fn thread_goal_set(
        &mut self,
        thread_id: ThreadId,
        objective: Option<String>,
        status: Option<ThreadGoalStatus>,
        token_budget: Option<Option<i64>>,
    ) -> Result<ThreadGoalSetResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ThreadGoalSet {
                request_id,
                params: ThreadGoalSetParams {
                    thread_id: thread_id.to_string(),
                    objective,
                    status,
                    token_budget,
                },
            })
            .await
            .wrap_err("thread/goal/set failed in TUI")
    }

    pub async fn thread_goal_clear(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<ThreadGoalClearResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ThreadGoalClear {
                request_id,
                params: ThreadGoalClearParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/goal/clear failed in TUI")
    }

    pub async fn logout_account(&mut self) -> Result<()> {
        let request_id = self.next_request_id();
        let _: LogoutAccountResponse = self
            .client
            .request_typed(ClientRequest::LogoutAccount {
                request_id,
                params: None,
            })
            .await
            .wrap_err("account/logout failed in TUI")?;
        Ok(())
    }

    pub async fn thread_unsubscribe(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadUnsubscribeResponse = self
            .client
            .request_typed(ClientRequest::ThreadUnsubscribe {
                request_id,
                params: ThreadUnsubscribeParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/unsubscribe failed in TUI")?;
        Ok(())
    }

    pub async fn thread_compact_start(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadCompactStartResponse = self
            .client
            .request_typed(ClientRequest::ThreadCompactStart {
                request_id,
                params: ThreadCompactStartParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/compact/start failed in TUI")?;
        Ok(())
    }

    pub async fn thread_shell_command(
        &mut self,
        thread_id: ThreadId,
        command: String,
    ) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadShellCommandResponse = self
            .client
            .request_typed(ClientRequest::ThreadShellCommand {
                request_id,
                params: ThreadShellCommandParams {
                    thread_id: thread_id.to_string(),
                    command,
                },
            })
            .await
            .wrap_err("thread/shellCommand failed in TUI")?;
        Ok(())
    }

    pub async fn thread_background_terminals_clean(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadBackgroundTerminalsCleanResponse = self
            .client
            .request_typed(ClientRequest::ThreadBackgroundTerminalsClean {
                request_id,
                params: ThreadBackgroundTerminalsCleanParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/backgroundTerminals/clean failed in TUI")?;
        Ok(())
    }

    pub async fn review_start(
        &mut self,
        thread_id: ThreadId,
        target: ReviewTarget,
    ) -> Result<ReviewStartResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ReviewStart {
                request_id,
                params: ReviewStartParams {
                    thread_id: thread_id.to_string(),
                    target,
                    delivery: Some(ReviewDelivery::Inline),
                },
            })
            .await
            .wrap_err("review/start failed in TUI")
    }

    pub async fn skills_list(&mut self, params: SkillsListParams) -> Result<SkillsListResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::SkillsList { request_id, params })
            .await
            .wrap_err("skills/list failed in TUI")
    }

    /// Fetches the approved session-extension commands after an interaction changes extension
    /// state. The notification can race with TUI thread attachment, so callers use this as a
    /// reliable post-approval refresh.
    pub async fn session_extension_list(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<SessionExtensionListResponse> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::SessionExtensionList {
                request_id,
                params: SessionExtensionListParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .map_err(|err| bootstrap_request_error("sessionExtension/list failed", err))
    }

    pub async fn reload_user_config(&mut self) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ConfigWriteResponse = self
            .client
            .request_typed(ClientRequest::ConfigBatchWrite {
                request_id,
                params: ConfigBatchWriteParams {
                    edits: Vec::new(),
                    file_path: None,
                    expected_version: None,
                    reload_user_config: true,
                },
            })
            .await
            .wrap_err("config/batchWrite failed while reloading user config in TUI")?;
        Ok(())
    }

    pub async fn reject_server_request(
        &self,
        request_id: RequestId,
        error: JSONRPCErrorError,
    ) -> std::io::Result<()> {
        self.client.reject_server_request(request_id, error).await
    }

    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: serde_json::Value,
    ) -> std::io::Result<()> {
        self.client.resolve_server_request(request_id, result).await
    }

    pub async fn shutdown(self) -> std::io::Result<()> {
        self.client.shutdown().await
    }

    pub fn request_handle(&self) -> AppServerRequestHandle {
        self.client.request_handle()
    }

    fn next_request_id(&mut self) -> RequestId {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        RequestId::Integer(request_id)
    }
}

pub async fn start_thread_with_request_handle(
    request_handle: AppServerRequestHandle,
    config: Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<PathBuf>,
) -> Result<AppServerStartedThread> {
    let response: ThreadStartResponse = request_handle
        .request_typed(ClientRequest::ThreadStart {
            request_id: RequestId::String(format!("startup-thread-start-{}", Uuid::new_v4())),
            params: thread_start_params_from_config(
                &config,
                thread_params_mode,
                remote_cwd_override.as_deref(),
                /*session_start_source*/ None,
            ),
        })
        .await
        .map_err(|err| bootstrap_request_error("thread/start failed during TUI bootstrap", err))?;
    sync_environment_variables_with_request_handle(
        &request_handle,
        &response.thread.id,
        thread_params_mode,
    )
    .await?;
    started_thread_from_start_response(response, &config, thread_params_mode).await
}

async fn sync_environment_variables_with_request_handle(
    request_handle: &AppServerRequestHandle,
    thread_id: &str,
    thread_params_mode: ThreadParamsMode,
) -> Result<()> {
    let Some(environment_variables) = environment_variables_for_mode(thread_params_mode) else {
        return Ok(());
    };
    request_handle
        .request_typed::<ThreadSettingsUpdateResponse>(ClientRequest::ThreadSettingsUpdate {
            request_id: RequestId::String(format!("startup-thread-environment-{}", Uuid::new_v4())),
            params: ThreadSettingsUpdateParams {
                thread_id: thread_id.to_string(),
                environment_variables: Some(environment_variables),
                ..ThreadSettingsUpdateParams::default()
            },
        })
        .await
        .map_err(|err| {
            bootstrap_request_error(
                "thread/settings/update failed during TUI environment synchronization",
                err,
            )
        })?;
    Ok(())
}

fn environment_variables_for_mode(
    thread_params_mode: ThreadParamsMode,
) -> Option<BTreeMap<String, String>> {
    matches!(thread_params_mode, ThreadParamsMode::Embedded).then(|| std::env::vars().collect())
}

pub fn status_account_display_from_auth_mode(
    auth_mode: Option<AuthMode>,
    plan_type: Option<xedoc_protocol::account::PlanType>,
) -> Option<StatusAccountDisplay> {
    match auth_mode {
        Some(AuthMode::ApiKey) => Some(StatusAccountDisplay::ApiKey),
        Some(AuthMode::Chatgpt)
        | Some(AuthMode::ChatgptAuthTokens)
        | Some(AuthMode::AgentIdentity)
        | Some(AuthMode::PersonalAccessToken) => Some(StatusAccountDisplay::ChatGpt {
            email: None,
            plan: plan_type.map(plan_type_display_name),
        }),
        Some(AuthMode::Headers) | Some(AuthMode::BedrockApiKey) => None,
        None => None,
    }
}

fn model_preset_from_api_model(model: ApiModel) -> ModelPreset {
    let upgrade = model.upgrade.map(|upgrade_id| {
        let upgrade_info = model.upgrade_info.clone();
        ModelUpgrade {
            id: upgrade_id,
            migration_config_key: model.model.clone(),
            model_link: upgrade_info
                .as_ref()
                .and_then(|info| info.model_link.clone()),
            upgrade_copy: upgrade_info
                .as_ref()
                .and_then(|info| info.upgrade_copy.clone()),
            migration_markdown: upgrade_info.and_then(|info| info.migration_markdown),
        }
    });

    ModelPreset {
        id: model.id,
        model: model.model,
        display_name: model.display_name,
        description: model.description,
        default_reasoning_effort: model.default_reasoning_effort,
        supported_reasoning_efforts: model
            .supported_reasoning_efforts
            .into_iter()
            .map(|effort| ReasoningEffortPreset {
                effort: effort.reasoning_effort,
                description: effort.description,
            })
            .collect(),
        supports_personality: model.supports_personality,
        additional_speed_tiers: model.additional_speed_tiers,
        service_tiers: model
            .service_tiers
            .into_iter()
            .map(|service_tier| ModelServiceTier {
                id: service_tier.id,
                name: service_tier.name,
                description: service_tier.description,
            })
            .collect(),
        default_service_tier: model.default_service_tier,
        is_default: model.is_default,
        upgrade,
        show_in_picker: !model.hidden,
        multi_agent_version: None,
        availability_nux: model.availability_nux.map(|nux| ModelAvailabilityNux {
            message: nux.message,
        }),
        // `model/list` already returns models filtered for the active client/auth context.
        supported_in_api: true,
        input_modalities: model.input_modalities,
        provider_id: model.provider_id,
    }
}

fn config_request_overrides_from_config(
    config: &Config,
) -> Option<HashMap<String, serde_json::Value>> {
    let mut overrides = HashMap::new();
    let mut insert = |key: &str, value: Option<String>| {
        if let Some(value) = value {
            overrides.insert(key.to_string(), serde_json::Value::String(value));
        }
    };
    insert(
        "model_reasoning_effort",
        config
            .model_reasoning_effort
            .as_ref()
            .map(std::string::ToString::to_string),
    );
    insert(
        "model_reasoning_summary",
        config
            .model_reasoning_summary
            .map(|summary| summary.to_string()),
    );
    insert(
        "model_verbosity",
        config
            .model_verbosity
            .map(|verbosity| verbosity.to_string()),
    );
    insert(
        "personality",
        config
            .personality
            .map(|personality| personality.to_string()),
    );
    insert(
        "web_search",
        Some(config.web_search_mode.value().to_string()),
    );
    if config.bypass_hook_trust {
        overrides.insert("bypass_hook_trust".to_string(), true.into());
    }
    Some(overrides)
}

fn service_tier_override_from_config(config: &Config) -> Option<Option<String>> {
    config.service_tier.clone().map(Some).or_else(|| {
        (config.notices.fast_default_opt_out == Some(true))
            .then(|| Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()))
    })
}

fn sandbox_mode_from_permission_profile(
    permission_profile: &PermissionProfile,
    cwd: &std::path::Path,
) -> Option<xedoc_app_server_protocol::SandboxMode> {
    match permission_profile {
        PermissionProfile::Disabled => {
            Some(xedoc_app_server_protocol::SandboxMode::DangerFullAccess)
        }
        PermissionProfile::External { .. } => None,
        PermissionProfile::Managed { .. } => {
            let file_system_policy = permission_profile.file_system_sandbox_policy();
            if file_system_policy.has_full_disk_write_access() {
                permission_profile
                    .network_sandbox_policy()
                    .is_enabled()
                    .then_some(xedoc_app_server_protocol::SandboxMode::DangerFullAccess)
            } else if file_system_policy.can_write_path_with_cwd(cwd, cwd) {
                Some(xedoc_app_server_protocol::SandboxMode::WorkspaceWrite)
            } else {
                Some(xedoc_app_server_protocol::SandboxMode::ReadOnly)
            }
        }
    }
}

fn permission_profile_id_from_active_profile(active: ActivePermissionProfile) -> String {
    active.id
}

fn turn_permissions_overrides(
    permissions_override: TurnPermissionsOverride,
    cwd: &std::path::Path,
) -> (
    Option<xedoc_app_server_protocol::SandboxPolicy>,
    Option<String>,
) {
    match permissions_override {
        TurnPermissionsOverride::Preserve => (None, None),
        TurnPermissionsOverride::ActiveProfile(active_permission_profile) => (
            None,
            Some(permission_profile_id_from_active_profile(
                active_permission_profile,
            )),
        ),
        TurnPermissionsOverride::LegacySandbox(permission_profile) => {
            let legacy_profile = legacy_compatible_permission_profile(&permission_profile, cwd);
            let policy = legacy_profile
                .to_legacy_sandbox_policy(cwd)
                .unwrap_or_else(|err| {
                    unreachable!(
                        "legacy-compatible permissions must project to legacy policy: {err}"
                    )
                });
            (Some(policy.into()), None)
        }
    }
}

fn permissions_selection_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Option<String> {
    if matches!(thread_params_mode, ThreadParamsMode::Remote) {
        return None;
    }

    config
        .permissions
        .active_permission_profile()
        .map(permission_profile_id_from_active_profile)
}

fn thread_start_params_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
    session_start_source: Option<ThreadStartSource>,
) -> ThreadStartParams {
    let permissions = permissions_selection_from_config(config, thread_params_mode);
    let sandbox = permissions
        .is_none()
        .then(|| {
            sandbox_mode_from_permission_profile(
                &config.permissions.effective_permission_profile(),
                config.cwd.as_path(),
            )
        })
        .flatten();
    ThreadStartParams {
        model: config.model.clone(),
        model_provider: thread_params_mode.model_provider_from_config(config),
        service_tier: service_tier_override_from_config(config),
        cwd: thread_cwd_from_config(config, thread_params_mode, remote_cwd_override),
        runtime_workspace_roots: Some(config.workspace_roots.clone()),
        approval_policy: Some(config.permissions.approval_policy.value().into()),
        sandbox,
        permissions,
        config: config_request_overrides_from_config(config),
        ephemeral: Some(config.ephemeral),
        session_start_source,
        thread_source: Some(ThreadSource::User),
        history_mode: Some(ThreadHistoryMode::Paginated),
        developer_instructions: with_terminal_visualization_instructions(
            config, /*control_instructions*/ None,
        ),
        ..ThreadStartParams::default()
    }
}

fn thread_resume_params_from_config(
    config: Config,
    thread_id: ThreadId,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
    model_settings: ResumeModelSettings,
) -> ThreadResumeParams {
    let permissions = permissions_selection_from_config(&config, thread_params_mode);
    let sandbox = permissions
        .is_none()
        .then(|| {
            sandbox_mode_from_permission_profile(
                &config.permissions.effective_permission_profile(),
                config.cwd.as_path(),
            )
        })
        .flatten();
    let mut config_overrides = config_request_overrides_from_config(&config);
    if model_settings == ResumeModelSettings::RestoreFromThread
        && let Some(overrides) = config_overrides.as_mut()
    {
        overrides.remove("model_reasoning_effort");
        if overrides.is_empty() {
            config_overrides = None;
        }
    }
    let (model, model_provider) = match model_settings {
        ResumeModelSettings::OverrideFromCurrentConfig => (
            config.model.clone(),
            thread_params_mode.model_provider_from_config(&config),
        ),
        ResumeModelSettings::RestoreFromThread => (None, None),
    };
    ThreadResumeParams {
        thread_id: thread_id.to_string(),
        model,
        model_provider,
        service_tier: service_tier_override_from_config(&config),
        cwd: thread_cwd_from_config(&config, thread_params_mode, remote_cwd_override),
        runtime_workspace_roots: Some(config.workspace_roots.clone()),
        approval_policy: Some(config.permissions.approval_policy.value().into()),
        sandbox,
        permissions,
        config: config_overrides,
        developer_instructions: with_terminal_visualization_instructions(
            &config, /*control_instructions*/ None,
        ),
        // Rebuild the terminal scrollback from the complete ordered transcript. Remote transports
        // replace this with a bounded page before sending the request.
        exclude_turns: false,
        initial_turns_page: None,
        ..ThreadResumeParams::default()
    }
}

fn thread_fork_params_from_config(
    config: Config,
    thread_id: ThreadId,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
) -> ThreadForkParams {
    let permissions = permissions_selection_from_config(&config, thread_params_mode);
    let sandbox = permissions
        .is_none()
        .then(|| {
            sandbox_mode_from_permission_profile(
                &config.permissions.effective_permission_profile(),
                config.cwd.as_path(),
            )
        })
        .flatten();
    ThreadForkParams {
        thread_id: thread_id.to_string(),
        model: config.model.clone(),
        model_provider: thread_params_mode.model_provider_from_config(&config),
        service_tier: service_tier_override_from_config(&config),
        cwd: thread_cwd_from_config(&config, thread_params_mode, remote_cwd_override),
        runtime_workspace_roots: Some(config.workspace_roots.clone()),
        approval_policy: Some(config.permissions.approval_policy.value().into()),
        sandbox,
        permissions,
        config: config_request_overrides_from_config(&config),
        base_instructions: config.base_instructions.clone(),
        developer_instructions: with_terminal_visualization_instructions(
            &config,
            config.developer_instructions.clone(),
        ),
        ephemeral: config.ephemeral,
        thread_source: Some(ThreadSource::User),
        ..ThreadForkParams::default()
    }
}

fn thread_cwd_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
) -> Option<String> {
    match thread_params_mode {
        ThreadParamsMode::Embedded => Some(config.cwd.to_string_lossy().to_string()),
        ThreadParamsMode::Remote => {
            remote_cwd_override.map(|cwd| cwd.to_string_lossy().to_string())
        }
    }
}

async fn started_thread_from_start_response(
    response: ThreadStartResponse,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Result<AppServerStartedThread> {
    let blocks_direct_input = thread_blocks_direct_input(&response.thread);
    let session =
        thread_session_state_from_thread_start_response(&response, config, thread_params_mode)
            .await
            .map_err(color_eyre::eyre::Report::msg)?;
    Ok(AppServerStartedThread {
        session,
        turns: response.thread.turns,
        blocks_direct_input,
    })
}

async fn started_thread_from_resume_response(
    response: ThreadResumeResponse,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Result<AppServerStartedThread> {
    let blocks_direct_input = thread_blocks_direct_input(&response.thread);
    let session =
        thread_session_state_from_thread_resume_response(&response, config, thread_params_mode)
            .await
            .map_err(color_eyre::eyre::Report::msg)?;
    let turns = hydrated_resume_turns(response.thread.turns, response.initial_turns_page);
    Ok(AppServerStartedThread {
        session,
        turns,
        blocks_direct_input,
    })
}

fn hydrated_resume_turns(
    thread_turns: Vec<Turn>,
    initial_turns_page: Option<TurnsPage>,
) -> Vec<Turn> {
    initial_turns_page.map_or(thread_turns, |mut page| {
        page.data.reverse();
        page.data
    })
}

async fn started_thread_from_fork_response(
    response: ThreadForkResponse,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Result<AppServerStartedThread> {
    let blocks_direct_input = thread_blocks_direct_input(&response.thread);
    let session =
        thread_session_state_from_thread_fork_response(&response, config, thread_params_mode)
            .await
            .map_err(color_eyre::eyre::Report::msg)?;
    Ok(AppServerStartedThread {
        session,
        turns: response.thread.turns,
        blocks_direct_input,
    })
}

async fn thread_session_state_from_thread_start_response(
    response: &ThreadStartResponse,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Result<ThreadSessionState, String> {
    let permission_profile = display_permission_profile_from_thread_response(
        &response.sandbox,
        response.cwd.as_path(),
        config,
        thread_params_mode,
    );
    thread_session_state_from_thread_response(
        &response.thread.id,
        response.thread.forked_from_id.clone(),
        response.thread.name.clone(),
        response.thread.path.clone(),
        response.model.clone(),
        response.model_provider.clone(),
        response.service_tier.clone(),
        response.approval_policy,
        permission_profile,
        response.active_permission_profile.clone().map(Into::into),
        response.cwd.clone(),
        response.runtime_workspace_roots.clone(),
        response.instruction_source_path_uris(),
        response.reasoning_effort.clone(),
        config,
    )
    .await
}

async fn thread_session_state_from_thread_resume_response(
    response: &ThreadResumeResponse,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Result<ThreadSessionState, String> {
    let permission_profile = if matches!(thread_params_mode, ThreadParamsMode::Embedded)
        && response.active_permission_profile.is_none()
    {
        PermissionProfile::from_legacy_sandbox_policy_for_cwd(
            &response.sandbox.to_core(),
            response.cwd.as_path(),
        )
    } else {
        display_permission_profile_from_thread_response(
            &response.sandbox,
            response.cwd.as_path(),
            config,
            thread_params_mode,
        )
    };
    thread_session_state_from_thread_response(
        &response.thread.id,
        response.thread.forked_from_id.clone(),
        response.thread.name.clone(),
        response.thread.path.clone(),
        response.model.clone(),
        response.model_provider.clone(),
        response.service_tier.clone(),
        response.approval_policy,
        permission_profile,
        response.active_permission_profile.clone().map(Into::into),
        response.cwd.clone(),
        response.runtime_workspace_roots.clone(),
        response.instruction_source_path_uris(),
        response.reasoning_effort.clone(),
        config,
    )
    .await
}

async fn thread_session_state_from_thread_fork_response(
    response: &ThreadForkResponse,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Result<ThreadSessionState, String> {
    let permission_profile = display_permission_profile_from_thread_response(
        &response.sandbox,
        response.cwd.as_path(),
        config,
        thread_params_mode,
    );
    thread_session_state_from_thread_response(
        &response.thread.id,
        response.thread.forked_from_id.clone(),
        response.thread.name.clone(),
        response.thread.path.clone(),
        response.model.clone(),
        response.model_provider.clone(),
        response.service_tier.clone(),
        response.approval_policy,
        permission_profile,
        response.active_permission_profile.clone().map(Into::into),
        response.cwd.clone(),
        response.runtime_workspace_roots.clone(),
        response.instruction_source_path_uris(),
        response.reasoning_effort.clone(),
        config,
    )
    .await
}

fn display_permission_profile_from_thread_response(
    sandbox: &xedoc_app_server_protocol::SandboxPolicy,
    cwd: &std::path::Path,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> PermissionProfile {
    match thread_params_mode {
        ThreadParamsMode::Embedded => config.permissions.effective_permission_profile(),
        ThreadParamsMode::Remote => {
            PermissionProfile::from_legacy_sandbox_policy_for_cwd(&sandbox.to_core(), cwd)
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "session mapping keeps explicit fields"
)]
async fn thread_session_state_from_thread_response(
    thread_id: &str,
    forked_from_id: Option<String>,
    thread_name: Option<String>,
    rollout_path: Option<PathBuf>,
    model: String,
    model_provider_id: String,
    service_tier: Option<String>,
    approval_policy: AskForApproval,
    permission_profile: PermissionProfile,
    active_permission_profile: Option<ActivePermissionProfile>,
    cwd: AbsolutePathBuf,
    runtime_workspace_roots: Vec<AbsolutePathBuf>,
    instruction_source_paths: Vec<PathUri>,
    reasoning_effort: Option<xedoc_protocol::openai_models::ReasoningEffort>,
    config: &Config,
) -> Result<ThreadSessionState, String> {
    let thread_id = ThreadId::from_string(thread_id)
        .map_err(|err| format!("thread id `{thread_id}` is invalid: {err}"))?;
    let forked_from_id = forked_from_id
        .as_deref()
        .map(ThreadId::from_string)
        .transpose()
        .map_err(|err| format!("forked_from_id is invalid: {err}"))?;
    let history_config =
        xedoc_message_history::HistoryConfig::new(config.xedoc_home.clone(), &config.history);
    let (log_id, entry_count) = xedoc_message_history::history_metadata(&history_config).await;
    Ok(ThreadSessionState {
        thread_id,
        forked_from_id,
        fork_parent_title: None,
        thread_name,
        model,
        model_provider_id,
        service_tier,
        approval_policy,
        permission_profile,
        active_permission_profile,
        cwd,
        runtime_workspace_roots,
        instruction_source_paths,
        reasoning_effort,
        collaboration_mode: None,
        personality: config.personality,
        message_history: Some(MessageHistoryMetadata {
            log_id,
            entry_count,
        }),
        network_proxy: None,
        rollout_path,
    })
}
