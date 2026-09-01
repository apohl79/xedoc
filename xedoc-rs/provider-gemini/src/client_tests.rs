use bytes::Bytes;
use futures::StreamExt;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use xedoc_api::AuthProvider;
use xedoc_api::Provider;
use xedoc_api::ResponseEvent;
use xedoc_api::ResponsesApiRequest;
use xedoc_client::HttpTransport;
use xedoc_client::Request;
use xedoc_client::RequestBody;
use xedoc_client::Response;
use xedoc_client::StreamResponse;
use xedoc_client::TransportError;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::protocol::TokenUsage;

use super::GeminiClient;
use crate::GeminiThoughtSignatureStore;

#[derive(Clone)]
struct RecordingTransport {
    request: Arc<Mutex<Option<Request>>>,
    body: StreamBody,
}

#[derive(Clone)]
enum StreamBody {
    Data(String),
    Pending,
}

impl HttpTransport for RecordingTransport {
    async fn execute(&self, _request: Request) -> Result<Response, TransportError> {
        Err(TransportError::Build("execute should not run".to_string()))
    }

    async fn stream(&self, request: Request) -> Result<StreamResponse, TransportError> {
        *self
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
        let bytes = match &self.body {
            StreamBody::Data(body) => {
                Box::pin(futures::stream::iter(vec![Ok(Bytes::from(body.clone()))]))
                    as xedoc_client::ByteStream
            }
            StreamBody::Pending => Box::pin(futures::stream::pending()) as xedoc_client::ByteStream,
        };
        Ok(StreamResponse {
            status: StatusCode::OK,
            headers: HeaderMap::from_iter([(
                "x-request-id".parse().unwrap(),
                HeaderValue::from_static("google-request-1"),
            )]),
            bytes,
        })
    }
}

#[derive(Clone)]
struct GoogleApiKeyAuth;

impl AuthProvider for GoogleApiKeyAuth {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        let _ = headers.insert("x-goog-api-key", HeaderValue::from_static("test-key"));
    }
}

#[derive(Debug, PartialEq)]
enum ObservedEvent {
    Created,
    AddedMessage,
    TextDelta(String),
    DoneMessage(String),
    Completed {
        usage: Option<TokenUsage>,
        end_turn: Option<bool>,
    },
    ToolAdded {
        call_id: String,
        name: String,
    },
    ToolDone {
        call_id: String,
        name: String,
        arguments: String,
    },
}

fn recording_transport(body: StreamBody) -> (RecordingTransport, Arc<Mutex<Option<Request>>>) {
    let request = Arc::new(Mutex::new(None));
    (
        RecordingTransport {
            request: Arc::clone(&request),
            body,
        },
        request,
    )
}

fn provider(idle_timeout: Duration) -> Provider {
    Provider {
        name: "Gemini".to_string(),
        base_url: "https://example.test/v1beta".to_string(),
        query_params: Some(HashMap::from([(
            "quotaUser".to_string(),
            "xedoc".to_string(),
        )])),
        headers: HeaderMap::new(),
        retry: xedoc_api::RetryConfig {
            max_attempts: 0,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: idle_timeout,
    }
}

fn request() -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "gemini-3.6-flash".to_string(),
        instructions: "Follow the test contract.".to_string(),
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "reply natively".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        tools: None,
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream: true,
        stream_options: None,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

fn gemini_client(
    transport: RecordingTransport,
    thought_signatures: Arc<GeminiThoughtSignatureStore>,
    idle_timeout: Duration,
) -> GeminiClient<RecordingTransport> {
    GeminiClient::new(
        transport,
        provider(idle_timeout),
        Arc::new(GoogleApiKeyAuth),
        thought_signatures,
    )
}

fn observe(event: ResponseEvent) -> ObservedEvent {
    match event {
        ResponseEvent::Created => ObservedEvent::Created,
        ResponseEvent::OutputItemAdded(ResponseItem::Message { .. }) => ObservedEvent::AddedMessage,
        ResponseEvent::OutputTextDelta(delta) => ObservedEvent::TextDelta(delta),
        ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
            let text = content
                .into_iter()
                .filter_map(|item| match item {
                    ContentItem::OutputText { text } => Some(text),
                    ContentItem::InputText { .. }
                    | ContentItem::InputImage { .. }
                    | ContentItem::InputAudio { .. } => None,
                })
                .collect();
            ObservedEvent::DoneMessage(text)
        }
        ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        }) => ObservedEvent::ToolDone {
            call_id,
            name,
            arguments,
        },
        ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { call_id, name, .. }) => {
            ObservedEvent::ToolAdded { call_id, name }
        }
        ResponseEvent::Completed {
            token_usage,
            end_turn,
            ..
        } => ObservedEvent::Completed {
            usage: token_usage,
            end_turn,
        },
        ResponseEvent::OutputItemAdded(
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::AgentMessage { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::FunctionCallOutput { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other,
        )
        | ResponseEvent::OutputItemDone(
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::AgentMessage { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::FunctionCallOutput { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other,
        )
        | ResponseEvent::SafetyBuffering(_)
        | ResponseEvent::ServerModel(_)
        | ResponseEvent::ModelVerifications(_)
        | ResponseEvent::TurnModerationMetadata(_)
        | ResponseEvent::ServerReasoningIncluded(_)
        | ResponseEvent::ToolCallInputDelta { .. }
        | ResponseEvent::ReasoningSummaryDelta { .. }
        | ResponseEvent::ReasoningSummaryDone { .. }
        | ResponseEvent::ReasoningContentDelta { .. }
        | ResponseEvent::ReasoningSummaryPartAdded { .. }
        | ResponseEvent::RateLimits(_)
        | ResponseEvent::ModelsEtag(_) => panic!("unexpected response event"),
    }
}

async fn collect_success(
    client: &GeminiClient<RecordingTransport>,
) -> Result<Vec<ObservedEvent>, xedoc_api::ApiError> {
    let mut stream = client.stream_request(request()).await?;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(observe(event?));
    }
    Ok(events)
}

#[tokio::test]
async fn sends_generate_content_request_and_completes_text_stream() {
    let body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"native\"}]},",
        "\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,",
        "\"candidatesTokenCount\":2,\"cachedContentTokenCount\":1,",
        "\"thoughtsTokenCount\":1}}\n\n"
    );
    let (transport, captured) = recording_transport(StreamBody::Data(body.to_string()));
    let signatures = Arc::new(GeminiThoughtSignatureStore::new());
    let client = gemini_client(transport, signatures, Duration::from_secs(1));

    let actual = collect_success(&client).await.unwrap();

    assert_eq!(
        actual,
        vec![
            ObservedEvent::Created,
            ObservedEvent::AddedMessage,
            ObservedEvent::TextDelta("native".to_string()),
            ObservedEvent::DoneMessage("native".to_string()),
            ObservedEvent::Completed {
                usage: Some(TokenUsage {
                    input_tokens: 5,
                    cached_input_tokens: 1,
                    cache_write_input_tokens: 0,
                    output_tokens: 2,
                    reasoning_output_tokens: 1,
                    total_tokens: 7,
                }),
                end_turn: None,
            },
        ]
    );
    let request = captured
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(
        request.url,
        "https://example.test/v1beta/models/gemini-3.6-flash:streamGenerateContent?quotaUser=xedoc&alt=sse"
    );
    assert_eq!(
        request
            .headers
            .get("x-goog-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("test-key")
    );
    assert_eq!(
        request
            .headers
            .get(http::header::ACCEPT)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let Some(RequestBody::EncodedJson(body)) = request.body else {
        panic!("expected encoded JSON body");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(body.as_bytes()).unwrap(),
        json!({
            "contents": [{
                "role": "user",
                "parts": [{"text": "reply natively"}]
            }],
            "systemInstruction": {
                "parts": [{"text": "Follow the test contract."}]
            }
        })
    );
}

#[tokio::test]
async fn stores_tool_thought_signature_from_stream() {
    let body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":",
        "{\"name\":\"exec_command\",\"args\":{\"cmd\":\"ls\"}},",
        "\"thoughtSignature\":\"opaque-signature\"}]}}],",
        "\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n"
    );
    let (transport, _) = recording_transport(StreamBody::Data(body.to_string()));
    let signatures = Arc::new(GeminiThoughtSignatureStore::new());
    let client = gemini_client(transport, Arc::clone(&signatures), Duration::from_secs(1));

    let actual = collect_success(&client).await.unwrap();
    let call_id = actual
        .iter()
        .find_map(|event| match event {
            ObservedEvent::ToolDone { call_id, .. } => Some(call_id.clone()),
            ObservedEvent::Created
            | ObservedEvent::AddedMessage
            | ObservedEvent::TextDelta(_)
            | ObservedEvent::DoneMessage(_)
            | ObservedEvent::Completed { .. }
            | ObservedEvent::ToolAdded { .. } => None,
        })
        .unwrap();

    assert_eq!(
        signatures.signature(&call_id),
        Some("opaque-signature".to_string())
    );
    assert!(matches!(
        actual.last(),
        Some(ObservedEvent::Completed { .. })
    ));
}

#[tokio::test]
async fn reports_malformed_and_empty_streams() {
    let (transport, _) = recording_transport(StreamBody::Data("data: {\n\n".to_string()));
    let client = gemini_client(
        transport,
        Arc::new(GeminiThoughtSignatureStore::new()),
        Duration::from_secs(1),
    );
    let mut malformed = client.stream_request(request()).await.unwrap();
    let malformed_error = malformed.next().await.unwrap().unwrap_err().to_string();
    assert!(malformed_error.contains("failed to parse Gemini SSE"));

    let (transport, _) = recording_transport(StreamBody::Data(
        "data: {\"usageMetadata\":{\"promptTokenCount\":1}}\n\n".to_string(),
    ));
    let client = gemini_client(
        transport,
        Arc::new(GeminiThoughtSignatureStore::new()),
        Duration::from_secs(1),
    );
    let mut empty = client.stream_request(request()).await.unwrap();
    assert!(matches!(
        empty.next().await,
        Some(Ok(ResponseEvent::Created))
    ));
    let empty_error = empty.next().await.unwrap().unwrap_err().to_string();
    assert!(empty_error.contains("Gemini returned no visible output"));
}

#[tokio::test]
async fn reports_idle_timeout() {
    let (transport, _) = recording_transport(StreamBody::Pending);
    let client = gemini_client(
        transport,
        Arc::new(GeminiThoughtSignatureStore::new()),
        Duration::from_millis(1),
    );

    let mut stream = client.stream_request(request()).await.unwrap();
    let error = stream.next().await.unwrap().unwrap_err().to_string();

    assert!(error.contains("idle timeout waiting for Gemini SSE"));
}
