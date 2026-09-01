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

use crate::GeminiStreamTranslator;
use crate::GeminiThoughtSignatureStore;
use crate::translate_request;

const REQUEST_ID_HEADER: &str = "x-request-id";
const RESPONSE_STREAM_CHANNEL_CAPACITY: usize = 1600;

pub struct GeminiClient<T: HttpTransport> {
    transport: T,
    provider: Provider,
    auth: SharedAuthProvider,
    thought_signatures: Arc<GeminiThoughtSignatureStore>,
    request_telemetry: Option<Arc<dyn RequestTelemetry>>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
}

impl<T: HttpTransport> GeminiClient<T> {
    pub fn new(
        transport: T,
        provider: Provider,
        auth: SharedAuthProvider,
        thought_signatures: Arc<GeminiThoughtSignatureStore>,
    ) -> Self {
        Self {
            transport,
            provider,
            auth,
            thought_signatures,
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
        let translated = translate_request(&request, &self.thought_signatures)
            .map_err(|error| ApiError::Stream(format!("failed to translate request: {error}")))?;
        let endpoint = format!(
            "models/{}:streamGenerateContent",
            urlencoding::encode(&translated.model)
        );
        let body = EncodedJsonBody::encode(&translated)
            .map_err(|error| ApiError::Stream(format!("failed to encode request: {error}")))?;
        let mut outbound = self.provider.build_request(Method::POST, &endpoint);
        outbound
            .url
            .push(if outbound.url.contains('?') { '&' } else { '?' });
        outbound.url.push_str("alt=sse");
        outbound.headers.insert(
            http::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
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

        Ok(spawn_gemini_stream(
            stream_response,
            self.provider.stream_idle_timeout,
            Arc::clone(&self.thought_signatures),
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

fn spawn_gemini_stream(
    stream_response: StreamResponse,
    idle_timeout: std::time::Duration,
    thought_signatures: Arc<GeminiThoughtSignatureStore>,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (tx_event, rx_event) =
        mpsc::channel::<Result<ResponseEvent, ApiError>>(RESPONSE_STREAM_CHANNEL_CAPACITY);
    tokio::spawn(process_gemini_stream(
        stream_response,
        tx_event,
        idle_timeout,
        thought_signatures,
        telemetry,
    ));
    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

async fn process_gemini_stream(
    stream_response: StreamResponse,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: std::time::Duration,
    thought_signatures: Arc<GeminiThoughtSignatureStore>,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) {
    let mut stream = stream_response.bytes.eventsource();
    let mut translator = GeminiStreamTranslator::new(thought_signatures);
    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(telemetry) = telemetry.as_ref() {
            telemetry.on_sse_poll(&response, start.elapsed());
        }
        let event = match response {
            Ok(Some(Ok(event))) => event,
            Ok(Some(Err(error))) => {
                send_error(&tx_event, error.to_string()).await;
                return;
            }
            Ok(None) => {
                let translated = match translator.complete() {
                    Ok(events) => events,
                    Err(error) => {
                        send_error(&tx_event, error.to_string()).await;
                        return;
                    }
                };
                send_events(&tx_event, translated).await;
                return;
            }
            Err(_) => {
                send_error(&tx_event, "idle timeout waiting for Gemini SSE").await;
                return;
            }
        };

        let translated = match translator.translate_json(&event.data) {
            Ok(events) => events,
            Err(error) => {
                send_error(&tx_event, format!("failed to parse Gemini SSE: {error}")).await;
                return;
            }
        };
        if !send_events(&tx_event, translated).await {
            return;
        }
    }
}

async fn send_events(
    sender: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    events: Vec<ResponseEvent>,
) -> bool {
    for event in events {
        if sender.send(Ok(event)).await.is_err() {
            return false;
        }
    }
    true
}

async fn send_error(
    sender: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    message: impl Into<String>,
) {
    let _ = sender.send(Err(ApiError::Stream(message.into()))).await;
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
