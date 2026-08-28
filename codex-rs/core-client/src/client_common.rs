pub use codex_api::ResponseEvent;
use codex_protocol::error::Result;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_tools::ToolSpec;
use codex_tools::create_tools_json_for_responses_api;
use codex_utils_output_truncation::approx_token_count;
use futures::Stream;
use serde_json::Value;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// API request payload for a single model turn
#[derive(Debug, Clone)]
pub struct Prompt {
    /// Conversation context input items.
    pub input: Vec<ResponseItem>,

    /// Tools available to the model, including additional tools sourced from
    /// external MCP servers.
    pub tools: Vec<ToolSpec>,

    /// Whether parallel tool calls are permitted for this prompt.
    pub parallel_tool_calls: bool,

    pub base_instructions: BaseInstructions,

    /// Optional the output schema for the model's response.
    pub output_schema: Option<Value>,

    /// Whether the Responses API should strictly validate `output_schema`.
    pub output_schema_strict: bool,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            input: Vec::new(),
            tools: Vec::new(),
            parallel_tool_calls: false,
            base_instructions: BaseInstructions::default(),
            output_schema: None,
            output_schema_strict: true,
        }
    }
}

impl Prompt {
    /// Estimates the serialized input sent on the next request.
    ///
    /// Model switches cannot reuse a previous response, so the full request shape matters more
    /// than the previous provider's reported usage. This intentionally favors a conservative
    /// estimate over tokenizer-specific precision.
    pub fn estimated_request_token_count(&self, model_info: &ModelInfo) -> Option<i64> {
        let mut input = self.get_formatted_input_for_request(model_info.use_responses_lite);
        let tools = create_tools_json_for_responses_api(&self.tools).ok()?;
        let (instructions, request_tools) = if model_info.use_responses_lite {
            let mut prefix = vec![ResponseItem::AdditionalTools {
                id: None,
                role: "developer".to_string(),
                tools,
            }];
            if !self.base_instructions.text.is_empty() {
                prefix.push(ResponseItem::Message {
                    id: None,
                    role: "developer".to_string(),
                    content: vec![ContentItem::InputText {
                        text: self.base_instructions.text.clone(),
                    }],
                    phase: None,
                    internal_chat_message_metadata_passthrough: None,
                });
            }
            input.splice(0..0, prefix);
            (String::new(), serde_json::Value::Null)
        } else {
            (
                self.base_instructions.text.clone(),
                serde_json::Value::Array(tools),
            )
        };
        let request = serde_json::json!({
            "instructions": instructions,
            "input": input,
            "tools": request_tools,
            "parallel_tool_calls": self.parallel_tool_calls,
            "output_schema": self.output_schema,
        });
        let serialized = serde_json::to_string(&request).ok()?;
        i64::try_from(approx_token_count(&serialized)).ok()
    }

    pub fn get_formatted_input_for_request(&self, use_responses_lite: bool) -> Vec<ResponseItem> {
        let mut input = self.input.clone();
        if use_responses_lite {
            strip_image_details(&mut input);
        }
        input
    }
}

fn strip_image_details(items: &mut [ResponseItem]) {
    for item in items {
        match item {
            ResponseItem::Message { content, .. } => {
                for content_item in content {
                    if let ContentItem::InputImage { detail, .. } = content_item {
                        *detail = None;
                    }
                }
            }
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                if let Some(content) = output.content_items_mut() {
                    for content_item in content {
                        if let FunctionCallOutputContentItem::InputImage { detail, .. } =
                            content_item
                        {
                            *detail = None;
                        }
                    }
                }
            }
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::AgentMessage { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => {}
        }
    }
}

pub struct ResponseStream {
    pub rx_event: mpsc::Receiver<Result<ResponseEvent>>,
    /// Signals the mapper task that the consumer stopped polling before the
    /// provider stream reached its own terminal event.
    pub consumer_dropped: CancellationToken,
}

impl Stream for ResponseStream {
    type Item = Result<ResponseEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx_event.poll_recv(cx)
    }
}

impl Drop for ResponseStream {
    fn drop(&mut self) {
        self.consumer_dropped.cancel();
    }
}

#[cfg(test)]
#[path = "client_common_tests.rs"]
mod tests;
