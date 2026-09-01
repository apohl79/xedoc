use base64::Engine;
use base64::engine::general_purpose::URL_SAFE;
use serde_json::Value;
use serde_json::json;
use xedoc_protocol::models::AgentMessageInputContent;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::FunctionCallOutputBody;
use xedoc_protocol::models::ReasoningItemReasoningSummary;
use xedoc_protocol::models::ResponseItem;

use crate::types::AnthropicContentBlock;
use crate::types::AnthropicImageSource;
use crate::types::AnthropicMessage;
use crate::types::AnthropicMessagesRequest;
use crate::types::AnthropicRole;
use crate::types::AnthropicSystemBlock;

const FERNET_MIN_RAW_BYTES: usize = 57;
const FERNET_CIPHERTEXT_BLOCK_BYTES: usize = 16;

pub(crate) fn translate_history(
    input: &[ResponseItem],
    translated: &mut AnthropicMessagesRequest,
) -> Result<(), serde_json::Error> {
    for item in input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                translate_message(role, content, translated);
            }
            ResponseItem::AgentMessage { content, .. } => {
                translate_agent_message(content, &mut translated.messages);
            }
            ResponseItem::Reasoning {
                summary,
                encrypted_content,
                ..
            } => append_reasoning(summary, encrypted_content, &mut translated.messages),
            ResponseItem::FunctionCall {
                id,
                name,
                arguments,
                call_id,
                ..
            } => {
                let call_id = if call_id.is_empty() {
                    id.as_ref().map_or(call_id.as_str(), |id| id.as_str())
                } else {
                    call_id
                };
                append_tool_use(
                    name,
                    call_id,
                    parse_tool_input(arguments),
                    &mut translated.messages,
                );
            }
            ResponseItem::CustomToolCall {
                id,
                call_id,
                name,
                input,
                ..
            } => {
                let call_id = if call_id.is_empty() {
                    id.as_ref().map_or(call_id.as_str(), |id| id.as_str())
                } else {
                    call_id
                };
                append_tool_use(
                    name,
                    call_id,
                    json!({"input": input}),
                    &mut translated.messages,
                );
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            }
            | ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let content = match &output.body {
                    FunctionCallOutputBody::Text(content) => content.clone(),
                    FunctionCallOutputBody::ContentItems(items) => serde_json::to_string(items)?,
                };
                append_tool_result(call_id, content, &mut translated.messages);
            }
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::CompactionTrigger {}
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => {}
        }
    }
    Ok(())
}

fn translate_message(
    role: &str,
    content: &[ContentItem],
    translated: &mut AnthropicMessagesRequest,
) {
    if role == "system" || role == "developer" {
        let text = extract_message_text(content);
        if text.is_empty() {
            return;
        }
        if translated.messages.is_empty() {
            translated.system.push(AnthropicSystemBlock::Text { text });
        } else {
            translated.messages.push(AnthropicMessage {
                role: AnthropicRole::User,
                content: vec![AnthropicContentBlock::Text { text }],
            });
        }
        return;
    }

    let role = match role {
        "user" => AnthropicRole::User,
        "assistant" => AnthropicRole::Assistant,
        _ => return,
    };
    let content = content.iter().filter_map(translate_content).collect();
    if role == AnthropicRole::Assistant {
        append_assistant_content(content, &mut translated.messages);
    } else if !content.is_empty() {
        translated.messages.push(AnthropicMessage { role, content });
    }
}

fn extract_message_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .map(|part| match part {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => text.as_str(),
            ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => "",
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn translate_content(content: &ContentItem) -> Option<AnthropicContentBlock> {
    match content {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
            Some(AnthropicContentBlock::Text { text: text.clone() })
        }
        ContentItem::InputImage { image_url, .. } => Some(AnthropicContentBlock::Image {
            source: translate_image(image_url),
        }),
        ContentItem::InputAudio { .. } => None,
    }
}

fn translate_image(url: &str) -> AnthropicImageSource {
    if let Some(data) = url.strip_prefix("data:")
        && let Some((media_type, data)) = data.split_once(";base64,")
        && !media_type.is_empty()
        && !data.is_empty()
    {
        return AnthropicImageSource::Base64 {
            media_type: media_type.to_string(),
            data: data.to_string(),
        };
    }
    AnthropicImageSource::Url {
        url: url.to_string(),
    }
}

fn translate_agent_message(
    content: &[AgentMessageInputContent],
    messages: &mut Vec<AnthropicMessage>,
) {
    let text = content
        .iter()
        .filter_map(|part| match part {
            AgentMessageInputContent::InputText { text } if !text.is_empty() => Some(text.clone()),
            AgentMessageInputContent::EncryptedContent { encrypted_content }
                if !encrypted_content.is_empty() =>
            {
                Some(if is_fernet_token(encrypted_content) {
                    "[encrypted inter-agent content]".to_string()
                } else {
                    encrypted_content.clone()
                })
            }
            AgentMessageInputContent::InputText { .. }
            | AgentMessageInputContent::EncryptedContent { .. } => None,
        })
        .fold(String::new(), |mut joined, part| {
            if !joined.is_empty() && !joined.ends_with('\n') && !part.starts_with('\n') {
                joined.push('\n');
            }
            joined.push_str(&part);
            joined
        });
    if !text.is_empty() {
        messages.push(AnthropicMessage {
            role: AnthropicRole::Assistant,
            content: vec![AnthropicContentBlock::Text { text }],
        });
    }
}

fn is_fernet_token(value: &str) -> bool {
    let unpadded = value.trim_end_matches('=');
    let padding = value.len() - unpadded.len();
    if !value.starts_with("gAAAA")
        || padding > 2
        || unpadded.contains('=')
        || value.chars().any(|character| {
            !character.is_ascii_alphanumeric() && !matches!(character, '_' | '-' | '=')
        })
    {
        return false;
    }
    let mut padded = value.to_string();
    padded.extend(std::iter::repeat_n('=', (4 - value.len() % 4) % 4));
    URL_SAFE.decode(padded).is_ok_and(|decoded| {
        decoded.len() >= FERNET_MIN_RAW_BYTES
            && decoded[0] == 0x80
            && (decoded.len() - FERNET_MIN_RAW_BYTES).is_multiple_of(FERNET_CIPHERTEXT_BLOCK_BYTES)
    })
}

fn append_reasoning(
    summary: &[ReasoningItemReasoningSummary],
    encrypted_content: &Option<String>,
    messages: &mut Vec<AnthropicMessage>,
) {
    let thinking = summary
        .iter()
        .map(|part| match part {
            ReasoningItemReasoningSummary::SummaryText { text } => text.as_str(),
        })
        .collect::<String>();
    if thinking.is_empty() && encrypted_content.as_deref().unwrap_or_default().is_empty() {
        return;
    }
    append_assistant_content(
        vec![AnthropicContentBlock::Thinking {
            thinking,
            signature: encrypted_content
                .clone()
                .filter(|signature| !signature.is_empty()),
        }],
        messages,
    );
}

fn parse_tool_input(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}

fn append_tool_use(name: &str, call_id: &str, input: Value, messages: &mut Vec<AnthropicMessage>) {
    append_assistant_content(
        vec![AnthropicContentBlock::ToolUse {
            id: call_id.to_string(),
            name: name.to_string(),
            input,
        }],
        messages,
    );
}

fn append_assistant_content(
    content: Vec<AnthropicContentBlock>,
    messages: &mut Vec<AnthropicMessage>,
) {
    if content.is_empty() {
        return;
    }
    if let Some(previous) = messages.last_mut()
        && previous.role == AnthropicRole::Assistant
    {
        previous.content.extend(content);
        return;
    }
    messages.push(AnthropicMessage {
        role: AnthropicRole::Assistant,
        content,
    });
}

fn append_tool_result(call_id: &str, content: String, messages: &mut Vec<AnthropicMessage>) {
    let block = AnthropicContentBlock::ToolResult {
        tool_use_id: call_id.to_string(),
        content,
    };
    if let Some(previous) = messages.last_mut()
        && previous.role == AnthropicRole::User
        && previous
            .content
            .iter()
            .all(|block| matches!(block, AnthropicContentBlock::ToolResult { .. }))
    {
        previous.content.push(block);
        return;
    }
    messages.push(AnthropicMessage {
        role: AnthropicRole::User,
        content: vec![block],
    });
}

pub(crate) fn append_continuation_if_needed(messages: &mut Vec<AnthropicMessage>) {
    let Some(last) = messages.last() else {
        return;
    };
    let ends_with_tool_use = last
        .content
        .iter()
        .any(|block| matches!(block, AnthropicContentBlock::ToolUse { .. }));
    if last.role == AnthropicRole::Assistant && !ends_with_tool_use {
        messages.push(AnthropicMessage {
            role: AnthropicRole::User,
            content: vec![AnthropicContentBlock::Text {
                text: "Continue.".to_string(),
            }],
        });
    }
}
