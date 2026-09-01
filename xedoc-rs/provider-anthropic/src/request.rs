use serde_json::Value;
use serde_json::json;
use xedoc_api::ResponsesApiRequest;
use xedoc_protocol::config_types::ReasoningSummary;
use xedoc_protocol::openai_models::ReasoningEffort;

use crate::history::append_continuation_if_needed;
use crate::history::translate_history;
use crate::types::AnthropicBlockBinding;
use crate::types::AnthropicMessagesRequest;
use crate::types::AnthropicOutputConfig;
use crate::types::AnthropicOutputFormat;
use crate::types::AnthropicSystemBlock;
use crate::types::AnthropicThinking;
use crate::types::AnthropicTool;
use crate::types::AnthropicToolChoice;

const DEFAULT_MAX_TOKENS: u32 = 8192;
const DEFAULT_ADAPTIVE_MAX_TOKENS: u32 = 65536;
const THINKING_OUTPUT_TOKEN_HEADROOM: u32 = 4096;

pub fn translate_request(
    request: &ResponsesApiRequest,
) -> Result<AnthropicMessagesRequest, serde_json::Error> {
    let model = resolve_model(&request.model);
    let mut translated = AnthropicMessagesRequest {
        model,
        max_tokens: DEFAULT_MAX_TOKENS,
        stream: request.stream,
        system: instructions(&request.instructions),
        messages: Vec::new(),
        tools: translate_tools(request.tools.as_deref()),
        tool_choice: translate_tool_choice(&request.tool_choice, request.parallel_tool_calls),
        thinking: None,
        output_config: translate_output_format(request),
    };

    apply_reasoning(request, &mut translated);
    reconcile_forced_tool_choice(&mut translated);
    translate_history(&request.input, &mut translated)?;
    append_continuation_if_needed(&mut translated.messages);
    Ok(translated)
}

fn resolve_model(model: &str) -> String {
    let trimmed = model.trim();
    let without_long_context = trimmed
        .strip_suffix("[1m]")
        .or_else(|| trimmed.strip_suffix("[1M]"))
        .unwrap_or(trimmed);
    match without_long_context {
        "fable" => "claude-fable-5",
        "opus" | "opus-4.8" | "claude-opus-4-7" => "claude-opus-4-8",
        "opus-5" => "claude-opus-5",
        "sonnet" | "claude-sonnet-4-6" => "claude-sonnet-5",
        "haiku" | "claude-haiku-4-5" => "claude-haiku-4-5-20251001",
        model => model,
    }
    .to_string()
}

fn instructions(instructions: &str) -> Vec<AnthropicSystemBlock> {
    if instructions.is_empty() {
        Vec::new()
    } else {
        vec![AnthropicSystemBlock::Text {
            text: instructions.to_string(),
        }]
    }
}

fn translate_tools(tools: Option<&[Value]>) -> Vec<AnthropicTool> {
    tools
        .unwrap_or_default()
        .iter()
        .filter_map(|tool| {
            let object = tool.as_object()?;
            let name = object.get("name")?.as_str()?;
            let description = object
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let input_schema = match object.get("type").and_then(Value::as_str) {
                Some("function") => object
                    .get("parameters")
                    .or_else(|| object.get("input_schema"))
                    .cloned()
                    .unwrap_or_else(empty_object_schema),
                Some("custom") if name == "apply_patch" => freeform_tool_schema(),
                _ => return None,
            };
            Some(AnthropicTool {
                name: name.to_string(),
                description,
                input_schema,
            })
        })
        .collect()
}

fn empty_object_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

fn freeform_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "input": {
                "type": "string",
                "description": "Raw input for the freeform tool."
            }
        },
        "required": ["input"],
        "additionalProperties": false
    })
}

fn translate_tool_choice(
    tool_choice: &str,
    parallel_tool_calls: bool,
) -> Option<AnthropicToolChoice> {
    let disable_parallel_tool_use = !parallel_tool_calls;
    match tool_choice {
        "auto" => Some(AnthropicToolChoice::Auto {
            disable_parallel_tool_use,
        }),
        "required" => Some(AnthropicToolChoice::Any {
            disable_parallel_tool_use,
        }),
        "none" => Some(AnthropicToolChoice::None {
            disable_parallel_tool_use,
        }),
        _ => None,
    }
}

fn translate_output_format(request: &ResponsesApiRequest) -> Option<AnthropicOutputConfig> {
    request
        .text
        .as_ref()
        .and_then(|text| text.format.as_ref())
        .map(|format| AnthropicOutputConfig {
            effort: None,
            format: Some(AnthropicOutputFormat::JsonSchema {
                schema: format.schema.clone(),
                name: format.name.clone(),
            }),
        })
}

fn apply_reasoning(request: &ResponsesApiRequest, translated: &mut AnthropicMessagesRequest) {
    let Some(reasoning) = request.reasoning.as_ref() else {
        return;
    };
    let Some(effort) = reasoning.effort.as_ref() else {
        return;
    };
    if effort == &ReasoningEffort::None {
        return;
    }

    let display = reasoning.summary.map(|summary| match summary {
        ReasoningSummary::Auto
        | ReasoningSummary::Concise
        | ReasoningSummary::Detailed
        | ReasoningSummary::None => "summarized".to_string(),
    });
    if supports_adaptive_thinking(&translated.model) {
        translated.thinking = Some(AnthropicThinking::Adaptive {
            display,
            block_binding: supports_prefix_locked_thinking(&translated.model).then(|| {
                AnthropicBlockBinding {
                    prefix_mismatch_behavior: "drop_block".to_string(),
                }
            }),
        });
        translated.max_tokens = DEFAULT_ADAPTIVE_MAX_TOKENS;
        translated
            .output_config
            .get_or_insert(AnthropicOutputConfig {
                effort: None,
                format: None,
            })
            .effort = Some(effort.as_str().to_string());
    } else {
        let budget_tokens = fixed_thinking_budget(effort);
        translated.thinking = Some(AnthropicThinking::Enabled {
            budget_tokens,
            display,
        });
        if !matches!(effort, ReasoningEffort::Ultra | ReasoningEffort::Custom(_))
            && translated.max_tokens <= budget_tokens
        {
            translated.max_tokens = budget_tokens + THINKING_OUTPUT_TOKEN_HEADROOM;
        }
    }
}

fn supports_adaptive_thinking(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("claude-melon-")
        || matches!(
            model.as_str(),
            "claude-opus-4-6"
                | "claude-opus-4-7"
                | "claude-opus-4-8"
                | "claude-opus-5"
                | "claude-sonnet-4-6"
                | "claude-sonnet-5"
                | "claude-fable-5"
        )
}

fn supports_prefix_locked_thinking(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("claude-melon-") || model.starts_with("claude-fable-")
}

fn fixed_thinking_budget(effort: &ReasoningEffort) -> u32 {
    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 512,
        ReasoningEffort::Low => 1024,
        ReasoningEffort::Medium => 8192,
        ReasoningEffort::High => 24576,
        ReasoningEffort::XHigh | ReasoningEffort::Max => 32768,
        ReasoningEffort::Ultra | ReasoningEffort::Custom(_) => 8192,
    }
}

fn reconcile_forced_tool_choice(translated: &mut AnthropicMessagesRequest) {
    let Some(tool_choice) = translated.tool_choice.as_ref() else {
        return;
    };
    let (forced_name, disable_parallel_tool_use) = match tool_choice {
        AnthropicToolChoice::Any {
            disable_parallel_tool_use,
        } => (None, *disable_parallel_tool_use),
        AnthropicToolChoice::Tool {
            name,
            disable_parallel_tool_use,
        } => (Some(name.clone()), *disable_parallel_tool_use),
        AnthropicToolChoice::Auto { .. } | AnthropicToolChoice::None { .. } => return,
    };

    if rejects_forced_tool_choice(&translated.model) {
        translated.tool_choice = Some(AnthropicToolChoice::Auto {
            disable_parallel_tool_use,
        });
        let text = forced_name.map_or_else(
            || "You must respond by calling one of the available tools.".to_string(),
            |name| format!("You must respond by calling the `{name}` tool."),
        );
        translated.system.push(AnthropicSystemBlock::Text { text });
    } else {
        translated.thinking = None;
    }
}

fn rejects_forced_tool_choice(model: &str) -> bool {
    supports_prefix_locked_thinking(model) && !model.eq_ignore_ascii_case("claude-fable-5")
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
