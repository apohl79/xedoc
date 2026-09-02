use std::io;
use std::path::PathBuf;
use std::sync::Arc;

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
use xedoc_app_server_protocol::ModelProviderOauthStartParams;
use xedoc_app_server_protocol::ModelProviderOauthStartResponse;
use xedoc_core::config::Config;
use xedoc_login::AuthManager;
use xedoc_login::ProviderCredentialStore;
use xedoc_login::XedocAuth;
use xedoc_model_provider_info::ANTHROPIC_PROVIDER_ID;
use xedoc_model_provider_info::OPENAI_PROVIDER_ID;
use xedoc_models_manager::registry::ModelRegistry;
use xedoc_models_manager::registry::SharedModelRegistry;
use xedoc_provider_anthropic::AnthropicOAuthBrowserLogin;
use xedoc_provider_anthropic::load_anthropic_oauth_credentials;
use xedoc_provider_anthropic::store_anthropic_oauth_credential;

use crate::error_code::internal_error;
use crate::error_code::invalid_params;

const ANTHROPIC_ACCOUNTS_PATH: [&str; 3] = ["providers", "anthropic", "accounts"];

#[derive(Clone)]
pub(crate) struct ModelManagerRequestProcessor {
    config: Arc<Config>,
    auth_manager: Arc<AuthManager>,
    registry: SharedModelRegistry,
    credentials: ProviderCredentialStore,
}

impl ModelManagerRequestProcessor {
    pub(crate) fn new(config: Arc<Config>, auth_manager: Arc<AuthManager>) -> Self {
        Self {
            registry: config.model_registry.clone(),
            credentials: ProviderCredentialStore::new(config.xedoc_home.to_path_buf()),
            config,
            auth_manager,
        }
    }

    pub(crate) fn read(&self) -> Result<ModelManagerReadResponse, JSONRPCErrorError> {
        let registry = self.registry.snapshot();
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
                    ManagedModelSettings {
                        id: model_id,
                        display_name: info.display_name,
                        context_window,
                        max_context_window: info.max_context_window.unwrap_or(context_window),
                        auto_compact_token_limit,
                        base_instructions: info.base_instructions,
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

    pub(crate) fn set_api_key(
        &self,
        params: ModelProviderApiKeySetParams,
    ) -> Result<ModelProviderApiKeySetResponse, JSONRPCErrorError> {
        self.require_external_provider(&params.provider_id)?;
        self.credentials
            .set_api_key(&params.provider_id, &params.api_key)
            .map_err(credential_error)?;
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

    pub(crate) fn start_oauth(
        &self,
        params: ModelProviderOauthStartParams,
    ) -> Result<ModelProviderOauthStartResponse, JSONRPCErrorError> {
        if params.provider_id != ANTHROPIC_PROVIDER_ID {
            return Err(invalid_params(
                "modelProvider/oauth/start currently supports only Anthropic",
            ));
        }
        let login = AnthropicOAuthBrowserLogin::start()
            .map_err(|error| internal_error(format!("failed to start Anthropic OAuth: {error}")))?;
        let auth_url = login.auth_url().to_string();
        let transport = RouteAwareAuthHttpTransport::new(self.config.http_client_factory());
        let accounts_directory = anthropic_accounts_directory(&self.config.xedoc_home);
        tokio::spawn(async move {
            match login.complete(&transport).await {
                Ok(credential) => {
                    if let Err(error) =
                        store_anthropic_oauth_credential(&accounts_directory, &credential)
                    {
                        tracing::warn!("failed to store Anthropic OAuth credentials: {error}");
                    }
                }
                Err(error) => tracing::warn!("Anthropic OAuth login failed: {error}"),
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
