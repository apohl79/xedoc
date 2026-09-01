use std::sync::Arc;

use reqwest::StatusCode;
use tracing::instrument;
use xedoc_api::ApiError;
use xedoc_api::TransportError;
use xedoc_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use xedoc_protocol::error::Result;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use xedoc_provider_anthropic::AnthropicClient;

use super::AuthRequestTelemetryContext;
use super::ModelClientSession;
use super::PendingUnauthorizedRetry;
use super::Prompt;
use super::RequestRouteTelemetry;
use super::XedocResponsesMetadata;
use super::handle_unauthorized;
use super::map_response_stream;
use super::session_telemetry_for_request;
use xedoc_otel::SessionTelemetry;

const ANTHROPIC_MESSAGES_ENDPOINT: &str = "/messages";

impl ModelClientSession {
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_anthropic_api",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = %self.client.state.provider.info().wire_api,
            transport = "anthropic_http",
            http.method = "POST",
            api.path = "messages"
        )
    )]
    pub(super) async fn stream_anthropic_api(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<String>,
        responses_metadata: &XedocResponsesMetadata,
    ) -> Result<super::ResponseStream> {
        let auth_manager = self.client.state.provider.auth_manager();
        let mut auth_recovery = auth_manager
            .as_ref()
            .map(xedoc_login::AuthManager::unauthorized_recovery);
        let mut pending_retry = PendingUnauthorizedRetry::default();
        loop {
            let client_setup = self.client.current_client_setup().await?;
            let transport = self
                .client
                .build_api_transport(&client_setup.api_provider, ANTHROPIC_MESSAGES_ENDPOINT)?;
            let request_auth_context = AuthRequestTelemetryContext::new(
                client_setup.api_auth.as_ref(),
                client_setup.agent_identity_telemetry.clone(),
                pending_retry,
            );
            let (request_telemetry, sse_telemetry) = Self::build_streaming_telemetry(
                session_telemetry,
                request_auth_context,
                RequestRouteTelemetry::for_endpoint(ANTHROPIC_MESSAGES_ENDPOINT),
            );
            let mut request = self.client.build_responses_request(
                &client_setup.api_provider,
                prompt,
                model_info,
                effort.clone(),
                summary,
                service_tier.clone(),
                responses_metadata,
            )?;
            let store = request.store;
            self.client
                .prepare_response_items_for_request(&mut request.input, store);
            let request_session_telemetry =
                session_telemetry_for_request(session_telemetry, &request);
            let client =
                AnthropicClient::new(transport, client_setup.api_provider, client_setup.api_auth)
                    .with_telemetry(Some(request_telemetry), Some(sse_telemetry));

            match client.stream_request(request).await {
                Ok(stream) => {
                    let (stream, _) = map_response_stream(
                        stream,
                        request_session_telemetry,
                        Arc::clone(&self.client.state.provider),
                    );
                    return Ok(stream);
                }
                Err(ApiError::Transport(
                    unauthorized_transport @ TransportError::Http { status, .. },
                )) if status == StatusCode::UNAUTHORIZED => {
                    pending_retry = PendingUnauthorizedRetry::from_recovery(
                        handle_unauthorized(
                            unauthorized_transport,
                            &mut auth_recovery,
                            session_telemetry,
                            &self.client.state.provider,
                        )
                        .await?,
                    );
                }
                Err(error) => {
                    return Err(self.client.state.provider.map_api_error(error));
                }
            }
        }
    }
}
