use crate::bespoke_event_handling::apply_bespoke_event_handling;
use crate::command_exec::CommandExecManager;
use crate::command_exec::StartCommandExecParams;
use crate::config_manager::ConfigManager;
use crate::error_code::INPUT_TOO_LARGE_ERROR_CODE;
use crate::error_code::invalid_params;
use crate::models::supported_models;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::ConnectionRequestId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::RequestContext;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::skills_watcher::SkillsWatcher;
use crate::thread_status::ThreadWatchManager;
use crate::thread_status::resolve_thread_status;
use chrono::Duration as ChronoDuration;
use chrono::SecondsFormat;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Error as IoError;
use std::path::Path;
use std::path::PathBuf;
use std::result::Result;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;
use tokio::sync::SemaphorePermit;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use toml::Value as TomlValue;
use tracing::Instrument;
use tracing::error;
use tracing::info;
use tracing::warn;
use uuid::Uuid;
use xedoc_app_server_protocol::Account;
use xedoc_app_server_protocol::AccountLoginCompletedNotification;
use xedoc_app_server_protocol::AccountUpdatedNotification;
use xedoc_app_server_protocol::AdditionalContextEntry;
use xedoc_app_server_protocol::AdditionalContextKind;
use xedoc_app_server_protocol::AskForApproval;
use xedoc_app_server_protocol::AuthMode;
use xedoc_app_server_protocol::CancelLoginAccountParams;
use xedoc_app_server_protocol::CancelLoginAccountResponse;
use xedoc_app_server_protocol::CancelLoginAccountStatus;
use xedoc_app_server_protocol::ClientInfo;
use xedoc_app_server_protocol::ClientResponsePayload;
use xedoc_app_server_protocol::CollaborationModeListParams;
use xedoc_app_server_protocol::CollaborationModeListResponse;
use xedoc_app_server_protocol::CommandExecParams;
use xedoc_app_server_protocol::CommandExecResizeParams;
use xedoc_app_server_protocol::CommandExecTerminateParams;
use xedoc_app_server_protocol::CommandExecWriteParams;
use xedoc_app_server_protocol::ConfigWarningNotification;
use xedoc_app_server_protocol::ConversationGitInfo;
use xedoc_app_server_protocol::ConversationSummary;
use xedoc_app_server_protocol::DeprecationNoticeNotification;
use xedoc_app_server_protocol::DynamicToolFunctionSpec;
use xedoc_app_server_protocol::DynamicToolNamespaceTool;
use xedoc_app_server_protocol::DynamicToolSpec;
use xedoc_app_server_protocol::EnvironmentAddParams;
use xedoc_app_server_protocol::EnvironmentAddResponse;
use xedoc_app_server_protocol::EnvironmentInfoParams;
use xedoc_app_server_protocol::EnvironmentInfoResponse;
use xedoc_app_server_protocol::EnvironmentShellInfo;
use xedoc_app_server_protocol::EnvironmentStatusKind;
use xedoc_app_server_protocol::EnvironmentStatusParams;
use xedoc_app_server_protocol::EnvironmentStatusResponse;
use xedoc_app_server_protocol::ExperimentalFeature as ApiExperimentalFeature;
use xedoc_app_server_protocol::ExperimentalFeatureListParams;
use xedoc_app_server_protocol::ExperimentalFeatureListResponse;
use xedoc_app_server_protocol::ExperimentalFeatureStage as ApiExperimentalFeatureStage;
use xedoc_app_server_protocol::GetAccountParams;
use xedoc_app_server_protocol::GetAccountResponse;
use xedoc_app_server_protocol::GetAuthStatusParams;
use xedoc_app_server_protocol::GetAuthStatusResponse;
use xedoc_app_server_protocol::GetConversationSummaryParams;
use xedoc_app_server_protocol::GetConversationSummaryResponse;
use xedoc_app_server_protocol::GitDiffToRemoteParams;
use xedoc_app_server_protocol::GitDiffToRemoteResponse;
use xedoc_app_server_protocol::GitInfo as ApiGitInfo;
use xedoc_app_server_protocol::HookMetadata;
use xedoc_app_server_protocol::HooksListParams;
use xedoc_app_server_protocol::HooksListResponse;
use xedoc_app_server_protocol::InitializeParams;
use xedoc_app_server_protocol::InitializeResponse;
use xedoc_app_server_protocol::JSONRPCErrorError;
use xedoc_app_server_protocol::ListMcpServerStatusParams;
use xedoc_app_server_protocol::ListMcpServerStatusResponse;
use xedoc_app_server_protocol::LoginAccountParams;
use xedoc_app_server_protocol::LoginAccountResponse;
use xedoc_app_server_protocol::LoginApiKeyParams;
use xedoc_app_server_protocol::LoginAppBrand;
use xedoc_app_server_protocol::LogoutAccountResponse;
use xedoc_app_server_protocol::MarketplaceAddParams;
use xedoc_app_server_protocol::MarketplaceAddResponse;
use xedoc_app_server_protocol::MarketplaceInterface;
use xedoc_app_server_protocol::MarketplaceRemoveParams;
use xedoc_app_server_protocol::MarketplaceRemoveResponse;
use xedoc_app_server_protocol::MarketplaceUpgradeErrorInfo;
use xedoc_app_server_protocol::MarketplaceUpgradeParams;
use xedoc_app_server_protocol::MarketplaceUpgradeResponse;
use xedoc_app_server_protocol::McpResourceReadParams;
use xedoc_app_server_protocol::McpResourceReadResponse;
use xedoc_app_server_protocol::McpServerOauthLoginCompletedNotification;
use xedoc_app_server_protocol::McpServerOauthLoginParams;
use xedoc_app_server_protocol::McpServerOauthLoginResponse;
use xedoc_app_server_protocol::McpServerRefreshResponse;
use xedoc_app_server_protocol::McpServerStatus;
use xedoc_app_server_protocol::McpServerStatusDetail;
use xedoc_app_server_protocol::McpServerToolCallParams;
use xedoc_app_server_protocol::McpServerToolCallResponse;
use xedoc_app_server_protocol::MockExperimentalMethodParams;
use xedoc_app_server_protocol::MockExperimentalMethodResponse;
use xedoc_app_server_protocol::ModelListParams;
use xedoc_app_server_protocol::ModelListResponse;
use xedoc_app_server_protocol::PermissionProfileListParams;
use xedoc_app_server_protocol::PermissionProfileListResponse;
use xedoc_app_server_protocol::PermissionProfileSummary;
use xedoc_app_server_protocol::PluginDetail;
use xedoc_app_server_protocol::PluginInstallParams;
use xedoc_app_server_protocol::PluginInstallResponse;
use xedoc_app_server_protocol::PluginInstalledParams;
use xedoc_app_server_protocol::PluginInstalledResponse;
use xedoc_app_server_protocol::PluginInterface;
use xedoc_app_server_protocol::PluginListParams;
use xedoc_app_server_protocol::PluginListResponse;
use xedoc_app_server_protocol::PluginMarketplaceEntry;
use xedoc_app_server_protocol::PluginReadParams;
use xedoc_app_server_protocol::PluginReadResponse;
use xedoc_app_server_protocol::PluginSource;
use xedoc_app_server_protocol::PluginSummary;
use xedoc_app_server_protocol::PluginUninstallParams;
use xedoc_app_server_protocol::PluginUninstallResponse;
use xedoc_app_server_protocol::RequestId;
use xedoc_app_server_protocol::ReviewDelivery as ApiReviewDelivery;
use xedoc_app_server_protocol::ReviewStartParams;
use xedoc_app_server_protocol::ReviewStartResponse;
use xedoc_app_server_protocol::ReviewTarget as ApiReviewTarget;
use xedoc_app_server_protocol::SandboxMode;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ServerRequestResolvedNotification;
use xedoc_app_server_protocol::SkillSummary;
use xedoc_app_server_protocol::SkillsConfigWriteParams;
use xedoc_app_server_protocol::SkillsConfigWriteResponse;
use xedoc_app_server_protocol::SkillsExtraRootsSetParams;
use xedoc_app_server_protocol::SkillsExtraRootsSetResponse;
use xedoc_app_server_protocol::SkillsListParams;
use xedoc_app_server_protocol::SkillsListResponse;
use xedoc_app_server_protocol::SortDirection;
use xedoc_app_server_protocol::Thread;
use xedoc_app_server_protocol::ThreadArchiveParams;
use xedoc_app_server_protocol::ThreadArchiveResponse;
use xedoc_app_server_protocol::ThreadArchivedNotification;
use xedoc_app_server_protocol::ThreadBackgroundTerminal;
use xedoc_app_server_protocol::ThreadBackgroundTerminalsCleanParams;
use xedoc_app_server_protocol::ThreadBackgroundTerminalsCleanResponse;
use xedoc_app_server_protocol::ThreadBackgroundTerminalsListParams;
use xedoc_app_server_protocol::ThreadBackgroundTerminalsListResponse;
use xedoc_app_server_protocol::ThreadBackgroundTerminalsTerminateParams;
use xedoc_app_server_protocol::ThreadBackgroundTerminalsTerminateResponse;
use xedoc_app_server_protocol::ThreadClosedNotification;
use xedoc_app_server_protocol::ThreadCompactStartParams;
use xedoc_app_server_protocol::ThreadCompactStartResponse;
use xedoc_app_server_protocol::ThreadDecrementElicitationParams;
use xedoc_app_server_protocol::ThreadDecrementElicitationResponse;
use xedoc_app_server_protocol::ThreadDeleteParams;
use xedoc_app_server_protocol::ThreadDeleteResponse;
use xedoc_app_server_protocol::ThreadDeletedNotification;
use xedoc_app_server_protocol::ThreadForkParams;
use xedoc_app_server_protocol::ThreadForkResponse;
use xedoc_app_server_protocol::ThreadGoal;
use xedoc_app_server_protocol::ThreadGoalClearParams;
use xedoc_app_server_protocol::ThreadGoalClearResponse;
use xedoc_app_server_protocol::ThreadGoalClearedNotification;
use xedoc_app_server_protocol::ThreadGoalGetParams;
use xedoc_app_server_protocol::ThreadGoalGetResponse;
use xedoc_app_server_protocol::ThreadGoalSetParams;
use xedoc_app_server_protocol::ThreadGoalSetResponse;
use xedoc_app_server_protocol::ThreadGoalStatus;
use xedoc_app_server_protocol::ThreadGoalUpdatedNotification;
use xedoc_app_server_protocol::ThreadHistoryBuilder;
#[cfg(test)]
use xedoc_app_server_protocol::ThreadHistoryMode;
use xedoc_app_server_protocol::ThreadIncrementElicitationParams;
use xedoc_app_server_protocol::ThreadIncrementElicitationResponse;
use xedoc_app_server_protocol::ThreadInjectItemsParams;
use xedoc_app_server_protocol::ThreadInjectItemsResponse;
use xedoc_app_server_protocol::ThreadItem;
use xedoc_app_server_protocol::ThreadItemEntry;
use xedoc_app_server_protocol::ThreadItemsListParams;
use xedoc_app_server_protocol::ThreadItemsListResponse;
use xedoc_app_server_protocol::ThreadListCwdFilter;
use xedoc_app_server_protocol::ThreadListParams;
use xedoc_app_server_protocol::ThreadListResponse;
use xedoc_app_server_protocol::ThreadLoadedListParams;
use xedoc_app_server_protocol::ThreadLoadedListResponse;
use xedoc_app_server_protocol::ThreadMetadataGitInfoUpdateParams;
use xedoc_app_server_protocol::ThreadMetadataUpdateParams;
use xedoc_app_server_protocol::ThreadMetadataUpdateResponse;
use xedoc_app_server_protocol::ThreadNameUpdateSource;
use xedoc_app_server_protocol::ThreadNameUpdatedNotification;
use xedoc_app_server_protocol::ThreadReadParams;
use xedoc_app_server_protocol::ThreadReadResponse;
use xedoc_app_server_protocol::ThreadResumeInitialTurnsPageParams;
use xedoc_app_server_protocol::ThreadResumeParams;
use xedoc_app_server_protocol::ThreadResumeResponse;
use xedoc_app_server_protocol::ThreadRollbackParams;
use xedoc_app_server_protocol::ThreadSearchOccurrence;
use xedoc_app_server_protocol::ThreadSearchOccurrencesParams;
use xedoc_app_server_protocol::ThreadSearchOccurrencesResponse;
use xedoc_app_server_protocol::ThreadSearchParams;
use xedoc_app_server_protocol::ThreadSearchResponse;
use xedoc_app_server_protocol::ThreadSearchResult;
use xedoc_app_server_protocol::ThreadSearchTextRange;
use xedoc_app_server_protocol::ThreadSetNameParams;
use xedoc_app_server_protocol::ThreadSetNameResponse;
use xedoc_app_server_protocol::ThreadSettings;
use xedoc_app_server_protocol::ThreadSettingsUpdateParams;
use xedoc_app_server_protocol::ThreadSettingsUpdateResponse;
use xedoc_app_server_protocol::ThreadShellCommandParams;
use xedoc_app_server_protocol::ThreadShellCommandResponse;
use xedoc_app_server_protocol::ThreadSortKey;
use xedoc_app_server_protocol::ThreadSourceKind;
use xedoc_app_server_protocol::ThreadStartParams;
use xedoc_app_server_protocol::ThreadStartResponse;
use xedoc_app_server_protocol::ThreadStartedNotification;
use xedoc_app_server_protocol::ThreadStatus;
use xedoc_app_server_protocol::ThreadTurnsListParams;
use xedoc_app_server_protocol::ThreadTurnsListResponse;
use xedoc_app_server_protocol::ThreadUnarchiveParams;
use xedoc_app_server_protocol::ThreadUnarchiveResponse;
use xedoc_app_server_protocol::ThreadUnarchivedNotification;
use xedoc_app_server_protocol::ThreadUnsubscribeParams;
use xedoc_app_server_protocol::ThreadUnsubscribeResponse;
use xedoc_app_server_protocol::ThreadUnsubscribeStatus;
use xedoc_app_server_protocol::Turn;
use xedoc_app_server_protocol::TurnEnvironmentParams;
use xedoc_app_server_protocol::TurnError;
use xedoc_app_server_protocol::TurnInterruptParams;
use xedoc_app_server_protocol::TurnInterruptResponse;
use xedoc_app_server_protocol::TurnItemsView;
use xedoc_app_server_protocol::TurnStartParams;
use xedoc_app_server_protocol::TurnStartResponse;
use xedoc_app_server_protocol::TurnStatus;
use xedoc_app_server_protocol::TurnSteerCancelParams;
use xedoc_app_server_protocol::TurnSteerCancelResponse;
use xedoc_app_server_protocol::TurnSteerCancelStatus;
use xedoc_app_server_protocol::TurnSteerParams;
use xedoc_app_server_protocol::TurnSteerResponse;
use xedoc_app_server_protocol::UserInput as V2UserInput;
use xedoc_app_server_protocol::XedocErrorInfo;
use xedoc_arg0::Arg0DispatchPaths;
use xedoc_config::CloudConfigBundleLoadError;
use xedoc_config::CloudConfigBundleLoadErrorCode;
use xedoc_config::ConfigLayerStack;
use xedoc_config::loader::project_trust_key;
use xedoc_config::types::McpServerTransportConfig;
use xedoc_core::ForkSnapshot;
use xedoc_core::NewThread;
#[cfg(test)]
use xedoc_core::SessionMeta;
use xedoc_core::StartThreadOptions;
use xedoc_core::SteerInputError;
use xedoc_core::ThreadConfigSnapshot;
use xedoc_core::ThreadManager;
use xedoc_core::XedocThread;
use xedoc_core::XedocThreadSettingsOverrides;
use xedoc_core::config::Config;
use xedoc_core::config::ConfigOverrides;
use xedoc_core::config::NetworkProxyAuditMetadata;
use xedoc_core::config::edit::ConfigEdit;
use xedoc_core::config::edit::ConfigEditsBuilder;
use xedoc_core::exec::ExecCapturePolicy;
use xedoc_core::exec::ExecExpiration;
use xedoc_core::exec::ExecParams;
use xedoc_core::exec_env::create_env;
use xedoc_core::path_utils;
#[cfg(test)]
use xedoc_core::read_head_for_summary;
use xedoc_core::sandboxing::SandboxPermissions;
use xedoc_core::truncate_rollout_after_turn_id;
use xedoc_core::truncate_rollout_before_turn_id;
use xedoc_core_plugins::PluginInstallError as CorePluginInstallError;
use xedoc_core_plugins::PluginInstallRequest;
use xedoc_core_plugins::PluginReadRequest;
use xedoc_core_plugins::PluginUninstallError as CorePluginUninstallError;
use xedoc_core_plugins::loader::load_plugin_mcp_servers;
use xedoc_core_plugins::manifest::PluginManifestInterface;
use xedoc_core_plugins::marketplace::MarketplaceError;
use xedoc_core_plugins::marketplace::MarketplacePluginSource;
use xedoc_core_plugins::marketplace_add::MarketplaceAddError;
use xedoc_core_plugins::marketplace_add::MarketplaceAddRequest;
use xedoc_core_plugins::marketplace_add::add_marketplace as add_marketplace_to_xedoc_home;
use xedoc_core_plugins::marketplace_remove::MarketplaceRemoveError;
use xedoc_core_plugins::marketplace_remove::MarketplaceRemoveRequest as CoreMarketplaceRemoveRequest;
use xedoc_core_plugins::marketplace_remove::remove_marketplace;
use xedoc_exec_server::EnvironmentManager;
use xedoc_exec_server::EnvironmentObservedStatus;
use xedoc_exec_server::LOCAL_ENVIRONMENT_ID;
use xedoc_exec_server::LOCAL_FS;
use xedoc_features::FEATURES;
use xedoc_features::Feature;
use xedoc_features::Stage;
use xedoc_git_utils::git_diff_to_remote;
use xedoc_git_utils::resolve_root_git_project_for_trust;
use xedoc_login::AuthManager;
use xedoc_login::LoginSuccessPage;
use xedoc_login::LoginSuccessPageBrand;
use xedoc_login::ServerOptions as LoginServerOptions;
use xedoc_login::ShutdownHandle;
use xedoc_login::XEDOC_OPEN_APP_URL;
use xedoc_login::XedocAuth;
use xedoc_login::complete_device_code_login;
use xedoc_login::login_with_api_key;
use xedoc_login::login_with_bedrock_api_key;
use xedoc_login::oauth_client_id;
use xedoc_login::request_device_code;
use xedoc_login::run_login_server;
use xedoc_mcp::McpRuntimeContext;
use xedoc_mcp::McpServerStatusSnapshot;
use xedoc_mcp::McpSnapshotDetail;
use xedoc_mcp::collect_mcp_server_status_snapshot_with_detail;
use xedoc_mcp::discover_supported_scopes_with_http_client;
use xedoc_mcp::read_mcp_resource as read_mcp_resource_without_thread;
use xedoc_mcp::resolve_oauth_scopes;
use xedoc_model_provider::create_model_provider;
use xedoc_models_manager::collaboration_mode_presets::builtin_collaboration_mode_presets;
use xedoc_protocol::ThreadId;
use xedoc_protocol::config_types::CollaborationMode;
use xedoc_protocol::config_types::ForcedLoginMethod;
use xedoc_protocol::config_types::Personality;
use xedoc_protocol::config_types::ReasoningSummary;
use xedoc_protocol::config_types::TrustLevel;
use xedoc_protocol::error::Result as XedocResult;
use xedoc_protocol::error::XedocErr;
#[cfg(test)]
use xedoc_protocol::items::TurnItem;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::openai_models::ReasoningEffort;
#[cfg(test)]
use xedoc_protocol::permissions::FileSystemSandboxPolicy;
use xedoc_protocol::protocol::AgentStatus;
use xedoc_protocol::protocol::EventMsg;
#[cfg(test)]
use xedoc_protocol::protocol::GitInfo as CoreGitInfo;
use xedoc_protocol::protocol::InitialHistory;
use xedoc_protocol::protocol::McpAuthStatus as CoreMcpAuthStatus;
use xedoc_protocol::protocol::Op;
use xedoc_protocol::protocol::ResumedHistory;
use xedoc_protocol::protocol::ReviewDelivery as CoreReviewDelivery;
use xedoc_protocol::protocol::ReviewRequest;
use xedoc_protocol::protocol::ReviewTarget as CoreReviewTarget;
use xedoc_protocol::protocol::RolloutItem;
use xedoc_protocol::protocol::SessionConfiguredEvent;
#[cfg(test)]
use xedoc_protocol::protocol::SessionMetaLine;
use xedoc_protocol::protocol::TurnEnvironmentSelection;
use xedoc_protocol::protocol::TurnEnvironmentSelections;
use xedoc_protocol::protocol::W3cTraceContext;
use xedoc_protocol::protocol::strip_user_message_prefix;
use xedoc_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use xedoc_protocol::user_input::UserInput as CoreInputItem;
use xedoc_rmcp_client::perform_oauth_login_return_url_with_http_client;
use xedoc_rollout::is_persisted_rollout_item;
use xedoc_rollout::state_db::StateDbHandle;
use xedoc_rollout::state_db::reconcile_rollout;
use xedoc_state::ThreadMetadata;
use xedoc_state::log_db::LogDbLayer;
use xedoc_thread_store::ArchiveThreadParams as StoreArchiveThreadParams;
use xedoc_thread_store::DeleteThreadParams as StoreDeleteThreadParams;
use xedoc_thread_store::GitInfoPatch as StoreGitInfoPatch;
use xedoc_thread_store::ListItemsParams as StoreListItemsParams;
use xedoc_thread_store::ListThreadsParams as StoreListThreadsParams;
use xedoc_thread_store::ListTurnsParams as StoreListTurnsParams;
use xedoc_thread_store::LoadThreadHistoryParams as StoreLoadThreadHistoryParams;
use xedoc_thread_store::LocalThreadStore;
use xedoc_thread_store::ReadThreadByRolloutPathParams as StoreReadThreadByRolloutPathParams;
use xedoc_thread_store::ReadThreadParams as StoreReadThreadParams;
use xedoc_thread_store::SearchThreadOccurrencesParams as StoreSearchThreadOccurrencesParams;
use xedoc_thread_store::SearchThreadsParams as StoreSearchThreadsParams;
use xedoc_thread_store::SortDirection as StoreSortDirection;
use xedoc_thread_store::StoredThread;
use xedoc_thread_store::StoredTurn;
use xedoc_thread_store::StoredTurnItemsView;
use xedoc_thread_store::StoredTurnStatus;
use xedoc_thread_store::ThreadMetadataPatch as StoreThreadMetadataPatch;
use xedoc_thread_store::ThreadRelationFilter as StoreThreadRelationFilter;
use xedoc_thread_store::ThreadSortKey as StoreThreadSortKey;
use xedoc_thread_store::ThreadStore;
use xedoc_thread_store::ThreadStoreError;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_pty::DEFAULT_OUTPUT_BYTES_CAP;

#[cfg(test)]
use xedoc_app_server_protocol::ServerRequest;

mod account_processor;
mod bedrock_auth;
mod catalog_processor;
mod command_exec_processor;
mod config_processor;
mod environment_processor;
mod fs_processor;
mod git_processor;
mod initialize_processor;
mod marketplace_processor;
mod mcp_processor;
mod model_manager_processor;
mod plugins;
mod process_exec_processor;
mod search;
mod thread_fork_goal;
mod thread_processor;
mod token_usage_replay;
mod turn_processor;

pub(crate) use account_processor::AccountRequestProcessor;
pub(crate) use catalog_processor::CatalogRequestProcessor;
pub(crate) use command_exec_processor::CommandExecRequestProcessor;
pub(crate) use config_processor::ConfigRequestProcessor;
pub(crate) use environment_processor::EnvironmentRequestProcessor;
pub(crate) use fs_processor::FsRequestProcessor;
pub(crate) use git_processor::GitRequestProcessor;
pub(crate) use initialize_processor::InitializeRequestProcessor;
pub(crate) use marketplace_processor::MarketplaceRequestProcessor;
pub(crate) use mcp_processor::McpRequestProcessor;
pub(crate) use model_manager_processor::ModelManagerRequestProcessor;
pub(crate) use plugins::PluginRequestProcessor;
pub(crate) use process_exec_processor::ProcessExecRequestProcessor;
pub(crate) use search::SearchRequestProcessor;
pub(crate) use thread_goal_processor::ThreadGoalRequestProcessor;
pub(crate) use thread_processor::ThreadRequestProcessor;
pub(crate) use turn_processor::TurnRequestProcessor;

use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use crate::filters::compute_source_filters;
use crate::filters::source_kind_matches;
use crate::thread_state::ThreadListenerCommand;
use crate::thread_state::ThreadState;
use crate::thread_state::ThreadStateManager;
use token_usage_replay::restored_token_usage_turn_id;
use token_usage_replay::send_thread_token_usage_update_to_connection;

fn resolve_request_cwd(cwd: Option<PathBuf>) -> Result<Option<AbsolutePathBuf>, JSONRPCErrorError> {
    cwd.map(|cwd| {
        AbsolutePathBuf::relative_to_current_dir(path_utils::normalize_for_native_workdir(cwd))
            .map_err(|err| invalid_request(format!("invalid cwd: {err}")))
    })
    .transpose()
}

fn resolve_turn_environment_selections(
    thread_manager: &ThreadManager,
    environments: Option<Vec<TurnEnvironmentParams>>,
) -> Result<Option<Vec<TurnEnvironmentSelection>>, JSONRPCErrorError> {
    let Some(environments) = environments else {
        return Ok(None);
    };
    let mut selections = Vec::with_capacity(environments.len());
    for environment in environments {
        let environment_id = environment.environment_id;
        let cwd = environment
            .cwd
            .to_inferred_path_uri()
            .ok_or_else(|| {
                invalid_request(format!(
                    "invalid cwd for environment `{environment_id}`: path `{}` does not use absolute POSIX or Windows path syntax",
                    environment.cwd
                ))
            })?;
        let workspace_roots = environment
            .runtime_workspace_roots
            .map(|roots| {
                let mut resolved_roots = Vec::new();
                for root in roots {
                    let root = root.to_inferred_path_uri().ok_or_else(|| {
                        invalid_request(format!(
                            "invalid runtime workspace root for environment `{environment_id}`: path `{root}` does not use absolute POSIX or Windows path syntax"
                        ))
                    })?;
                    if !resolved_roots.contains(&root) {
                        resolved_roots.push(root);
                    }
                }
                Ok::<_, JSONRPCErrorError>(resolved_roots)
            })
            .transpose()?
            .unwrap_or_else(|| vec![cwd.clone()]);
        selections.push(TurnEnvironmentSelection {
            environment_id,
            cwd,
            workspace_roots,
        });
    }
    thread_manager
        .validate_environment_selections(&selections)
        .map_err(environment_selection_error)?;
    Ok(Some(selections))
}

fn resolve_runtime_workspace_roots(workspace_roots: Vec<AbsolutePathBuf>) -> Vec<AbsolutePathBuf> {
    let mut resolved_roots = Vec::new();
    for root in workspace_roots {
        if !resolved_roots.iter().any(|existing| existing == &root) {
            resolved_roots.push(root);
        }
    }
    resolved_roots
}

mod config_errors;
mod request_errors;
mod thread_delete;
mod thread_goal_processor;
mod thread_lifecycle;
mod thread_resume_redaction;
mod thread_summary;

use self::config_errors::*;
use self::request_errors::*;
use self::thread_goal_processor::api_thread_goal_from_state;
use self::thread_lifecycle::*;
use self::thread_resume_redaction::*;
use self::thread_summary::*;

pub(crate) use self::thread_lifecycle::populate_thread_turns_from_history;
pub(crate) use self::thread_processor::thread_from_stored_thread;
#[cfg(test)]
pub(crate) use self::thread_summary::read_summary_from_rollout;
#[cfg(test)]
pub(crate) use self::thread_summary::summary_to_thread;
pub(crate) use self::thread_summary::thread_settings_from_config_snapshot;
pub(crate) use self::thread_summary::thread_settings_from_core_snapshot;

pub(crate) fn build_legacy_api_turns_from_rollout_items(items: &[RolloutItem]) -> Vec<Turn> {
    let mut builder = ThreadHistoryBuilder::new();
    for item in items {
        if is_persisted_rollout_item(item, xedoc_protocol::protocol::ThreadHistoryMode::Legacy) {
            builder.handle_rollout_item(item);
        }
    }
    builder.finish()
}
