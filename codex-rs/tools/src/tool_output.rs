use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_utils_string::take_bytes_at_char_boundary;
use serde_json::Value as JsonValue;

use crate::ToolPayload;

const TELEMETRY_PREVIEW_MAX_BYTES: usize = 2 * 1024;
const TELEMETRY_PREVIEW_MAX_LINES: usize = 64;
const TELEMETRY_PREVIEW_TRUNCATION_NOTICE: &str = "[... telemetry preview truncated ...]";

/// Model-facing output contract returned by executable tool runtimes.
pub trait ToolOutput: Send {
    fn log_preview(&self) -> String;

    fn success_for_logging(&self) -> bool;

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem;

    /// Returns the tool call id exposed to `PostToolUse` hooks for this output.
    fn post_tool_use_id(&self, call_id: &str) -> String {
        call_id.to_string()
    }

    /// Returns the tool input exposed to `PostToolUse` hooks for this output.
    fn post_tool_use_input(&self, _payload: &ToolPayload) -> Option<JsonValue> {
        None
    }

    /// Returns the stable value exposed to `PostToolUse` hooks for this tool output.
    ///
    /// Tool handlers decide whether a tool participates in `PostToolUse`, but
    /// this method lets the output type own any conversion from model-facing
    /// response content to hook-facing data. Returning `None` means the output
    /// should not produce a post-use hook payload, not merely that the tool had
    /// empty output.
    fn post_tool_use_response(&self, _call_id: &str, _payload: &ToolPayload) -> Option<JsonValue> {
        None
    }
}

impl<T> ToolOutput for Box<T>
where
    T: ToolOutput + ?Sized,
{
    fn log_preview(&self) -> String {
        (**self).log_preview()
    }

    fn success_for_logging(&self) -> bool {
        (**self).success_for_logging()
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        (**self).to_response_item(call_id, payload)
    }

    fn post_tool_use_id(&self, call_id: &str) -> String {
        (**self).post_tool_use_id(call_id)
    }

    fn post_tool_use_input(&self, payload: &ToolPayload) -> Option<JsonValue> {
        (**self).post_tool_use_input(payload)
    }

    fn post_tool_use_response(&self, call_id: &str, payload: &ToolPayload) -> Option<JsonValue> {
        (**self).post_tool_use_response(call_id, payload)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JsonToolOutput {
    value: JsonValue,
    success: Option<bool>,
}

impl JsonToolOutput {
    pub fn new(value: JsonValue) -> Self {
        Self {
            value,
            success: Some(true),
        }
    }

    pub fn with_success(value: JsonValue, success: Option<bool>) -> Self {
        Self { value, success }
    }
}

impl ToolOutput for JsonToolOutput {
    fn log_preview(&self) -> String {
        telemetry_preview(&self.value.to_string())
    }

    fn success_for_logging(&self) -> bool {
        self.success.unwrap_or(true)
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        let output = FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(self.value.to_string()),
            success: self.success,
        };

        if matches!(payload, ToolPayload::Custom { .. }) {
            return ResponseInputItem::CustomToolCallOutput {
                call_id: call_id.to_string(),
                name: None,
                output,
            };
        }

        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output,
        }
    }

    fn post_tool_use_response(&self, _call_id: &str, _payload: &ToolPayload) -> Option<JsonValue> {
        Some(self.value.clone())
    }
}

impl ToolOutput for codex_protocol::mcp::CallToolResult {
    fn log_preview(&self) -> String {
        let output = self.as_function_call_output_payload();
        let preview = output.body.to_text().unwrap_or_else(|| output.to_string());
        telemetry_preview(&preview)
    }

    fn success_for_logging(&self) -> bool {
        self.success()
    }

    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        ResponseInputItem::McpToolCallOutput {
            call_id: call_id.to_string(),
            output: self.clone(),
        }
    }
}

fn telemetry_preview(content: &str) -> String {
    let truncated_slice = take_bytes_at_char_boundary(content, TELEMETRY_PREVIEW_MAX_BYTES);
    let truncated_by_bytes = truncated_slice.len() < content.len();

    let mut preview = String::new();
    let mut lines_iter = truncated_slice.lines();
    for idx in 0..TELEMETRY_PREVIEW_MAX_LINES {
        match lines_iter.next() {
            Some(line) => {
                if idx > 0 {
                    preview.push('\n');
                }
                preview.push_str(line);
            }
            None => break,
        }
    }
    let truncated_by_lines = lines_iter.next().is_some();

    if !truncated_by_bytes && !truncated_by_lines {
        return content.to_string();
    }

    if preview.len() < truncated_slice.len()
        && truncated_slice
            .as_bytes()
            .get(preview.len())
            .is_some_and(|byte| *byte == b'\n')
    {
        preview.push('\n');
    }

    if !preview.is_empty() && !preview.ends_with('\n') {
        preview.push('\n');
    }
    preview.push_str(TELEMETRY_PREVIEW_TRUNCATION_NOTICE);

    preview
}
