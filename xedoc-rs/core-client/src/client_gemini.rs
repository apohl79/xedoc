use std::sync::Arc;

use tracing::instrument;
use xedoc_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use xedoc_protocol::error::Result;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use xedoc_provider_gemini::GeminiClient;

use super::AuthRequestTelemetryContext;
use super::ModelClientSession;
use super::PendingUnauthorizedRetry;
use super::Prompt;
use super::RequestRouteTelemetry;
use super::XedocResponsesMetadata;
use super::map_response_stream;
use super::session_telemetry_for_request;
use xedoc_otel::SessionTelemetry;

const GEMINI_MODELS_ENDPOINT: &str = "/models";

impl ModelClientSession {
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_gemini_api",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = %self.client.state.provider.info().wire_api,
            transport = "gemini_http",
            http.method = "POST",
            api.path = "models/*:streamGenerateContent"
        )
    )]
    pub(super) async fn stream_gemini_api(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<String>,
        responses_metadata: &XedocResponsesMetadata,
    ) -> Result<super::ResponseStream> {
        let client_setup = self.client.current_client_setup().await?;
        let transport = self
            .client
            .build_api_transport(&client_setup.api_provider, GEMINI_MODELS_ENDPOINT)?;
        let request_auth_context = AuthRequestTelemetryContext::new(
            client_setup.api_auth.as_ref(),
            client_setup.agent_identity_telemetry,
            PendingUnauthorizedRetry::default(),
        );
        let (request_telemetry, sse_telemetry) = Self::build_streaming_telemetry(
            session_telemetry,
            request_auth_context,
            RequestRouteTelemetry::for_endpoint(GEMINI_MODELS_ENDPOINT),
        );
        let mut request = self.client.build_responses_request(
            &client_setup.api_provider,
            prompt,
            model_info,
            effort,
            summary,
            service_tier,
            responses_metadata,
        )?;
        let store = request.store;
        self.client
            .prepare_response_items_for_request(&mut request.input, store);
        let request_session_telemetry = session_telemetry_for_request(session_telemetry, &request);
        let client = GeminiClient::new(
            transport,
            client_setup.api_provider,
            client_setup.api_auth,
            Arc::clone(&self.client.state.gemini_thought_signatures),
        )
        .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
        let stream = client
            .stream_request(request)
            .await
            .map_err(|error| self.client.state.provider.map_api_error(error))?;
        let (stream, _) = map_response_stream(
            stream,
            request_session_telemetry,
            Arc::clone(&self.client.state.provider),
        );
        Ok(stream)
    }
}

#[cfg(test)]
#[path = "client_gemini_tests.rs"]
mod tests;
