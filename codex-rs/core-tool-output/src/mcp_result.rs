//! MCP result transformations for model and event consumers.

use codex_protocol::mcp::CallToolResult;
use codex_protocol::openai_models::InputModality;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;

/// Replaces unsupported MCP media content with explicit text placeholders.
#[doc(hidden)]
pub fn sanitize_mcp_tool_result_for_model(
    input_modalities: &[InputModality],
    result: Result<CallToolResult, String>,
) -> Result<CallToolResult, String> {
    let supports_image_input = input_modalities.contains(&InputModality::Image);
    let supports_audio_input = input_modalities.contains(&InputModality::Audio);
    if supports_image_input && supports_audio_input {
        return result;
    }

    result.map(|call_tool_result| CallToolResult {
        content: call_tool_result
            .content
            .iter()
            .map(|block| {
                if let Some(content_type) = block.get("type").and_then(serde_json::Value::as_str) {
                    if content_type == "image" && !supports_image_input {
                        return serde_json::json!({
                            "type": "text",
                            "text": "<image content omitted because you do not support image input>",
                        });
                    }
                    if content_type == "audio" && !supports_audio_input {
                        return serde_json::json!({
                            "type": "text",
                            "text": "<audio content omitted because you do not support audio input>",
                        });
                    }
                }

                block.clone()
            })
            .collect(),
        structured_content: call_tool_result.structured_content,
        is_error: call_tool_result.is_error,
        meta: call_tool_result.meta,
    })
}

/// Bounds the event-safe copy of an MCP result or error.
#[doc(hidden)]
pub fn truncate_mcp_tool_result_for_event(
    result: &Result<CallToolResult, String>,
    max_bytes: usize,
) -> Result<CallToolResult, String> {
    match result {
        Ok(call_tool_result) => {
            let Ok(serialized) = serde_json::to_string(call_tool_result) else {
                return Ok(call_tool_result.clone());
            };
            if serialized.len() <= max_bytes {
                return Ok(call_tool_result.clone());
            }

            let truncated = truncate_text(&serialized, TruncationPolicy::Bytes(max_bytes));
            Ok(CallToolResult {
                content: vec![serde_json::json!({
                    "type": "text",
                    "text": truncated,
                })],
                structured_content: None,
                is_error: call_tool_result.is_error,
                meta: None,
            })
        }
        Err(message) => Err(truncate_text(message, TruncationPolicy::Bytes(max_bytes))),
    }
}

#[cfg(test)]
#[path = "mcp_result_tests.rs"]
mod tests;
