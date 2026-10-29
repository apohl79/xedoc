use crate::auth::SharedAuthProvider;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::images::ImageEditRequest;
use crate::images::ImageGenerationRequest;
use crate::images::ImageResponse;
use crate::provider::Provider;
use http::HeaderMap;
use http::Method;
use serde::Serialize;
use serde_json::to_value;
use std::sync::Arc;
use xedoc_client::HttpTransport;
use xedoc_client::RequestTelemetry;

pub struct ImagesClient<T: HttpTransport> {
    session: EndpointSession<T>,
}

impl<T: HttpTransport> ImagesClient<T> {
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

    pub async fn generate(
        &self,
        request: &ImageGenerationRequest,
        extra_headers: HeaderMap,
    ) -> Result<ImageResponse, ApiError> {
        self.post_image_request(
            "images/generations",
            request,
            extra_headers,
            "image generation",
        )
        .await
    }

    pub async fn edit(
        &self,
        request: &ImageEditRequest,
        extra_headers: HeaderMap,
    ) -> Result<ImageResponse, ApiError> {
        self.post_image_request("images/edits", request, extra_headers, "image edit")
            .await
    }

    async fn post_image_request<R: Serialize>(
        &self,
        path: &str,
        request: &R,
        extra_headers: HeaderMap,
        operation: &str,
    ) -> Result<ImageResponse, ApiError> {
        let body = to_value(request)
            .map_err(|e| ApiError::Stream(format!("failed to encode {operation} request: {e}")))?;
        let resp = self
            .session
            .execute(Method::POST, path, extra_headers, Some(body))
            .await?;
        serde_json::from_slice(&resp.body)
            .map_err(|e| ApiError::Stream(format!("failed to decode {operation} response: {e}")))
    }
}
