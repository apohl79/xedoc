use std::collections::HashSet;
use std::sync::Arc;

use reqwest::StatusCode;
use tracing::instrument;
use xedoc_api::ApiError;
use xedoc_api::AuthRecoveryAction;
use xedoc_api::AuthRefreshPolicy;
use xedoc_api::RouteAwareAuthHttpTransport;
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
const MANAGED_AUTH_MAX_ATTEMPTS: usize = 3;

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
        let mut managed_attempts = 0;
        let mut refreshed_credentials = HashSet::new();
        let auth_transport =
            RouteAwareAuthHttpTransport::new(self.client.http_client_factory.clone());
        loop {
            let client_setup = self.client.current_client_setup().await?;
            let recovery_identity = client_setup.api_auth.recovery_identity();
            if recovery_identity.is_some() {
                managed_attempts += 1;
            }
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
            let recovery_auth = Arc::clone(&client_setup.api_auth);
            let client =
                AnthropicClient::new(transport, client_setup.api_provider, client_setup.api_auth)
                    .with_telemetry(Some(request_telemetry), Some(sse_telemetry));

            match client.stream_request(request).await {
                Ok(stream) => {
                    recovery_auth.record_success();
                    let (stream, _) = map_response_stream(
                        stream,
                        request_session_telemetry,
                        Arc::clone(&self.client.state.provider),
                        self.client.provider_provenance(model_info),
                    );
                    return Ok(stream);
                }
                Err(ApiError::Transport(transport_error)) => {
                    if let Some(identity) = recovery_identity {
                        let retry_budget_exhausted = managed_attempts >= MANAGED_AUTH_MAX_ATTEMPTS;
                        let refresh_policy = if retry_budget_exhausted
                            || refreshed_credentials.contains(&identity)
                        {
                            AuthRefreshPolicy::AlreadyAttempted
                        } else {
                            AuthRefreshPolicy::Allowed
                        };
                        let action = recovery_auth
                            .recover_from_error(&transport_error, &auth_transport, refresh_policy)
                            .await
                            .map_err(|error| {
                                self.client
                                    .state
                                    .provider
                                    .map_api_error(ApiError::Transport(error.into()))
                            })?;
                        if retry_budget_exhausted {
                            return Err(self
                                .client
                                .state
                                .provider
                                .map_api_error(ApiError::Transport(transport_error)));
                        }
                        match action {
                            AuthRecoveryAction::RetryAfterRefresh => {
                                let _ = refreshed_credentials.insert(identity);
                            }
                            AuthRecoveryAction::RetryWithNextCredential => {}
                            AuthRecoveryAction::Propagate => {
                                return Err(self
                                    .client
                                    .state
                                    .provider
                                    .map_api_error(ApiError::Transport(transport_error)));
                            }
                        }
                    } else if matches!(
                        &transport_error,
                        TransportError::Http { status, .. } if *status == StatusCode::UNAUTHORIZED
                    ) {
                        pending_retry = PendingUnauthorizedRetry::from_recovery(
                            handle_unauthorized(
                                transport_error,
                                &mut auth_recovery,
                                session_telemetry,
                                &self.client.state.provider,
                            )
                            .await?,
                        );
                    } else {
                        return Err(self
                            .client
                            .state
                            .provider
                            .map_api_error(ApiError::Transport(transport_error)));
                    }
                }
                Err(error) => {
                    return Err(self.client.state.provider.map_api_error(error));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "client_anthropic_tests.rs"]
mod tests;
