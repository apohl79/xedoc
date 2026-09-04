use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use tokio::sync::Mutex as AsyncMutex;
use xedoc_api::RouteAwareAuthHttpTransport;
use xedoc_app_server_protocol::JSONRPCErrorError;
use xedoc_app_server_protocol::ManagedModelSettings;
use xedoc_app_server_protocol::ManagedProviderSettings;
use xedoc_app_server_protocol::ModelManagerReadResponse;
use xedoc_app_server_protocol::ModelManagerUpdateParams;
use xedoc_app_server_protocol::ModelManagerUpdateResponse;
use xedoc_app_server_protocol::ModelProviderApiKeyDeleteParams;
use xedoc_app_server_protocol::ModelProviderApiKeyDeleteResponse;
use xedoc_app_server_protocol::ModelProviderApiKeySetParams;
use xedoc_app_server_protocol::ModelProviderApiKeySetResponse;
use xedoc_app_server_protocol::ModelProviderOauthDeleteParams;
use xedoc_app_server_protocol::ModelProviderOauthDeleteResponse;
use xedoc_app_server_protocol::ModelProviderOauthStartParams;
use xedoc_app_server_protocol::ModelProviderOauthStartResponse;
use xedoc_core::config::Config;
use xedoc_login::AuthManager;
use xedoc_login::ProviderCredentialStore;
use xedoc_login::XedocAuth;
use xedoc_model_provider_info::ANTHROPIC_PROVIDER_ID;
use xedoc_model_provider_info::OPENAI_PROVIDER_ID;
use xedoc_models_manager::model_info::model_info_from_provider_catalog_slug;
use xedoc_models_manager::registry::ManagedModel;
use xedoc_models_manager::registry::ModelRegistry;
use xedoc_models_manager::registry::ProviderModelConfig;
use xedoc_models_manager::registry::SharedModelRegistry;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::openai_models::ReasoningEffortPreset;
use xedoc_provider_anthropic::AnthropicOAuthBrowserLogin;
use xedoc_provider_anthropic::AnthropicOAuthBrowserLoginCancellation;
use xedoc_provider_anthropic::AnthropicOAuthCredential;
use xedoc_provider_anthropic::clear_anthropic_oauth_credentials;
use xedoc_provider_anthropic::load_anthropic_oauth_credentials;
use xedoc_provider_anthropic::restore_anthropic_oauth_credentials;
use xedoc_provider_anthropic::store_anthropic_oauth_credential_with_status;
use xedoc_provider_anthropic::take_anthropic_oauth_credentials;

use crate::error_code::internal_error;
use crate::error_code::invalid_params;

const ANTHROPIC_ACCOUNTS_PATH: [&str; 3] = ["providers", "anthropic", "accounts"];

#[derive(Default)]
struct AnthropicOauthLoginState {
    generation: u64,
    active_auth_url: Option<String>,
    active_cancellation: Option<AnthropicOAuthBrowserLoginCancellation>,
}

#[derive(Clone)]
pub(crate) struct ModelManagerRequestProcessor {
    config: Arc<Config>,
    auth_manager: Arc<AuthManager>,
    registry: SharedModelRegistry,
    credentials: ProviderCredentialStore,
    anthropic_oauth_login_state: Arc<Mutex<AnthropicOauthLoginState>>,
    anthropic_oauth_operation_lock: Arc<AsyncMutex<()>>,
}

impl ModelManagerRequestProcessor {
    pub(crate) fn new(config: Arc<Config>, auth_manager: Arc<AuthManager>) -> Self {
        Self {
            registry: config.model_registry.clone(),
            credentials: auth_manager.provider_credentials(),
            anthropic_oauth_login_state: Arc::new(Mutex::new(AnthropicOauthLoginState::default())),
            anthropic_oauth_operation_lock: Arc::new(AsyncMutex::new(())),
            config,
            auth_manager,
        }
    }

    pub(crate) fn read(&self) -> Result<ModelManagerReadResponse, JSONRPCErrorError> {
        let mut registry = self.registry.snapshot();
        // Configured third-party providers may not have been persisted in the
        // model registry yet. Seed a minimal editable entry so the model
        // manager can expose the active model and accept subsequent edits.
        let active_model = self.config.model.clone();
        for (provider_id, provider_info) in &self.config.model_providers {
            let Some(model_id) = (provider_id == &self.config.model_provider_id)
                .then(|| active_model.clone())
                .flatten()
            else {
                continue;
            };
            let provider = registry
                .providers
                .entry(provider_id.clone())
                .or_insert_with(|| {
                    provider_config_for_model(provider_info.name.clone(), model_id.clone())
                });
            if !provider.models.contains_key(&model_id) {
                let info = model_info_from_provider_catalog_slug(&model_id, &provider.display_name);
                provider
                    .models
                    .insert(model_id, ManagedModel { info, prices: None });
            }
        }
        let mut providers = Vec::with_capacity(registry.providers.len());
        for (provider_id, provider) in registry.providers {
            let api_key_configured = self.provider_has_api_key(&provider_id)?;
            let oauth_supported = matches!(
                provider_id.as_str(),
                OPENAI_PROVIDER_ID | ANTHROPIC_PROVIDER_ID
            );
            let oauth_configured = self.provider_has_oauth(&provider_id)?;
            let models = provider
                .models
                .into_iter()
                .map(|(model_id, model)| {
                    let info = model.info;
                    let context_window = info.resolved_context_window().unwrap_or_default();
                    let auto_compact_token_limit =
                        info.auto_compact_token_limit().unwrap_or_default();
                    let base_instructions =
                        load_prompt_override(&self.config.xedoc_home, &provider_id, &model_id)
                            .unwrap_or(info.base_instructions);
                    ManagedModelSettings {
                        id: model_id,
                        display_name: info.display_name,
                        context_window,
                        max_context_window: info.max_context_window.unwrap_or(context_window),
                        auto_compact_token_limit,
                        base_instructions,
                        supported_reasoning_efforts: info
                            .supported_reasoning_levels
                            .into_iter()
                            .map(|preset| preset.effort)
                            .collect(),
                    }
                })
                .collect();
            providers.push(ManagedProviderSettings {
                id: provider_id,
                display_name: provider.display_name,
                default_model: provider.default_model,
                fast_model: provider.fast_model,
                default_reasoning_effort: provider.default_reasoning_effort,
                api_key_configured,
                oauth_supported,
                oauth_configured,
                models,
            });
        }
        Ok(ModelManagerReadResponse { providers })
    }

    pub(crate) fn update(
        &self,
        params: ModelManagerUpdateParams,
    ) -> Result<ModelManagerUpdateResponse, JSONRPCErrorError> {
        self.registry
            .update(|registry| match params {
                ModelManagerUpdateParams::ProviderDefaults {
                    provider_id,
                    default_model,
                    fast_model,
                    default_reasoning_effort,
                } => {
                    ensure_provider_config(registry, &self.config, &provider_id);
                    let provider = provider_mut(registry, &provider_id)?;
                    provider.default_model = default_model;
                    provider.fast_model = fast_model;
                    provider.default_reasoning_effort = default_reasoning_effort;
                    Ok(())
                }
                ModelManagerUpdateParams::ModelSettings {
                    provider_id,
                    model_id,
                    context_window,
                    max_context_window,
                    auto_compact_token_limit,
                    base_instructions,
                } => {
                    ensure_provider_config(registry, &self.config, &provider_id);
                    let provider = provider_mut(registry, &provider_id)?;
                    let model = provider.models.get_mut(&model_id).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("provider `{provider_id}` has no model `{model_id}`"),
                        )
                    })?;
                    model.info.context_window = Some(context_window);
                    model.info.max_context_window = Some(max_context_window);
                    model.info.auto_compact_token_limit = Some(auto_compact_token_limit);
                    model.info.base_instructions = base_instructions;
                    Ok(())
                }
            })
            .map_err(registry_error)?;
        Ok(ModelManagerUpdateResponse {})
    }

    pub(crate) async fn set_api_key(
        &self,
        params: ModelProviderApiKeySetParams,
    ) -> Result<ModelProviderApiKeySetResponse, JSONRPCErrorError> {
        self.require_external_provider(&params.provider_id)?;
        if params.provider_id == ANTHROPIC_PROVIDER_ID {
            let _operation_guard = self.anthropic_oauth_operation_lock.lock().await;
            cancel_active_anthropic_oauth_login(&self.anthropic_oauth_login_state).await;
            replace_anthropic_oauth_with_api_key(
                &anthropic_accounts_directory(&self.config.xedoc_home),
                &self.credentials,
                &self.anthropic_oauth_login_state,
                &params.api_key,
            )
            .map_err(credential_error)?;
        } else {
            self.credentials
                .set_api_key(&params.provider_id, &params.api_key)
                .map_err(credential_error)?;
        }
        Ok(ModelProviderApiKeySetResponse {})
    }

    pub(crate) fn delete_api_key(
        &self,
        params: ModelProviderApiKeyDeleteParams,
    ) -> Result<ModelProviderApiKeyDeleteResponse, JSONRPCErrorError> {
        self.require_external_provider(&params.provider_id)?;
        let deleted = self
            .credentials
            .delete_api_key(&params.provider_id)
            .map_err(credential_error)?;
        Ok(ModelProviderApiKeyDeleteResponse { deleted })
    }

    pub(crate) async fn delete_oauth(
        &self,
        params: ModelProviderOauthDeleteParams,
    ) -> Result<ModelProviderOauthDeleteResponse, JSONRPCErrorError> {
        self.require_external_provider(&params.provider_id)?;
        if params.provider_id != ANTHROPIC_PROVIDER_ID {
            return Err(invalid_params(
                "modelProvider/oauth/delete currently supports only Anthropic",
            ));
        }
        let _operation_guard = self.anthropic_oauth_operation_lock.lock().await;
        cancel_active_anthropic_oauth_login(&self.anthropic_oauth_login_state).await;
        let mut oauth_login_state = self
            .anthropic_oauth_login_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        oauth_login_state.generation += 1;
        let deleted = clear_anthropic_oauth_credentials(&anthropic_accounts_directory(
            &self.config.xedoc_home,
        ))
        .map_err(credential_error)?;
        drop(oauth_login_state);
        Ok(ModelProviderOauthDeleteResponse { deleted })
    }

    pub(crate) async fn start_oauth(
        &self,
        params: ModelProviderOauthStartParams,
    ) -> Result<ModelProviderOauthStartResponse, JSONRPCErrorError> {
        if params.provider_id != ANTHROPIC_PROVIDER_ID {
            return Err(invalid_params(
                "modelProvider/oauth/start currently supports only Anthropic",
            ));
        }
        let _operation_guard = self.anthropic_oauth_operation_lock.lock().await;
        let (login, oauth_login_generation, auth_url) = {
            let mut oauth_login_state = self
                .anthropic_oauth_login_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(auth_url) = &oauth_login_state.active_auth_url {
                return Ok(ModelProviderOauthStartResponse {
                    auth_url: auth_url.clone(),
                });
            }
            let login = AnthropicOAuthBrowserLogin::start().map_err(|error| {
                internal_error(format!("failed to start Anthropic OAuth: {error}"))
            })?;
            oauth_login_state.generation += 1;
            let oauth_login_generation = oauth_login_state.generation;
            let auth_url = login.auth_url().to_string();
            oauth_login_state.active_auth_url = Some(auth_url.clone());
            oauth_login_state.active_cancellation = Some(login.cancellation_handle());
            (login, oauth_login_generation, auth_url)
        };
        let transport = RouteAwareAuthHttpTransport::new(self.config.http_client_factory());
        let accounts_directory = anthropic_accounts_directory(&self.config.xedoc_home);
        let credentials = self.credentials.clone();
        let oauth_login_state = Arc::clone(&self.anthropic_oauth_login_state);
        tokio::spawn(async move {
            match login.complete(&transport).await {
                Ok(credential) => {
                    match complete_anthropic_oauth_login(
                        &accounts_directory,
                        &credentials,
                        &oauth_login_state,
                        oauth_login_generation,
                        &credential,
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::info!("ignored stale Anthropic OAuth login completion");
                        }
                        Err(error) => {
                            tracing::warn!("failed to complete Anthropic OAuth login: {error}");
                        }
                    }
                }
                Err(error) => {
                    clear_active_anthropic_oauth_login(&oauth_login_state, oauth_login_generation);
                    tracing::warn!("Anthropic OAuth login failed: {error}");
                }
            }
        });
        Ok(ModelProviderOauthStartResponse { auth_url })
    }

    fn provider_has_api_key(&self, provider_id: &str) -> Result<bool, JSONRPCErrorError> {
        if provider_id == OPENAI_PROVIDER_ID {
            return Ok(matches!(
                self.auth_manager.auth_cached(),
                Some(XedocAuth::ApiKey(_))
            ));
        }
        self.credentials
            .has_api_key(provider_id)
            .map_err(credential_error)
    }

    fn provider_has_oauth(&self, provider_id: &str) -> Result<bool, JSONRPCErrorError> {
        if provider_id == OPENAI_PROVIDER_ID {
            return Ok(matches!(
                self.auth_manager.auth_cached(),
                Some(XedocAuth::Chatgpt(_)) | Some(XedocAuth::ChatgptAuthTokens(_))
            ));
        }
        if provider_id != ANTHROPIC_PROVIDER_ID {
            return Ok(false);
        }
        let loaded = load_anthropic_oauth_credentials(&anthropic_accounts_directory(
            &self.config.xedoc_home,
        ))
        .map_err(|error| {
            internal_error(format!(
                "failed to read Anthropic OAuth credentials: {error}"
            ))
        })?;
        Ok(!loaded.accounts.is_empty())
    }

    fn require_external_provider(&self, provider_id: &str) -> Result<(), JSONRPCErrorError> {
        if provider_id == OPENAI_PROVIDER_ID {
            return Err(invalid_params(
                "use account/login/start or account/logout for OpenAI credentials",
            ));
        }
        if self.registry.snapshot().provider(provider_id).is_none() {
            return Err(invalid_params(format!(
                "unknown model provider `{provider_id}`"
            )));
        }
        Ok(())
    }
}

fn ensure_provider_config(registry: &mut ModelRegistry, config: &Config, provider_id: &str) {
    if registry.providers.contains_key(provider_id) {
        return;
    }
    let Some(provider_info) = config.model_providers.get(provider_id) else {
        return;
    };
    let Some(model_id) = (provider_id == config.model_provider_id)
        .then(|| config.model.clone())
        .flatten()
    else {
        return;
    };
    registry.providers.insert(
        provider_id.to_string(),
        provider_config_for_model(provider_info.name.clone(), model_id),
    );
}

fn provider_config_for_model(display_name: String, model_id: String) -> ProviderModelConfig {
    let mut info = model_info_from_provider_catalog_slug(&model_id, &display_name);
    info.supported_reasoning_levels = vec![ReasoningEffortPreset {
        effort: ReasoningEffort::None,
        description: "No reasoning budget".to_string(),
    }];
    info.default_reasoning_level = Some(ReasoningEffort::None);
    let model = ManagedModel {
        info: info.clone(),
        prices: None,
    };
    ProviderModelConfig {
        display_name,
        default_model: model_id.clone(),
        fast_model: model_id.clone(),
        default_reasoning_effort: ReasoningEffort::None,
        template: ManagedModel {
            info: model_info_from_provider_catalog_slug("*", &info.display_name),
            prices: None,
        },
        models: BTreeMap::from([(model_id, model)]),
    }
}

fn load_prompt_override(
    xedoc_home: &std::path::Path,
    provider_id: &str,
    model_id: &str,
) -> Option<String> {
    [
        xedoc_home
            .join("prompts")
            .join(provider_id)
            .join(format!("{model_id}.md")),
        xedoc_home.join("prompts").join(format!("{model_id}.md")),
    ]
    .into_iter()
    .find_map(|path| {
        std::fs::read_to_string(path)
            .ok()
            .filter(|contents| !contents.trim().is_empty())
    })
}

fn complete_anthropic_oauth_login(
    accounts_directory: &std::path::Path,
    credentials: &ProviderCredentialStore,
    oauth_login_state: &Mutex<AnthropicOauthLoginState>,
    oauth_login_generation: u64,
    credential: &AnthropicOAuthCredential,
) -> io::Result<bool> {
    let mut oauth_login_state = oauth_login_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if oauth_login_state.generation != oauth_login_generation {
        return Ok(false);
    }
    oauth_login_state.active_auth_url = None;
    oauth_login_state.active_cancellation = None;

    let previous_api_key = credentials.api_key(ANTHROPIC_PROVIDER_ID)?;
    if previous_api_key.is_some() {
        credentials.delete_api_key(ANTHROPIC_PROVIDER_ID)?;
    }
    if let Err(store_error) =
        store_anthropic_oauth_credential_with_status(accounts_directory, credential)
    {
        if store_error.destination_is_unchanged()
            && let Some(api_key) = previous_api_key
            && let Err(restore_error) = credentials.set_api_key(ANTHROPIC_PROVIDER_ID, &api_key)
        {
            return Err(io::Error::other(format!(
                "failed to store Anthropic OAuth credentials: {store_error}; failed to restore the API key: {restore_error}"
            )));
        }
        return Err(store_error.into_io_error());
    }
    drop(oauth_login_state);
    Ok(true)
}

fn replace_anthropic_oauth_with_api_key(
    accounts_directory: &std::path::Path,
    credentials: &ProviderCredentialStore,
    oauth_login_state: &Mutex<AnthropicOauthLoginState>,
    api_key: &str,
) -> io::Result<()> {
    let mut oauth_login_state = oauth_login_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    oauth_login_state.generation += 1;
    oauth_login_state.active_auth_url = None;
    oauth_login_state.active_cancellation = None;
    let previous_oauth_credentials = take_anthropic_oauth_credentials(accounts_directory)?;
    if let Err(api_key_error) = credentials.set_api_key(ANTHROPIC_PROVIDER_ID, api_key) {
        if let Err(delete_api_key_error) = credentials.delete_api_key(ANTHROPIC_PROVIDER_ID) {
            return Err(io::Error::other(format!(
                "failed to store the Anthropic API key: {api_key_error}; failed to remove the possibly stored API key: {delete_api_key_error}"
            )));
        }
        if let Err(restore_oauth_error) =
            restore_anthropic_oauth_credentials(&previous_oauth_credentials)
        {
            return Err(io::Error::other(format!(
                "failed to store the Anthropic API key: {api_key_error}; failed to restore OAuth credentials: {restore_oauth_error}"
            )));
        }
        return Err(api_key_error);
    }
    drop(oauth_login_state);
    Ok(())
}

fn clear_active_anthropic_oauth_login(
    oauth_login_state: &Mutex<AnthropicOauthLoginState>,
    oauth_login_generation: u64,
) {
    let mut oauth_login_state = oauth_login_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if oauth_login_state.generation == oauth_login_generation {
        oauth_login_state.active_auth_url = None;
        oauth_login_state.active_cancellation = None;
    }
}

async fn cancel_active_anthropic_oauth_login(oauth_login_state: &Mutex<AnthropicOauthLoginState>) {
    let cancellation = {
        let mut oauth_login_state = oauth_login_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        oauth_login_state.generation += 1;
        oauth_login_state.active_auth_url = None;
        oauth_login_state.active_cancellation.take()
    };
    if let Some(cancellation) = cancellation {
        cancellation.cancel().await;
    }
}

fn provider_mut<'a>(
    registry: &'a mut ModelRegistry,
    provider_id: &str,
) -> io::Result<&'a mut xedoc_models_manager::registry::ProviderModelConfig> {
    registry.provider_mut(provider_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("unknown model provider `{provider_id}`"),
        )
    })
}

fn anthropic_accounts_directory(xedoc_home: &std::path::Path) -> PathBuf {
    ANTHROPIC_ACCOUNTS_PATH
        .into_iter()
        .fold(xedoc_home.to_path_buf(), |path, component| {
            path.join(component)
        })
}

fn registry_error(error: io::Error) -> JSONRPCErrorError {
    match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::NotFound => {
            invalid_params(error.to_string())
        }
        _ => internal_error(format!("failed to update model settings: {error}")),
    }
}

fn credential_error(error: io::Error) -> JSONRPCErrorError {
    match error.kind() {
        io::ErrorKind::InvalidInput => invalid_params(error.to_string()),
        _ => internal_error(format!("failed to update provider credentials: {error}")),
    }
}

#[cfg(test)]
#[path = "model_manager_processor_tests.rs"]
mod tests;
