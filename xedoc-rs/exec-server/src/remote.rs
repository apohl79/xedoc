use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use futures::FutureExt;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use reqwest::StatusCode;
use serde::Deserialize;
use tokio::time::sleep;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tracing::debug;
use tracing::info;
use tracing::warn;
use xedoc_api::AuthProvider;
use xedoc_api::SharedAuthProvider;

use xedoc_utils_rustls_provider::ensure_rustls_crypto_provider;

use crate::EnvironmentRegistryConnectRequest;
use crate::EnvironmentRegistryConnectResponse;
use crate::EnvironmentRegistryHarnessKeyValidationRequest;
use crate::EnvironmentRegistryHarnessKeyValidationResponse;
use crate::EnvironmentRegistryRegistrationRequest;
use crate::EnvironmentRegistryRegistrationResponse;
use crate::ExecServerError;
use crate::ExecServerRuntimePaths;
use crate::ExecServerTelemetry;
use crate::NoiseChannelIdentity;
use crate::NoiseChannelPublicKey;
use crate::NoiseRendezvousConnectBundle;
use crate::NoiseRendezvousConnectProvider;
use crate::client_api::DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT;
use crate::noise_relay::noise_relay_websocket_config;
use crate::relay::HarnessKeyValidator;
use crate::relay::run_multiplexed_environment;
use crate::server::ConnectionProcessor;
use crate::trace_context::current_trace_context_headers;

const ERROR_BODY_PREVIEW_BYTES: usize = 4096;
const NOISE_RELAY_SECURITY_PROFILE: &str = "noise_hybrid_ik_v1";

#[derive(Clone)]
struct EnvironmentRegistryClient {
    base_url: String,
    auth_provider: SharedAuthProvider,
    http: reqwest::Client,
    connect_timeout: Duration,
    telemetry: ExecServerTelemetry,
}

impl std::fmt::Debug for EnvironmentRegistryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentRegistryClient")
            .field("base_url", &self.base_url)
            .field("auth_provider", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl EnvironmentRegistryClient {
    #[cfg(test)]
    fn new(base_url: String, auth_provider: SharedAuthProvider) -> Result<Self, ExecServerError> {
        Self::new_with_telemetry(base_url, auth_provider, ExecServerTelemetry::default())
    }

    fn new_with_telemetry(
        base_url: String,
        auth_provider: SharedAuthProvider,
        telemetry: ExecServerTelemetry,
    ) -> Result<Self, ExecServerError> {
        let base_url = normalize_base_url(base_url)?;
        Ok(Self {
            base_url,
            auth_provider,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            connect_timeout: DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT,
            telemetry,
        })
    }

    /// Register the executor public key and obtain the rendezvous allocation.
    /// The returned registration ID is included in each stream's Noise prologue.
    #[tracing::instrument(
        name = "xedoc.exec_server.remote.register",
        skip_all,
        fields(
            otel.kind = "client",
            otel.name = "xedoc.exec_server.remote.register",
            result = tracing::field::Empty,
        )
    )]
    async fn register_environment(
        &self,
        environment_id: &str,
        executor_public_key: &NoiseChannelPublicKey,
    ) -> Result<EnvironmentRegistryRegistrationResponse, ExecServerError> {
        let started_at = Instant::now();
        let response = self
            .register_environment_inner(environment_id, executor_public_key)
            .await;
        let result = if response.is_ok() { "success" } else { "error" };
        tracing::Span::current().record("result", result);
        self.telemetry
            .remote_registration_completed(result, started_at.elapsed());
        response
    }

    async fn register_environment_inner(
        &self,
        environment_id: &str,
        executor_public_key: &NoiseChannelPublicKey,
    ) -> Result<EnvironmentRegistryRegistrationResponse, ExecServerError> {
        let response = self
            .http
            .post(endpoint_url(
                &self.base_url,
                &format!("/cloud/environment/{environment_id}/register"),
            ))
            .headers(self.auth_provider.to_auth_headers())
            .headers(current_trace_context_headers())
            .json(&EnvironmentRegistryRegistrationRequest {
                security_profile: NOISE_RELAY_SECURITY_PROFILE.to_string(),
                executor_public_key: executor_public_key.clone(),
            })
            .send()
            .await?;
        let response: EnvironmentRegistryRegistrationResponse =
            self.parse_json_response(response).await?;
        if response.environment_id != environment_id {
            return Err(ExecServerError::Protocol(
                "environment registry returned a different environment id".to_string(),
            ));
        }
        if response.security_profile != NOISE_RELAY_SECURITY_PROFILE {
            return Err(ExecServerError::Protocol(format!(
                "environment registry returned unsupported security profile `{}`",
                response.security_profile
            )));
        }
        info!(
            noise_event = "registration",
            noise_outcome = "ok",
            security_profile = NOISE_RELAY_SECURITY_PROFILE,
            "Noise executor registration completed"
        );
        debug!(
            environment_id = response.environment_id,
            executor_registration_id = response.executor_registration_id,
            "Noise executor registration details"
        );
        Ok(response)
    }

    /// Authorize one Noise harness key and obtain the full rendezvous bundle.
    async fn connect_environment(
        &self,
        environment_id: &str,
        harness_public_key: NoiseChannelPublicKey,
    ) -> Result<NoiseRendezvousConnectBundle, ExecServerError> {
        let response = self
            .http
            .post(endpoint_url(
                &self.base_url,
                &format!("/cloud/environment/{environment_id}/connect"),
            ))
            .headers(self.auth_provider.to_auth_headers())
            .json(&EnvironmentRegistryConnectRequest { harness_public_key })
            .timeout(self.connect_timeout)
            .send()
            .await?;
        let response: EnvironmentRegistryConnectResponse =
            self.parse_json_response(response).await?;
        if response.environment_id != environment_id {
            return Err(ExecServerError::Protocol(
                "environment registry returned a different environment id".to_string(),
            ));
        }
        if response.security_profile != NOISE_RELAY_SECURITY_PROFILE {
            return Err(ExecServerError::Protocol(format!(
                "environment registry returned unsupported security profile `{}`",
                response.security_profile
            )));
        }
        if response.url.trim().is_empty()
            || response.executor_registration_id.trim().is_empty()
            || response.harness_key_authorization.trim().is_empty()
        {
            return Err(ExecServerError::Protocol(
                "environment registry returned incomplete Noise connection data".to_string(),
            ));
        }
        Ok(NoiseRendezvousConnectBundle {
            websocket_url: response.url,
            environment_id: response.environment_id,
            executor_registration_id: response.executor_registration_id,
            executor_public_key: response.executor_public_key,
            harness_key_authorization: response.harness_key_authorization,
        })
    }

    async fn parse_json_response<R>(
        &self,
        response: reqwest::Response,
    ) -> Result<R, ExecServerError>
    where
        R: for<'de> Deserialize<'de>,
    {
        if response.status().is_success() {
            return response.json::<R>().await.map_err(ExecServerError::from);
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(environment_registry_auth_error(status, &body));
        }

        Err(environment_registry_http_error(status, &body))
    }
}

#[derive(Clone)]
struct RegistryHarnessKeyValidator {
    client: EnvironmentRegistryClient,
    environment_id: String,
    executor_registration_id: String,
}

impl HarnessKeyValidator for RegistryHarnessKeyValidator {
    /// Authorize the harness key recovered from the first IK message.
    /// Noise proves key possession; the registry decides whether that key may use
    /// this executor. The authorization token and public key are checked together.
    async fn validate_harness_key(
        &self,
        harness_public_key: &NoiseChannelPublicKey,
        authorization: &str,
    ) -> Result<(), ExecServerError> {
        let environment_id = &self.environment_id;
        let response = self
            .client
            .http
            .post(endpoint_url(
                &self.client.base_url,
                &format!("/cloud/environment/{environment_id}/validate"),
            ))
            .headers(self.client.auth_provider.to_auth_headers())
            .json(&EnvironmentRegistryHarnessKeyValidationRequest {
                executor_registration_id: self.executor_registration_id.clone(),
                harness_public_key: harness_public_key.clone(),
                harness_key_authorization: authorization.to_string(),
            })
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            // The request contains the short-lived authorization. Do not include
            // a response body that might echo it in logs or error chains.
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                return Err(ExecServerError::EnvironmentRegistryAuth(format!(
                    "environment registry harness key validation authentication failed ({status})"
                )));
            }
            return Err(ExecServerError::EnvironmentRegistryHttp {
                status,
                code: None,
                message: "environment registry harness key validation failed".to_string(),
            });
        }
        let response = response
            .json::<EnvironmentRegistryHarnessKeyValidationResponse>()
            .await?;
        if !response.valid {
            return Err(ExecServerError::Protocol(
                "environment registry rejected Noise relay harness key".to_string(),
            ));
        }
        Ok(())
    }
}

/// Noise connection configuration for a Xedoc harness.
///
/// The provider holds the authenticated registry client so every reconnect
/// receives fresh URL and harness-key authorization material.
#[derive(Clone)]
pub(crate) struct NoiseRendezvousEnvironmentConfig {
    provider: Arc<dyn NoiseRendezvousConnectProvider>,
}

impl std::fmt::Debug for NoiseRendezvousEnvironmentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseRendezvousEnvironmentConfig")
            .field("provider", &"<redacted>")
            .finish()
    }
}

impl NoiseRendezvousEnvironmentConfig {
    pub(crate) fn new(
        base_url: String,
        environment_id: String,
        bearer_token: String,
        chatgpt_account_id: Option<String>,
    ) -> Result<Self, ExecServerError> {
        let environment_id = normalize_environment_id(environment_id)?;
        let auth_provider = static_bearer_auth_provider(bearer_token, chatgpt_account_id)?;
        let client = EnvironmentRegistryClient::new_with_telemetry(
            base_url,
            auth_provider,
            ExecServerTelemetry::default(),
        )?;
        Ok(Self {
            provider: Arc::new(EnvironmentRegistryNoiseConnectProvider {
                client,
                environment_id,
            }),
        })
    }

    pub(crate) fn connect_provider(&self) -> Arc<dyn NoiseRendezvousConnectProvider> {
        Arc::clone(&self.provider)
    }
}

#[derive(Clone, Debug)]
struct EnvironmentRegistryNoiseConnectProvider {
    client: EnvironmentRegistryClient,
    environment_id: String,
}

impl NoiseRendezvousConnectProvider for EnvironmentRegistryNoiseConnectProvider {
    fn connect_bundle(
        &self,
        harness_public_key: NoiseChannelPublicKey,
    ) -> futures::future::BoxFuture<'_, Result<NoiseRendezvousConnectBundle, ExecServerError>> {
        async move {
            self.client
                .connect_environment(&self.environment_id, harness_public_key)
                .await
        }
        .boxed()
    }
}

#[derive(Clone)]
struct StaticBearerAuthProvider {
    authorization: HeaderValue,
    chatgpt_account_id: Option<HeaderValue>,
}

impl std::fmt::Debug for StaticBearerAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticBearerAuthProvider")
            .field("authorization", &"<redacted>")
            .field(
                "chatgpt_account_id",
                &self.chatgpt_account_id.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl AuthProvider for StaticBearerAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        headers.insert(http::header::AUTHORIZATION, self.authorization.clone());
        if let Some(chatgpt_account_id) = &self.chatgpt_account_id {
            headers.insert(
                HeaderName::from_static("chatgpt-account-id"),
                chatgpt_account_id.clone(),
            );
        }
    }
}

fn static_bearer_auth_provider(
    bearer_token: String,
    chatgpt_account_id: Option<String>,
) -> Result<SharedAuthProvider, ExecServerError> {
    let bearer_token = bearer_token.trim();
    if bearer_token.is_empty() {
        return Err(ExecServerError::EnvironmentRegistryConfig(
            "environment registry bearer token is required".to_string(),
        ));
    }
    let authorization =
        HeaderValue::try_from(format!("Bearer {bearer_token}")).map_err(|error| {
            ExecServerError::EnvironmentRegistryConfig(format!(
                "environment registry bearer token is not a valid HTTP header: {error}"
            ))
        })?;
    let chatgpt_account_id = chatgpt_account_id
        .as_deref()
        .map(str::trim)
        .filter(|account_id| !account_id.is_empty())
        .map(|account_id| {
            HeaderValue::try_from(account_id).map_err(|error| {
                ExecServerError::EnvironmentRegistryConfig(format!(
                    "ChatGPT account id is not a valid HTTP header: {error}"
                ))
            })
        })
        .transpose()?;
    Ok(Arc::new(StaticBearerAuthProvider {
        authorization,
        chatgpt_account_id,
    }))
}

/// Configuration for registering an exec-server for remote use.
#[derive(Clone)]
pub struct RemoteEnvironmentConfig {
    pub base_url: String,
    pub environment_id: String,
    pub name: String,
    auth_provider: SharedAuthProvider,
    telemetry: ExecServerTelemetry,
}

impl std::fmt::Debug for RemoteEnvironmentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteEnvironmentConfig")
            .field("base_url", &self.base_url)
            .field("environment_id", &self.environment_id)
            .field("name", &self.name)
            .field("auth_provider", &"<redacted>")
            .finish()
    }
}

impl RemoteEnvironmentConfig {
    pub fn new(
        base_url: String,
        environment_id: String,
        auth_provider: SharedAuthProvider,
    ) -> Result<Self, ExecServerError> {
        let environment_id = normalize_environment_id(environment_id)?;
        Ok(Self {
            base_url,
            environment_id,
            name: "xedoc-exec-server".to_string(),
            auth_provider,
            telemetry: ExecServerTelemetry::default(),
        })
    }

    pub fn with_telemetry(mut self, telemetry: ExecServerTelemetry) -> Self {
        self.telemetry = telemetry;
        self
    }
}

/// Register an exec-server for remote use and serve requests over Noise.
///
/// The executor identity is generated once per process and reused across
/// reconnects. The registration and rendezvous URL are also reused until
/// rendezvous rejects the URL, at which point the next attempt registers again.
/// The websocket carries cleartext routing metadata and encrypted payloads.
pub async fn run_remote_environment(
    config: RemoteEnvironmentConfig,
    runtime_paths: ExecServerRuntimePaths,
) -> Result<(), ExecServerError> {
    ensure_rustls_crypto_provider();
    let client = EnvironmentRegistryClient::new_with_telemetry(
        config.base_url.clone(),
        config.auth_provider.clone(),
        config.telemetry.clone(),
    )?;
    let processor =
        ConnectionProcessor::new_with_telemetry(runtime_paths, config.telemetry.clone());
    let identity = NoiseChannelIdentity::generate().map_err(|error| {
        ExecServerError::Protocol(format!("failed to generate Noise relay identity: {error}"))
    })?;
    let mut backoff = Duration::from_secs(1);
    let mut response = client
        .register_environment(&config.environment_id, &identity.public_key())
        .await?;

    loop {
        match connect_rendezvous(&response.url, &config.telemetry).await {
            Ok(websocket) => {
                backoff = Duration::from_secs(1);
                let executor_registration_id = response.executor_registration_id.clone();
                info!(
                    noise_event = "rendezvous_connection",
                    noise_outcome = "ok",
                    "Noise executor connected to rendezvous"
                );
                let disconnect_reason = run_multiplexed_environment(
                    websocket,
                    processor.clone(),
                    response.environment_id.clone(),
                    executor_registration_id.clone(),
                    identity.clone(),
                    RegistryHarnessKeyValidator {
                        client: client.clone(),
                        environment_id: config.environment_id.clone(),
                        executor_registration_id,
                    },
                )
                .await;
                info!(
                    noise_event = "rendezvous_connection",
                    noise_outcome = "disconnected",
                    noise_reason = disconnect_reason.as_str(),
                    "Noise executor disconnected from rendezvous"
                );
                config
                    .telemetry
                    .remote_reconnect(disconnect_reason.as_str());
            }
            Err(error) => {
                let registration_rejected = matches!(
                    &error,
                    tokio_tungstenite::tungstenite::Error::Http(response)
                        if response.status().is_client_error()
                );
                warn!(
                    noise_event = "rendezvous_connection",
                    noise_outcome = "error",
                    noise_reason = "websocket_error",
                    "Noise executor failed to connect to rendezvous"
                );
                debug!(error = %error, "Noise executor rendezvous connection error");
                if registration_rejected {
                    config.telemetry.remote_reconnect("registration_rejected");
                    response = client
                        .register_environment(&config.environment_id, &identity.public_key())
                        .await?;
                } else {
                    config.telemetry.remote_reconnect("connect_failed");
                }
            }
        }

        sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

#[tracing::instrument(
    name = "xedoc.exec_server.remote.rendezvous.connect",
    skip_all,
    fields(
        otel.kind = "client",
        otel.name = "xedoc.exec_server.remote.rendezvous.connect",
        result = tracing::field::Empty,
    )
)]
async fn connect_rendezvous(
    url: &str,
    telemetry: &ExecServerTelemetry,
) -> Result<
    WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    let started_at = Instant::now();
    let result = async {
        let mut request = url.into_client_request()?;
        request
            .headers_mut()
            .extend(current_trace_context_headers());
        connect_async_with_config(
            request,
            Some(noise_relay_websocket_config()),
            // Rendezvous sends small, latency-sensitive frames, so avoid Nagle's coalescing delay.
            /*disable_nagle*/
            true,
        )
        .await
        .map(|(websocket, _)| websocket)
    }
    .await;
    let result_name = if result.is_ok() { "success" } else { "error" };
    tracing::Span::current().record("result", result_name);
    telemetry.remote_rendezvous_completed(result_name, started_at.elapsed());
    result
}

fn normalize_environment_id(environment_id: String) -> Result<String, ExecServerError> {
    let environment_id = environment_id.trim().to_string();
    if environment_id.is_empty() {
        return Err(ExecServerError::EnvironmentRegistryConfig(
            "environment id is required for remote exec-server registration".to_string(),
        ));
    }
    Ok(environment_id)
}

#[derive(Deserialize)]
struct RegistryErrorBody {
    error: Option<RegistryError>,
}

#[derive(Deserialize)]
struct RegistryError {
    code: Option<String>,
    message: Option<String>,
}

fn normalize_base_url(base_url: String) -> Result<String, ExecServerError> {
    let trimmed = base_url.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(ExecServerError::EnvironmentRegistryConfig(
            "environment registry base URL is required".to_string(),
        ));
    }
    Ok(trimmed)
}

fn endpoint_url(base_url: &str, path: &str) -> String {
    format!("{base_url}/{}", path.trim_start_matches('/'))
}

fn environment_registry_auth_error(status: StatusCode, body: &str) -> ExecServerError {
    let message = registry_error_message(body).unwrap_or_else(|| "empty error body".to_string());
    ExecServerError::EnvironmentRegistryAuth(format!(
        "environment registry authentication failed ({status}): {message}"
    ))
}

fn environment_registry_http_error(status: StatusCode, body: &str) -> ExecServerError {
    let parsed = serde_json::from_str::<RegistryErrorBody>(body).ok();
    let (code, message) = parsed
        .and_then(|body| body.error)
        .map(|error| {
            (
                error.code,
                error.message.unwrap_or_else(|| {
                    preview_error_body(body).unwrap_or_else(|| "empty error body".to_string())
                }),
            )
        })
        .unwrap_or_else(|| {
            (
                None,
                preview_error_body(body)
                    .unwrap_or_else(|| "empty or malformed error body".to_string()),
            )
        });
    ExecServerError::EnvironmentRegistryHttp {
        status,
        code,
        message,
    }
}

fn registry_error_message(body: &str) -> Option<String> {
    serde_json::from_str::<RegistryErrorBody>(body)
        .ok()
        .and_then(|body| body.error)
        .and_then(|error| error.message)
        .or_else(|| preview_error_body(body))
}

fn preview_error_body(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(ERROR_BODY_PREVIEW_BYTES).collect())
}

#[cfg(test)]
#[path = "remote/noise_tests.rs"]
mod noise_tests;
