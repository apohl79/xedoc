use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use xedoc_api::ResponseEvent;
use xedoc_protocol::ResponseItemId;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::protocol::TokenUsage;
use xedoc_protocol::provider_item_metadata::ProviderItemMetadata;

use super::GeminiStreamError;
use super::GeminiStreamTranslator;
use crate::GeminiThoughtSignatureStore;

#[derive(Debug, PartialEq)]
enum ObservedEvent {
    Created,
    Added(ResponseItem),
    Done(ResponseItem),
    TextDelta(String),
    ToolDelta {
        item_id: String,
        call_id: Option<String>,
        delta: String,
    },
    Completed {
        response_id: String,
        usage: Option<TokenUsage>,
        end_turn: Option<bool>,
    },
}

fn translator() -> (GeminiStreamTranslator, Arc<GeminiThoughtSignatureStore>) {
    let thought_signatures = Arc::new(GeminiThoughtSignatureStore::new());
    (
        GeminiStreamTranslator::new(Arc::clone(&thought_signatures)),
        thought_signatures,
    )
}

fn translate(translator: &mut GeminiStreamTranslator, chunk: Value) -> Vec<ObservedEvent> {
    let data = serde_json::to_string(&chunk).unwrap();
    observe(translator.translate_json(&data).unwrap())
}

fn observe(events: Vec<ResponseEvent>) -> Vec<ObservedEvent> {
    events.into_iter().map(observe_event).collect()
}

fn observe_event(event: ResponseEvent) -> ObservedEvent {
    match event {
        ResponseEvent::Created => ObservedEvent::Created,
        ResponseEvent::OutputItemAdded(item) => ObservedEvent::Added(item),
        ResponseEvent::OutputItemDone(item) => ObservedEvent::Done(item),
        ResponseEvent::OutputTextDelta(delta) => ObservedEvent::TextDelta(delta),
        ResponseEvent::ToolCallInputDelta {
            item_id,
            call_id,
            delta,
        } => ObservedEvent::ToolDelta {
            item_id,
            call_id,
            delta,
        },
        ResponseEvent::Completed {
            response_id,
            token_usage,
            end_turn,
        } => ObservedEvent::Completed {
            response_id,
            usage: token_usage,
            end_turn,
        },
        ResponseEvent::SafetyBuffering(_)
        | ResponseEvent::ServerModel(_)
        | ResponseEvent::ModelVerifications(_)
        | ResponseEvent::TurnModerationMetadata(_)
        | ResponseEvent::ServerReasoningIncluded(_)
        | ResponseEvent::ReasoningSummaryDelta { .. }
        | ResponseEvent::ReasoningSummaryDone { .. }
        | ResponseEvent::ReasoningContentDelta { .. }
        | ResponseEvent::ReasoningSummaryPartAdded { .. }
        | ResponseEvent::RateLimits(_)
        | ResponseEvent::ModelsEtag(_) => panic!("unexpected response event"),
    }
}

fn message_item(id: ResponseItemId, content: Vec<ContentItem>) -> ResponseItem {
    ResponseItem::Message {
        id: Some(id),
        role: "assistant".to_string(),
        content,
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn translates_text_chunks_and_terminal_usage() {
    let (mut translator, _) = translator();
    let response_id = translator.response_id.clone();
    let message_id = translator.message_id.clone();
    let mut actual = translate(
        &mut translator,
        json!({
            "candidates": [{
                "content": {"parts": [{"text": "Hel"}, {"text": "lo"}]}
            }]
        }),
    );
    actual.extend(translate(
        &mut translator,
        json!({
            "candidates": [{"finishReason": "STOP"}],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 50,
                "cachedContentTokenCount": 10,
                "thoughtsTokenCount": 12
            }
        }),
    ));
    actual.extend(observe(translator.complete().unwrap()));

    assert_eq!(
        actual,
        vec![
            ObservedEvent::Created,
            ObservedEvent::Added(message_item(message_id.clone(), Vec::new())),
            ObservedEvent::TextDelta("Hello".to_string()),
            ObservedEvent::Done(message_item(
                message_id,
                vec![ContentItem::OutputText {
                    text: "Hello".to_string(),
                }],
            )),
            ObservedEvent::Completed {
                response_id,
                usage: Some(TokenUsage {
                    input_tokens: 100,
                    cached_input_tokens: 10,
                    cache_write_input_tokens: 0,
                    output_tokens: 50,
                    reasoning_output_tokens: 12,
                    total_tokens: 150,
                }),
                end_turn: None,
            },
        ]
    );
    assert!(translator.complete().unwrap().is_empty());
}

#[test]
fn translates_tool_only_chunk_and_preserves_thought_signatures() {
    let (mut translator, thought_signatures) = translator();

    let actual = translate(
        &mut translator,
        json!({
            "candidates": [{
                "content": {"parts": [
                    {
                        "functionCall": {
                            "name": "exec_command",
                            "args": {"cmd": "ls"}
                        },
                        "thoughtSignature": "sig_shell"
                    },
                    {
                        "functionCall": {
                            "name": "apply_patch",
                            "args": {"input": "*** Begin Patch"},
                            "thoughtSignature": "sig_patch"
                        }
                    }
                ]}
            }]
        }),
    );
    let shell_call_id = match &actual[1] {
        ObservedEvent::Added(ResponseItem::FunctionCall { call_id, .. }) => call_id.clone(),
        event => panic!("expected function call, got {event:?}"),
    };
    let patch_call_id = match &actual[3] {
        ObservedEvent::Added(ResponseItem::CustomToolCall { call_id, .. }) => call_id.clone(),
        event => panic!("expected custom tool call, got {event:?}"),
    };

    assert_eq!(
        actual,
        vec![
            ObservedEvent::Created,
            ObservedEvent::Added(ResponseItem::FunctionCall {
                id: Some(ResponseItemId::from_server(format!("fc_{shell_call_id}"))),
                name: "exec_command".to_string(),
                namespace: None,
                arguments: String::new(),
                call_id: shell_call_id.clone(),
                provider_metadata: None,
                internal_chat_message_metadata_passthrough: None,
            }),
            ObservedEvent::Done(ResponseItem::FunctionCall {
                id: Some(ResponseItemId::from_server(format!("fc_{shell_call_id}"))),
                name: "exec_command".to_string(),
                namespace: None,
                arguments: "{\"cmd\":\"ls\"}".to_string(),
                call_id: shell_call_id.clone(),
                provider_metadata: Some(ProviderItemMetadata::Gemini {
                    provider_id: String::new(),
                    thought_signature: "sig_shell".to_string(),
                }),
                internal_chat_message_metadata_passthrough: None,
            }),
            ObservedEvent::Added(ResponseItem::CustomToolCall {
                id: Some(ResponseItemId::from_server(format!("ctc_{patch_call_id}"))),
                status: Some("in_progress".to_string()),
                call_id: patch_call_id.clone(),
                name: "apply_patch".to_string(),
                namespace: None,
                input: String::new(),
                provider_metadata: None,
                internal_chat_message_metadata_passthrough: None,
            }),
            ObservedEvent::ToolDelta {
                item_id: format!("ctc_{patch_call_id}"),
                call_id: Some(patch_call_id.clone()),
                delta: "*** Begin Patch".to_string(),
            },
            ObservedEvent::Done(ResponseItem::CustomToolCall {
                id: Some(ResponseItemId::from_server(format!("ctc_{patch_call_id}"))),
                status: Some("completed".to_string()),
                call_id: patch_call_id.clone(),
                name: "apply_patch".to_string(),
                namespace: None,
                input: "*** Begin Patch".to_string(),
                provider_metadata: Some(ProviderItemMetadata::Gemini {
                    provider_id: String::new(),
                    thought_signature: "sig_patch".to_string(),
                }),
                internal_chat_message_metadata_passthrough: None,
            }),
        ]
    );
    assert_eq!(
        thought_signatures.signature(&shell_call_id),
        Some("sig_shell".to_string())
    );
    assert_eq!(
        thought_signatures.signature(&patch_call_id),
        Some("sig_patch".to_string())
    );
}

#[test]
fn emits_text_before_tools_for_mixed_parts() {
    let (mut translator, _) = translator();

    let actual = translate(
        &mut translator,
        json!({
            "candidates": [{
                "content": {"parts": [
                    {"functionCall": {"name": "shell", "args": {}}},
                    {"text": "Running it."}
                ]}
            }]
        }),
    );

    assert!(matches!(actual[0], ObservedEvent::Created));
    assert!(matches!(
        actual[1],
        ObservedEvent::Added(ResponseItem::Message { .. })
    ));
    assert_eq!(
        actual[2],
        ObservedEvent::TextDelta("Running it.".to_string())
    );
    assert!(matches!(
        actual[3],
        ObservedEvent::Added(ResponseItem::FunctionCall { .. })
    ));
    assert!(matches!(
        actual[4],
        ObservedEvent::Done(ResponseItem::FunctionCall { .. })
    ));
}

#[test]
fn reports_blocked_and_terminal_no_output_diagnostics() {
    let (mut blocked, _) = translator();
    let actual = translate(
        &mut blocked,
        json!({"promptFeedback": {"blockReason": "SAFETY"}}),
    );

    assert_eq!(actual, vec![ObservedEvent::Created]);
    assert_eq!(
        blocked.complete().unwrap_err(),
        GeminiStreamError::NoVisibleOutput("Gemini blocked the prompt: SAFETY.".to_string())
    );
    assert!(blocked.complete().unwrap().is_empty());

    let (mut terminal, _) = translator();
    translate(
        &mut terminal,
        json!({
            "candidates": [{
                "finishReason": "RECITATION",
                "finishMessage": "citation match"
            }]
        }),
    );
    assert_eq!(
        terminal.complete().unwrap_err(),
        GeminiStreamError::NoVisibleOutput(
            "Gemini returned no visible output: RECITATION (citation match).".to_string()
        )
    );
}

#[test]
fn maps_max_tokens_and_zero_usage_to_partial_completion() {
    let (mut translator, _) = translator();
    let response_id = translator.response_id.clone();
    translate(
        &mut translator,
        json!({
            "candidates": [{
                "content": {"parts": [{"text": "x"}]},
                "finishReason": "MAX_TOKENS"
            }],
            "usageMetadata": {}
        }),
    );

    let actual = observe(translator.complete().unwrap());

    assert_eq!(
        actual,
        vec![
            ObservedEvent::Done(message_item(
                translator.message_id.clone(),
                vec![ContentItem::OutputText {
                    text: "x".to_string()
                }],
            )),
            ObservedEvent::Completed {
                response_id,
                usage: Some(TokenUsage::default()),
                end_turn: Some(false),
            },
        ]
    );
}

#[test]
fn rejects_malformed_json_and_reports_empty_stream() {
    let (mut malformed, _) = translator();
    assert!(malformed.translate_json("{").is_err());

    let (mut empty, _) = translator();
    assert_eq!(
        empty.complete().unwrap_err(),
        GeminiStreamError::NoVisibleOutput("Gemini returned no visible output.".to_string())
    );
}
