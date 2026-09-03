use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use xedoc_api::ResponseEvent;
use xedoc_protocol::ResponseItemId;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ReasoningItemReasoningSummary;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::protocol::TokenUsage;

use super::ActiveBlock;
use super::AnthropicStreamTranslator;

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
    ReasoningDelta {
        delta: String,
        summary_index: i64,
    },
    ReasoningDone {
        item_id: String,
        text: String,
        summary_index: i64,
    },
    ReasoningPartAdded {
        summary_index: i64,
    },
    Completed {
        response_id: String,
        usage: Option<TokenUsage>,
        end_turn: Option<bool>,
    },
}

fn translate(translator: &mut AnthropicStreamTranslator, event: Value) -> Vec<ObservedEvent> {
    let data = serde_json::to_string(&event).unwrap();
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
        ResponseEvent::ReasoningSummaryDelta {
            delta,
            summary_index,
        } => ObservedEvent::ReasoningDelta {
            delta,
            summary_index,
        },
        ResponseEvent::ReasoningSummaryDone {
            item_id,
            text,
            summary_index,
        } => ObservedEvent::ReasoningDone {
            item_id,
            text,
            summary_index,
        },
        ResponseEvent::ReasoningSummaryPartAdded { summary_index } => {
            ObservedEvent::ReasoningPartAdded { summary_index }
        }
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
        | ResponseEvent::ReasoningContentDelta { .. }
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
fn translates_text_stream_and_terminal_usage() {
    let mut translator = AnthropicStreamTranslator::new();
    let response_id = translator.response_id.clone();
    let message_id = translator.message_id.clone();
    let mut actual = translate(
        &mut translator,
        json!({"type": "message_start", "message": {}}),
    );
    actual.extend(translate(
        &mut translator,
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
    ));
    actual.extend(translate(
        &mut translator,
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hello"}
        }),
    ));
    actual.extend(translate(
        &mut translator,
        json!({"type": "content_block_stop", "index": 0}),
    ));
    actual.extend(translate(
        &mut translator,
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": 20,
                "cache_read_input_tokens": 10
            }
        }),
    ));
    actual.extend(translate(&mut translator, json!({"type": "message_stop"})));

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
                    reasoning_output_tokens: 0,
                    total_tokens: 150,
                }),
                end_turn: None,
            },
        ]
    );
}

#[test]
fn preserves_streamed_reasoning_signature() {
    let mut translator = AnthropicStreamTranslator::new();
    let mut actual = translate(
        &mut translator,
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "thinking", "thinking": "", "signature": ""}
        }),
    );
    let reasoning_id = match translator.active_block.as_ref() {
        Some(ActiveBlock::Thinking { id, .. }) => id.clone(),
        Some(ActiveBlock::Text { .. } | ActiveBlock::Tool { .. }) | None => {
            panic!("expected active reasoning block")
        }
    };
    actual.extend(translate(
        &mut translator,
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "consider"}
        }),
    ));
    actual.extend(translate(
        &mut translator,
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "signature_delta", "signature": "sig_stream"}
        }),
    ));
    actual.extend(translate(
        &mut translator,
        json!({"type": "content_block_stop", "index": 0}),
    ));

    assert_eq!(
        actual,
        vec![
            ObservedEvent::Added(ResponseItem::Reasoning {
                id: Some(reasoning_id.clone()),
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
            }),
            ObservedEvent::ReasoningPartAdded { summary_index: 0 },
            ObservedEvent::ReasoningDelta {
                delta: "consider".to_string(),
                summary_index: 0,
            },
            ObservedEvent::ReasoningDone {
                item_id: reasoning_id.to_string(),
                text: "consider".to_string(),
                summary_index: 0,
            },
            ObservedEvent::Done(ResponseItem::Reasoning {
                id: Some(reasoning_id),
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "consider".to_string(),
                }],
                content: None,
                encrypted_content: Some("sig_stream".to_string()),
                internal_chat_message_metadata_passthrough: None,
            }),
        ]
    );
}

#[test]
fn translates_function_and_custom_tool_streams() {
    let mut translator = AnthropicStreamTranslator::new();
    let mut actual = translate(
        &mut translator,
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_shell",
                "name": "exec_command"
            }
        }),
    );
    actual.extend(translate(
        &mut translator,
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "{\"cmd\":\"ls\"}"}
        }),
    ));
    actual.extend(translate(
        &mut translator,
        json!({"type": "content_block_stop", "index": 0}),
    ));
    actual.extend(translate(
        &mut translator,
        json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_patch",
                "name": "apply_patch"
            }
        }),
    ));
    actual.extend(translate(
        &mut translator,
        json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {
                "type": "input_json_delta",
                "partial_json": "{\"input\":\"*** Begin Patch\"}"
            }
        }),
    ));
    actual.extend(translate(
        &mut translator,
        json!({"type": "content_block_stop", "index": 1}),
    ));

    assert_eq!(
        actual,
        vec![
            ObservedEvent::Added(ResponseItem::FunctionCall {
                id: Some(ResponseItemId::from_server("fc_toolu_shell".to_string())),
                name: "exec_command".to_string(),
                namespace: None,
                arguments: String::new(),
                call_id: "toolu_shell".to_string(),
                provider_metadata: None,
                internal_chat_message_metadata_passthrough: None,
            }),
            ObservedEvent::Done(ResponseItem::FunctionCall {
                id: Some(ResponseItemId::from_server("fc_toolu_shell".to_string())),
                name: "exec_command".to_string(),
                namespace: None,
                arguments: "{\"cmd\":\"ls\"}".to_string(),
                call_id: "toolu_shell".to_string(),
                provider_metadata: None,
                internal_chat_message_metadata_passthrough: None,
            }),
            ObservedEvent::Added(ResponseItem::CustomToolCall {
                id: Some(ResponseItemId::from_server("ctc_toolu_patch".to_string())),
                status: Some("in_progress".to_string()),
                call_id: "toolu_patch".to_string(),
                name: "apply_patch".to_string(),
                namespace: None,
                input: String::new(),
                provider_metadata: None,
                internal_chat_message_metadata_passthrough: None,
            }),
            ObservedEvent::ToolDelta {
                item_id: "ctc_toolu_patch".to_string(),
                call_id: Some("toolu_patch".to_string()),
                delta: "*** Begin Patch".to_string(),
            },
            ObservedEvent::Done(ResponseItem::CustomToolCall {
                id: Some(ResponseItemId::from_server("ctc_toolu_patch".to_string())),
                status: Some("completed".to_string()),
                call_id: "toolu_patch".to_string(),
                name: "apply_patch".to_string(),
                namespace: None,
                input: "*** Begin Patch".to_string(),
                provider_metadata: None,
                internal_chat_message_metadata_passthrough: None,
            }),
        ]
    );
}

#[test]
fn maps_max_tokens_to_partial_completion() {
    let mut translator = AnthropicStreamTranslator::new();
    let response_id = translator.response_id.clone();
    let mut actual = translate(
        &mut translator,
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "max_tokens"},
            "usage": {
                "input_tokens": 69360,
                "output_tokens": 8192,
                "cache_read_input_tokens": 7857
            }
        }),
    );
    actual.extend(translate(&mut translator, json!({"type": "message_stop"})));

    assert_eq!(
        actual,
        vec![ObservedEvent::Completed {
            response_id,
            usage: Some(TokenUsage {
                input_tokens: 69360,
                cached_input_tokens: 7857,
                cache_write_input_tokens: 0,
                output_tokens: 8192,
                reasoning_output_tokens: 0,
                total_tokens: 77552,
            }),
            end_turn: Some(false),
        }]
    );
}

#[test]
fn ignores_unknown_events() {
    let mut translator = AnthropicStreamTranslator::new();

    let actual = translate(&mut translator, json!({"type": "ping"}));

    assert_eq!(actual, Vec::<ObservedEvent>::new());
}
