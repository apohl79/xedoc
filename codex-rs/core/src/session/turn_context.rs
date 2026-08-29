use super::*;
use crate::environment_selection::TurnEnvironmentSnapshot;
pub(crate) use codex_core_environment::TurnEnvironment;
use codex_core_skills::HostSkillsSnapshot;
pub use codex_core_turn_context::TurnContext;
pub(crate) use codex_core_turn_context::TurnSkillsContext;
use codex_model_provider::create_model_provider;
use std::sync::atomic::AtomicBool;

pub(super) enum TurnMultiAgentRuntime {
    ResolveAndStore,
    Preview,
}

fn local_time_context() -> (String, String) {
    match iana_time_zone::get_timezone() {
        Ok(timezone) => (Local::now().format("%Y-%m-%d").to_string(), timezone),
        Err(_) => (
            Utc::now().format("%Y-%m-%d").to_string(),
            "Etc/UTC".to_string(),
        ),
    }
}

impl Session {
    /// Don't expand the number of mutated arguments on config. We are in the process of getting rid of it.
    pub(crate) fn build_per_turn_config(
        session_configuration: &SessionConfiguration,
        cwd: AbsolutePathBuf,
    ) -> Config {
        // todo(aibrahim): store this state somewhere else so we don't need to mut config
        let config = session_configuration.original_config_do_not_use.clone();
        let mut per_turn_config = (*config).clone();
        per_turn_config.cwd = cwd;
        let workspace_roots = session_configuration.primary_workspace_roots();
        per_turn_config.workspace_roots = workspace_roots.clone();
        per_turn_config
            .permissions
            .set_workspace_roots(workspace_roots);
        per_turn_config.model_reasoning_effort =
            session_configuration.collaboration_mode.reasoning_effort();
        per_turn_config.model_reasoning_summary = session_configuration.model_reasoning_summary;
        per_turn_config.service_tier = session_configuration.service_tier.clone();
        per_turn_config.personality = session_configuration.personality;
        per_turn_config.approvals_reviewer = session_configuration.approvals_reviewer;
        session_configuration
            .apply_permission_profile_to_permissions(&mut per_turn_config.permissions);
        let permission_profile = session_configuration.permission_profile();
        let resolved_web_search_mode =
            resolve_web_search_mode_for_turn(&per_turn_config.web_search_mode, &permission_profile);
        if let Err(err) = per_turn_config
            .web_search_mode
            .set(resolved_web_search_mode)
        {
            let fallback_value = per_turn_config.web_search_mode.value();
            tracing::warn!(
                error = %err,
                ?resolved_web_search_mode,
                ?fallback_value,
                "resolved web_search_mode is disallowed by requirements; keeping constrained value"
            );
        }
        per_turn_config.features = config.features.clone();
        per_turn_config
    }

    pub(crate) fn build_effective_session_config(
        session_configuration: &SessionConfiguration,
    ) -> Config {
        let mut config =
            Self::build_per_turn_config(session_configuration, session_configuration.cwd().clone());
        config.model = Some(session_configuration.collaboration_mode.model().to_string());
        config.permissions.approval_policy = session_configuration.approval_policy.clone();
        config
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn make_turn_context(
        thread_id: ThreadId,
        session_id: SessionId,
        auth_manager: Option<Arc<AuthManager>>,
        session_telemetry: &SessionTelemetry,
        provider: ModelProviderInfo,
        session_configuration: &SessionConfiguration,
        multi_agent_version: MultiAgentVersion,
        user_shell: &shell::Shell,
        shell_zsh_path: Option<&PathBuf>,
        main_execve_wrapper_exe: Option<&PathBuf>,
        per_turn_config: Config,
        model_info: ModelInfo,
        models_manager: &SharedModelsManager,
        network: Option<NetworkProxy>,
        environments: TurnEnvironmentSnapshot,
        cwd: AbsolutePathBuf,
        sub_id: String,
        skills_snapshot: HostSkillsSnapshot,
    ) -> TurnContext {
        let collaboration_mode = &session_configuration.collaboration_mode;
        let reasoning_effort = collaboration_mode.reasoning_effort();
        let reasoning_summary = session_configuration
            .model_reasoning_summary
            .unwrap_or(model_info.default_reasoning_summary);
        let session_telemetry = session_telemetry.clone().with_model(
            session_configuration.collaboration_mode.model(),
            model_info.slug.as_str(),
        );
        let session_source = session_configuration.session_source.clone();
        let auth_manager_for_context = auth_manager.clone();
        let provider_for_context = create_model_provider(provider, auth_manager);
        let session_telemetry_for_context = session_telemetry;
        let available_models = models_manager.try_list_models().unwrap_or_default();
        let unified_exec_shell_mode = UnifiedExecShellMode::for_session(
            codex_tools::unified_exec_feature_mode_for_features(per_turn_config.features.get()),
            crate::tools::tool_user_shell_type(user_shell),
            shell_zsh_path,
            main_execve_wrapper_exe,
        );

        let mut per_turn_config = per_turn_config;
        per_turn_config.service_tier = get_service_tier(
            per_turn_config.service_tier,
            per_turn_config.features.enabled(Feature::FastMode),
            &model_info,
        );
        let permission_profile = per_turn_config.permissions.effective_permission_profile();
        let per_turn_config = Arc::new(per_turn_config);
        let turn_metadata_state = Arc::new(TurnMetadataState::new(
            session_id.to_string(),
            thread_id.to_string(),
            session_configuration.forked_from_thread_id,
            session_configuration.parent_thread_id,
            &session_configuration.session_source,
            session_configuration.thread_source.clone(),
            sub_id.clone(),
            cwd.clone(),
            &permission_profile,
            network.is_some(),
        ));
        let (current_date, timezone) = local_time_context();
        let extension_data = Arc::new(codex_extension_api::ExtensionData::new(sub_id.clone()));
        extension_data.insert(skills_snapshot.clone());
        TurnContext {
            sub_id,
            trace_id: current_span_trace_id(),
            realtime_active: false,
            config: per_turn_config,
            auth_manager: auth_manager_for_context,
            model_info,
            session_telemetry: session_telemetry_for_context,
            provider: provider_for_context,
            reasoning_effort,
            reasoning_summary,
            session_source,
            history_mode: session_configuration.history_mode,
            parent_thread_id: session_configuration.parent_thread_id,
            originator: session_configuration.originator.clone(),
            environments,
            #[allow(deprecated)]
            cwd,
            current_date: Some(current_date),
            timezone: Some(timezone),
            app_server_client_name: session_configuration.app_server_client_name.clone(),
            developer_instructions: session_configuration.developer_instructions.clone(),
            mode: collaboration_mode.mode,
            collaboration_mode_developer_instructions: collaboration_mode
                .settings
                .developer_instructions
                .clone(),
            multi_agent_version,
            personality: session_configuration.personality,
            approval_policy: session_configuration.approval_policy.clone(),
            permission_profile,
            network,
            available_models,
            unified_exec_shell_mode,
            final_output_json_schema: None,
            dynamic_tools: session_configuration.dynamic_tools.clone(),
            turn_metadata_state,
            extension_data,
            turn_skills: TurnSkillsContext::new(skills_snapshot),
            turn_timing_state: Arc::new(TurnTimingState::default()),
            terminal_error: Arc::new(Mutex::new(None)),
            server_model_warning_emitted: AtomicBool::new(false),
            model_verification_emitted: AtomicBool::new(false),
        }
    }

    pub(crate) async fn new_turn_with_sub_id(
        &self,
        sub_id: String,
        updates: SessionSettingsUpdate,
    ) -> CodexResult<Arc<TurnContext>> {
        let notify_config_contributors = !self.services.extensions.config_contributors().is_empty();
        let update_result: CodexResult<_> = {
            let mut state = self.state.lock().await;
            match state.session_configuration.clone().apply(&updates) {
                Ok(next) => {
                    let previous_permission_profile =
                        state.session_configuration.permission_profile();
                    let next_permission_profile = next.permission_profile();
                    let permission_profile_changed =
                        previous_permission_profile != next_permission_profile;
                    let previous_config = notify_config_contributors.then(|| {
                        Self::build_effective_session_config(&state.session_configuration)
                    });
                    let new_config = notify_config_contributors
                        .then(|| Self::build_effective_session_config(&next));
                    if updates.environments.is_some() {
                        self.services
                            .turn_environments
                            .update_selections(next.environment_selections());
                    }
                    state.session_configuration = next.clone();
                    Ok((
                        next,
                        permission_profile_changed,
                        previous_config,
                        new_config,
                    ))
                }
                Err(err) => Err(CodexErr::InvalidRequest(err.to_string())),
            }
        };

        let (session_configuration, permission_profile_changed, previous_config, new_config) =
            match update_result {
                Ok(update) => update,
                Err(err) => {
                    let message = err.to_string();
                    self.send_event_raw(Event {
                        id: sub_id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: message.clone(),
                            codex_error_info: Some(CodexErrorInfo::BadRequest),
                        }),
                    })
                    .await;
                    return Err(CodexErr::InvalidRequest(message));
                }
            };
        self.emit_config_changed_contributors(previous_config.as_ref(), new_config.as_ref());

        if permission_profile_changed {
            self.refresh_managed_network_proxy_for_current_permission_profile()
                .await;
        }

        Ok(self
            .new_turn_from_configuration(
                sub_id,
                session_configuration,
                updates.final_output_json_schema,
            )
            .await)
    }

    async fn new_turn_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
        final_output_json_schema: Option<Option<Value>>,
    ) -> Arc<TurnContext> {
        self.new_turn_context_from_configuration(
            sub_id,
            session_configuration,
            final_output_json_schema,
            TurnMultiAgentRuntime::ResolveAndStore,
        )
        .await
    }

    async fn new_startup_prewarm_turn_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
    ) -> Arc<TurnContext> {
        self.new_turn_context_from_configuration(
            sub_id,
            session_configuration,
            /*final_output_json_schema*/ None,
            TurnMultiAgentRuntime::Preview,
        )
        .await
    }

    #[instrument(name = "turn_context.build", level = "trace", skip_all)]
    pub(super) async fn new_turn_context_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
        final_output_json_schema: Option<Option<Value>>,
        multi_agent_runtime: TurnMultiAgentRuntime,
    ) -> Arc<TurnContext> {
        let turn_environments = self.services.turn_environments.snapshot().await;
        let primary_turn_environment = turn_environments.primary();
        // TODO(anp): Migrate per-turn config and legacy TurnContext cwd consumers to PathUri so
        // a foreign primary environment does not fall back to the session's host cwd.
        let cwd = primary_turn_environment
            .as_ref()
            .and_then(|turn_environment| turn_environment.cwd().to_abs_path().ok())
            .unwrap_or_else(|| session_configuration.cwd().clone());
        let per_turn_config = Self::build_per_turn_config(&session_configuration, cwd.clone());
        {
            let mcp_runtime = self.services.latest_mcp_runtime();
            let mcp_connection_manager = mcp_runtime.manager();
            mcp_connection_manager.set_approval_policy(&session_configuration.approval_policy);
            mcp_connection_manager
                .set_permission_profile(session_configuration.permission_profile());
        }

        let model_info = self
            .services
            .models_manager
            .get_model_info_for_provider(
                session_configuration.collaboration_mode.model(),
                per_turn_config.model_provider_id.as_str(),
                &per_turn_config.to_models_manager_config(),
            )
            .await;
        self.services
            .thread_extension_data
            .insert(model_info.clone());

        let multi_agent_version = match multi_agent_runtime {
            TurnMultiAgentRuntime::ResolveAndStore => {
                self.resolve_multi_agent_version_for_model(&model_info, &per_turn_config)
            }
            TurnMultiAgentRuntime::Preview => per_turn_config.multi_agent_version_for_model(
                self.multi_agent_version()
                    .or(model_info.multi_agent_version),
            ),
        };
        let plugins_input = per_turn_config.plugins_config_input();
        let plugin_outcome = self
            .services
            .plugins_manager
            .plugins_for_config(&plugins_input)
            .await;
        let effective_skill_roots = plugin_outcome.effective_plugin_skill_roots();
        let plugin_skill_snapshots = self
            .services
            .plugins_manager
            .plugin_skill_snapshots_for_config(&plugins_input);
        let skills_input = skills_load_input_from_config(&per_turn_config, effective_skill_roots)
            .with_plugin_skill_snapshots(plugin_skill_snapshots);
        let fs = primary_turn_environment
            .map(|turn_environment| turn_environment.environment.get_filesystem());
        let skills_snapshot = self
            .services
            .skills_service
            .snapshot_for_config(&skills_input, fs)
            .await;
        let mut turn_context: TurnContext = Self::make_turn_context(
            self.thread_id(),
            self.session_id(),
            Some(Arc::clone(&self.services.auth_manager)),
            &self.services.session_telemetry,
            session_configuration.provider.clone(),
            &session_configuration,
            multi_agent_version,
            self.services.user_shell.as_ref(),
            self.services.shell_zsh_path.as_ref(),
            self.services.main_execve_wrapper_exe.as_ref(),
            per_turn_config,
            model_info,
            &self.services.models_manager,
            self.services
                .network_proxy
                .load_full()
                .as_ref()
                .and_then(|started_proxy| {
                    Self::managed_network_proxy_active_for_permission_profile(
                        &session_configuration.permission_profile(),
                    )
                    .then(|| started_proxy.proxy())
                }),
            turn_environments,
            cwd,
            sub_id,
            skills_snapshot,
        );
        turn_context.realtime_active = self.conversation.running_state().await.is_some();

        if let Some(final_schema) = final_output_json_schema {
            turn_context.final_output_json_schema = final_schema;
        }
        let turn_context = Arc::new(turn_context);
        if turn_context
            .environments
            .single_local_environment_cwd()
            .is_some()
        {
            turn_context.turn_metadata_state.spawn_git_enrichment_task();
        }
        turn_context
    }

    pub(crate) async fn maybe_emit_model_warnings_for_turn(&self, tc: &TurnContext) {
        if tc.model_info.used_fallback_model_metadata {
            self.send_event(
                tc,
                EventMsg::Warning(WarningEvent {
                    message: format!(
                        "Model metadata for `{}` not found. Defaulting to fallback metadata; this can degrade performance and cause issues.",
                        tc.model_info.slug
                    ),
                }),
            )
            .await;
        }

        if let Some(message) =
            unsupported_code_mode_warning(&tc.model_info, tc.config.features.get())
        {
            self.send_event(tc, EventMsg::Warning(WarningEvent { message }))
                .await;
        }
    }

    pub(crate) async fn new_default_turn(&self) -> Arc<TurnContext> {
        self.new_default_turn_with_sub_id(self.next_internal_sub_id())
            .await
    }

    pub(crate) async fn new_default_turn_with_sub_id(&self, sub_id: String) -> Arc<TurnContext> {
        let session_configuration = self.default_turn_configuration().await;
        self.new_turn_from_configuration(
            sub_id,
            session_configuration,
            /*final_output_json_schema*/ None,
        )
        .await
    }

    pub(crate) async fn new_startup_prewarm_turn_with_sub_id(
        &self,
        sub_id: String,
    ) -> Arc<TurnContext> {
        let session_configuration = self.default_turn_configuration().await;
        self.new_startup_prewarm_turn_from_configuration(sub_id, session_configuration)
            .await
    }

    async fn default_turn_configuration(&self) -> SessionConfiguration {
        let state = self.state.lock().await;
        state.session_configuration.clone()
    }
}
