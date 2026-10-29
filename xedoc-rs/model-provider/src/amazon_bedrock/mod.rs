mod auth;
mod catalog;
mod error;
mod mantle;

use std::path::PathBuf;
use std::sync::Arc;

use xedoc_api::ApiError;
use xedoc_api::Provider;
use xedoc_api::SharedAuthProvider;
use xedoc_login::AuthManager;
use xedoc_login::XedocAuth;
use xedoc_login::auth::BedrockApiKeyAuth;
use xedoc_model_provider_info::ModelProviderAwsAuthInfo;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_models_manager::manager::SharedModelsManager;
use xedoc_models_manager::manager::StaticModelsManager;
use xedoc_protocol::account::ProviderAccount;
use xedoc_protocol::error::Result;
use xedoc_protocol::error::XedocErr;
use xedoc_protocol::openai_models::ModelsResponse;

use crate::auth::auth_manager_for_provider;
use crate::auth::resolve_provider_auth as resolve_configured_provider_auth;
use crate::provider::ModelProvider;
use crate::provider::ModelProviderFuture;
use crate::provider::ProviderAccountResult;
use crate::provider::ProviderAccountState;
use crate::provider::ProviderCapabilities;
use auth::resolve_provider_auth as resolve_bedrock_provider_auth;
pub(crate) use catalog::static_model_catalog;
use catalog::with_default_only_service_tier;
use mantle::bedrock_mantle_runtime_base_url;
pub use mantle::is_supported_amazon_bedrock_region;

/// Runtime provider for Amazon Bedrock's OpenAI-compatible Mantle endpoint.
#[derive(Clone, Debug)]
pub(crate) struct AmazonBedrockModelProvider {
    pub(crate) info: ModelProviderInfo,
    pub(crate) aws: ModelProviderAwsAuthInfo,
    auth_manager: Option<Arc<AuthManager>>,
}

impl AmazonBedrockModelProvider {
    pub(crate) fn new(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        let auth_manager = auth_manager_for_provider(auth_manager, &provider_info);
        let aws = provider_info
            .aws
            .clone()
            .unwrap_or(ModelProviderAwsAuthInfo {
                profile: None,
                region: None,
            });
        Self {
            info: provider_info,
            aws,
            auth_manager,
        }
    }

    fn managed_auth(&self) -> Option<BedrockApiKeyAuth> {
        self.auth_manager
            .as_ref()
            .and_then(|auth_manager| auth_manager.auth_cached())
            .and_then(|auth| match auth {
                XedocAuth::BedrockApiKey(auth) => Some(auth),
                XedocAuth::ApiKey(_)
                | XedocAuth::Chatgpt(_)
                | XedocAuth::ChatgptAuthTokens(_)
                | XedocAuth::Headers(_)
                | XedocAuth::AgentIdentity(_)
                | XedocAuth::PersonalAccessToken(_) => None,
            })
    }

    async fn auth(&self) -> Option<XedocAuth> {
        if self.info.has_command_auth() {
            match self.auth_manager.as_ref() {
                Some(auth_manager) => auth_manager.auth().await,
                None => None,
            }
        } else {
            self.managed_auth().map(XedocAuth::BedrockApiKey)
        }
    }

    async fn api_provider(&self) -> Result<Provider> {
        let mut api_provider_info = self.info.clone();
        api_provider_info.base_url = self.runtime_base_url().await?;
        api_provider_info.to_api_provider(/*auth_mode*/ None)
    }

    async fn runtime_base_url(&self) -> Result<Option<String>> {
        if let Some(base_url) = self.info.base_url.clone() {
            return Ok(Some(base_url));
        }
        let managed_auth = self.managed_auth();
        Ok(Some(
            bedrock_mantle_runtime_base_url(managed_auth.as_ref(), &self.aws).await?,
        ))
    }

    async fn api_auth(&self) -> Result<SharedAuthProvider> {
        if self.info.has_command_auth() {
            let auth = self.auth().await;
            return resolve_configured_provider_auth(auth.as_ref(), &self.info);
        }
        let managed_auth = self.managed_auth();
        resolve_bedrock_provider_auth(managed_auth.as_ref(), &self.aws).await
    }
}

impl ModelProvider for AmazonBedrockModelProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            namespace_tools: true,
            image_generation: false,
            web_search: false,
        }
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        if self.info.has_command_auth() || self.managed_auth().is_some() {
            self.auth_manager.clone()
        } else {
            None
        }
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<XedocAuth>> {
        Box::pin(AmazonBedrockModelProvider::auth(self))
    }

    fn account_state(&self) -> ProviderAccountResult {
        Ok(ProviderAccountState {
            account: Some(ProviderAccount::AmazonBedrock {
                uses_xedoc_managed_credentials: self.managed_auth().is_some(),
            }),
            requires_openai_auth: false,
        })
    }

    fn map_api_error(&self, error: ApiError) -> XedocErr {
        error::map_api_error(error)
    }

    fn api_provider(&self) -> ModelProviderFuture<'_, Result<Provider>> {
        Box::pin(AmazonBedrockModelProvider::api_provider(self))
    }

    fn runtime_base_url(&self) -> ModelProviderFuture<'_, Result<Option<String>>> {
        Box::pin(AmazonBedrockModelProvider::runtime_base_url(self))
    }

    fn api_auth(&self) -> ModelProviderFuture<'_, Result<SharedAuthProvider>> {
        Box::pin(AmazonBedrockModelProvider::api_auth(self))
    }

    fn models_manager(
        &self,
        _xedoc_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        Arc::new(StaticModelsManager::new(
            /*auth_manager*/ None,
            config_model_catalog.map_or_else(static_model_catalog, with_default_only_service_tier),
        ))
    }

    fn models_manager_without_cache(
        &self,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        Arc::new(StaticModelsManager::new(
            /*auth_manager*/ None,
            config_model_catalog.map_or_else(static_model_catalog, with_default_only_service_tier),
        ))
    }
}
