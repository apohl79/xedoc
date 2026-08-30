use super::*;
use crate::effective_plugin_change::EffectivePluginsChangedCallback;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use codex_config::types::McpServerConfig;
use codex_mcp::McpOAuthLoginSupport;
use codex_mcp::oauth_login_support;
use codex_mcp::should_retry_without_scopes;
use codex_rmcp_client::perform_oauth_login_silent;

#[derive(Clone)]
pub(crate) struct PluginRequestProcessor {
    auth_manager: Arc<AuthManager>,
    thread_manager: Arc<ThreadManager>,
    outgoing: Arc<OutgoingMessageSender>,
    config_manager: ConfigManager,
    on_effective_plugins_changed: EffectivePluginsChangedCallback,
}

fn plugin_skills_to_info(
    skills: &[codex_core::skills::SkillMetadata],
    disabled_skill_paths: &HashSet<AbsolutePathBuf>,
) -> Vec<SkillSummary> {
    skills
        .iter()
        .map(|skill| SkillSummary {
            name: skill.name.clone(),
            description: skill.description.clone(),
            short_description: skill.short_description.clone(),
            interface: skill.interface.clone().map(|interface| {
                codex_app_server_protocol::SkillInterface {
                    display_name: interface.display_name,
                    short_description: interface.short_description,
                    icon_small: interface.icon_small,
                    icon_large: interface.icon_large,
                    brand_color: interface.brand_color,
                    default_prompt: interface.default_prompt,
                }
            }),
            path: Some(skill.path_to_skills_md.clone()),
            enabled: !disabled_skill_paths.contains(&skill.path_to_skills_md),
        })
        .collect()
}

fn local_plugin_interface_to_info(interface: PluginManifestInterface) -> PluginInterface {
    PluginInterface {
        display_name: interface.display_name,
        short_description: interface.short_description,
        long_description: interface.long_description,
        developer_name: interface.developer_name,
        category: interface.category,
        capabilities: interface.capabilities,
        website_url: interface.website_url,
        privacy_policy_url: interface.privacy_policy_url,
        terms_of_service_url: interface.terms_of_service_url,
        default_prompt: interface.default_prompt,
        brand_color: interface.brand_color,
        composer_icon: interface.composer_icon,
        logo: interface.logo,
        logo_dark: interface.logo_dark,
        screenshots: interface.screenshots,
    }
}

fn marketplace_plugin_source_to_info(source: MarketplacePluginSource) -> PluginSource {
    match source {
        MarketplacePluginSource::Local { path } => PluginSource::Local { path },
        MarketplacePluginSource::Git {
            url,
            path,
            ref_name,
            sha,
        } => PluginSource::Git {
            url,
            path,
            ref_name,
            sha,
        },
        MarketplacePluginSource::Npm {
            package,
            version,
            registry,
        } => PluginSource::Npm {
            package,
            version,
            registry,
        },
    }
}

fn convert_configured_marketplace_plugin_to_plugin_summary(
    plugin: codex_core_plugins::ConfiguredMarketplacePlugin,
) -> PluginSummary {
    PluginSummary {
        id: plugin.id,
        local_version: plugin.local_version,
        installed: plugin.installed,
        enabled: plugin.enabled,
        name: plugin.name,
        source: marketplace_plugin_source_to_info(plugin.source),
        install_policy: plugin.policy.installation.into(),
        auth_policy: plugin.policy.authentication.into(),
        interface: plugin.interface.map(local_plugin_interface_to_info),
        keywords: plugin.keywords,
    }
}

fn marketplace_entries_from_outcome(
    outcome: codex_core_plugins::ConfiguredMarketplaceListOutcome,
    include_plugin: impl Fn(&codex_core_plugins::ConfiguredMarketplacePlugin) -> bool,
) -> (
    Vec<PluginMarketplaceEntry>,
    Vec<codex_app_server_protocol::MarketplaceLoadErrorInfo>,
) {
    (
        outcome
            .marketplaces
            .into_iter()
            .filter_map(|marketplace| {
                let plugins = marketplace
                    .plugins
                    .into_iter()
                    .filter(&include_plugin)
                    .map(convert_configured_marketplace_plugin_to_plugin_summary)
                    .collect::<Vec<_>>();
                (!plugins.is_empty()).then_some(PluginMarketplaceEntry {
                    name: marketplace.name,
                    path: Some(marketplace.path),
                    interface: marketplace.interface.map(|interface| MarketplaceInterface {
                        display_name: interface.display_name,
                    }),
                    plugins,
                })
            })
            .collect(),
        outcome
            .errors
            .into_iter()
            .map(|err| codex_app_server_protocol::MarketplaceLoadErrorInfo {
                marketplace_path: err.path,
                message: err.message,
            })
            .collect(),
    )
}

impl PluginRequestProcessor {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        thread_manager: Arc<ThreadManager>,
        outgoing: Arc<OutgoingMessageSender>,
        config_manager: ConfigManager,
        on_effective_plugins_changed: EffectivePluginsChangedCallback,
    ) -> Self {
        Self {
            auth_manager,
            thread_manager,
            outgoing,
            config_manager,
            on_effective_plugins_changed,
        }
    }

    pub(crate) async fn plugin_list(
        &self,
        params: PluginListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.plugin_list_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn plugin_installed(
        &self,
        params: PluginInstalledParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.plugin_installed_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn plugin_read(
        &self,
        params: PluginReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.plugin_read_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn plugin_install(
        &self,
        params: PluginInstallParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.plugin_install_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn plugin_uninstall(
        &self,
        params: PluginUninstallParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.plugin_uninstall_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    fn on_effective_plugins_changed(&self) {
        (self.on_effective_plugins_changed)();
    }

    fn clear_plugin_related_caches(&self) {
        self.thread_manager.plugins_manager().clear_cache();
        self.thread_manager.skills_service().clear_cache();
    }

    async fn load_latest_config(
        &self,
        fallback_cwd: Option<PathBuf>,
    ) -> Result<Config, JSONRPCErrorError> {
        self.config_manager
            .load_latest_config(fallback_cwd)
            .await
            .map_err(|err| internal_error(format!("failed to reload config: {err}")))
    }

    async fn plugin_list_response(
        &self,
        params: PluginListParams,
    ) -> Result<PluginListResponse, JSONRPCErrorError> {
        let plugins_manager = self.thread_manager.plugins_manager();
        let PluginListParams { cwds } = params;
        let roots = cwds.unwrap_or_default();

        let config = self.load_latest_config(/*fallback_cwd*/ None).await?;
        if !config.features.enabled(Feature::Plugins) {
            return Ok(PluginListResponse {
                marketplaces: Vec::new(),
                marketplace_load_errors: Vec::new(),
            });
        }
        let auth = self.auth_manager.auth().await;
        plugins_manager.set_auth_mode(auth.as_ref().map(CodexAuth::api_auth_mode));
        let plugins_input = config.plugins_config_input();
        let (marketplaces, marketplace_load_errors) = self
            .load_marketplace_plugins(
                plugins_manager.clone(),
                &plugins_input,
                roots.clone(),
                |_| true,
                "list marketplace plugins",
            )
            .await?;
        plugins_manager.maybe_start_non_curated_plugin_cache_refresh(&plugins_input, &roots);

        Ok(PluginListResponse {
            marketplaces,
            marketplace_load_errors,
        })
    }

    async fn plugin_installed_response(
        &self,
        params: PluginInstalledParams,
    ) -> Result<PluginInstalledResponse, JSONRPCErrorError> {
        let plugins_manager = self.thread_manager.plugins_manager();
        let PluginInstalledParams {
            cwds,
            install_suggestion_plugin_names,
        } = params;
        let roots = cwds.unwrap_or_default();
        let install_suggestion_plugin_names = install_suggestion_plugin_names
            .unwrap_or_default()
            .into_iter()
            .collect::<HashSet<_>>();

        let config = self.load_latest_config(/*fallback_cwd*/ None).await?;
        if !config.features.enabled(Feature::Plugins) {
            return Ok(PluginInstalledResponse {
                marketplaces: Vec::new(),
                marketplace_load_errors: Vec::new(),
            });
        }
        let auth = self.auth_manager.auth().await;
        plugins_manager.set_auth_mode(auth.as_ref().map(CodexAuth::api_auth_mode));

        let plugins_input = config.plugins_config_input();
        let (marketplaces, marketplace_load_errors) = self
            .load_marketplace_plugins(
                plugins_manager,
                &plugins_input,
                roots,
                move |plugin| {
                    plugin.installed || install_suggestion_plugin_names.contains(&plugin.name)
                },
                "list installed and suggested marketplace plugins",
            )
            .await?;

        Ok(PluginInstalledResponse {
            marketplaces,
            marketplace_load_errors,
        })
    }

    async fn load_marketplace_plugins(
        &self,
        plugins_manager: Arc<codex_core_plugins::PluginsManager>,
        plugins_input: &codex_core_plugins::PluginsConfigInput,
        roots: Vec<AbsolutePathBuf>,
        include_plugin: impl Fn(&codex_core_plugins::ConfiguredMarketplacePlugin) -> bool
        + Send
        + 'static,
        action: &str,
    ) -> Result<
        (
            Vec<PluginMarketplaceEntry>,
            Vec<codex_app_server_protocol::MarketplaceLoadErrorInfo>,
        ),
        JSONRPCErrorError,
    > {
        let config_for_marketplace_listing = plugins_input.clone();
        match tokio::task::spawn_blocking(move || {
            let outcome = plugins_manager.list_marketplaces_for_config(
                &config_for_marketplace_listing,
                &roots,
                /*include_openai_curated*/ true,
            )?;
            Ok::<_, MarketplaceError>(marketplace_entries_from_outcome(outcome, include_plugin))
        })
        .await
        {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(err)) => Err(Self::marketplace_error(err, action)),
            Err(err) => Err(internal_error(format!("failed to {action}: {err}"))),
        }
    }

    async fn plugin_read_response(
        &self,
        params: PluginReadParams,
    ) -> Result<PluginReadResponse, JSONRPCErrorError> {
        let plugins_manager = self.thread_manager.plugins_manager();
        let PluginReadParams {
            marketplace_path,
            plugin_name,
        } = params;
        let config_cwd = marketplace_path.as_path().parent().map(Path::to_path_buf);

        let config = self.load_latest_config(config_cwd).await?;
        let plugins_input = config.plugins_config_input();
        let auth = self.auth_manager.auth().await;
        plugins_manager.set_auth_mode(auth.as_ref().map(CodexAuth::api_auth_mode));

        let request = PluginReadRequest {
            plugin_name,
            marketplace_path,
        };
        let outcome = plugins_manager
            .read_plugin_for_config(&plugins_input, &request)
            .await
            .map_err(|err| Self::marketplace_error(err, "read plugin details"))?;
        let visible_skills = outcome
            .plugin
            .skills
            .iter()
            .filter(|skill| {
                skill.matches_product_restriction_for_product(
                    self.thread_manager.session_source().restriction_product(),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let plugin = PluginDetail {
            marketplace_name: outcome.marketplace_name,
            marketplace_path: outcome.marketplace_path,
            summary: PluginSummary {
                id: outcome.plugin.id,
                local_version: outcome.plugin.local_version,
                name: outcome.plugin.name,
                source: marketplace_plugin_source_to_info(outcome.plugin.source),
                installed: outcome.plugin.installed,
                enabled: outcome.plugin.enabled,
                install_policy: outcome.plugin.policy.installation.into(),
                auth_policy: outcome.plugin.policy.authentication.into(),
                interface: outcome.plugin.interface.map(local_plugin_interface_to_info),
                keywords: outcome.plugin.keywords,
            },
            description: outcome.plugin.description,
            skills: plugin_skills_to_info(&visible_skills, &outcome.plugin.disabled_skill_paths),
            hooks: outcome
                .plugin
                .hooks
                .into_iter()
                .map(|hook| codex_app_server_protocol::PluginHookSummary {
                    key: hook.key,
                    event_name: hook.event_name.into(),
                })
                .collect(),
            mcp_servers: outcome.plugin.mcp_server_names,
        };

        Ok(PluginReadResponse { plugin })
    }

    async fn plugin_install_response(
        &self,
        params: PluginInstallParams,
    ) -> Result<PluginInstallResponse, JSONRPCErrorError> {
        let PluginInstallParams {
            marketplace_path,
            plugin_name,
        } = params;
        let config_cwd = marketplace_path.as_path().parent().map(Path::to_path_buf);
        let config = self.load_latest_config(config_cwd.clone()).await?;

        let plugins_manager = self.thread_manager.plugins_manager();
        let marketplace_display = marketplace_path.display().to_string();
        let plugin_name_for_log = plugin_name.clone();
        let request = PluginInstallRequest {
            plugin_name,
            marketplace_path,
        };

        let result = match plugins_manager
            .install_plugin(&config.config_layer_stack, request)
            .await
        {
            Ok(result) => result,
            Err(err) => {
                warn!(
                    marketplace = %marketplace_display,
                    plugin_name = %plugin_name_for_log,
                    "failed to install plugin: {err}"
                );
                return Err(Self::plugin_install_error(err));
            }
        };
        let config = match self.load_latest_config(config_cwd).await {
            Ok(config) => config,
            Err(err) => {
                warn!(
                    "failed to reload config after plugin install, using current config: {err:?}"
                );
                config
            }
        };

        self.on_effective_plugins_changed();

        let plugin_mcp_servers = load_plugin_mcp_servers(result.installed_path.as_path()).await;
        if !plugin_mcp_servers.is_empty() {
            self.start_plugin_mcp_oauth_logins(&config, plugin_mcp_servers)
                .await;
        }

        Ok(PluginInstallResponse {
            auth_policy: result.auth_policy.into(),
        })
    }

    async fn start_plugin_mcp_oauth_logins(
        &self,
        config: &Config,
        plugin_mcp_servers: HashMap<String, McpServerConfig>,
    ) {
        for (name, server) in plugin_mcp_servers {
            let oauth_config = match oauth_login_support(&server.transport).await {
                McpOAuthLoginSupport::Supported(config) => config,
                McpOAuthLoginSupport::Unsupported => continue,
                McpOAuthLoginSupport::Unknown(err) => {
                    warn!(
                        "MCP server may or may not require login for plugin install {name}: {err}"
                    );
                    continue;
                }
            };

            let resolved_scopes = resolve_oauth_scopes(
                /*explicit_scopes*/ None,
                server.scopes.clone(),
                oauth_config.discovered_scopes.clone(),
            );

            let store_mode = config.mcp_oauth_credentials_store_mode;
            let keyring_backend_kind = config.auth_keyring_backend_kind();
            let callback_port = config.mcp_oauth_callback_port;
            let callback_url = config.mcp_oauth_callback_url.clone();
            let outgoing = Arc::clone(&self.outgoing);
            let notification_name = name.clone();

            tokio::spawn(async move {
                let oauth_client_id = server.oauth_client_id();
                let first_attempt = perform_oauth_login_silent(
                    &name,
                    &oauth_config.url,
                    store_mode,
                    keyring_backend_kind,
                    oauth_config.http_headers.clone(),
                    oauth_config.env_http_headers.clone(),
                    &resolved_scopes.scopes,
                    oauth_client_id,
                    server.oauth_resource.as_deref(),
                    callback_port,
                    callback_url.as_deref(),
                )
                .await;

                let final_result = match first_attempt {
                    Err(err) if should_retry_without_scopes(&resolved_scopes, &err) => {
                        perform_oauth_login_silent(
                            &name,
                            &oauth_config.url,
                            store_mode,
                            keyring_backend_kind,
                            oauth_config.http_headers,
                            oauth_config.env_http_headers,
                            &[],
                            oauth_client_id,
                            server.oauth_resource.as_deref(),
                            callback_port,
                            callback_url.as_deref(),
                        )
                        .await
                    }
                    result => result,
                };

                let (success, error) = match final_result {
                    Ok(()) => (true, None),
                    Err(err) => (false, Some(err.to_string())),
                };

                let notification = ServerNotification::McpServerOauthLoginCompleted(
                    McpServerOauthLoginCompletedNotification {
                        name: notification_name,
                        thread_id: None,
                        success,
                        error,
                    },
                );
                outgoing.send_server_notification(notification).await;
            });
        }
    }

    async fn plugin_uninstall_response(
        &self,
        params: PluginUninstallParams,
    ) -> Result<PluginUninstallResponse, JSONRPCErrorError> {
        let PluginUninstallParams { plugin_id } = params;
        let plugins_manager = self.thread_manager.plugins_manager();

        plugins_manager
            .uninstall_plugin(plugin_id)
            .await
            .map_err(Self::plugin_uninstall_error)?;
        match self.load_latest_config(/*fallback_cwd*/ None).await {
            Ok(_) => self.on_effective_plugins_changed(),
            Err(err) => {
                warn!(
                    "failed to reload config after plugin uninstall, clearing plugin-related caches only: {err:?}"
                );
                self.clear_plugin_related_caches();
            }
        }
        Ok(PluginUninstallResponse {})
    }

    fn plugin_install_error(err: CorePluginInstallError) -> JSONRPCErrorError {
        if err.is_invalid_request() {
            return invalid_request(err.to_string());
        }

        match err {
            CorePluginInstallError::Marketplace(err) => {
                Self::marketplace_error(err, "install plugin")
            }
            CorePluginInstallError::Config(err) => {
                internal_error(format!("failed to persist installed plugin config: {err}"))
            }
            CorePluginInstallError::Join(err) => {
                internal_error(format!("failed to install plugin: {err}"))
            }
            CorePluginInstallError::Store(err) => {
                internal_error(format!("failed to install plugin: {err}"))
            }
        }
    }

    fn plugin_uninstall_error(err: CorePluginUninstallError) -> JSONRPCErrorError {
        if err.is_invalid_request() {
            return invalid_request(err.to_string());
        }

        match err {
            CorePluginUninstallError::Config(err) => {
                internal_error(format!("failed to clear plugin config: {err}"))
            }
            CorePluginUninstallError::Join(err) => {
                internal_error(format!("failed to uninstall plugin: {err}"))
            }
            CorePluginUninstallError::Store(err) => {
                internal_error(format!("failed to uninstall plugin: {err}"))
            }
            CorePluginUninstallError::InvalidPluginId(err) => invalid_request(err.to_string()),
        }
    }

    fn marketplace_error(err: MarketplaceError, action: &str) -> JSONRPCErrorError {
        match err {
            MarketplaceError::MarketplaceNotFound { .. }
            | MarketplaceError::InvalidMarketplaceFile { .. }
            | MarketplaceError::PluginNotFound { .. }
            | MarketplaceError::PluginNotAvailable { .. }
            | MarketplaceError::PluginsDisabled
            | MarketplaceError::InvalidPlugin(_) => invalid_request(err.to_string()),
            MarketplaceError::Io { .. } => internal_error(format!("failed to {action}: {err}")),
        }
    }
}
