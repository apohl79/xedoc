use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::responses_metadata::XedocResponsesMetadata;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use xedoc_api::ResponseEvent;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;
use xedoc_login::auth::AgentIdentityAuthPolicy;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_model_provider_info::WireApi;
use xedoc_otel::SessionTelemetry;
use xedoc_protocol::ThreadId;
use xedoc_protocol::config_types::ReasoningSummary;
use xedoc_protocol::models::BaseInstructions;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::FunctionCallOutputPayload;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::protocol::SessionSource;

fn gemini_client(server: &MockServer) -> ModelClient {
    ModelClient::new(
        /*auth_manager*/ None,
        AgentIdentityAuthPolicy::JwtOnly,
        ModelProviderInfo {
            name: "Gemini".to_string(),
            base_url: Some(format!("{}/v1beta", server.uri())),
            env_key: Some("PATH".to_string()),
            wire_api: WireApi::Gemini,
            request_max_retries: Some(0),
            stream_idle_timeout_ms: Some(5_000),
            namespace_tools: false,
            ..ModelProviderInfo::default()
        },
        SessionSource::Cli,
        "test-originator".to_string(),
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*item_ids_enabled*/ false,
        /*concurrent_reasoning_summaries_enabled*/ false,
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    )
}

fn model_info() -> ModelInfo {
    serde_json::from_value(json!({
        "slug": "gemini-3.6-flash",
        "display_name": "gemini-3.6-flash",
        "description": "test",
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [
            {"effort": "medium", "description": "medium"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "base_instructions": "base",
        "support_verbosity": false,
        "truncation_policy": {"mode": "bytes", "limit": 10_000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 1_000_000,
        "experimental_supported_tools": []
    }))
    .expect("model info")
}

fn prompt(input: Vec<ResponseItem>) -> Prompt {
    Prompt {
        input,
        base_instructions: BaseInstructions {
            text: "Follow the test contract.".to_string(),
        },
        ..Prompt::default()
    }
}

fn user_message() -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "Check Berlin weather.".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn metadata(thread_id: ThreadId, turn: usize) -> XedocResponsesMetadata {
    XedocResponsesMetadata::new(
        "installation".to_string(),
        thread_id.to_string(),
        thread_id.to_string(),
        format!("{thread_id}:{turn}"),
    )
}

fn telemetry(thread_id: ThreadId) -> SessionTelemetry {
    SessionTelemetry::new(
        thread_id,
        "gemini-3.6-flash",
        "gemini-3.6-flash",
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test-originator".to_string(),
        /*log_user_prompts*/ false,
        "test-terminal".to_string(),
        SessionSource::Cli,
    )
}

async fn run_turn(
    client: &ModelClient,
    prompt: &Prompt,
    thread_id: ThreadId,
    turn: usize,
) -> anyhow::Result<Vec<ResponseItem>> {
    let mut session = client.new_session();
    let mut stream = session
        .stream(
            prompt,
            &model_info(),
            &telemetry(thread_id),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            &metadata(thread_id, turn),
        )
        .await?;
    let mut items = Vec::new();
    let mut completed = false;
    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::OutputItemDone(item) => items.push(item),
            ResponseEvent::Completed { .. } => completed = true,
            _ => {}
        }
    }
    assert!(completed);
    Ok(items)
}

fn tool_stream() -> ResponseTemplate {
    ResponseTemplate::new(/*status*/ 200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":",
            "{\"name\":\"weather\",\"args\":{\"city\":\"Berlin\"}},",
            "\"thoughtSignature\":\"opaque-signature\"}]},\"finishReason\":\"STOP\"}],",
            "\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":2}}\n\n"
        ))
}

fn text_stream() -> ResponseTemplate {
    ResponseTemplate::new(/*status*/ 200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"sunny\"}]},",
            "\"finishReason\":\"STOP\"}],",
            "\"usageMetadata\":{\"promptTokenCount\":8,\"candidatesTokenCount\":1}}\n\n"
        ))
}

#[tokio::test]
async fn gemini_wire_replays_thought_signatures_after_client_restart() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = Arc::clone(&attempts);
    Mock::given(method("POST"))
        .and(path(
            "/v1beta/models/gemini-3.6-flash:streamGenerateContent",
        ))
        .respond_with(move |_request: &wiremock::Request| {
            if response_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                tool_stream()
            } else {
                text_stream()
            }
        })
        .expect(/*requests*/ 2)
        .mount(&server)
        .await;
    let client = gemini_client(&server);
    let thread_id = ThreadId::new();

    let first_items = run_turn(&client, &prompt(vec![user_message()]), thread_id, 0).await?;
    let function_call = first_items
        .into_iter()
        .find(|item| matches!(item, ResponseItem::FunctionCall { .. }))
        .expect("Gemini should return a function call");
    let function_call = serde_json::from_value(serde_json::to_value(function_call)?)?;
    let call_id = match &function_call {
        ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } => {
            assert_eq!(
                (name.as_str(), arguments.as_str()),
                ("weather", r#"{"city":"Berlin"}"#)
            );
            call_id.clone()
        }
        _ => unreachable!("function call was selected above"),
    };
    let resumed_client = gemini_client(&server);
    let second_items = run_turn(
        &resumed_client,
        &prompt(vec![
            user_message(),
            function_call,
            ResponseItem::FunctionCallOutput {
                id: None,
                call_id,
                output: FunctionCallOutputPayload::from_text("sunny".to_string()),
                internal_chat_message_metadata_passthrough: None,
            },
        ]),
        thread_id,
        1,
    )
    .await?;

    assert!(matches!(
        second_items.as_slice(),
        [ResponseItem::Message { content, .. }]
            if content == &[ContentItem::OutputText {
                text: "sunny".to_string()
            }]
    ));
    let api_key = std::env::var("PATH").expect("PATH should be set for the test");
    let requests = server
        .received_requests()
        .await
        .expect("server should record requests")
        .into_iter()
        .map(|request| {
            json!({
                "path": request.url.path(),
                "query": request.url.query(),
                "api_key": request
                    .headers
                    .get("x-goog-api-key")
                    .and_then(|value| value.to_str().ok()),
                "body": serde_json::from_slice::<Value>(&request.body)
                    .expect("request body should be JSON"),
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests,
        vec![
            json!({
                "path": "/v1beta/models/gemini-3.6-flash:streamGenerateContent",
                "query": "alt=sse",
                "api_key": api_key,
                "body": {
                    "contents": [{
                        "role": "user",
                        "parts": [{"text": "Check Berlin weather."}]
                    }],
                    "systemInstruction": {
                        "parts": [{"text": "Follow the test contract."}]
                    },
                    "generationConfig": {
                        "thinkingConfig": {"thinkingLevel": "MEDIUM"}
                    }
                }
            }),
            json!({
                "path": "/v1beta/models/gemini-3.6-flash:streamGenerateContent",
                "query": "alt=sse",
                "api_key": api_key,
                "body": {
                    "contents": [
                        {
                            "role": "user",
                            "parts": [{"text": "Check Berlin weather."}]
                        },
                        {
                            "role": "model",
                            "parts": [{
                                "functionCall": {
                                    "name": "weather",
                                    "args": {"city": "Berlin"}
                                },
                                "thoughtSignature": "opaque-signature"
                            }]
                        },
                        {
                            "role": "user",
                            "parts": [{
                                "functionResponse": {
                                    "name": "weather",
                                    "response": {"output": "sunny"}
                                }
                            }]
                        }
                    ],
                    "systemInstruction": {
                        "parts": [{"text": "Follow the test contract."}]
                    },
                    "generationConfig": {
                        "thinkingConfig": {"thinkingLevel": "MEDIUM"}
                    }
                }
            }),
        ]
    );
    Ok(())
}
