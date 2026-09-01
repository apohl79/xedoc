use eventsource_stream::Eventsource;
use futures::StreamExt;
use http::HeaderValue;
use http::Method;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::timeout;
use xedoc_api::ApiError;
use xedoc_api::Provider;
use xedoc_api::ResponseEvent;
use xedoc_api::ResponseStream;
use xedoc_api::ResponsesApiRequest;
use xedoc_api::SharedAuthProvider;
use xedoc_api::SseTelemetry;
use xedoc_client::EncodedJsonBody;
use xedoc_client::HttpTransport;
use xedoc_client::RequestBody;
use xedoc_client::RequestTelemetry;
use xedoc_client::StreamResponse;
use xedoc_client::TransportError;
use xedoc_client::run_with_retry;

use crate::AnthropicStreamTranslator;
use crate::AnthropicThinking;
use crate::translate_request;

const MESSAGES_ENDPOINT: &str = "messages";
const REQUEST_ID_HEADER: &str = "request-id";
const X_REQUEST_ID_HEADER: &str = "x-request-id";
const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
const RESPONSE_STREAM_CHANNEL_CAPACITY: usize = 1600;

pub struct AnthropicClient<T: HttpTransport> {
    transport: T,
    provider: Provider,
    auth: SharedAuthProvider,
    request_telemetry: Option<Arc<dyn RequestTelemetry>>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
}

impl<T: HttpTransport> AnthropicClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            transport,
            provider,
            auth,
            request_telemetry: None,
            sse_telemetry: None,
        }
    }

    pub fn with_telemetry(
        mut self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        self.request_telemetry = request;
        self.sse_telemetry = sse;
        self
    }

    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
    ) -> Result<ResponseStream, ApiError> {
        let translated = translate_request(&request)
            .map_err(|error| ApiError::Stream(format!("failed to translate request: {error}")))?;
        let body = EncodedJsonBody::encode(&translated)
            .map_err(|error| ApiError::Stream(format!("failed to encode request: {error}")))?;
        let mut outbound = self.provider.build_request(Method::POST, MESSAGES_ENDPOINT);
        outbound.headers.insert(
            http::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        if matches!(
            translated.thinking,
            Some(AnthropicThinking::Adaptive {
                block_binding: Some(_),
                ..
            })
        ) {
            let existing = outbound
                .headers
                .get("anthropic-beta")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let value = if existing
                .split(',')
                .any(|beta| beta.trim() == THINKING_BINDING_BETA)
            {
                existing.to_string()
            } else if existing.is_empty() {
                THINKING_BINDING_BETA.to_string()
            } else {
                format!("{existing},{THINKING_BINDING_BETA}")
            };
            outbound.headers.insert(
                "anthropic-beta",
                HeaderValue::from_str(&value).map_err(|error| {
                    ApiError::Stream(format!("failed to build Anthropic beta header: {error}"))
                })?,
            );
        }
        outbound.body = Some(RequestBody::EncodedJson(body));
        let outbound = outbound
            .into_prepared()
            .map_err(|error| ApiError::Transport(TransportError::Build(error)))?;

        let stream_response = run_with_retry(
            self.provider.retry.to_policy(),
            || outbound.clone(),
            |request, attempt| {
                let auth = Arc::clone(&self.auth);
                let telemetry = self.request_telemetry.clone();
                let transport = &self.transport;
                async move {
                    let start = Instant::now();
                    let result = match auth.apply_auth(request).await {
                        Ok(request) => transport.stream(request).await,
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

        Ok(spawn_anthropic_stream(
            stream_response,
            self.provider.stream_idle_timeout,
            self.sse_telemetry.clone(),
        ))
    }
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

fn spawn_anthropic_stream(
    stream_response: StreamResponse,
    idle_timeout: std::time::Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .or_else(|| stream_response.headers.get(X_REQUEST_ID_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (tx_event, rx_event) =
        mpsc::channel::<Result<ResponseEvent, ApiError>>(RESPONSE_STREAM_CHANNEL_CAPACITY);
    tokio::spawn(process_anthropic_stream(
        stream_response,
        tx_event,
        idle_timeout,
        telemetry,
    ));
    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

async fn process_anthropic_stream(
    stream_response: StreamResponse,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: std::time::Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) {
    let mut stream = stream_response.bytes.eventsource();
    let mut translator = AnthropicStreamTranslator::new();
    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(telemetry) = telemetry.as_ref() {
            telemetry.on_sse_poll(&response, start.elapsed());
        }
        let event = match response {
            Ok(Some(Ok(event))) => event,
            Ok(Some(Err(error))) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(error.to_string())))
                    .await;
                return;
            }
            Ok(None) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "stream closed before message_stop".to_string(),
                    )))
                    .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "idle timeout waiting for Anthropic SSE".to_string(),
                    )))
                    .await;
                return;
            }
        };

        let translated = match translator.translate_json(&event.data) {
            Ok(events) => events,
            Err(error) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(format!(
                        "failed to parse Anthropic SSE: {error}"
                    ))))
                    .await;
                return;
            }
        };
        for event in translated {
            let completed = matches!(event, ResponseEvent::Completed { .. });
            if tx_event.send(Ok(event)).await.is_err() || completed {
                return;
            }
        }
    }
}
