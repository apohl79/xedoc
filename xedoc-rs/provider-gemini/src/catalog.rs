use http::Method;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use xedoc_api::ApiError;
use xedoc_api::Provider;
use xedoc_api::SharedAuthProvider;
use xedoc_client::HttpTransport;
use xedoc_client::RequestTelemetry;
use xedoc_client::Response;
use xedoc_client::TransportError;
use xedoc_client::run_with_retry;

const CATALOG_PATH: &str = "models";
const CATALOG_PAGE_SIZE: &str = "1000";
const MAX_CATALOG_PAGES: usize = 100;

/// One Gemini model advertised by Google's native model catalog.
#[derive(Debug, PartialEq, Eq)]
pub struct GeminiCatalogModel {
    /// Model identifier without Google's `models/` resource prefix.
    pub slug: String,
    /// User-facing name supplied by Google.
    pub display_name: String,
    /// User-facing model description supplied by Google.
    pub description: Option<String>,
    /// Maximum number of input tokens accepted by the model.
    pub input_token_limit: Option<i64>,
}

/// Fetches Google's native, paginated Gemini model catalog.
pub struct GeminiCatalogClient<T: HttpTransport> {
    transport: T,
    provider: Provider,
    auth: SharedAuthProvider,
    request_telemetry: Option<Arc<dyn RequestTelemetry>>,
}

impl<T: HttpTransport> GeminiCatalogClient<T> {
    /// Creates a client for one configured Google API endpoint.
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            transport,
            provider,
            auth,
            request_telemetry: None,
        }
    }

    /// Attaches request telemetry to catalog calls.
    pub fn with_telemetry(mut self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        self.request_telemetry = request;
        self
    }

    /// Lists native Gemini models that support `generateContent`.
    ///
    /// # Errors
    ///
    /// Returns an API error when transport, authentication, decoding, or pagination fails.
    pub async fn list_models(&self) -> Result<Vec<GeminiCatalogModel>, ApiError> {
        let mut models = Vec::new();
        let mut next_page_token = None;
        let mut seen_page_tokens = HashSet::new();

        for _ in 0..MAX_CATALOG_PAGES {
            let page = self.fetch_page(next_page_token.as_deref()).await?;
            models.extend(
                page.models
                    .into_iter()
                    .filter_map(GeminiModelResource::catalog_model),
            );

            let Some(page_token) = page
                .next_page_token
                .filter(|page_token| !page_token.is_empty())
            else {
                return Ok(models);
            };
            if !seen_page_tokens.insert(page_token.clone()) {
                return Err(ApiError::Stream(
                    "Gemini model catalog repeated a page token".to_string(),
                ));
            }
            next_page_token = Some(page_token);
        }

        Err(ApiError::Stream(format!(
            "Gemini model catalog exceeded {MAX_CATALOG_PAGES} pages"
        )))
    }

    async fn fetch_page(&self, page_token: Option<&str>) -> Result<GeminiCatalogPage, ApiError> {
        let mut request = self.provider.build_request(Method::GET, CATALOG_PATH);
        append_query_param(&mut request.url, "pageSize", CATALOG_PAGE_SIZE);
        if let Some(page_token) = page_token {
            append_query_param(&mut request.url, "pageToken", page_token);
        }
        let response = run_with_retry(
            self.provider.retry.to_policy(),
            || request.clone(),
            |request, attempt| {
                let auth = Arc::clone(&self.auth);
                let telemetry = self.request_telemetry.clone();
                let transport = &self.transport;
                async move {
                    let start = Instant::now();
                    let result = match auth.apply_auth(request).await {
                        Ok(request) => transport.execute(request).await,
                        Err(error) => Err(TransportError::from(error)),
                    };
                    if let Some(telemetry) = telemetry {
                        let (status, error) = match &result {
                            Ok(response) => (Some(response.status), None),
                            Err(error) => (transport_error_status(error), Some(error)),
                        };
                        telemetry.on_request(attempt, status, error, start.elapsed());
                    }
                    result
                }
            },
        )
        .await?;

        decode_page(response)
    }
}

fn append_query_param(url: &mut String, name: &str, value: &str) {
    url.push(if url.contains('?') { '&' } else { '?' });
    url.push_str(name);
    url.push('=');
    url.push_str(&urlencoding::encode(value));
}

fn decode_page(response: Response) -> Result<GeminiCatalogPage, ApiError> {
    serde_json::from_slice(&response.body).map_err(|error| {
        ApiError::Stream(format!("failed to decode Gemini models response: {error}"))
    })
}

fn transport_error_status(error: &TransportError) -> Option<http::StatusCode> {
    match error {
        TransportError::Http { status, .. } => Some(*status),
        TransportError::RetryLimit
        | TransportError::Timeout
        | TransportError::Network(_)
        | TransportError::Build(_) => None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCatalogPage {
    #[serde(default)]
    models: Vec<GeminiModelResource>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiModelResource {
    name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    input_token_limit: Option<i64>,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}

impl GeminiModelResource {
    fn catalog_model(self) -> Option<GeminiCatalogModel> {
        let slug = self.name.strip_prefix("models/gemini-")?;
        if !self
            .supported_generation_methods
            .iter()
            .any(|method| method == "generateContent")
        {
            return None;
        }
        let slug = format!("gemini-{slug}");
        let display_name = if self.display_name.is_empty() {
            slug.clone()
        } else {
            self.display_name
        };

        Some(GeminiCatalogModel {
            slug,
            display_name,
            description: self
                .description
                .filter(|description| !description.is_empty()),
            input_token_limit: self.input_token_limit.filter(|limit| *limit > 0),
        })
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
