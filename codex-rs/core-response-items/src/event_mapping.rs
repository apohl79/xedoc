use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::ReasoningItem;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::items::WebSearchItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::WebSearchAction;
use codex_protocol::models::is_audio_close_tag_text;
use codex_protocol::models::is_audio_open_tag_text;
use codex_protocol::models::is_image_close_tag_text;
use codex_protocol::models::is_image_open_tag_text;
use codex_protocol::models::is_local_audio_close_tag_text;
use codex_protocol::models::is_local_audio_open_tag_text;
use codex_protocol::models::is_local_image_close_tag_text;
use codex_protocol::models::is_local_image_open_tag_text;
use codex_protocol::user_input::UserInput;
use tracing::warn;
use uuid::Uuid;

use crate::web_search::web_search_action_detail;
use codex_core_context::parse_visible_hook_prompt_message;

use codex_core_context_manager::is_contextual_user_message_content;
fn parse_user_message(message: &[ContentItem]) -> Option<UserMessageItem> {
    if is_contextual_user_message_content(message) {
        return None;
    }

    let mut content: Vec<UserInput> = Vec::new();

    for (idx, content_item) in message.iter().enumerate() {
        match content_item {
            ContentItem::InputText { text } => {
                let is_image_label = ((is_local_image_open_tag_text(text)
                    || is_image_open_tag_text(text))
                    && matches!(message.get(idx + 1), Some(ContentItem::InputImage { .. })))
                    || (idx > 0
                        && (is_local_image_close_tag_text(text) || is_image_close_tag_text(text))
                        && matches!(message.get(idx - 1), Some(ContentItem::InputImage { .. })));
                let is_audio_label = ((is_local_audio_open_tag_text(text)
                    || is_audio_open_tag_text(text))
                    && matches!(message.get(idx + 1), Some(ContentItem::InputAudio { .. })))
                    || (idx > 0
                        && (is_local_audio_close_tag_text(text) || is_audio_close_tag_text(text))
                        && matches!(message.get(idx - 1), Some(ContentItem::InputAudio { .. })));
                if is_image_label || is_audio_label {
                    continue;
                }
                content.push(UserInput::Text {
                    text: text.clone(),
                    // Model input content does not carry UI element ranges.
                    text_elements: Vec::new(),
                });
            }
            ContentItem::InputImage { image_url, detail } => {
                content.push(UserInput::Image {
                    image_url: image_url.clone(),
                    detail: *detail,
                });
            }
            ContentItem::InputAudio { audio_url } => {
                content.push(UserInput::Audio {
                    audio_url: audio_url.clone(),
                });
            }
            ContentItem::OutputText { text } => {
                warn!("Output text in user message: {}", text);
            }
        }
    }

    Some(UserMessageItem::new(&content))
}

fn parse_agent_message(
    id: Option<&str>,
    message: &[ContentItem],
    phase: Option<MessagePhase>,
) -> AgentMessageItem {
    let mut content: Vec<AgentMessageContent> = Vec::new();
    for content_item in message.iter() {
        match content_item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                content.push(AgentMessageContent::Text { text: text.clone() });
            }
            _ => {
                warn!(
                    "Unexpected content item in agent message: {:?}",
                    content_item
                );
            }
        }
    }
    let id = id
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    AgentMessageItem {
        id,
        content,
        phase,
        memory_citation: None,
    }
}

pub fn parse_turn_item(item: &ResponseItem) -> Option<TurnItem> {
    match item {
        ResponseItem::Message {
            role,
            content,
            id,
            phase,
            ..
        } => match role.as_str() {
            "user" => parse_visible_hook_prompt_message(id.as_deref(), content)
                .map(TurnItem::HookPrompt)
                .or_else(|| parse_user_message(content).map(TurnItem::UserMessage)),
            "assistant" => Some(TurnItem::AgentMessage(parse_agent_message(
                id.as_deref(),
                content,
                phase.clone(),
            ))),
            "system" => None,
            _ => None,
        },
        ResponseItem::Reasoning {
            id,
            summary,
            content,
            ..
        } => {
            let summary_text = summary
                .iter()
                .map(|entry| match entry {
                    ReasoningItemReasoningSummary::SummaryText { text } => text.clone(),
                })
                .collect();
            let raw_content = content
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|entry| match entry {
                    ReasoningItemContent::ReasoningText { text }
                    | ReasoningItemContent::Text { text } => text,
                })
                .collect();
            Some(TurnItem::Reasoning(ReasoningItem {
                id: id.as_deref().unwrap_or_default().to_string(),
                summary_text,
                raw_content,
            }))
        }
        ResponseItem::WebSearchCall { id, action, .. } => {
            let (action, query) = match action {
                Some(action) => (action.clone(), web_search_action_detail(action)),
                None => (WebSearchAction::Other, String::new()),
            };
            Some(TurnItem::WebSearch(WebSearchItem {
                id: id.as_deref().unwrap_or_default().to_string(),
                query,
                action,
                results: None,
            }))
        }
        ResponseItem::ImageGenerationCall {
            id,
            status,
            revised_prompt,
            result,
            ..
        } => Some(TurnItem::ImageGeneration(
            codex_protocol::items::ImageGenerationItem {
                id: id.as_deref()?.to_string(),
                status: status.clone(),
                revised_prompt: revised_prompt.clone(),
                result: result.clone(),
                saved_path: None,
            },
        )),
        _ => None,
    }
}
