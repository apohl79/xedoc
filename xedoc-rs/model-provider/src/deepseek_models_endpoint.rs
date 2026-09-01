use std::sync::Arc;

use xedoc_http_client::HttpClientFactory;
use xedoc_login::AuthManager;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_models_manager::manager::ModelsEndpointClient;
use xedoc_models_manager::manager::ModelsEndpointFuture;
use xedoc_protocol::error::Result as CoreResult;
use xedoc_protocol::openai_models::ModelInfo;

use crate::models_endpoint::OpenAiModelsEndpoint;

const DEEPSEEK_MESSAGES_BASE_SUFFIX: &str = "/anthropic/v1";
const DEEPSEEK_CATALOG_BASE_SUFFIX: &str = "/v1";
const DEEPSEEK_DEFAULT_CATALOG_BASE_URL: &str = "https://api.deepseek.com/v1";

/// Provider-owned model catalog endpoint for DeepSeek's OpenAI-compatible API.
#[derive(Debug)]
pub(crate) struct DeepSeekModelsEndpoint {
    inner: OpenAiModelsEndpoint,
}

impl DeepSeekModelsEndpoint {
    pub(crate) fn new(
        mut provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        let catalog_base_url = provider_info
            .base_url
            .as_deref()
            .and_then(|base_url| base_url.strip_suffix(DEEPSEEK_MESSAGES_BASE_SUFFIX))
            .map_or_else(
                || DEEPSEEK_DEFAULT_CATALOG_BASE_URL.to_string(),
                |api_root| format!("{api_root}{DEEPSEEK_CATALOG_BASE_SUFFIX}"),
            );
        provider_info.base_url = Some(catalog_base_url);
        Self {
            inner: OpenAiModelsEndpoint::new(provider_info, auth_manager),
        }
    }
}

impl ModelsEndpointClient for DeepSeekModelsEndpoint {
    fn has_command_auth(&self) -> bool {
        self.inner.has_command_auth()
    }

    fn has_authoritative_remote_catalog(&self) -> bool {
        true
    }

    fn uses_xedoc_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        self.inner.uses_xedoc_backend()
    }

    fn list_models<'a>(
        &'a self,
        client_version: &'a str,
        http_client_factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<(Vec<ModelInfo>, Option<String>)>> {
        Box::pin(async move {
            let (mut models, etag) = self
                .inner
                .list_models(client_version, http_client_factory)
                .await?;
            models.retain(|model| model.slug.starts_with("deepseek-"));
            Ok((models, etag))
        })
    }
}

#[cfg(test)]
#[path = "deepseek_models_endpoint_tests.rs"]
mod tests;
