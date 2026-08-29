//! Normalizes completed stream items before session orchestration persists them.

use codex_protocol::items::TurnItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_utils_stream_parser::strip_citations;
use codex_utils_stream_parser::strip_proposed_plan_blocks;

/// Returns the concatenated text from an assistant message response item.
#[doc(hidden)]
pub fn raw_assistant_output_text_from_item(item: &ResponseItem) -> Option<String> {
    if let ResponseItem::Message { role, content, .. } = item
        && role == "assistant"
    {
        let combined = content
            .iter()
            .filter_map(|ci| match ci {
                codex_protocol::models::ContentItem::OutputText { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        return Some(combined);
    }
    None
}

/// Rewrites visible assistant output with citations and plan blocks stripped.
#[doc(hidden)]
pub fn sanitize_agent_message(turn_item: &mut TurnItem, plan_mode: bool) {
    if let TurnItem::AgentMessage(agent_message) = turn_item {
        let combined = agent_message
            .content
            .iter()
            .map(|entry| match entry {
                codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
            })
            .collect::<String>();
        let (without_citations, _) = strip_citations(&combined);
        let stripped = if plan_mode {
            strip_proposed_plan_blocks(&without_citations)
        } else {
            without_citations
        };
        agent_message.content =
            vec![codex_protocol::items::AgentMessageContent::Text { text: stripped }];
    }
}

/// Returns visible assistant text from a response item, if it remains after filtering.
#[doc(hidden)]
pub fn last_assistant_message_from_item(item: &ResponseItem, plan_mode: bool) -> Option<String> {
    let combined = raw_assistant_output_text_from_item(item)?;
    if combined.is_empty() {
        return None;
    }
    let (without_citations, _) = strip_citations(&combined);
    let stripped = if plan_mode {
        strip_proposed_plan_blocks(&without_citations)
    } else {
        without_citations
    };
    (!stripped.trim().is_empty()).then_some(stripped)
}

/// Whether a completed response item should defer mailbox delivery to another turn.
#[doc(hidden)]
pub fn completed_item_defers_mailbox_delivery_to_next_turn(
    item: &ResponseItem,
    plan_mode: bool,
) -> bool {
    match item {
        ResponseItem::Message { role, phase, .. } => {
            role == "assistant"
                && !matches!(phase, Some(MessagePhase::Commentary))
                && last_assistant_message_from_item(item, plan_mode).is_some()
        }
        _ => false,
    }
}

/// Converts a model-input tool output into the equivalent persisted response item.
#[doc(hidden)]
pub fn response_input_to_response_item(input: &ResponseInputItem) -> Option<ResponseItem> {
    match input {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            Some(ResponseItem::FunctionCallOutput {
                id: None,
                call_id: call_id.clone(),
                output: output.clone(),
                internal_chat_message_metadata_passthrough: None,
            })
        }
        ResponseInputItem::CustomToolCallOutput {
            call_id,
            name,
            output,
        } => Some(ResponseItem::CustomToolCallOutput {
            id: None,
            call_id: call_id.clone(),
            name: name.clone(),
            output: output.clone(),
            internal_chat_message_metadata_passthrough: None,
        }),
        ResponseInputItem::McpToolCallOutput { call_id, output } => {
            let output = output.as_function_call_output_payload();
            Some(ResponseItem::FunctionCallOutput {
                id: None,
                call_id: call_id.clone(),
                output,
                internal_chat_message_metadata_passthrough: None,
            })
        }
        ResponseInputItem::ToolSearchOutput {
            call_id,
            status,
            execution,
            tools,
        } => Some(ResponseItem::ToolSearchOutput {
            id: None,
            call_id: Some(call_id.clone()),
            status: status.clone(),
            execution: execution.clone(),
            tools: tools.clone(),
            internal_chat_message_metadata_passthrough: None,
        }),
        _ => None,
    }
}
