use super::AuthRequestTelemetryContext;
use super::CompactConversationRequestSettings;
use super::ModelClient;
use super::PendingUnauthorizedRetry;
use super::Prompt;
use super::UnauthorizedRecoveryExecution;
use super::X_OPENAI_SUBAGENT_HEADER;
use super::X_XEDOC_INSTALLATION_ID_HEADER;
use super::X_XEDOC_PARENT_THREAD_ID_HEADER;
use super::X_XEDOC_TURN_METADATA_HEADER;
use super::X_XEDOC_WINDOW_ID_HEADER;
use crate::responses_metadata::XedocResponsesMetadata;
use crate::responses_metadata::XedocResponsesRequestKind;
use crate::responses_metadata::subagent_header_value;
use crate::responses_metadata::subagent_metadata_kind;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use xedoc_api::AgentIdentityTelemetry;
use xedoc_api::ResponseEvent;
use xedoc_api::TransportError;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;
use xedoc_login::AuthCredentialsStoreMode;
use xedoc_login::AuthKeyringBackendKind;
use xedoc_login::AuthManager;
use xedoc_login::auth::AgentIdentityAuthPolicy;
use xedoc_model_provider::BearerAuthProvider;
use xedoc_model_provider::create_model_provider;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_model_provider_info::WireApi;
use xedoc_model_provider_info::create_oss_provider_with_base_url;
use xedoc_otel::SessionTelemetry;
use xedoc_protocol::ThreadId;
use xedoc_protocol::models::AgentMessageInputContent;
use xedoc_protocol::models::BaseInstructions;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::protocol::SessionSource;
use xedoc_protocol::protocol::SubAgentSource;
use xedoc_protocol::protocol::TokenUsage;
use xedoc_protocol::provider_item_metadata::ProviderItemMetadata;

#[derive(Clone, Copy)]
enum TestXedocResponsesRequestKind {
    Turn,
}

#[allow(clippy::too_many_arguments)]
fn test_responses_metadata(
    installation_id: &str,
    session_id: &str,
    thread_id: &str,
    turn_id: Option<&str>,
    window_id: String,
    session_source: &SessionSource,
    parent_thread_id: Option<ThreadId>,
    request_kind: TestXedocResponsesRequestKind,
) -> XedocResponsesMetadata {
    let request_kind = match request_kind {
        TestXedocResponsesRequestKind::Turn => Some(XedocResponsesRequestKind::Turn),
    };
    XedocResponsesMetadata {
        turn_id: request_kind.and(turn_id.map(ToString::to_string)),
        request_kind,
        parent_thread_id,
        subagent_header: subagent_header_value(session_source),
        subagent_kind: request_kind.and_then(|_| subagent_metadata_kind(session_source)),
        ..XedocResponsesMetadata::new(
            installation_id.to_string(),
            session_id.to_string(),
            thread_id.to_string(),
            window_id,
        )
    }
}

const TEST_CHATGPT_ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJlbWFpbF92ZXJpZmllZCI6dHJ1ZSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfdXNlcl9pZCI6InVzZXItMTIzNDUiLCJ1c2VyX2lkIjoidXNlci0xMjM0NSIsImNoYXRncHRfcGxhbl90eXBlIjoicHJvIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.c2ln";
const TEST_INSTALLATION_ID: &str = "11111111-1111-4111-8111-111111111111";

fn test_model_client(session_source: SessionSource) -> ModelClient {
    let provider = create_oss_provider_with_base_url("https://example.com/v1", WireApi::Responses);
    ModelClient::new(
        /*auth_manager*/ None,
        AgentIdentityAuthPolicy::JwtOnly,
        provider,
        session_source,
        "test_originator".to_string(),
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*item_ids_enabled*/ false,
        /*concurrent_reasoning_summaries_enabled*/ false,
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    )
}

#[tokio::test]
async fn compact_uses_bearer_after_agent_identity_session_fallback() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let registration_count = Arc::new(AtomicUsize::new(0));
    let response_count = Arc::clone(&registration_count);
    Mock::given(method("POST"))
        .and(path("/v1/agent/register"))
        .respond_with(move |_request: &wiremock::Request| {
            response_count.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(/*status*/ 503)
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses/compact"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
            "output": []
        })))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;

    let xedoc_home = TempDir::new()?;
    let auth_manager = chatgpt_auth_manager(&xedoc_home, server.uri()).await;
    let mut provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider.supports_websockets = false;
    let thread_id = ThreadId::new();
    let client = ModelClient::new(
        Some(auth_manager),
        AgentIdentityAuthPolicy::ChatGptAuth,
        provider,
        SessionSource::Cli,
        "test_originator".to_string(),
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*item_ids_enabled*/ false,
        /*concurrent_reasoning_summaries_enabled*/ false,
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    );
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "please compact".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        base_instructions: BaseInstructions {
            text: "base instructions".to_string(),
        },
        ..Default::default()
    };
    let responses_metadata = test_responses_metadata_for_client(
        &client,
        thread_id,
        /*turn_id*/ None,
        format!("{thread_id}:0"),
        /*parent_thread_id*/ None,
        TestXedocResponsesRequestKind::Turn,
    );

    let output = client
        .compact_conversation_history(
            &prompt,
            &test_model_info(),
            /*turn_state*/ None,
            CompactConversationRequestSettings {
                effort: None,
                summary: xedoc_protocol::config_types::ReasoningSummary::None,
                service_tier: None,
            },
            &test_session_telemetry(),
            &responses_metadata,
        )
        .await?;

    assert!(output.is_empty());
    assert_eq!(registration_count.load(Ordering::SeqCst), 3);
    let requests = server
        .received_requests()
        .await
        .expect("server should record requests");
    let compact_request = requests
        .iter()
        .find(|request| request.url.path() == "/v1/responses/compact")
        .expect("compact request should be captured");
    assert_eq!(
        compact_request
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-access-token")
    );
    assert_eq!(
        compact_request
            .headers
            .get("ChatGPT-Account-ID")
            .and_then(|value| value.to_str().ok()),
        Some("account-123")
    );

    Ok(())
}

#[derive(Debug, PartialEq)]
enum ObservedAnthropicEvent {
    Created,
    MessageAdded,
    TextDelta(String),
    MessageDone(String),
    Completed {
        usage: Option<TokenUsage>,
        end_turn: Option<bool>,
    },
}

#[tokio::test]
async fn anthropic_wire_translates_request_and_stream_end_to_end() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let stream_body = [
        r#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":21,"cache_creation_input_tokens":2,"cache_read_input_tokens":3}}}

"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"native"}}

"#,
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}

"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}

"#,
        r#"event: message_stop
data: {"type":"message_stop"}

"#,
    ]
    .concat();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("request-id", "req-anthropic-native")
                .set_body_string(stream_body),
        )
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;

    let provider = ModelProviderInfo {
        name: "Anthropic".to_string(),
        base_url: Some(format!("{}/v1", server.uri())),
        wire_api: WireApi::Anthropic,
        http_headers: Some(HashMap::from([
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ("x-api-key".to_string(), "test-anthropic-key".to_string()),
        ])),
        request_max_retries: Some(0),
        stream_idle_timeout_ms: Some(5_000),
        namespace_tools: false,
        ..ModelProviderInfo::default()
    };
    let thread_id = ThreadId::new();
    let client = ModelClient::new(
        /*auth_manager*/ None,
        AgentIdentityAuthPolicy::JwtOnly,
        provider,
        SessionSource::Cli,
        "test_originator".to_string(),
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*item_ids_enabled*/ false,
        /*concurrent_reasoning_summaries_enabled*/ false,
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    );
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "reply natively".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        base_instructions: BaseInstructions {
            text: "Follow the test contract.".to_string(),
        },
        ..Prompt::default()
    };
    let mut model_info = test_model_info();
    model_info.slug = "claude-fable-5-1".to_string();
    model_info.display_name = "claude-fable-5-1".to_string();
    let responses_metadata = test_responses_metadata_for_client(
        &client,
        thread_id,
        /*turn_id*/ None,
        format!("{thread_id}:0"),
        /*parent_thread_id*/ None,
        TestXedocResponsesRequestKind::Turn,
    );

    let mut session = client.new_session();
    let mut stream = session
        .stream(
            &prompt,
            &model_info,
            &test_session_telemetry(),
            Some(ReasoningEffort::Medium),
            xedoc_protocol::config_types::ReasoningSummary::Auto,
            /*service_tier*/ None,
            &responses_metadata,
        )
        .await?;
    let mut observed = Vec::new();
    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::Created => observed.push(ObservedAnthropicEvent::Created),
            ResponseEvent::OutputItemAdded(ResponseItem::Message { .. }) => {
                observed.push(ObservedAnthropicEvent::MessageAdded);
            }
            ResponseEvent::OutputTextDelta(delta) => {
                observed.push(ObservedAnthropicEvent::TextDelta(delta));
            }
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
                let text = content
                    .into_iter()
                    .filter_map(|item| match item {
                        ContentItem::OutputText { text } => Some(text),
                        _ => None,
                    })
                    .collect::<String>();
                observed.push(ObservedAnthropicEvent::MessageDone(text));
            }
            ResponseEvent::Completed {
                token_usage,
                end_turn,
                ..
            } => observed.push(ObservedAnthropicEvent::Completed {
                usage: token_usage,
                end_turn,
            }),
            event => panic!("unexpected Anthropic response event: {event:?}"),
        }
    }

    assert_eq!(
        observed,
        vec![
            ObservedAnthropicEvent::Created,
            ObservedAnthropicEvent::MessageAdded,
            ObservedAnthropicEvent::TextDelta("native".to_string()),
            ObservedAnthropicEvent::MessageDone("native".to_string()),
            ObservedAnthropicEvent::Completed {
                usage: Some(TokenUsage {
                    input_tokens: 26,
                    cached_input_tokens: 3,
                    cache_write_input_tokens: 2,
                    output_tokens: 4,
                    reasoning_output_tokens: 0,
                    total_tokens: 30,
                }),
                end_turn: None,
            },
        ]
    );
    let requests = server
        .received_requests()
        .await
        .expect("server should record requests");
    let request = requests.first().expect("request should be captured");
    assert_eq!(
        request
            .headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("test-anthropic-key")
    );
    assert_eq!(
        request
            .headers
            .get("anthropic-version")
            .and_then(|value| value.to_str().ok()),
        Some("2023-06-01")
    );
    assert_eq!(
        request
            .headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok()),
        Some("thinking-binding-controls-2026-08-01")
    );
    let body: serde_json::Value = serde_json::from_slice(&request.body)?;
    assert_eq!(
        body,
        json!({
            "model": "claude-fable-5-1",
            "max_tokens": 65536,
            "stream": true,
            "system": [{"type": "text", "text": "Follow the test contract."}],
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "reply natively"}]
            }],
            "tool_choice": {"type": "auto", "disable_parallel_tool_use": true},
            "thinking": {
                "type": "adaptive",
                "display": "summarized",
                "block_binding": {"prefix_mismatch_behavior": "drop_block"}
            },
            "output_config": {"effort": "medium"}
        })
    );

    Ok(())
}

fn test_responses_metadata_for_client(
    client: &ModelClient,
    thread_id: ThreadId,
    turn_id: Option<&str>,
    window_id: String,
    parent_thread_id: Option<ThreadId>,
    request_kind: TestXedocResponsesRequestKind,
) -> XedocResponsesMetadata {
    let thread_id = thread_id.to_string();
    test_responses_metadata(
        TEST_INSTALLATION_ID,
        &thread_id,
        &thread_id,
        turn_id,
        window_id,
        &client.state.session_source,
        parent_thread_id,
        request_kind,
    )
}

fn test_model_info() -> ModelInfo {
    serde_json::from_value(json!({
        "slug": "gpt-test",
        "display_name": "gpt-test",
        "description": "desc",
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [
            {"effort": "medium", "description": "medium"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "upgrade": null,
        "base_instructions": "base instructions",
        "model_messages": null,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 272000,
        "auto_compact_token_limit": null,
        "experimental_supported_tools": []
    }))
    .expect("deserialize test model info")
}

fn test_session_telemetry() -> SessionTelemetry {
    SessionTelemetry::new(
        ThreadId::new(),
        "gpt-test",
        "gpt-test",
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test-originator".to_string(),
        /*log_user_prompts*/ false,
        "test-terminal".to_string(),
        SessionSource::Cli,
    )
}

#[test]
fn compatible_provider_input_normalizes_plaintext_agent_messages() {
    let mut input = vec![ResponseItem::AgentMessage {
        id: None,
        author: "parent".to_string(),
        recipient: "child".to_string(),
        content: vec![AgentMessageInputContent::InputText {
            text: "Check the weather.".to_string(),
        }],
        internal_chat_message_metadata_passthrough: None,
    }];

    super::normalize_agent_messages_for_compatible_provider(&mut input);

    assert_eq!(
        input,
        vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Check the weather.".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }]
    );
}

#[tokio::test]
async fn responses_wire_strips_native_provider_metadata() -> anyhow::Result<()> {
    let client = test_model_client(SessionSource::Cli);
    let client_setup = client.current_client_setup().await?;
    let thread_id = ThreadId::new();
    let request = client.build_responses_request(
        &client_setup.api_provider,
        &Prompt {
            input: vec![ResponseItem::FunctionCall {
                id: None,
                name: "weather".to_string(),
                namespace: None,
                arguments: r#"{"city":"Berlin"}"#.to_string(),
                call_id: "call-1".to_string(),
                provider_metadata: Some(ProviderItemMetadata::Gemini {
                    provider_id: String::new(),
                    thought_signature: "opaque-signature".to_string(),
                }),
                internal_chat_message_metadata_passthrough: None,
            }],
            base_instructions: BaseInstructions {
                text: "base instructions".to_string(),
            },
            ..Default::default()
        },
        &test_model_info(),
        Some(ReasoningEffort::Medium),
        xedoc_protocol::config_types::ReasoningSummary::None,
        /*service_tier*/ None,
        &test_responses_metadata_for_client(
            &client,
            thread_id,
            Some("turn-1"),
            format!("{thread_id}:0"),
            /*parent_thread_id*/ None,
            TestXedocResponsesRequestKind::Turn,
        ),
    )?;

    assert_eq!(
        request.input,
        vec![ResponseItem::FunctionCall {
            id: None,
            name: "weather".to_string(),
            namespace: None,
            arguments: r#"{"city":"Berlin"}"#.to_string(),
            call_id: "call-1".to_string(),
            provider_metadata: None,
            internal_chat_message_metadata_passthrough: None,
        }]
    );
    Ok(())
}

#[tokio::test]
async fn responses_wire_strips_persisted_provenance_without_dropping_opaque_content()
-> anyhow::Result<()> {
    let client = ModelClient::new(
        /*auth_manager*/ None,
        AgentIdentityAuthPolicy::JwtOnly,
        ModelProviderInfo::create_openai_provider(/*base_url*/ None),
        SessionSource::Cli,
        "test_originator".to_string(),
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*item_ids_enabled*/ false,
        /*concurrent_reasoning_summaries_enabled*/ false,
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    );
    let client_setup = client.current_client_setup().await?;
    let thread_id = ThreadId::new();
    let request = client.build_responses_request(
        &client_setup.api_provider,
        &Prompt {
            input: vec![
                ResponseItem::Reasoning {
                    id: None,
                    summary: Vec::new(),
                    content: None,
                    encrypted_content: Some("reasoning-continuity".to_string()),
                    provider_metadata: Some(ProviderItemMetadata::Responses {
                        provider_id: "OpenAI".to_string(),
                    }),
                    internal_chat_message_metadata_passthrough: None,
                },
                ResponseItem::Compaction {
                    id: None,
                    encrypted_content: "compaction-continuity".to_string(),
                    provider_metadata: Some(ProviderItemMetadata::Responses {
                        provider_id: "OpenAI".to_string(),
                    }),
                    internal_chat_message_metadata_passthrough: None,
                },
                ResponseItem::ContextCompaction {
                    id: None,
                    encrypted_content: Some("context-compaction-continuity".to_string()),
                    provider_metadata: Some(ProviderItemMetadata::Responses {
                        provider_id: "OpenAI".to_string(),
                    }),
                    internal_chat_message_metadata_passthrough: None,
                },
            ],
            base_instructions: BaseInstructions {
                text: "base instructions".to_string(),
            },
            ..Default::default()
        },
        &test_model_info(),
        Some(ReasoningEffort::Medium),
        xedoc_protocol::config_types::ReasoningSummary::None,
        /*service_tier*/ None,
        &test_responses_metadata_for_client(
            &client,
            thread_id,
            Some("turn-1"),
            format!("{thread_id}:0"),
            /*parent_thread_id*/ None,
            TestXedocResponsesRequestKind::Turn,
        ),
    )?;

    assert_eq!(
        request.input,
        vec![
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: Some("reasoning-continuity".to_string()),
                provider_metadata: None,
                internal_chat_message_metadata_passthrough: None,
            },
            ResponseItem::Compaction {
                id: None,
                encrypted_content: "compaction-continuity".to_string(),
                provider_metadata: None,
                internal_chat_message_metadata_passthrough: None,
            },
            ResponseItem::ContextCompaction {
                id: None,
                encrypted_content: Some("context-compaction-continuity".to_string()),
                provider_metadata: None,
                internal_chat_message_metadata_passthrough: None,
            },
        ]
    );
    Ok(())
}

#[test]
fn ultra_reasoning_uses_max_for_requests() {
    assert_eq!(
        (
            super::reasoning_effort_for_request(ReasoningEffort::Ultra),
            super::reasoning_effort_for_request(ReasoningEffort::High),
        ),
        (ReasoningEffort::Max, ReasoningEffort::High,)
    );
}

fn write_chatgpt_auth_json(xedoc_home: &std::path::Path) {
    let auth_json = json!({
        "tokens": {
            "id_token": TEST_CHATGPT_ID_TOKEN,
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token",
            "account_id": "account-123"
        },
        "last_refresh": "2099-01-01T00:00:00Z"
    });
    std::fs::write(
        xedoc_home.join("auth.json"),
        serde_json::to_string_pretty(&auth_json).expect("serialize auth.json"),
    )
    .expect("write auth.json");
}

async fn chatgpt_auth_manager(
    xedoc_home: &TempDir,
    agent_identity_authapi_base_url: String,
) -> Arc<AuthManager> {
    write_chatgpt_auth_json(xedoc_home.path());
    let auth_manager = AuthManager::shared(
        xedoc_home.path().to_path_buf(),
        /*enable_xedoc_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        /*auth_route_config*/ None,
    )
    .await;
    let auth = auth_manager.auth().await.expect("auth should load");
    AuthManager::from_auth_for_testing_with_agent_identity_authapi_base_url(
        auth,
        agent_identity_authapi_base_url,
    )
}

#[test]
fn build_ws_client_metadata_includes_window_lineage_and_turn_metadata() {
    let parent_thread_id = ThreadId::new();
    let client = test_model_client(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth: 2,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
    }));

    let thread_id = ThreadId::new();
    let expected_window_id = format!("{thread_id}:1");
    let responses_metadata = test_responses_metadata_for_client(
        &client,
        thread_id,
        Some("turn-123"),
        expected_window_id.clone(),
        Some(parent_thread_id),
        TestXedocResponsesRequestKind::Turn,
    );
    let client_metadata =
        client.build_ws_client_metadata(&responses_metadata, /*use_responses_lite*/ false);
    let parent_thread_id = parent_thread_id.to_string();
    let thread_id = thread_id.to_string();
    let turn_metadata: serde_json::Value = serde_json::from_str(
        client_metadata
            .get(X_XEDOC_TURN_METADATA_HEADER)
            .expect("turn metadata"),
    )
    .expect("valid turn metadata");
    for (client_key, metadata_key, expected) in [
        (
            X_XEDOC_INSTALLATION_ID_HEADER,
            "installation_id",
            "11111111-1111-4111-8111-111111111111",
        ),
        ("session_id", "session_id", thread_id.as_str()),
        ("thread_id", "thread_id", thread_id.as_str()),
        ("turn_id", "turn_id", "turn-123"),
        (
            X_XEDOC_WINDOW_ID_HEADER,
            "window_id",
            expected_window_id.as_str(),
        ),
        (
            X_XEDOC_PARENT_THREAD_ID_HEADER,
            "parent_thread_id",
            parent_thread_id.as_str(),
        ),
    ] {
        assert_eq!(
            client_metadata.get(client_key).map(String::as_str),
            Some(expected)
        );
        assert_eq!(turn_metadata[metadata_key].as_str(), Some(expected));
    }
    assert_eq!(
        client_metadata
            .get(X_OPENAI_SUBAGENT_HEADER)
            .map(String::as_str),
        Some("collab_spawn")
    );
}

#[tokio::test]
async fn bedrock_unauthorized_error_uses_provider_mapping() {
    let provider = create_model_provider(
        ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
        /*auth_manager*/ None,
    );
    let mut auth_recovery = None;
    let url = "https://bedrock-mantle.us-east-2.api.aws/openai/v1/responses";
    let error = super::handle_unauthorized(
        TransportError::Http {
            status: http::StatusCode::UNAUTHORIZED,
            url: Some(url.to_string()),
            headers: None,
            body: Some(
                "Signature expired: 20260609T133205Z is now earlier than 20260614T062525Z"
                    .to_string(),
            ),
        },
        &mut auth_recovery,
        &test_session_telemetry(),
        &provider,
    )
    .await
    .expect_err("expired Bedrock signature should fail");

    assert_eq!(
        error.to_string(),
        format!(
            "Amazon Bedrock rejected the request because its AWS signature has expired. Refresh your AWS credentials and retry. If `AWS_BEARER_TOKEN_BEDROCK` is set, update or unset it, then restart Xedoc, url: {url}"
        )
    );
}

#[test]
fn auth_request_telemetry_context_tracks_attached_auth_and_retry_phase() {
    let auth_context = AuthRequestTelemetryContext::new(
        &BearerAuthProvider::for_test(Some("access-token"), Some("workspace-123")),
        /*agent_identity_telemetry*/ None,
        PendingUnauthorizedRetry::from_recovery(UnauthorizedRecoveryExecution {
            mode: "managed",
            phase: "refresh_token",
        }),
    );

    assert!(auth_context.auth_header_attached);
    assert_eq!(auth_context.auth_header_name, Some("authorization"));
    assert!(auth_context.retry_after_unauthorized);
    assert_eq!(auth_context.recovery_mode, Some("managed"));
    assert_eq!(auth_context.recovery_phase, Some("refresh_token"));
}

#[test]
fn auth_request_telemetry_context_tracks_agent_identity_ids() {
    let auth_context = AuthRequestTelemetryContext::new(
        &BearerAuthProvider::for_test(/*token*/ None, /*account_id*/ None),
        Some(AgentIdentityTelemetry {
            agent_id: "agent-runtime-context".to_string(),
            task_id: "task-run-context".to_string(),
        }),
        PendingUnauthorizedRetry::default(),
    );

    assert_eq!(
        auth_context.agent_identity_telemetry(),
        Some(&AgentIdentityTelemetry {
            agent_id: "agent-runtime-context".to_string(),
            task_id: "task-run-context".to_string(),
        })
    );
}
