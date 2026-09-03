use std::collections::HashMap;

use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use xedoc_api::ResponsesApiRequest;
use xedoc_protocol::models::AgentMessageInputContent;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::FunctionCallOutputBody;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::provider_item_metadata::ProviderItemMetadata;

use crate::GeminiContent;
use crate::GeminiFunctionCall;
use crate::GeminiFunctionDeclaration;
use crate::GeminiFunctionResponse;
use crate::GeminiFunctionResponseBody;
use crate::GeminiGenerateContentRequest;
use crate::GeminiGenerationConfig;
use crate::GeminiInlineData;
use crate::GeminiPart;
use crate::GeminiRole;
use crate::GeminiSystemInstruction;
use crate::GeminiThinkingConfig;
use crate::GeminiThinkingLevel;
use crate::GeminiThoughtSignatureStore;
use crate::GeminiTool;
use crate::GeminiToolConfig;
use crate::GeminiToolMode;
use crate::types::GeminiFunctionCallingConfig;

const CUSTOM_TOOL_NAME: &str = "apply_patch";

pub fn translate_request(
    request: &ResponsesApiRequest,
    thought_signatures: &GeminiThoughtSignatureStore,
) -> Result<GeminiGenerateContentRequest, serde_json::Error> {
    let mut system_texts = (!request.instructions.is_empty())
        .then(|| request.instructions.clone())
        .into_iter()
        .collect::<Vec<_>>();
    let contents = translate_history(&request.input, &mut system_texts, thought_signatures)?;
    Ok(GeminiGenerateContentRequest {
        model: request.model.trim().to_string(),
        stream: request.stream,
        contents,
        system_instruction: system_instruction(system_texts),
        tools: translate_tools(request.tools.as_deref()),
        tool_config: translate_tool_choice(&request.tool_choice),
        generation_config: generation_config(request),
    })
}

fn translate_history(
    input: &[ResponseItem],
    system_texts: &mut Vec<String>,
    thought_signatures: &GeminiThoughtSignatureStore,
) -> Result<Vec<GeminiContent>, serde_json::Error> {
    let mut contents = Vec::new();
    let mut function_names = HashMap::new();
    for item in input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                append_message(role, content, system_texts, &mut contents);
            }
            ResponseItem::AgentMessage { content, .. } => {
                append_content(
                    &mut contents,
                    GeminiRole::User,
                    agent_message_parts(content),
                );
            }
            ResponseItem::FunctionCall {
                id,
                name,
                arguments,
                call_id,
                provider_metadata,
                ..
            } => {
                let call_id = resolved_call_id(
                    id.as_ref().map(xedoc_protocol::ResponseItemId::as_str),
                    call_id,
                );
                function_names.insert(call_id.to_string(), name.to_string());
                append_content(
                    &mut contents,
                    GeminiRole::Model,
                    vec![function_call_part(
                        name,
                        parse_arguments(arguments),
                        replay_thought_signature(
                            provider_metadata.as_ref(),
                            thought_signatures,
                            call_id,
                        ),
                    )],
                );
            }
            ResponseItem::CustomToolCall {
                id,
                call_id,
                name,
                input,
                provider_metadata,
                ..
            } => {
                let call_id = resolved_call_id(
                    id.as_ref().map(xedoc_protocol::ResponseItemId::as_str),
                    call_id,
                );
                function_names.insert(call_id.to_string(), name.to_string());
                append_content(
                    &mut contents,
                    GeminiRole::Model,
                    vec![function_call_part(
                        name,
                        json!({"input": input}),
                        replay_thought_signature(
                            provider_metadata.as_ref(),
                            thought_signatures,
                            call_id,
                        ),
                    )],
                );
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            }
            | ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let output = match &output.body {
                    FunctionCallOutputBody::Text(output) => output.clone(),
                    FunctionCallOutputBody::ContentItems(items) => serde_json::to_string(items)?,
                };
                append_content(
                    &mut contents,
                    GeminiRole::User,
                    vec![GeminiPart::FunctionResponse {
                        function_response: GeminiFunctionResponse {
                            name: function_names
                                .get(call_id)
                                .map_or_else(|| "tool".to_string(), String::clone),
                            response: GeminiFunctionResponseBody { output },
                        },
                    }],
                );
            }
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::Reasoning { .. }
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
    Ok(contents)
}

fn replay_thought_signature(
    provider_metadata: Option<&ProviderItemMetadata>,
    thought_signatures: &GeminiThoughtSignatureStore,
    call_id: &str,
) -> Option<String> {
    provider_metadata
        .and_then(ProviderItemMetadata::gemini_thought_signature)
        .filter(|signature| !signature.is_empty())
        .map(str::to_string)
        .or_else(|| thought_signatures.signature(call_id))
}

fn append_message(
    role: &str,
    content: &[ContentItem],
    system_texts: &mut Vec<String>,
    contents: &mut Vec<GeminiContent>,
) {
    if role == "system" || role == "developer" {
        let text = content_text(content);
        if !text.is_empty() {
            system_texts.push(text);
        }
        return;
    }
    let role = match role {
        "assistant" | "model" => GeminiRole::Model,
        "user" => GeminiRole::User,
        _ => return,
    };
    append_content(contents, role, content_parts(content));
}

fn content_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => None,
        })
        .collect()
}

fn content_parts(content: &[ContentItem]) -> Vec<GeminiPart> {
    content
        .iter()
        .filter_map(|part| match part {
            ContentItem::InputText { text } | ContentItem::OutputText { text }
                if !text.is_empty() =>
            {
                Some(GeminiPart::Text { text: text.clone() })
            }
            ContentItem::InputImage { image_url, .. } => inline_data(image_url),
            ContentItem::InputText { .. }
            | ContentItem::OutputText { .. }
            | ContentItem::InputAudio { .. } => None,
        })
        .collect()
}

fn agent_message_parts(content: &[AgentMessageInputContent]) -> Vec<GeminiPart> {
    content
        .iter()
        .filter_map(|part| match part {
            AgentMessageInputContent::InputText { text } if !text.is_empty() => {
                Some(GeminiPart::Text { text: text.clone() })
            }
            AgentMessageInputContent::InputText { .. }
            | AgentMessageInputContent::EncryptedContent { .. } => None,
        })
        .collect()
}

fn inline_data(image_url: &str) -> Option<GeminiPart> {
    let data = image_url.strip_prefix("data:")?;
    let (mime_type, data) = data.split_once(";base64,")?;
    if mime_type.is_empty() || data.is_empty() {
        return None;
    }
    Some(GeminiPart::InlineData {
        inline_data: GeminiInlineData {
            mime_type: mime_type.to_string(),
            data: data.to_string(),
        },
    })
}

fn append_content(contents: &mut Vec<GeminiContent>, role: GeminiRole, parts: Vec<GeminiPart>) {
    if parts.is_empty() {
        return;
    }
    if let Some(previous) = contents.last_mut()
        && previous.role == role
    {
        previous.parts.extend(parts);
        return;
    }
    contents.push(GeminiContent { role, parts });
}

fn resolved_call_id<'a>(id: Option<&'a str>, call_id: &'a str) -> &'a str {
    if call_id.is_empty() {
        id.unwrap_or(call_id)
    } else {
        call_id
    }
}

fn parse_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}

fn function_call_part(name: &str, args: Value, thought_signature: Option<String>) -> GeminiPart {
    GeminiPart::FunctionCall {
        function_call: GeminiFunctionCall {
            name: name.to_string(),
            args,
        },
        thought_signature,
    }
}

fn system_instruction(system_texts: Vec<String>) -> Option<GeminiSystemInstruction> {
    let text = system_texts
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then(|| GeminiSystemInstruction {
        parts: vec![GeminiPart::Text { text }],
    })
}

fn translate_tools(tools: Option<&[Value]>) -> Vec<GeminiTool> {
    let function_declarations = tools
        .unwrap_or_default()
        .iter()
        .filter_map(function_declaration)
        .collect::<Vec<_>>();
    if function_declarations.is_empty() {
        Vec::new()
    } else {
        vec![GeminiTool {
            function_declarations,
        }]
    }
}

fn function_declaration(tool: &Value) -> Option<GeminiFunctionDeclaration> {
    let object = tool.as_object()?;
    let name = object.get("name")?.as_str()?;
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let parameters = match object.get("type").and_then(Value::as_str) {
        Some("function") => object
            .get("parameters")
            .or_else(|| object.get("input_schema"))
            .cloned()
            .unwrap_or_else(empty_object_schema),
        Some("custom") if name == CUSTOM_TOOL_NAME => custom_tool_schema(),
        _ => return None,
    };
    Some(GeminiFunctionDeclaration {
        name: name.to_string(),
        description,
        parameters: strip_additional_properties(parameters),
    })
}

fn empty_object_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

fn custom_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"input": {"type": "string"}},
        "required": ["input"],
        "additionalProperties": false
    })
}

fn strip_additional_properties(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(strip_additional_properties)
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .filter(|(key, _)| key != "additionalProperties")
                .map(|(key, value)| (key, strip_additional_properties(value)))
                .collect::<Map<_, _>>(),
        ),
        value => value,
    }
}

fn translate_tool_choice(tool_choice: &str) -> Option<GeminiToolConfig> {
    let mode = match tool_choice {
        "none" => GeminiToolMode::None,
        "required" => GeminiToolMode::Any,
        _ => return None,
    };
    Some(GeminiToolConfig {
        function_calling_config: GeminiFunctionCallingConfig { mode },
    })
}

fn generation_config(request: &ResponsesApiRequest) -> Option<GeminiGenerationConfig> {
    let output_schema = request
        .text
        .as_ref()
        .and_then(|text| text.format.as_ref())
        .map(|format| format.schema.clone());
    let thinking_level = request
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.effort.as_ref())
        .and_then(thinking_level);
    if output_schema.is_none() && thinking_level.is_none() {
        return None;
    }
    Some(GeminiGenerationConfig {
        response_mime_type: output_schema
            .as_ref()
            .map(|_| "application/json".to_string()),
        response_json_schema: output_schema,
        thinking_config: thinking_level
            .map(|thinking_level| GeminiThinkingConfig { thinking_level }),
    })
}

fn thinking_level(effort: &ReasoningEffort) -> Option<GeminiThinkingLevel> {
    match effort {
        ReasoningEffort::Minimal | ReasoningEffort::Low => Some(GeminiThinkingLevel::Low),
        ReasoningEffort::Medium => Some(GeminiThinkingLevel::Medium),
        ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max => {
            Some(GeminiThinkingLevel::High)
        }
        ReasoningEffort::None | ReasoningEffort::Ultra | ReasoningEffort::Custom(_) => None,
    }
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
