//! Runtime configuration persistence helpers for the TUI app.
//!
//! This module owns the app-level glue between config.toml edits, in-memory `Config` refreshes,
//! and the ChatWidget copy of session settings, keeping persistence-heavy code out of the main app
//! loop.

use super::*;
use xedoc_config::ConfigLayerSource;

async fn build_config_on_runtime_worker(
    builder: ConfigBuilder,
    error_context: String,
) -> Result<Config> {
    match tokio::spawn(async move { builder.build().await }).await {
        Ok(build_result) => build_result.wrap_err(error_context),
        Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
        Err(err) => Err(err).wrap_err_with(|| format!("{error_context} task failed")),
    }
}

pub(super) fn resume_model_settings_for_overrides(
    config: &Config,
    harness_overrides: &ConfigOverrides,
) -> crate::app_server_session::ResumeModelSettings {
    let has_session_flag_override = config
        .config_layer_stack
        .layers_high_to_low()
        .into_iter()
        .any(|layer| {
            matches!(&layer.name, ConfigLayerSource::SessionFlags)
                && ["model", "model_provider", "model_reasoning_effort"]
                    .iter()
                    .any(|key| layer.config.get(*key).is_some())
        });
    if harness_overrides.model.is_some()
        || harness_overrides.model_provider.is_some()
        || has_session_flag_override
    {
        crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig
    } else {
        crate::app_server_session::ResumeModelSettings::RestoreFromThread
    }
}

impl App {
    pub(super) async fn update_token_usage_optimizer_level(
        &mut self,
        app_server: &mut AppServerSession,
        level: String,
    ) {
        let level_name = level.to_ascii_lowercase();
        let level = match level_name.as_str() {
            "conservative" => xedoc_features::TokenUsageOptimizerLevel::Conservative,
            "balanced" => xedoc_features::TokenUsageOptimizerLevel::Balanced,
            "aggressive" => xedoc_features::TokenUsageOptimizerLevel::Aggressive,
            _ => {
                self.chat_widget
                    .add_error_message("Invalid token usage optimizer level".to_string());
                return;
            }
        };
        let write_response = match crate::config_update::write_config_batch(
            app_server.request_handle(),
            vec![crate::config_update::replace_config_value(
                "token_usage_optimizer.level",
                serde_json::json!(level),
            )],
        )
        .await
        {
            Ok(response) => response,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to save token usage optimizer level: {err}"
                ));
                return;
            }
        };
        if write_response.status == WriteStatus::OkOverridden {
            self.chat_widget.add_error_message(
                "Token usage optimizer level was saved but not applied by the active config."
                    .to_string(),
            );
            return;
        }
        self.config.token_usage_optimizer.level = level;
        self.chat_widget.add_info_message(
            format!("Token usage optimizer level set to {level_name}"),
            /*hint*/ None,
        );
    }

    pub(super) async fn rebuild_config_for_cwd(&self, cwd: PathBuf) -> Result<Config> {
        let mut overrides = self.harness_overrides.clone();
        overrides.cwd = Some(cwd.clone());
        let cwd_display = cwd.display().to_string();
        let builder = ConfigBuilder::default()
            .xedoc_home(self.config.xedoc_home.to_path_buf())
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(overrides)
            .loader_overrides(self.loader_overrides.clone())
            .cloud_config_bundle(self.cloud_config_bundle.clone());
        build_config_on_runtime_worker(
            builder,
            format!("Failed to rebuild config for cwd {cwd_display}"),
        )
        .await
    }

    pub(super) async fn rebuild_config_for_permission_profile(
        &self,
        profile_id: &str,
    ) -> Result<Config> {
        let mut overrides = self.harness_overrides.clone();
        overrides.cwd = Some(self.chat_widget.config_ref().cwd.to_path_buf());
        overrides.sandbox_mode = None;
        overrides.permission_profile = None;
        overrides.default_permissions = Some(profile_id.to_string());
        let builder = ConfigBuilder::default()
            .xedoc_home(self.config.xedoc_home.to_path_buf())
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(overrides)
            .loader_overrides(self.loader_overrides.clone())
            .cloud_config_bundle(self.cloud_config_bundle.clone());
        build_config_on_runtime_worker(
            builder,
            format!("Failed to rebuild config for permission profile {profile_id}"),
        )
        .await
    }

    pub(super) async fn apply_permission_profile_selection(
        &mut self,
        selection: PermissionProfileSelection,
    ) -> bool {
        let PermissionProfileSelection {
            profile_id,
            approval_policy,
            display_label,
        } = selection;
        let selected_config = match self
            .rebuild_config_for_permission_profile(profile_id.as_str())
            .await
        {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    profile_id,
                    "failed to resolve selected permission profile"
                );
                self.chat_widget.add_error_message(format!(
                    "Failed to set permission profile `{profile_id}`: {err}"
                ));
                return false;
            }
        };
        let permission_profile = selected_config.permissions.permission_profile();
        let active_permission_profile = selected_config.permissions.active_permission_profile();
        let network = selected_config.permissions.network.clone();

        let mut config = self.config.clone();
        if let Some(policy) = approval_policy
            && !self.try_set_approval_policy_on_config(
                &mut config,
                policy,
                "Failed to set approval policy",
                "failed to set selected permission profile approval policy on app config",
            )
        {
            return false;
        }
        if let Err(err) = config
            .permissions
            .set_permission_profile_from_session_snapshot(
                PermissionProfileSnapshot::from_session_snapshot(
                    permission_profile.clone(),
                    active_permission_profile.clone(),
                ),
            )
        {
            tracing::warn!(
                error = %err,
                profile_id,
                "failed to set selected permission profile on app config"
            );
            self.chat_widget.add_error_message(format!(
                "Failed to set permission profile `{profile_id}`: {err}"
            ));
            return false;
        }
        config.permissions.network = network.clone();
        self.config = config;

        if let Some(policy) = approval_policy {
            self.runtime_approval_policy_override = Some(policy);
            self.chat_widget.set_approval_policy(policy);
        }
        if let Err(err) = self.chat_widget.set_permission_profile_with_active_profile(
            permission_profile.clone(),
            active_permission_profile.clone(),
        ) {
            tracing::warn!(
                error = %err,
                profile_id,
                "failed to set selected permission profile on chat config"
            );
            self.chat_widget.add_error_message(format!(
                "Failed to set permission profile `{profile_id}`: {err}"
            ));
            return false;
        }
        self.chat_widget.set_permission_network(network);
        self.runtime_permission_profile_override =
            Some(RuntimePermissionProfileOverride::from_config(&self.config));
        self.sync_active_thread_permission_settings_to_cached_session()
            .await;
        self.app_event_tx
            .send(AppEvent::XedocOp(AppCommand::override_turn_context(
                /*cwd*/ None,
                approval_policy,
                Some(permission_profile.clone()),
                active_permission_profile,
                /*model*/ None,
                /*effort*/ None,
                /*summary*/ None,
                /*service_tier*/ None,
                /*collaboration_mode*/ None,
                /*personality*/ None,
            )));
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::new_info_event(
                format!("Permissions updated to {display_label}"),
                /*hint*/ None,
            ),
        )));
        true
    }

    pub(super) async fn refresh_in_memory_config_from_disk(&mut self) -> Result<()> {
        let mut config = self
            .rebuild_config_for_cwd(self.chat_widget.config_ref().cwd.to_path_buf())
            .await?;
        self.apply_runtime_policy_overrides(&mut config);
        self.config = config;
        self.chat_widget.sync_plugin_mentions_config(&self.config);
        Ok(())
    }

    pub(super) async fn refresh_in_memory_config_from_disk_best_effort(&mut self, action: &str) {
        if let Err(err) = self.refresh_in_memory_config_from_disk().await {
            tracing::warn!(
                error = %err,
                action,
                "failed to refresh config before thread transition; continuing with current in-memory config"
            );
        }
    }

    pub(super) async fn read_effective_config_after_overridden_write(
        &mut self,
        app_server: &mut AppServerSession,
        setting: &str,
    ) -> Option<ConfigReadResponse> {
        let cwd = self.chat_widget.config_ref().cwd.display().to_string();
        match crate::config_update::read_effective_config(app_server.request_handle(), cwd).await {
            Ok(response) => Some(response),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    setting,
                    "failed to refresh effective config after an overridden write"
                );
                self.chat_widget.add_error_message(format!(
                    "{setting} were saved, but Xedoc could not refresh the effective config: {err}"
                ));
                None
            }
        }
    }

    pub(super) async fn rebuild_config_for_resume_or_fallback(
        &mut self,
        current_cwd: &Path,
        resume_cwd: PathBuf,
    ) -> Result<Config> {
        match self.rebuild_config_for_cwd(resume_cwd.clone()).await {
            Ok(config) => Ok(config),
            Err(err) => {
                if crate::session_resume::cwds_differ(current_cwd, &resume_cwd) {
                    Err(err)
                } else {
                    let resume_cwd_display = resume_cwd.display().to_string();
                    tracing::warn!(
                        error = %err,
                        cwd = %resume_cwd_display,
                        "failed to rebuild config for same-cwd resume; using current in-memory config"
                    );
                    Ok(self.config.clone())
                }
            }
        }
    }

    pub(super) fn apply_runtime_policy_overrides(&mut self, config: &mut Config) {
        if let Some(policy) = self.runtime_approval_policy_override.as_ref()
            && let Err(err) = config.permissions.approval_policy.set(policy.to_core())
        {
            tracing::warn!(%err, "failed to carry forward approval policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward approval policy override: {err}"
            ));
        }
        if let Some(profile_override) = self.runtime_permission_profile_override.as_ref() {
            match config
                .permissions
                .set_permission_profile_from_session_snapshot(
                    PermissionProfileSnapshot::from_session_snapshot(
                        profile_override.permission_profile.clone(),
                        profile_override.active_permission_profile.clone(),
                    ),
                ) {
                Ok(()) => {
                    config.permissions.network = profile_override.network.clone();
                }
                Err(err) => {
                    tracing::warn!(%err, "failed to carry forward permission profile override");
                    self.chat_widget.add_error_message(format!(
                        "Failed to carry forward permission profile override: {err}"
                    ));
                }
            }
        }
    }

    pub(super) fn try_set_approval_policy_on_config(
        &mut self,
        config: &mut Config,
        policy: AskForApproval,
        user_message_prefix: &str,
        log_message: &str,
    ) -> bool {
        if let Err(err) = config.permissions.approval_policy.set(policy.to_core()) {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return false;
        }

        true
    }

    pub(super) fn try_set_builtin_active_permission_profile_on_config(
        &mut self,
        config: &mut Config,
        active_permission_profile: ActivePermissionProfile,
        user_message_prefix: &str,
        log_message: &str,
    ) -> Option<PermissionProfile> {
        let Some(permission_profile) =
            builtin_permission_profile_for_active_permission_profile(&active_permission_profile)
        else {
            tracing::warn!(
                id = %active_permission_profile.id,
                "{log_message}: unsupported active permission profile"
            );
            self.chat_widget.add_error_message(format!(
                "{user_message_prefix}: unsupported active permission profile `{}`",
                active_permission_profile.id
            ));
            return None;
        };

        if let Err(err) = config
            .permissions
            .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
                permission_profile.clone(),
                active_permission_profile,
            ))
        {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return None;
        }

        Some(permission_profile)
    }

    pub(super) async fn update_feature_flags(
        &mut self,
        app_server: &mut AppServerSession,
        updates: Vec<(Feature, bool)>,
    ) {
        if updates.is_empty() {
            return;
        }

        let mut next_config = self.config.clone();
        let mut feature_updates_to_apply = Vec::with_capacity(updates.len());
        let mut config_edits = Vec::new();

        for (feature, enabled) in updates {
            let feature_key = feature.key();
            let mut feature_config = next_config.clone();
            if let Err(err) = feature_config.features.set_enabled(feature, enabled) {
                tracing::error!(
                    error = %err,
                    feature = feature_key,
                    "failed to update constrained feature flags"
                );
                self.chat_widget.add_error_message(format!(
                    "Failed to update experimental feature `{feature_key}`: {err}"
                ));
                continue;
            }
            let effective_enabled = feature_config.features.enabled(feature);
            next_config = feature_config;
            if feature == Feature::TokenUsageOptimizer {
                next_config.token_usage_optimizer.enabled = Some(effective_enabled);
            }
            feature_updates_to_apply.push((feature, effective_enabled));
            config_edits.push(crate::config_update::build_feature_enabled_edit(
                feature_key,
                effective_enabled,
            ));
        }

        // Persist first so the live session does not diverge from disk if the
        // config edit fails. Runtime/UI state is patched below only after the
        // durable config update succeeds.
        let write_response = match crate::config_update::write_config_batch(
            app_server.request_handle(),
            config_edits,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => {
                let error = crate::config_update::format_config_error(&err);
                tracing::error!(error = %error, "failed to persist feature flags");
                self.chat_widget
                    .add_error_message(format!("Failed to update experimental features: {error}"));
                return;
            }
        };
        if write_response.status == WriteStatus::OkOverridden {
            let message = overridden_write_message(&write_response);
            tracing::warn!(
                message,
                "feature flag config write was overridden by effective config"
            );
            self.chat_widget.add_error_message(format!(
                "Experimental feature changes were saved but not applied: {message}"
            ));
            if let Some(effective_config) = self
                .read_effective_config_after_overridden_write(
                    app_server,
                    "Experimental feature changes",
                )
                .await
            {
                self.sync_feature_state_from_effective_config(
                    &effective_config,
                    &feature_updates_to_apply,
                );
            }
            return;
        }

        self.config = next_config;
        for (feature, effective_enabled) in feature_updates_to_apply {
            self.chat_widget
                .set_feature_enabled(feature, effective_enabled);
        }
    }

    pub(super) async fn update_auto_session_name_setting_with_app_server(
        &mut self,
        app_server: &mut AppServerSession,
        enabled: bool,
    ) {
        let edits = crate::config_update::build_auto_session_name_edits(enabled);
        let write_response = match crate::config_update::write_config_batch(
            app_server.request_handle(),
            edits,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => {
                tracing::error!(error = %err, "failed to persist generated session name setting");
                self.chat_widget.add_error_message(format!(
                    "Failed to save generated session name setting: {err}"
                ));
                return;
            }
        };
        if write_response.status == WriteStatus::OkOverridden {
            let message = overridden_write_message(&write_response);
            tracing::warn!(
                message,
                "generated session name setting config write was overridden by effective config"
            );
            self.chat_widget.add_error_message(format!(
                "Generated session name setting was saved but not applied: {message}"
            ));
            let Some(effective_config) = self
                .read_effective_config_after_overridden_write(
                    app_server,
                    "Generated session name setting",
                )
                .await
            else {
                return;
            };
            let enabled =
                auto_session_name_from_effective_config(&effective_config).unwrap_or(enabled);
            self.config.auto_session_name = enabled;
            self.chat_widget.set_auto_session_name(enabled);
            return;
        }

        self.config.auto_session_name = enabled;
        self.chat_widget.set_auto_session_name(enabled);
    }

    pub(super) async fn update_tool_call_rendering_setting_with_app_server(
        &mut self,
        app_server: &mut AppServerSession,
        mode: ToolCallRenderingMode,
    ) {
        let previous_mode = self.config.tui_tool_call_rendering;
        let write_response = match crate::config_update::write_config_batch(
            app_server.request_handle(),
            vec![crate::config_update::build_tool_call_rendering_edit(mode)],
        )
        .await
        {
            Ok(response) => response,
            Err(err) => {
                tracing::error!(error = %err, "failed to persist tool call rendering mode");
                if self.chat_widget.tool_call_rendering_mode() != previous_mode {
                    self.chat_widget
                        .set_tool_call_rendering_mode_and_notify(previous_mode);
                }
                self.chat_widget
                    .add_error_message(format!("Failed to save tool call rendering mode: {err}"));
                return;
            }
        };
        if write_response.status == WriteStatus::OkOverridden {
            let message = overridden_write_message(&write_response);
            tracing::warn!(
                message,
                "tool call rendering mode config write was overridden by effective config"
            );
            self.chat_widget.add_error_message(format!(
                "Tool call rendering mode was saved but not applied: {message}"
            ));
            let Some(effective_config) = self
                .read_effective_config_after_overridden_write(
                    app_server,
                    "Tool call rendering mode",
                )
                .await
            else {
                if self.chat_widget.tool_call_rendering_mode() != previous_mode {
                    self.chat_widget
                        .set_tool_call_rendering_mode_and_notify(previous_mode);
                }
                return;
            };
            let effective_mode =
                tool_call_rendering_from_effective_config(&effective_config).unwrap_or_default();
            self.config.tui_tool_call_rendering = effective_mode;
            if self.chat_widget.tool_call_rendering_mode() != effective_mode {
                self.chat_widget
                    .set_tool_call_rendering_mode_and_notify(effective_mode);
            }
            return;
        }

        self.config.tui_tool_call_rendering = mode;
        if self.chat_widget.tool_call_rendering_mode() != mode {
            self.chat_widget
                .set_tool_call_rendering_mode_and_notify(mode);
        }
    }

    pub(super) fn reasoning_label(reasoning_effort: Option<&ReasoningEffortConfig>) -> String {
        match reasoning_effort {
            None | Some(ReasoningEffortConfig::None) => "default".to_string(),
            Some(reasoning_effort) => reasoning_effort.as_str().to_string(),
        }
    }

    pub(super) fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<&ReasoningEffortConfig>,
    ) -> Option<String> {
        (!model.starts_with("xedoc-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> crate::token_usage::TokenUsage {
        self.chat_widget.token_usage()
    }

    pub(super) fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        let clear_ephemeral_plan_effort = effort != Some(ReasoningEffortConfig::Ultra)
            && self.chat_widget.config_ref().plan_mode_reasoning_effort
                == Some(ReasoningEffortConfig::Ultra)
            && self.config.plan_mode_reasoning_effort != Some(ReasoningEffortConfig::Ultra);
        // TODO(aibrahim): Remove this and don't use config as a state object.
        // Instead, explicitly pass the stored collaboration mode's effort into new sessions.
        self.config.model_reasoning_effort = effort.clone();
        self.chat_widget.set_reasoning_effort(effort.clone());
        if clear_ephemeral_plan_effort {
            self.chat_widget.set_plan_mode_reasoning_effort(effort);
        }
    }

    pub(super) fn on_update_plan_mode_reasoning_effort(
        &mut self,
        effort: Option<ReasoningEffortConfig>,
    ) {
        let clear_ephemeral_default_effort = effort != Some(ReasoningEffortConfig::Ultra)
            && self
                .chat_widget
                .current_collaboration_mode()
                .reasoning_effort()
                == Some(ReasoningEffortConfig::Ultra)
            && self.config.model_reasoning_effort != Some(ReasoningEffortConfig::Ultra);
        self.config.plan_mode_reasoning_effort = effort.clone();
        self.chat_widget
            .set_plan_mode_reasoning_effort(effort.clone());
        if clear_ephemeral_default_effort {
            self.chat_widget.set_reasoning_effort(effort);
        }
    }

    pub(super) fn on_apply_advanced_reasoning(
        &mut self,
        model: &str,
        effort: ReasoningEffortConfig,
    ) -> Option<ReasoningEffortConfig> {
        let default_effort = self.default_reasoning_effort_for_conversation_model(model);
        if let Some(default_effort) = default_effort.as_ref() {
            self.config.model = Some(model.to_string());
            self.config.model_reasoning_effort = Some(default_effort.clone());
        }
        self.chat_widget.set_model(model);
        self.chat_widget.set_reasoning_effort(Some(effort.clone()));
        self.chat_widget
            .set_plan_mode_reasoning_effort(Some(effort));
        default_effort
    }

    fn default_reasoning_effort_for_conversation_model(
        &self,
        model: &str,
    ) -> Option<ReasoningEffortConfig> {
        let configured_effort = self
            .config
            .model_reasoning_effort
            .as_ref()
            .filter(|effort| **effort != ReasoningEffortConfig::Ultra);
        let preset = self
            .model_catalog
            .try_list_models()
            .ok()?
            .into_iter()
            .find(|preset| preset.model == model)?;
        let supported = &preset.supported_reasoning_efforts;

        configured_effort
            .filter(|effort| supported.iter().any(|option| option.effort == **effort))
            .cloned()
            .or_else(|| {
                (preset.default_reasoning_effort != ReasoningEffortConfig::Ultra)
                    .then_some(preset.default_reasoning_effort)
            })
            .or_else(|| {
                supported
                    .iter()
                    .find(|option| option.effort != ReasoningEffortConfig::Ultra)
                    .map(|option| option.effort.clone())
            })
    }

    pub(super) fn resume_model_settings(&self) -> crate::app_server_session::ResumeModelSettings {
        resume_model_settings_for_overrides(&self.config, &self.harness_overrides)
    }

    pub(super) fn on_update_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
        self.chat_widget.set_personality(personality);
    }

    pub(super) fn sync_tui_theme_selection(&mut self, name: String) {
        self.config.tui_theme = Some(name.clone());
        self.chat_widget.set_tui_theme(Some(name));
    }

    pub(super) fn restore_runtime_theme_from_config(&self) {
        if let Some(name) = self.config.tui_theme.as_deref()
            && let Some(theme) =
                crate::render::highlight::resolve_theme_by_name(name, Some(&self.config.xedoc_home))
        {
            crate::render::highlight::set_syntax_theme(theme);
            return;
        }

        let auto_theme_name = crate::render::highlight::adaptive_default_theme_name();
        if let Some(theme) = crate::render::highlight::resolve_theme_by_name(
            auto_theme_name,
            Some(&self.config.xedoc_home),
        ) {
            crate::render::highlight::set_syntax_theme(theme);
        }
    }

    pub(super) fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    fn sync_feature_state_from_effective_config(
        &mut self,
        effective_config: &ConfigReadResponse,
        feature_updates: &[(Feature, bool)],
    ) {
        for (feature, _) in feature_updates {
            let enabled = feature_enabled_from_effective_config(effective_config, *feature);
            if let Err(err) = self.config.features.set_enabled(*feature, enabled) {
                tracing::warn!(
                    error = %err,
                    feature = feature.key(),
                    "failed to sync effective feature state after an overridden write"
                );
                continue;
            }
            self.chat_widget.set_feature_enabled(*feature, enabled);
        }
    }
}

fn overridden_write_message(write_response: &ConfigWriteResponse) -> &str {
    write_response
        .overridden_metadata
        .as_ref()
        .map(|metadata| metadata.message.as_str())
        .unwrap_or("the effective config is overridden by a higher-priority layer")
}

fn feature_enabled_from_effective_config(
    effective_config: &ConfigReadResponse,
    feature: Feature,
) -> bool {
    let root_features = effective_config
        .config
        .additional
        .get("features")
        .and_then(features_toml_from_json);
    root_features
        .as_ref()
        .and_then(|features| features.entries().get(feature.key()).copied())
        .unwrap_or_else(|| feature.default_enabled())
}

fn auto_session_name_from_effective_config(effective_config: &ConfigReadResponse) -> Option<bool> {
    effective_config
        .config
        .additional
        .get("auto_session_name")
        .and_then(serde_json::Value::as_bool)
}

fn tool_call_rendering_from_effective_config(
    effective_config: &ConfigReadResponse,
) -> Option<ToolCallRenderingMode> {
    effective_config
        .config
        .additional
        .get("tui")
        .and_then(|tui| tui.get("tool_call_rendering"))
        .and_then(serde_json::Value::as_str)
        .and_then(|mode| match mode {
            "normal" => Some(ToolCallRenderingMode::Normal),
            "optimized" => Some(ToolCallRenderingMode::Optimized),
            _ => None,
        })
}

fn features_toml_from_json(value: &serde_json::Value) -> Option<FeaturesToml> {
    serde_json::from_value(value.clone()).ok()
}
