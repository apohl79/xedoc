use std::sync::Arc;

use crate::config_manager::ConfigManager;
use crate::config_manager_service::ConfigManagerError;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use crate::outgoing_message::ConnectionRequestId;
use crate::outgoing_message::OutgoingMessageSender;
use serde_json::json;
use std::path::PathBuf;
use xedoc_app_server_protocol::ClientResponsePayload;
use xedoc_app_server_protocol::ComputerUseRequirements;
use xedoc_app_server_protocol::ConfigBatchWriteParams;
use xedoc_app_server_protocol::ConfigReadParams;
use xedoc_app_server_protocol::ConfigReadResponse;
use xedoc_app_server_protocol::ConfigRequirements;
use xedoc_app_server_protocol::ConfigRequirementsReadResponse;
use xedoc_app_server_protocol::ConfigValueWriteParams;
use xedoc_app_server_protocol::ConfigWriteErrorCode;
use xedoc_app_server_protocol::ConfigWriteResponse;
use xedoc_app_server_protocol::ConfiguredHookHandler;
use xedoc_app_server_protocol::ConfiguredHookMatcherGroup;
use xedoc_app_server_protocol::ExperimentalFeatureEnablementSetParams;
use xedoc_app_server_protocol::ExperimentalFeatureEnablementSetResponse;
use xedoc_app_server_protocol::JSONRPCErrorError;
use xedoc_app_server_protocol::ManagedHooksRequirements;
use xedoc_app_server_protocol::ModelProviderCapabilitiesReadResponse;
use xedoc_app_server_protocol::ModelsRequirements;
use xedoc_app_server_protocol::NetworkDomainPermission;
use xedoc_app_server_protocol::NetworkRequirements;
use xedoc_app_server_protocol::NetworkUnixSocketPermission;
use xedoc_app_server_protocol::NewThreadModelDefaults;
use xedoc_app_server_protocol::SandboxMode;
use xedoc_app_server_protocol::TokenUsageOptimizerBreakdown;
use xedoc_app_server_protocol::TokenUsageOptimizerInsights;
use xedoc_app_server_protocol::TokenUsageOptimizerLevel;
use xedoc_app_server_protocol::TokenUsageOptimizerReadResponse;
use xedoc_app_server_protocol::TokenUsageOptimizerReportDay;
use xedoc_app_server_protocol::TokenUsageOptimizerReportModel;
use xedoc_app_server_protocol::TokenUsageOptimizerReportParams;
use xedoc_app_server_protocol::TokenUsageOptimizerReportResponse;
use xedoc_app_server_protocol::TokenUsageOptimizerTopReduction;
use xedoc_app_server_protocol::TokenUsageOptimizerWriteParams;
use xedoc_app_server_protocol::TokenUsageOptimizerWriteResponse;
use xedoc_config::ConfigRequirementsToml;
use xedoc_config::HookEventsToml;
use xedoc_config::HookHandlerConfig as CoreHookHandlerConfig;
use xedoc_config::ManagedHooksRequirementsToml;
use xedoc_config::MatcherGroup as CoreMatcherGroup;
use xedoc_config::ResidencyRequirement as CoreResidencyRequirement;
use xedoc_config::SandboxModeRequirement as CoreSandboxModeRequirement;
use xedoc_core::ThreadManager;
use xedoc_features::TokenUsageOptimizerLevel as CoreTokenUsageOptimizerLevel;
use xedoc_features::canonical_feature_for_key;
use xedoc_features::feature_for_key;
use xedoc_model_provider::create_model_provider;
use xedoc_protocol::config_types::WebSearchMode;
use xedoc_rollout::state_db::StateDbHandle;

const SUPPORTED_EXPERIMENTAL_FEATURE_ENABLEMENT: &[&str] = &["auth_elicitation", "mentions_v2"];

#[derive(Clone)]
pub(crate) struct ConfigRequestProcessor {
    outgoing: Arc<OutgoingMessageSender>,
    config_manager: ConfigManager,
    thread_manager: Arc<ThreadManager>,
    state_db: Option<StateDbHandle>,
}

impl ConfigRequestProcessor {
    pub(crate) fn new(
        outgoing: Arc<OutgoingMessageSender>,
        config_manager: ConfigManager,
        thread_manager: Arc<ThreadManager>,
        state_db: Option<StateDbHandle>,
    ) -> Self {
        Self {
            outgoing,
            config_manager,
            thread_manager,
            state_db,
        }
    }

    pub(crate) async fn read(
        &self,
        params: ConfigReadParams,
    ) -> Result<ConfigReadResponse, JSONRPCErrorError> {
        let fallback_cwd = params.cwd.as_ref().map(PathBuf::from);
        let mut response = self.config_manager.read(params).await.map_err(map_error)?;
        let config = self.load_latest_config(fallback_cwd).await?;
        for feature_key in SUPPORTED_EXPERIMENTAL_FEATURE_ENABLEMENT {
            let Some(feature) = feature_for_key(feature_key) else {
                continue;
            };
            let features = response
                .config
                .additional
                .entry("features".to_string())
                .or_insert_with(|| json!({}));
            if !features.is_object() {
                *features = json!({});
            }
            if let Some(features) = features.as_object_mut() {
                features.insert(
                    (*feature_key).to_string(),
                    json!(config.features.enabled(feature)),
                );
            }
        }
        Ok(response)
    }

    pub(crate) async fn config_requirements_read(
        &self,
    ) -> Result<ConfigRequirementsReadResponse, JSONRPCErrorError> {
        let requirements = self
            .config_manager
            .read_requirements()
            .await
            .map_err(map_error)?
            .map(map_requirements_toml_to_api);

        Ok(ConfigRequirementsReadResponse { requirements })
    }

    pub(crate) async fn value_write(
        &self,
        params: ConfigValueWriteParams,
    ) -> Result<ClientResponsePayload, JSONRPCErrorError> {
        self.handle_config_mutation_result(self.write_value(params).await)
            .await
            .map(ClientResponsePayload::ConfigValueWrite)
    }

    pub(crate) async fn batch_write(
        &self,
        params: ConfigBatchWriteParams,
    ) -> Result<ClientResponsePayload, JSONRPCErrorError> {
        self.handle_config_mutation_result(self.batch_write_inner(params).await)
            .await
            .map(ClientResponsePayload::ConfigBatchWrite)
    }

    pub(crate) async fn token_usage_optimizer_read(
        &self,
    ) -> Result<TokenUsageOptimizerReadResponse, JSONRPCErrorError> {
        let config = self.load_latest_config(/*fallback_cwd*/ None).await?;
        let (reduction_count, tokens_saved) = match &self.state_db {
            Some(state_db) => state_db
                .tool_output_reduction_stats()
                .await
                .map_err(|err| internal_error(format!("failed to read optimizer stats: {err}")))?,
            None => (0, 0),
        };
        let (insights, cost_saved_usd) = match &self.state_db {
            Some(state_db) => {
                let insights = state_db
                    .tool_output_reduction_insights(None)
                    .await
                    .map_err(|err| {
                        internal_error(format!("failed to read optimizer insights: {err}"))
                    })?;
                let cost_saved_usd = insights.cost_saved_usd;
                (map_optimizer_insights(insights), cost_saved_usd)
            }
            None => (empty_optimizer_insights(), 0.0),
        };
        Ok(TokenUsageOptimizerReadResponse {
            enabled: config.token_usage_optimizer.enabled.unwrap_or(false),
            level: match config.token_usage_optimizer.level {
                CoreTokenUsageOptimizerLevel::Conservative => {
                    TokenUsageOptimizerLevel::Conservative
                }
                CoreTokenUsageOptimizerLevel::Balanced => TokenUsageOptimizerLevel::Balanced,
                CoreTokenUsageOptimizerLevel::Aggressive => TokenUsageOptimizerLevel::Aggressive,
            },
            reduction_count,
            tokens_saved,
            cost_saved_usd,
            insights,
        })
    }

    pub(crate) async fn token_usage_optimizer_report(
        &self,
        params: TokenUsageOptimizerReportParams,
    ) -> Result<TokenUsageOptimizerReportResponse, JSONRPCErrorError> {
        let today = chrono::Utc::now().date_naive();
        let since = params.since_day.unwrap_or_else(|| {
            (today - chrono::Duration::days(89))
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp()
        });
        let until = params
            .until_day
            .unwrap_or_else(|| today.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp());
        let report = match &self.state_db {
            Some(state_db) => state_db
                .tool_output_reduction_report(since, until, params.model.as_deref())
                .await
                .map_err(|err| internal_error(format!("failed to read optimizer report: {err}")))?,
            None => xedoc_state::ToolOutputReductionReport {
                since_day: since,
                until_day: until,
                days: Vec::new(),
                reductions: 0,
                tokens_saved: 0,
                cost_saved_usd: 0.0,
            },
        };
        Ok(TokenUsageOptimizerReportResponse {
            since_day: report.since_day,
            until_day: report.until_day,
            days: report
                .days
                .into_iter()
                .map(|day| TokenUsageOptimizerReportDay {
                    day: day.day,
                    partial: day.partial,
                    by_model: day
                        .by_model
                        .into_iter()
                        .map(|model| TokenUsageOptimizerReportModel {
                            model: model.model_slug,
                            reductions: model.reductions,
                            tokens_saved: model.tokens_saved,
                            cost_saved_usd: model.cost_saved_usd,
                        })
                        .collect(),
                    reductions: day.reductions,
                    tokens_saved: day.tokens_saved,
                    cost_saved_usd: day.cost_saved_usd,
                })
                .collect(),
            reductions: report.reductions,
            tokens_saved: report.tokens_saved,
            cost_saved_usd: report.cost_saved_usd,
        })
    }

    pub(crate) async fn token_usage_optimizer_write(
        &self,
        params: TokenUsageOptimizerWriteParams,
    ) -> Result<TokenUsageOptimizerWriteResponse, JSONRPCErrorError> {
        if params.reset_stats
            && let Some(state_db) = &self.state_db
        {
            state_db
                .reset_tool_output_reduction_stats()
                .await
                .map_err(|err| internal_error(format!("failed to reset optimizer stats: {err}")))?;
        }
        let mut value = serde_json::Map::new();
        if let Some(enabled) = params.enabled {
            value.insert("enabled".to_string(), json!(enabled));
        }
        if let Some(level) = params.level {
            value.insert("level".to_string(), json!(level));
        }
        if !value.is_empty() {
            self.config_manager
                .write_value(ConfigValueWriteParams {
                    key_path: "token_usage_optimizer".to_string(),
                    value: serde_json::Value::Object(value),
                    merge_strategy: xedoc_app_server_protocol::MergeStrategy::Upsert,
                    file_path: None,
                    expected_version: params.expected_version,
                })
                .await
                .map_err(map_error)?;
            self.handle_config_mutation().await;
        }
        self.token_usage_optimizer_read()
            .await
            .map(|response| TokenUsageOptimizerWriteResponse {
                enabled: response.enabled,
                level: response.level,
                reduction_count: response.reduction_count,
                tokens_saved: response.tokens_saved,
                cost_saved_usd: response.cost_saved_usd,
                insights: response.insights,
            })
    }

    pub(crate) async fn experimental_feature_enablement_set(
        &self,
        request_id: ConnectionRequestId,
        params: ExperimentalFeatureEnablementSetParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let response = self
            .handle_config_mutation_result(self.set_experimental_feature_enablement(params).await)
            .await?;
        self.outgoing
            .send_response_as(
                request_id,
                ClientResponsePayload::ExperimentalFeatureEnablementSet(response),
            )
            .await;
        Ok(None)
    }

    pub(crate) async fn model_provider_capabilities_read(
        &self,
    ) -> Result<ModelProviderCapabilitiesReadResponse, JSONRPCErrorError> {
        let config = self.load_latest_config(/*fallback_cwd*/ None).await?;
        let provider = create_model_provider(config.model_provider, /*auth_manager*/ None);
        let capabilities = provider.capabilities();
        Ok(ModelProviderCapabilitiesReadResponse {
            namespace_tools: capabilities.namespace_tools,
            image_generation: capabilities.image_generation,
            web_search: capabilities.web_search,
        })
    }

    pub(crate) async fn handle_config_mutation(&self) {
        self.thread_manager.plugins_manager().clear_cache();
        self.thread_manager.skills_service().clear_cache();
    }

    async fn handle_config_mutation_result<T>(
        &self,
        result: std::result::Result<T, JSONRPCErrorError>,
    ) -> Result<T, JSONRPCErrorError> {
        let response = result?;
        self.handle_config_mutation().await;
        Ok(response)
    }

    async fn load_latest_config(
        &self,
        fallback_cwd: Option<PathBuf>,
    ) -> Result<xedoc_core::config::Config, JSONRPCErrorError> {
        self.config_manager
            .load_latest_config(fallback_cwd)
            .await
            .map_err(|err| {
                internal_error(format!(
                    "failed to resolve feature override precedence: {err}"
                ))
            })
    }

    async fn write_value(
        &self,
        params: ConfigValueWriteParams,
    ) -> Result<ConfigWriteResponse, JSONRPCErrorError> {
        let response = self
            .config_manager
            .write_value(params)
            .await
            .map_err(map_error)?;
        Ok(response)
    }

    async fn batch_write_inner(
        &self,
        params: ConfigBatchWriteParams,
    ) -> Result<ConfigWriteResponse, JSONRPCErrorError> {
        let reload_user_config = params.reload_user_config;
        let response = self
            .config_manager
            .batch_write(params)
            .await
            .map_err(map_error)?;
        if reload_user_config {
            self.reload_user_config().await;
        }
        Ok(response)
    }

    async fn set_experimental_feature_enablement(
        &self,
        params: ExperimentalFeatureEnablementSetParams,
    ) -> Result<ExperimentalFeatureEnablementSetResponse, JSONRPCErrorError> {
        let ExperimentalFeatureEnablementSetParams { mut enablement } = params;
        let mut invalid_keys = Vec::new();
        enablement.retain(|key, _| {
            let valid = canonical_feature_for_key(key).is_some()
                && SUPPORTED_EXPERIMENTAL_FEATURE_ENABLEMENT.contains(&key.as_str());
            if !valid {
                invalid_keys.push(key.clone());
            }
            valid
        });
        if !invalid_keys.is_empty() {
            let invalid_keys = invalid_keys.join(", ");
            tracing::warn!("ignoring invalid experimental feature enablement keys: {invalid_keys}");
        }

        if enablement.is_empty() {
            return Ok(ExperimentalFeatureEnablementSetResponse { enablement });
        }

        self.config_manager
            .extend_runtime_feature_enablement(
                enablement
                    .iter()
                    .map(|(name, enabled)| (name.clone(), *enabled)),
            )
            .map_err(|_| internal_error("failed to update feature enablement"))?;

        self.load_latest_config(/*fallback_cwd*/ None).await?;
        self.reload_user_config().await;

        Ok(ExperimentalFeatureEnablementSetResponse { enablement })
    }

    pub(crate) async fn reload_user_config(&self) {
        let next_config = match self.load_latest_config(/*fallback_cwd*/ None).await {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(
                    "failed to rebuild user config for runtime refresh: {}",
                    err.message
                );
                return;
            }
        };
        let thread_ids = self.thread_manager.list_thread_ids().await;
        for thread_id in thread_ids {
            let Ok(thread) = self.thread_manager.get_thread(thread_id).await else {
                continue;
            };
            thread.refresh_runtime_config(next_config.clone()).await;
        }
    }
}

fn empty_optimizer_insights() -> TokenUsageOptimizerInsights {
    TokenUsageOptimizerInsights {
        by_kind: Vec::new(),
        by_reducer: Vec::new(),
        by_tool: Vec::new(),
        by_model: Vec::new(),
        top_reductions: Vec::new(),
        retrievals: 0,
        spilled: 0,
    }
}

fn map_optimizer_insights(
    insights: xedoc_state::ToolOutputReductionInsights,
) -> TokenUsageOptimizerInsights {
    let map_breakdown = |items: Vec<xedoc_state::ToolOutputReductionBreakdown>| {
        items
            .into_iter()
            .map(|item| TokenUsageOptimizerBreakdown {
                dimension: item.dimension,
                reductions: item.reductions,
                bytes_in: item.bytes_in,
                bytes_out: item.bytes_out,
                retrievals: item.retrievals,
                reruns: item.reruns,
                cost_saved_usd: None,
            })
            .collect()
    };
    TokenUsageOptimizerInsights {
        by_kind: map_breakdown(insights.by_kind),
        by_reducer: map_breakdown(insights.by_reducer),
        by_tool: map_breakdown(insights.by_tool),
        by_model: insights
            .by_model
            .into_iter()
            .map(|item| TokenUsageOptimizerBreakdown {
                dimension: item.dimension,
                reductions: item.reductions,
                bytes_in: 0,
                bytes_out: 0,
                retrievals: 0,
                reruns: 0,
                cost_saved_usd: item.cost_saved_usd,
            })
            .collect(),
        top_reductions: insights
            .top_reductions
            .into_iter()
            .map(|item| TokenUsageOptimizerTopReduction {
                call_id: item.call_id,
                tool_name: item.tool_name,
                kind: item.kind,
                bytes_in: item.bytes_in,
                bytes_out: item.bytes_out,
                tokens_saved: item.tokens_saved,
                cost_saved_usd: item.cost_saved_usd,
            })
            .collect(),
        retrievals: insights.retrievals,
        spilled: insights.spilled,
    }
}

fn map_requirements_toml_to_api(requirements: ConfigRequirementsToml) -> ConfigRequirements {
    ConfigRequirements {
        allowed_approval_policies: requirements.allowed_approval_policies.map(|policies| {
            policies
                .into_iter()
                .map(xedoc_app_server_protocol::AskForApproval::from)
                .collect()
        }),
        allowed_sandbox_modes: requirements.allowed_sandbox_modes.map(|modes| {
            modes
                .into_iter()
                .filter_map(map_sandbox_mode_requirement_to_api)
                .collect()
        }),
        allowed_permission_profiles: requirements.allowed_permission_profiles,
        default_permissions: requirements.default_permissions,
        allowed_web_search_modes: requirements.allowed_web_search_modes.map(|modes| {
            let mut normalized = modes
                .into_iter()
                .map(Into::into)
                .collect::<Vec<WebSearchMode>>();
            if !normalized.contains(&WebSearchMode::Disabled) {
                normalized.push(WebSearchMode::Disabled);
            }
            normalized
        }),
        allow_managed_hooks_only: requirements.allow_managed_hooks_only,
        allow_appshots: requirements.allow_appshots,
        computer_use: requirements
            .computer_use
            .map(map_computer_use_requirements_to_api),
        feature_requirements: requirements
            .feature_requirements
            .map(|requirements| requirements.entries),
        hooks: requirements.hooks.map(map_hooks_requirements_to_api),
        enforce_residency: requirements
            .enforce_residency
            .map(map_residency_requirement_to_api),
        network: requirements.network.map(map_network_requirements_to_api),
        models: requirements.models.map(|models| ModelsRequirements {
            new_thread: models.new_thread.map(|new_thread| NewThreadModelDefaults {
                model: new_thread.model,
                model_reasoning_effort: new_thread.model_reasoning_effort,
                service_tier: new_thread.service_tier,
            }),
        }),
    }
}

fn map_computer_use_requirements_to_api(
    computer_use: xedoc_config::ComputerUseRequirementsToml,
) -> ComputerUseRequirements {
    ComputerUseRequirements {
        allow_locked_computer_use: computer_use.allow_locked_computer_use,
    }
}

fn map_hooks_requirements_to_api(hooks: ManagedHooksRequirementsToml) -> ManagedHooksRequirements {
    let ManagedHooksRequirementsToml {
        managed_dir,
        windows_managed_dir,
        hooks,
    } = hooks;
    let HookEventsToml {
        pre_tool_use,
        permission_request,
        post_tool_use,
        pre_compact,
        post_compact,
        session_start,
        session_end,
        user_prompt_submit,
        subagent_start,
        subagent_stop,
        stop,
    } = hooks;

    ManagedHooksRequirements {
        managed_dir,
        windows_managed_dir,
        pre_tool_use: map_hook_matcher_groups_to_api(pre_tool_use),
        permission_request: map_hook_matcher_groups_to_api(permission_request),
        post_tool_use: map_hook_matcher_groups_to_api(post_tool_use),
        pre_compact: map_hook_matcher_groups_to_api(pre_compact),
        post_compact: map_hook_matcher_groups_to_api(post_compact),
        session_start: map_hook_matcher_groups_to_api(session_start),
        session_end: map_hook_matcher_groups_to_api(session_end),
        user_prompt_submit: map_hook_matcher_groups_to_api(user_prompt_submit),
        subagent_start: map_hook_matcher_groups_to_api(subagent_start),
        subagent_stop: map_hook_matcher_groups_to_api(subagent_stop),
        stop: map_hook_matcher_groups_to_api(stop),
    }
}

fn map_hook_matcher_groups_to_api(
    groups: Vec<CoreMatcherGroup>,
) -> Vec<ConfiguredHookMatcherGroup> {
    groups
        .into_iter()
        .map(map_hook_matcher_group_to_api)
        .collect()
}

fn map_hook_matcher_group_to_api(group: CoreMatcherGroup) -> ConfiguredHookMatcherGroup {
    ConfiguredHookMatcherGroup {
        matcher: group.matcher,
        hooks: group
            .hooks
            .into_iter()
            .map(map_hook_handler_to_api)
            .collect(),
    }
}

fn map_hook_handler_to_api(handler: CoreHookHandlerConfig) -> ConfiguredHookHandler {
    match handler {
        CoreHookHandlerConfig::Command {
            command,
            command_windows,
            timeout_sec,
            r#async,
            status_message,
            additional_context_limit,
        } => ConfiguredHookHandler::Command {
            command,
            command_windows,
            timeout_sec,
            r#async,
            status_message,
            additional_context_limit,
        },
        CoreHookHandlerConfig::Prompt {} => ConfiguredHookHandler::Prompt {},
        CoreHookHandlerConfig::Agent {} => ConfiguredHookHandler::Agent {},
    }
}

fn map_sandbox_mode_requirement_to_api(mode: CoreSandboxModeRequirement) -> Option<SandboxMode> {
    match mode {
        CoreSandboxModeRequirement::ReadOnly => Some(SandboxMode::ReadOnly),
        CoreSandboxModeRequirement::WorkspaceWrite => Some(SandboxMode::WorkspaceWrite),
        CoreSandboxModeRequirement::DangerFullAccess => Some(SandboxMode::DangerFullAccess),
        CoreSandboxModeRequirement::ExternalSandbox => None,
    }
}

fn map_residency_requirement_to_api(
    residency: CoreResidencyRequirement,
) -> xedoc_app_server_protocol::ResidencyRequirement {
    match residency {
        CoreResidencyRequirement::Us => xedoc_app_server_protocol::ResidencyRequirement::Us,
    }
}

fn map_network_requirements_to_api(
    network: xedoc_config::NetworkRequirementsToml,
) -> NetworkRequirements {
    let allowed_domains = network
        .domains
        .as_ref()
        .and_then(xedoc_config::NetworkDomainPermissionsToml::allowed_domains);
    let denied_domains = network
        .domains
        .as_ref()
        .and_then(xedoc_config::NetworkDomainPermissionsToml::denied_domains);
    let allow_unix_sockets = network
        .unix_sockets
        .as_ref()
        .map(xedoc_config::NetworkUnixSocketPermissionsToml::allow_unix_sockets)
        .filter(|entries| !entries.is_empty());

    NetworkRequirements {
        enabled: network.enabled,
        http_port: network.http_port,
        socks_port: network.socks_port,
        allow_upstream_proxy: network.allow_upstream_proxy,
        dangerously_allow_non_loopback_proxy: network.dangerously_allow_non_loopback_proxy,
        dangerously_allow_all_unix_sockets: network.dangerously_allow_all_unix_sockets,
        domains: network.domains.map(|domains| {
            domains
                .entries
                .into_iter()
                .map(|(pattern, permission)| {
                    (pattern, map_network_domain_permission_to_api(permission))
                })
                .collect()
        }),
        managed_allowed_domains_only: network.managed_allowed_domains_only,
        allowed_domains,
        denied_domains,
        unix_sockets: network.unix_sockets.map(|unix_sockets| {
            unix_sockets
                .entries
                .into_iter()
                .map(|(path, permission)| {
                    (path, map_network_unix_socket_permission_to_api(permission))
                })
                .collect()
        }),
        allow_unix_sockets,
        allow_local_binding: network.allow_local_binding,
    }
}

fn map_network_domain_permission_to_api(
    permission: xedoc_config::NetworkDomainPermissionToml,
) -> NetworkDomainPermission {
    match permission {
        xedoc_config::NetworkDomainPermissionToml::Allow => NetworkDomainPermission::Allow,
        xedoc_config::NetworkDomainPermissionToml::Deny => NetworkDomainPermission::Deny,
    }
}

fn map_network_unix_socket_permission_to_api(
    permission: xedoc_config::NetworkUnixSocketPermissionToml,
) -> NetworkUnixSocketPermission {
    match permission {
        xedoc_config::NetworkUnixSocketPermissionToml::Allow => NetworkUnixSocketPermission::Allow,
        xedoc_config::NetworkUnixSocketPermissionToml::Deny => NetworkUnixSocketPermission::Deny,
    }
}

pub(super) fn map_error(err: ConfigManagerError) -> JSONRPCErrorError {
    if let Some(code) = err.write_error_code() {
        return config_write_error(code, err.to_string());
    }

    internal_error(err.to_string())
}

fn config_write_error(code: ConfigWriteErrorCode, message: impl Into<String>) -> JSONRPCErrorError {
    let mut error = invalid_request(message);
    error.data = Some(json!({
        "config_write_error_code": code,
    }));
    error
}

#[cfg(test)]
mod tests {
    use super::map_requirements_toml_to_api;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use xedoc_config::ComputerUseRequirementsToml;
    use xedoc_config::ConfigRequirementsToml;
    use xedoc_config::ModelsRequirementsToml;
    use xedoc_config::NewThreadModelDefaultsToml;
    use xedoc_protocol::openai_models::ReasoningEffort;

    #[test]
    fn requirements_api_includes_allow_managed_hooks_only() {
        let mapped = map_requirements_toml_to_api(ConfigRequirementsToml {
            allow_managed_hooks_only: Some(true),
            ..ConfigRequirementsToml::default()
        });

        assert_eq!(mapped.allow_managed_hooks_only, Some(true));
        assert_eq!(mapped.hooks, None);
    }

    #[test]
    fn requirements_api_includes_permission_default_and_allowlist() {
        let mapped = map_requirements_toml_to_api(ConfigRequirementsToml {
            allowed_permission_profiles: Some(BTreeMap::from([
                ("managed-build".to_string(), false),
                ("managed-standard".to_string(), true),
            ])),
            default_permissions: Some("managed-standard".to_string()),
            ..ConfigRequirementsToml::default()
        });

        assert_eq!(
            mapped.allowed_permission_profiles,
            Some(BTreeMap::from([
                ("managed-build".to_string(), false),
                ("managed-standard".to_string(), true),
            ]))
        );
        assert_eq!(
            mapped.default_permissions,
            Some("managed-standard".to_string())
        );
    }

    #[test]
    fn requirements_api_includes_allow_appshots() {
        let mapped = map_requirements_toml_to_api(ConfigRequirementsToml {
            allow_appshots: Some(false),
            ..ConfigRequirementsToml::default()
        });

        assert_eq!(mapped.allow_appshots, Some(false));
        assert_eq!(mapped.hooks, None);
    }

    #[test]
    fn requirements_api_includes_new_thread_model_defaults() {
        let mapped = map_requirements_toml_to_api(ConfigRequirementsToml {
            models: Some(ModelsRequirementsToml {
                new_thread: Some(NewThreadModelDefaultsToml {
                    model: Some("gpt-managed".to_string()),
                    model_reasoning_effort: Some(ReasoningEffort::Medium),
                    service_tier: Some("fast".to_string()),
                }),
            }),
            ..ConfigRequirementsToml::default()
        });

        let defaults = mapped
            .models
            .and_then(|models| models.new_thread)
            .expect("new-thread defaults");
        assert_eq!(defaults.model.as_deref(), Some("gpt-managed"));
        assert_eq!(
            defaults.model_reasoning_effort,
            Some(ReasoningEffort::Medium)
        );
        assert_eq!(defaults.service_tier.as_deref(), Some("fast"));
    }

    #[test]
    fn requirements_api_includes_computer_use_requirements() {
        let mapped = map_requirements_toml_to_api(ConfigRequirementsToml {
            computer_use: Some(ComputerUseRequirementsToml {
                allow_locked_computer_use: Some(false),
            }),
            ..ConfigRequirementsToml::default()
        });

        assert_eq!(
            mapped
                .computer_use
                .and_then(|requirements| requirements.allow_locked_computer_use),
            Some(false)
        );
    }
}
