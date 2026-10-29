use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use http::HeaderMap;
use tokio::sync::OnceCell;
use tokio::time::timeout;
use xedoc_api::AgentIdentityTelemetry;
use xedoc_api::ModelCatalog;
use xedoc_api::ModelsClient;
use xedoc_api::RequestTelemetry;
use xedoc_api::ReqwestTransport;
use xedoc_api::TransportError;
use xedoc_api::auth_header_telemetry;
use xedoc_api::map_api_error;
use xedoc_http_client::ClientRouteClass;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;
use xedoc_login::AuthEnvTelemetry;
use xedoc_login::AuthManager;
use xedoc_login::XedocAuth;
use xedoc_login::collect_auth_env_telemetry;
use xedoc_login::default_client::build_default_reqwest_client_for_route_async;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_models_manager::manager::ModelsEndpointClient;
use xedoc_models_manager::manager::ModelsEndpointFuture;
use xedoc_models_manager::model_info::model_info_from_provider_catalog_slug;
use xedoc_otel::TelemetryAuthMode;
use xedoc_protocol::error::Result as CoreResult;
use xedoc_protocol::error::XedocErr;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_response_debug_context::extract_response_debug_context;
use xedoc_response_debug_context::telemetry_transport_error_message;

use crate::auth::agent_identity_telemetry;
use crate::auth::resolve_provider_auth;
use crate::auth::resolve_provider_auth_with_api_key;
use crate::provider_api_key::ProviderApiKeySource;

pub(crate) const MODELS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const MODELS_ENDPOINT: &str = "/models";

/// Provider-owned OpenAI-compatible `/models` endpoint.
#[derive(Debug)]
pub(crate) struct OpenAiModelsEndpoint {
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
    api_key_source: Option<ProviderApiKeySource>,
    transport_builder: Arc<dyn ModelsTransportBuilder>,
    reqwest_default_transport: OnceCell<ReqwestTransport>,
    system_proxy_transport: OnceCell<ReqwestTransport>,
}

impl OpenAiModelsEndpoint {
    pub(crate) fn new_with_api_key_source(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
        api_key_source: Option<ProviderApiKeySource>,
    ) -> Self {
        Self {
            provider_info,
            auth_manager,
            api_key_source,
            transport_builder: Arc::new(RouteAwareModelsTransportBuilder),
            reqwest_default_transport: OnceCell::new(),
            system_proxy_transport: OnceCell::new(),
        }
    }

    async fn auth(&self) -> Option<XedocAuth> {
        match self.auth_manager.as_ref() {
            Some(auth_manager) => auth_manager.auth().await,
            None => None,
        }
    }

    async fn uses_xedoc_backend(&self) -> bool {
        self.auth()
            .await
            .as_ref()
            .is_some_and(XedocAuth::uses_xedoc_backend)
    }

    async fn list_models(
        &self,
        client_version: &str,
        http_client_factory: HttpClientFactory,
    ) -> CoreResult<(Vec<ModelInfo>, Option<String>)> {
        let _timer =
            xedoc_otel::start_global_timer("xedoc.remote_models.fetch_update.duration_ms", &[]);
        let auth = self.auth().await;
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
        let request_url =
            ModelsClient::<ReqwestTransport>::request_url(&api_provider, client_version);
        let request_telemetry = models_request_telemetry(
            &self.provider_info,
            self.auth_manager.as_ref(),
            auth.as_ref(),
            api_auth.as_ref(),
        );
        timeout(MODELS_REFRESH_TIMEOUT, async {
            let transport = match http_client_factory.outbound_proxy_policy() {
                OutboundProxyPolicy::ReqwestDefault => {
                    self.reqwest_default_transport
                        .get_or_try_init(|| {
                            self.transport_builder
                                .build(http_client_factory, request_url.clone())
                        })
                        .await?
                }
                OutboundProxyPolicy::RespectSystemProxy => {
                    self.system_proxy_transport
                        .get_or_try_init(|| {
                            self.transport_builder
                                .build(http_client_factory, request_url.clone())
                        })
                        .await?
                }
            }
            .clone();
            let client = ModelsClient::new(transport, api_provider, api_auth)
                .with_telemetry(Some(request_telemetry));
            let (catalog, etag) = client
                .list_models(request_url, HeaderMap::new())
                .await
                .map_err(map_api_error)?;
            let models = match catalog {
                ModelCatalog::Xedoc(models) => models,
                ModelCatalog::OpenAiCompatible(models) => models
                    .into_iter()
                    .filter(|model| {
                        self.provider_info
                            .query_params
                            .as_ref()
                            .and_then(|query_params| query_params.get("provider"))
                            .is_none_or(|provider_id| {
                                model
                                    .owned_by
                                    .as_deref()
                                    .is_none_or(|owned_by| owned_by == provider_id)
                            })
                    })
                    .enumerate()
                    .map(|(priority, model)| {
                        let model_id = model.id.strip_prefix("models/").unwrap_or(&model.id);
                        let mut model_info = model_info_from_provider_catalog_slug(
                            model_id,
                            &self.provider_info.name,
                        );
                        model_info.priority = i32::try_from(priority).unwrap_or(i32::MAX);
                        model_info
                    })
                    .collect(),
            };
            Ok((models, etag))
        })
        .await
        .map_err(|_| XedocErr::Timeout)?
    }
}

impl ModelsEndpointClient for OpenAiModelsEndpoint {
    fn has_command_auth(&self) -> bool {
        self.provider_info.has_command_auth()
    }

    fn has_authoritative_remote_catalog(&self) -> bool {
        !self.provider_info.requires_openai_auth
    }

    fn uses_xedoc_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        Box::pin(OpenAiModelsEndpoint::uses_xedoc_backend(self))
    }

    fn list_models<'a>(
        &'a self,
        client_version: &'a str,
        http_client_factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<(Vec<ModelInfo>, Option<String>)>> {
        Box::pin(OpenAiModelsEndpoint::list_models(
            self,
            client_version,
            http_client_factory,
        ))
    }
}

type ModelsTransportFuture<'a> =
    Pin<Box<dyn Future<Output = std::io::Result<ReqwestTransport>> + Send + 'a>>;

/// Builds the concrete transport selected for one models request.
///
/// Implementations must honor the supplied request-time client factory and exact request URL.
trait ModelsTransportBuilder: fmt::Debug + Send + Sync {
    fn build(
        &self,
        http_client_factory: HttpClientFactory,
        request_url: String,
    ) -> ModelsTransportFuture<'_>;
}

#[derive(Debug)]
struct RouteAwareModelsTransportBuilder;

impl ModelsTransportBuilder for RouteAwareModelsTransportBuilder {
    fn build(
        &self,
        http_client_factory: HttpClientFactory,
        request_url: String,
    ) -> ModelsTransportFuture<'_> {
        Box::pin(build_models_transport(http_client_factory, request_url))
    }
}

pub(crate) async fn build_models_transport(
    http_client_factory: HttpClientFactory,
    request_url: String,
) -> std::io::Result<ReqwestTransport> {
    build_default_reqwest_client_for_route_async(
        http_client_factory,
        request_url,
        ClientRouteClass::Api,
    )
    .await
    .map(ReqwestTransport::new)
}

pub(crate) fn models_request_telemetry(
    provider_info: &ModelProviderInfo,
    auth_manager: Option<&Arc<AuthManager>>,
    auth: Option<&XedocAuth>,
    api_auth: &dyn xedoc_api::AuthProvider,
) -> Arc<dyn RequestTelemetry> {
    let auth_telemetry = auth_header_telemetry(api_auth);
    let agent_identity_telemetry = if let Some(XedocAuth::AgentIdentity(auth)) = auth {
        Some(agent_identity_telemetry(auth))
    } else {
        None
    };
    let xedoc_api_key_env_enabled =
        auth_manager.is_some_and(|auth_manager| auth_manager.xedoc_api_key_env_enabled());

    Arc::new(ModelsRequestTelemetry {
        auth_mode: auth
            .map(XedocAuth::auth_mode)
            .map(|mode| TelemetryAuthMode::from(mode).to_string()),
        auth_header_attached: auth_telemetry.attached,
        auth_header_name: auth_telemetry.name,
        agent_identity_telemetry,
        auth_env: collect_auth_env_telemetry(provider_info, xedoc_api_key_env_enabled),
    })
}

#[derive(Clone)]
struct ModelsRequestTelemetry {
    auth_mode: Option<String>,
    auth_header_attached: bool,
    auth_header_name: Option<&'static str>,
    agent_identity_telemetry: Option<AgentIdentityTelemetry>,
    auth_env: AuthEnvTelemetry,
}

impl RequestTelemetry for ModelsRequestTelemetry {
    fn on_request(
        &self,
        attempt: u64,
        status: Option<http::StatusCode>,
        error: Option<&TransportError>,
        duration: Duration,
    ) {
        let success = status.is_some_and(|code| code.is_success()) && error.is_none();
        let error_message = error.map(telemetry_transport_error_message);
        let response_debug = error
            .map(extract_response_debug_context)
            .unwrap_or_default();
        let status = status.map(|status| status.as_u16());
        tracing::event!(
            target: "xedoc_otel.log_only",
            tracing::Level::INFO,
            event.name = "xedoc.api_request",
            duration_ms = %duration.as_millis(),
            http.response.status_code = status,
            success = success,
            error.message = error_message.as_deref(),
            attempt = attempt,
            endpoint = MODELS_ENDPOINT,
            auth.header_attached = self.auth_header_attached,
            auth.header_name = self.auth_header_name,
            auth.env_openai_api_key_present = self.auth_env.openai_api_key_env_present,
            auth.env_xedoc_api_key_present = self.auth_env.xedoc_api_key_env_present,
            auth.env_xedoc_api_key_enabled = self.auth_env.xedoc_api_key_env_enabled,
            auth.env_provider_key_name = self.auth_env.provider_env_key_name.as_deref(),
            auth.env_provider_key_present = self.auth_env.provider_env_key_present,
            auth.env_refresh_token_url_override_present = self.auth_env.refresh_token_url_override_present,
            auth.request_id = response_debug.request_id.as_deref(),
            auth.cf_ray = response_debug.cf_ray.as_deref(),
            auth.error = response_debug.auth_error.as_deref(),
            auth.error_code = response_debug.auth_error_code.as_deref(),
            auth.mode = self.auth_mode.as_deref(),
            auth.agent_id = self.agent_identity_telemetry.as_ref().map(|metadata| metadata.agent_id.as_str()),
            auth.task_id = self.agent_identity_telemetry.as_ref().map(|metadata| metadata.task_id.as_str()),
        );
        tracing::event!(
            target: "xedoc_otel.trace_safe",
            tracing::Level::INFO,
            event.name = "xedoc.api_request",
            duration_ms = %duration.as_millis(),
            http.response.status_code = status,
            success = success,
            error.message = error_message.as_deref(),
            attempt = attempt,
            endpoint = MODELS_ENDPOINT,
            auth.header_attached = self.auth_header_attached,
            auth.header_name = self.auth_header_name,
            auth.env_openai_api_key_present = self.auth_env.openai_api_key_env_present,
            auth.env_xedoc_api_key_present = self.auth_env.xedoc_api_key_env_present,
            auth.env_xedoc_api_key_enabled = self.auth_env.xedoc_api_key_env_enabled,
            auth.env_provider_key_name = self.auth_env.provider_env_key_name.as_deref(),
            auth.env_provider_key_present = self.auth_env.provider_env_key_present,
            auth.env_refresh_token_url_override_present = self.auth_env.refresh_token_url_override_present,
            auth.request_id = response_debug.request_id.as_deref(),
            auth.cf_ray = response_debug.cf_ray.as_deref(),
            auth.error = response_debug.auth_error.as_deref(),
            auth.error_code = response_debug.auth_error_code.as_deref(),
            auth.mode = self.auth_mode.as_deref(),
            auth.agent_id = self.agent_identity_telemetry.as_ref().map(|metadata| metadata.agent_id.as_str()),
            auth.task_id = self.agent_identity_telemetry.as_ref().map(|metadata| metadata.task_id.as_str()),
        );
    }
}
