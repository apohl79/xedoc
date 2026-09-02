use std::sync::Arc;

use tokio::time::timeout;
use xedoc_api::ReqwestTransport;
use xedoc_api::map_api_error;
use xedoc_http_client::HttpClientFactory;
use xedoc_login::AuthManager;
use xedoc_login::XedocAuth;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_models_manager::manager::ModelsEndpointClient;
use xedoc_models_manager::manager::ModelsEndpointFuture;
use xedoc_models_manager::model_info::model_info_from_provider_catalog_slug;
use xedoc_protocol::error::Result as CoreResult;
use xedoc_protocol::error::XedocErr;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ModelVisibility;
use xedoc_provider_gemini::GeminiCatalogClient;

use crate::auth::resolve_provider_auth;
use crate::auth::resolve_provider_auth_with_api_key;
use crate::models_endpoint::MODELS_REFRESH_TIMEOUT;
use crate::models_endpoint::build_models_transport;
use crate::models_endpoint::models_request_telemetry;
use crate::provider_api_key::ProviderApiKeySource;

/// Provider-owned native Google model-catalog endpoint.
#[derive(Debug)]
pub(crate) struct GeminiModelsEndpoint {
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
    api_key_source: Option<ProviderApiKeySource>,
}

impl GeminiModelsEndpoint {
    pub(crate) fn new_with_api_key_source(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
        api_key_source: Option<ProviderApiKeySource>,
    ) -> Self {
        Self {
            provider_info,
            auth_manager,
            api_key_source,
        }
    }

    async fn list_models(
        &self,
        http_client_factory: HttpClientFactory,
    ) -> CoreResult<(Vec<ModelInfo>, Option<String>)> {
        let _timer =
            xedoc_otel::start_global_timer("xedoc.remote_models.fetch_update.duration_ms", &[]);
        let auth = match self.auth_manager.as_ref() {
            Some(auth_manager) => auth_manager.auth().await,
            None => None,
        };
        let auth_mode = auth.as_ref().map(XedocAuth::auth_mode);
        let api_provider = self.provider_info.to_api_provider(auth_mode)?;
        let api_auth = match &self.api_key_source {
            Some(source) => match source.resolve(&self.provider_info)? {
                Some(api_key) => resolve_provider_auth_with_api_key(
                    auth.as_ref(),
                    &self.provider_info,
                    Some(api_key),
                )?,
                None => resolve_provider_auth(auth.as_ref(), &self.provider_info)?,
            },
            None => resolve_provider_auth(auth.as_ref(), &self.provider_info)?,
        };
        let request_telemetry = models_request_telemetry(
            &self.provider_info,
            self.auth_manager.as_ref(),
            auth.as_ref(),
            api_auth.as_ref(),
        );

        timeout(MODELS_REFRESH_TIMEOUT, async {
            let request_url = api_provider.url_for_path("models");
            let transport = build_models_transport(http_client_factory, request_url).await?;
            let models =
                GeminiCatalogClient::<ReqwestTransport>::new(transport, api_provider, api_auth)
                    .with_telemetry(Some(request_telemetry))
                    .list_models()
                    .await
                    .map_err(map_api_error)?
                    .into_iter()
                    .enumerate()
                    .map(|(priority, model)| {
                        let mut model_info = model_info_from_provider_catalog_slug(
                            &model.slug,
                            &self.provider_info.name,
                        );
                        model_info.display_name = model.display_name;
                        model_info.description = model.description;
                        model_info.context_window = model.input_token_limit;
                        model_info.max_context_window = model.input_token_limit;
                        model_info.priority = i32::try_from(priority).unwrap_or(i32::MAX);
                        model_info.visibility = ModelVisibility::List;
                        model_info
                    })
                    .collect();
            Ok((models, None))
        })
        .await
        .map_err(|_| XedocErr::Timeout)?
    }
}

impl ModelsEndpointClient for GeminiModelsEndpoint {
    fn has_command_auth(&self) -> bool {
        self.provider_info.has_command_auth()
    }

    fn has_authoritative_remote_catalog(&self) -> bool {
        true
    }

    fn uses_xedoc_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        Box::pin(async { false })
    }

    fn list_models<'a>(
        &'a self,
        _client_version: &'a str,
        http_client_factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<(Vec<ModelInfo>, Option<String>)>> {
        Box::pin(GeminiModelsEndpoint::list_models(self, http_client_factory))
    }
}
