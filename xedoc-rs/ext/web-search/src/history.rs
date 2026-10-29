use xedoc_api::SearchInput;
use xedoc_core::parse_turn_item;
use xedoc_protocol::items::TurnItem;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::models::plaintext_agent_message_content;
use xedoc_tools::retain_tail_from_last_n_user_messages;
use xedoc_tools::truncate_assistant_output_text_to_token_budget;

const ASSISTANT_CONTEXT_TOKEN_LIMIT: usize = 1_000;
const ASSISTANT_ROLE: &str = "assistant";
const USER_ROLE: &str = "user";

/// Builds the conversation tail for standalone web search.
///
/// The tail keeps the previous user text message, up to 1k tokens of assistant
/// text that followed it, and the current user text message.
pub(crate) fn recent_input(items: &[ResponseItem]) -> Option<SearchInput> {
    let mut messages = Vec::new();
    for item in items {
        push_visible_message(&mut messages, item);
    }

    retain_tail_from_last_n_user_messages(&mut messages, /*user_message_count*/ 2);
    truncate_assistant_output_text_to_token_budget(&mut messages, ASSISTANT_CONTEXT_TOKEN_LIMIT);
    (!messages.is_empty()).then_some(SearchInput::Items(messages))
}

fn push_visible_message(messages: &mut Vec<ResponseItem>, item: &ResponseItem) {
    match item {
        ResponseItem::Message { role, .. } if role == ASSISTANT_ROLE => {
            let mut message = item.clone();
            message.set_id(/*new_id*/ None);
            messages.push(message);
        }
        ResponseItem::AgentMessage {
            author,
            content,
            internal_chat_message_metadata_passthrough: metadata,
            ..
        } => {
            if let Some(text) = plaintext_agent_message_content(content) {
                messages.push(ResponseItem::Message {
                    id: None,
                    role: ASSISTANT_ROLE.to_string(),
                    content: vec![ContentItem::OutputText {
                        text: format!("Agent message from {author}:\n{text}"),
                    }],
                    phase: None,
                    internal_chat_message_metadata_passthrough: metadata.clone(),
                });
            }
        }
        ResponseItem::Message {
            id: _,
            role,
            content,
            phase,
            internal_chat_message_metadata_passthrough: metadata,
        } if role == USER_ROLE
            && matches!(parse_turn_item(item), Some(TurnItem::UserMessage(_))) =>
        {
            let content = content
                .iter()
                .filter(|item| matches!(item, ContentItem::InputText { .. }))
                .cloned()
                .collect::<Vec<_>>();
            if !content.is_empty() {
                messages.push(ResponseItem::Message {
                    id: None,
                    role: role.clone(),
                    content,
                    phase: phase.clone(),
                    internal_chat_message_metadata_passthrough: metadata.clone(),
                });
            }
        }
        _ => {}
    }
}
