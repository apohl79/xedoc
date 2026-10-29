use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use xedoc_api::ApiError;
use xedoc_api::Provider;
use xedoc_api::SharedAuthProvider;
use xedoc_login::AuthManager;
use xedoc_login::XedocAuth;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_model_provider_info::WireApi;
use xedoc_models_manager::manager::ModelsEndpointClient;
use xedoc_models_manager::manager::OpenAiModelsManager;
use xedoc_models_manager::manager::SharedModelsManager;
use xedoc_models_manager::manager::StaticModelsManager;
use xedoc_protocol::account::ProviderAccount;
use xedoc_protocol::error::XedocErr;
use xedoc_protocol::openai_models::ModelsResponse;

use crate::amazon_bedrock::AmazonBedrockModelProvider;
use crate::anthropic::AnthropicModelProvider;
use crate::auth::ProviderAuthScope;
use crate::auth::ResolvedProviderAuth;
use crate::auth::auth_manager_for_provider;
use crate::auth::resolve_provider_auth;
use crate::auth::resolve_provider_auth_for_scope;
use crate::auth::resolve_provider_auth_with_api_key;
use crate::deepseek_models_endpoint::DeepSeekModelsEndpoint;
use crate::gemini_models_endpoint::GeminiModelsEndpoint;
use crate::models_endpoint::OpenAiModelsEndpoint;
use crate::provider_api_key::ProviderApiKeySource;

/// Optional provider-backed features that Xedoc may expose at runtime.
///
/// These capabilities are a provider-owned upper bound. Callers can disable
/// more functionality through normal config, but should not expose a feature
/// that the active provider marks unsupported here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub namespace_tools: bool,
    pub image_generation: bool,
    pub web_search: bool,
}

impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self {
            namespace_tools: true,
            image_generation: true,
            web_search: true,
        }
    }
}

/// Current app-visible account state for a model provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountState {
    pub account: Option<ProviderAccount>,
    pub requires_openai_auth: bool,
}

/// Error returned when a provider cannot construct its app-visible account state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAccountError {
    MissingChatgptAccountDetails,
    UnsupportedBedrockApiKeyAuth,
}

impl fmt::Display for ProviderAccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingChatgptAccountDetails => {
                write!(f, "plan type is required for chatgpt authentication")
            }
            Self::UnsupportedBedrockApiKeyAuth => {
                write!(
                    f,
                    "Bedrock API key auth is only supported by the Amazon Bedrock model provider"
                )
            }
        }
    }
}

impl std::error::Error for ProviderAccountError {}

pub type ProviderAccountResult = std::result::Result<ProviderAccountState, ProviderAccountError>;

/// Runtime provider abstraction used by model execution.
///
/// Implementations own provider-specific behavior for a model backend. The
/// `ModelProviderInfo` returned by `info` is the serialized/configured provider
/// metadata used by the default OpenAI-compatible implementation.
pub trait ModelProvider: fmt::Debug + Send + Sync {
    /// Returns the configured provider metadata.
    fn info(&self) -> &ModelProviderInfo;

    /// Returns the provider-owned capability upper bounds.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Returns the provider-scoped auth manager, when this provider uses one.
    ///
    /// TODO(celia-oai): Make auth manager access internal to this crate so callers
    /// resolve provider-specific auth only through `ModelProvider`. We first need
    /// to think through whether Xedoc should have a unified provider-specific auth
    /// manager throughout the codebase; that is a larger refactor than this change.
    fn auth_manager(&self) -> Option<Arc<AuthManager>>;

    /// Returns the current provider-scoped auth value, if one is configured.
    fn auth(&self) -> ModelProviderFuture<'_, Option<XedocAuth>>;

    /// Returns the current app-visible account state for this provider.
    fn account_state(&self) -> ProviderAccountResult;

    /// Maps an API client error into the provider's user-facing error representation.
    fn map_api_error(&self, error: ApiError) -> XedocErr {
        xedoc_api::map_api_error(error)
    }

    /// Returns provider configuration adapted for the API client.
    fn api_provider(&self) -> ModelProviderFuture<'_, xedoc_protocol::error::Result<Provider>> {
        Box::pin(async move {
            let auth = self.auth().await;
            self.info()
                .to_api_provider(auth.as_ref().map(XedocAuth::auth_mode))
        })
    }

    /// Returns the provider base URL that will be used at request time.
    fn runtime_base_url(
        &self,
    ) -> ModelProviderFuture<'_, xedoc_protocol::error::Result<Option<String>>> {
        Box::pin(async { Ok(self.info().base_url.clone()) })
    }

    /// Returns the auth provider used to attach request credentials.
    fn api_auth(
        &self,
    ) -> ModelProviderFuture<'_, xedoc_protocol::error::Result<SharedAuthProvider>> {
        Box::pin(async move {
            let auth = self.auth().await;
            resolve_provider_auth(auth.as_ref(), self.info())
        })
    }

    /// Returns request credentials, optionally scoped to a Xedoc session task.
    fn api_auth_for_scope(
        &self,
        scope: ProviderAuthScope,
    ) -> ModelProviderFuture<'_, xedoc_protocol::error::Result<ResolvedProviderAuth>> {
        Box::pin(async move {
            if !provider_uses_first_party_auth_path(self.info()) {
                return self.api_auth().await.map(ResolvedProviderAuth::new);
            }
            let auth = self.auth().await;
            resolve_provider_auth_for_scope(self.auth_manager(), auth.as_ref(), self.info(), scope)
                .await
        })
    }

    /// Creates the model manager implementation appropriate for this provider.
    fn models_manager(
        &self,
        xedoc_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager;

    /// Creates a model manager with caching disabled.
    ///
    /// Providers that fetch model catalogs should override this method. The default uses an
    /// authoritative in-memory catalog so hosted callers cannot accidentally write to disk.
    fn models_manager_without_cache(
        &self,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        let model_catalog = config_model_catalog
            .or_else(|| xedoc_models_manager::bundled_models_response().ok())
            .unwrap_or_default();
        Arc::new(StaticModelsManager::new(self.auth_manager(), model_catalog))
    }
}

pub type ModelProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Shared runtime model provider handle.
pub type SharedModelProvider = Arc<dyn ModelProvider>;

fn provider_uses_first_party_auth_path(provider: &ModelProviderInfo) -> bool {
    provider.requires_openai_auth
        && provider.env_key.is_none()
        && provider.experimental_bearer_token.is_none()
        && provider.auth.is_none()
        && provider.aws.is_none()
}

/// Creates the default runtime model provider for configured provider metadata.
pub fn create_model_provider(
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
) -> SharedModelProvider {
    if provider_info.is_amazon_bedrock() {
        Arc::new(AmazonBedrockModelProvider::new(provider_info, auth_manager))
    } else if provider_info.is_native_anthropic() {
        Arc::new(AnthropicModelProvider::new(provider_info, auth_manager))
    } else {
        Arc::new(ConfiguredModelProvider::new(provider_info, auth_manager))
    }
}

/// Creates a runtime model provider for a configured provider ID and metadata.
pub fn create_model_provider_for_configured_id(
    provider_id: String,
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
) -> SharedModelProvider {
    if provider_info.is_amazon_bedrock() {
        Arc::new(AmazonBedrockModelProvider::new(provider_info, auth_manager))
    } else if provider_info.is_native_anthropic() {
        Arc::new(AnthropicModelProvider::new_for_configured_id(
            provider_id,
            provider_info,
            auth_manager,
        ))
    } else {
        Arc::new(ConfiguredModelProvider::new_for_configured_id(
            provider_id,
            provider_info,
            auth_manager,
        ))
    }
}

/// Reports whether a configured provider currently has usable runtime credentials.
pub fn configured_provider_has_credentials(
    provider_id: &str,
    provider_info: &ModelProviderInfo,
    auth_manager: &Arc<AuthManager>,
) -> xedoc_protocol::error::Result<bool> {
    let api_key_source =
        ProviderApiKeySource::new(Some(provider_id.to_string()), Some(auth_manager));
    if api_key_source.resolve(provider_info)?.is_some() {
        return Ok(true);
    }
    if provider_info.is_native_anthropic() {
        return crate::anthropic::has_oauth_credentials(auth_manager);
    }
    Ok(false)
}

/// Runtime model provider backed by configured `ModelProviderInfo`.
#[derive(Clone, Debug)]
pub(crate) struct ConfiguredModelProvider {
    info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
    api_key_source: ProviderApiKeySource,
}

impl ConfiguredModelProvider {
    pub(crate) fn new(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        Self::new_with_provider_id(/*provider_id*/ None, provider_info, auth_manager)
    }

    pub(crate) fn new_for_configured_id(
        provider_id: String,
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        Self::new_with_provider_id(Some(provider_id), provider_info, auth_manager)
    }

    fn new_with_provider_id(
        provider_id: Option<String>,
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        let api_key_source = ProviderApiKeySource::new(provider_id, auth_manager.as_ref());
        let auth_manager = auth_manager_for_provider(auth_manager, &provider_info);
        Self {
            info: provider_info,
            auth_manager,
            api_key_source,
        }
    }

    pub(crate) fn provider_api_key(&self) -> xedoc_protocol::error::Result<Option<String>> {
        self.api_key_source.resolve(&self.info)
    }

    fn required_provider_api_key(&self) -> xedoc_protocol::error::Result<Option<String>> {
        match self.provider_api_key()? {
            Some(api_key) => Ok(Some(api_key)),
            None => self.info.api_key(),
        }
    }
}

impl ModelProvider for ConfiguredModelProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            namespace_tools: self.info.namespace_tools,
            ..ProviderCapabilities::default()
        }
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.auth_manager.clone()
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<XedocAuth>> {
        Box::pin(async move {
            match self.auth_manager.as_ref() {
                Some(auth_manager) => auth_manager.auth().await,
                None => None,
            }
        })
    }

    fn api_auth(
        &self,
    ) -> ModelProviderFuture<'_, xedoc_protocol::error::Result<SharedAuthProvider>> {
        Box::pin(async move {
            let auth = self.auth().await;
            let api_key = self.required_provider_api_key()?;
            resolve_provider_auth_with_api_key(auth.as_ref(), self.info(), api_key)
        })
    }

    fn account_state(&self) -> ProviderAccountResult {
        let account = if self.info.requires_openai_auth {
            self.auth_manager
                .as_ref()
                .and_then(|auth_manager| {
                    let auth = auth_manager.auth_cached()?;
                    if auth_manager.refresh_failure_for_auth(&auth).is_some() {
                        return None;
                    }
                    if matches!(auth, XedocAuth::Headers(_)) {
                        return None;
                    }
                    Some(auth)
                })
                .map(|auth| match &auth {
                    XedocAuth::ApiKey(_) => Ok(ProviderAccount::ApiKey),
                    XedocAuth::BedrockApiKey(_) => {
                        Err(ProviderAccountError::UnsupportedBedrockApiKeyAuth)
                    }
                    XedocAuth::Chatgpt(_)
                    | XedocAuth::ChatgptAuthTokens(_)
                    | XedocAuth::Headers(_)
                    | XedocAuth::AgentIdentity(_)
                    | XedocAuth::PersonalAccessToken(_) => {
                        let email = auth.get_account_email();
                        let plan_type = auth.account_plan_type();

                        plan_type
                            .map(|plan_type| ProviderAccount::Chatgpt { email, plan_type })
                            .ok_or(ProviderAccountError::MissingChatgptAccountDetails)
                    }
                })
                .transpose()?
        } else {
            None
        };

        Ok(ProviderAccountState {
            account,
            requires_openai_auth: self.info.requires_openai_auth,
        })
    }

    fn models_manager(
        &self,
        xedoc_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        match config_model_catalog {
            Some(model_catalog) => Arc::new(StaticModelsManager::new(
                self.auth_manager.clone(),
                model_catalog,
            )),
            None => {
                let endpoint = self.models_endpoint();
                Arc::new(OpenAiModelsManager::new(
                    xedoc_home,
                    endpoint,
                    self.auth_manager.clone(),
                ))
            }
        }
    }

    fn models_manager_without_cache(
        &self,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        match config_model_catalog {
            Some(model_catalog) => Arc::new(StaticModelsManager::new(
                self.auth_manager.clone(),
                model_catalog,
            )),
            None => {
                let endpoint = self.models_endpoint();
                Arc::new(OpenAiModelsManager::new_without_cache(
                    endpoint,
                    self.auth_manager.clone(),
                ))
            }
        }
    }
}

impl ConfiguredModelProvider {
    fn models_endpoint(&self) -> Arc<dyn ModelsEndpointClient> {
        if self.info.is_native_deepseek() {
            return Arc::new(DeepSeekModelsEndpoint::new_with_api_key_source(
                self.info.clone(),
                self.auth_manager.clone(),
                Some(self.api_key_source.clone()),
            ));
        }
        match self.info.wire_api {
            WireApi::Gemini => Arc::new(GeminiModelsEndpoint::new_with_api_key_source(
                self.info.clone(),
                self.auth_manager.clone(),
                Some(self.api_key_source.clone()),
            )),
            WireApi::Anthropic | WireApi::Responses => {
                Arc::new(OpenAiModelsEndpoint::new_with_api_key_source(
                    self.info.clone(),
                    self.auth_manager.clone(),
                    Some(self.api_key_source.clone()),
                ))
            }
        }
    }
}
