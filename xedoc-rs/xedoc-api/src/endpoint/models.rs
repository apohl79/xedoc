use crate::auth::SharedAuthProvider;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use http::HeaderMap;
use http::Method;
use http::header::ETAG;
use serde::Deserialize;
use std::sync::Arc;
use xedoc_client::HttpTransport;
use xedoc_client::RequestTelemetry;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ModelsResponse;

/// A model catalog returned by a Xedoc or OpenAI-compatible endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelCatalog {
    /// Complete metadata returned by the Xedoc `/models` endpoint.
    Xedoc(Vec<ModelInfo>),
    /// Model IDs returned by a standard OpenAI-compatible `/models` endpoint.
    OpenAiCompatible(Vec<OpenAiCompatibleModel>),
}

/// A model advertised by a standard OpenAI-compatible `/models` endpoint.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OpenAiCompatibleModel {
    /// Stable model identifier.
    pub id: String,
    /// Provider identifier reported by the catalog, when available.
    #[serde(default)]
    pub owned_by: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiCompatibleModelsResponse {
    data: Vec<OpenAiCompatibleModel>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ModelsResponseBody {
    Xedoc(ModelsResponse),
    OpenAiCompatible(OpenAiCompatibleModelsResponse),
}

pub struct ModelsClient<T: HttpTransport> {
    session: EndpointSession<T>,
}

impl<T: HttpTransport> ModelsClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
        }
    }

    fn path() -> &'static str {
        "models"
    }

    fn append_client_version_query(req: &mut xedoc_client::Request, client_version: &str) {
        let separator = if req.url.contains('?') { '&' } else { '?' };
        req.url = format!("{}{}client_version={client_version}", req.url, separator);
    }

    pub fn request_url(provider: &Provider, client_version: &str) -> String {
        let mut request = provider.build_request(Method::GET, Self::path());
        Self::append_client_version_query(&mut request, client_version);
        request.url
    }

    pub async fn list_models(
        &self,
        request_url: String,
        extra_headers: HeaderMap,
    ) -> Result<(ModelCatalog, Option<String>), ApiError> {
        let resp = self
            .session
            .execute_with(
                Method::GET,
                Self::path(),
                extra_headers,
                /*body*/ None,
                move |req| {
                    req.url.clone_from(&request_url);
                },
            )
            .await?;

        let header_etag = resp
            .headers
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);

        let catalog = serde_json::from_slice::<ModelsResponseBody>(&resp.body)
            .map(|response| match response {
                ModelsResponseBody::Xedoc(ModelsResponse { models }) => ModelCatalog::Xedoc(models),
                ModelsResponseBody::OpenAiCompatible(OpenAiCompatibleModelsResponse { data }) => {
                    ModelCatalog::OpenAiCompatible(data)
                }
            })
            .map_err(|error| {
                ApiError::Stream(format!("failed to decode models response: {error}"))
            })?;

        Ok((catalog, header_etag))
    }
}
