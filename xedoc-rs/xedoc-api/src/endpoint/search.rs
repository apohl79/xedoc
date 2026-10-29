use crate::auth::SharedAuthProvider;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::search::SearchRequest;
use crate::search::SearchResponse;
use http::HeaderMap;
use http::Method;
use serde_json::to_value;
use std::sync::Arc;
use xedoc_client::HttpTransport;
use xedoc_client::RequestTelemetry;

pub struct SearchClient<T: HttpTransport> {
    session: EndpointSession<T>,
}

impl<T: HttpTransport> SearchClient<T> {
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
        "alpha/search"
    }

    pub async fn search(
        &self,
        request: &SearchRequest,
        extra_headers: HeaderMap,
    ) -> Result<SearchResponse, ApiError> {
        let body = to_value(request)
            .map_err(|e| ApiError::Stream(format!("failed to encode search request: {e}")))?;
        let resp = self
            .session
            .execute(Method::POST, Self::path(), extra_headers, Some(body))
            .await?;
        serde_json::from_slice(&resp.body)
            .map_err(|e| ApiError::Stream(format!("failed to decode search response: {e}")))
    }
}
