use serde::Deserialize;
use serde_json::Value;
use xedoc_api::ResponseEvent;
use xedoc_protocol::ResponseItemId;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ReasoningItemReasoningSummary;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::protocol::TokenUsage;

pub struct AnthropicStreamTranslator {
    response_id: String,
    message_id: ResponseItemId,
    active_block: Option<ActiveBlock>,
    stop_reason: Option<String>,
    usage: AnthropicAccumulatedUsage,
}

impl AnthropicStreamTranslator {
    pub fn new() -> Self {
        Self {
            response_id: ResponseItemId::new("resp").to_string(),
            message_id: ResponseItemId::new("msg"),
            active_block: None,
            stop_reason: None,
            usage: AnthropicAccumulatedUsage::default(),
        }
    }

    pub fn translate_json(&mut self, data: &str) -> Result<Vec<ResponseEvent>, serde_json::Error> {
        let event = serde_json::from_str(data)?;
        Ok(self.translate(event))
    }

    fn translate(&mut self, event: AnthropicStreamEvent) -> Vec<ResponseEvent> {
        match event {
            AnthropicStreamEvent::MessageStart { message } => {
                self.apply_usage(message.usage);
                vec![ResponseEvent::Created]
            }
            AnthropicStreamEvent::ContentBlockStart { content_block, .. } => {
                self.start_block(content_block)
            }
            AnthropicStreamEvent::ContentBlockDelta { delta, .. } => self.apply_delta(delta),
            AnthropicStreamEvent::ContentBlockStop { .. } => self.finish_block(),
            AnthropicStreamEvent::MessageDelta { delta, usage } => {
                self.apply_message_delta(delta, usage);
                Vec::new()
            }
            AnthropicStreamEvent::MessageStop {} => vec![self.completed_event()],
            AnthropicStreamEvent::Other => Vec::new(),
        }
    }

    fn start_block(&mut self, block: AnthropicContentBlockStart) -> Vec<ResponseEvent> {
        match block {
            AnthropicContentBlockStart::Text {} => {
                self.active_block = Some(ActiveBlock::Text {
                    text: String::new(),
                });
                vec![ResponseEvent::OutputItemAdded(
                    self.message_item(Vec::new()),
                )]
            }
            AnthropicContentBlockStart::Thinking { signature } => {
                let id = ResponseItemId::new("rs");
                self.active_block = Some(ActiveBlock::Thinking {
                    id: id.clone(),
                    text: String::new(),
                    signature,
                });
                vec![
                    ResponseEvent::OutputItemAdded(reasoning_item(id, String::new(), None)),
                    ResponseEvent::ReasoningSummaryPartAdded { summary_index: 0 },
                ]
            }
            AnthropicContentBlockStart::ToolUse { id, name } => {
                let custom = name == "apply_patch";
                self.active_block = Some(ActiveBlock::Tool {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                    custom,
                });
                let item = if custom {
                    ResponseItem::CustomToolCall {
                        id: Some(prefixed_id("ctc", &id)),
                        status: Some("in_progress".to_string()),
                        call_id: id,
                        name,
                        namespace: None,
                        input: String::new(),
                        provider_metadata: None,
                        internal_chat_message_metadata_passthrough: None,
                    }
                } else {
                    ResponseItem::FunctionCall {
                        id: Some(prefixed_id("fc", &id)),
                        name,
                        namespace: None,
                        arguments: String::new(),
                        call_id: id,
                        provider_metadata: None,
                        internal_chat_message_metadata_passthrough: None,
                    }
                };
                vec![ResponseEvent::OutputItemAdded(item)]
            }
            AnthropicContentBlockStart::Other => Vec::new(),
        }
    }

    fn apply_delta(&mut self, delta: AnthropicContentBlockDelta) -> Vec<ResponseEvent> {
        match (self.active_block.as_mut(), delta) {
            (
                Some(ActiveBlock::Text { text }),
                AnthropicContentBlockDelta::Text { text: delta },
            ) => {
                text.push_str(&delta);
                vec![ResponseEvent::OutputTextDelta(delta)]
            }
            (
                Some(ActiveBlock::Thinking { text, .. }),
                AnthropicContentBlockDelta::Thinking { thinking: delta },
            ) => {
                text.push_str(&delta);
                vec![ResponseEvent::ReasoningSummaryDelta {
                    delta,
                    summary_index: 0,
                }]
            }
            (
                Some(ActiveBlock::Thinking { signature, .. }),
                AnthropicContentBlockDelta::Signature { signature: delta },
            ) => {
                signature.push_str(&delta);
                Vec::new()
            }
            (
                Some(ActiveBlock::Tool { arguments, .. }),
                AnthropicContentBlockDelta::InputJson {
                    partial_json: delta,
                },
            ) => {
                arguments.push_str(&delta);
                Vec::new()
            }
            (
                Some(
                    ActiveBlock::Text { .. }
                    | ActiveBlock::Thinking { .. }
                    | ActiveBlock::Tool { .. },
                )
                | None,
                AnthropicContentBlockDelta::Text { .. }
                | AnthropicContentBlockDelta::Thinking { .. }
                | AnthropicContentBlockDelta::Signature { .. }
                | AnthropicContentBlockDelta::InputJson { .. }
                | AnthropicContentBlockDelta::Other,
            ) => Vec::new(),
        }
    }

    fn finish_block(&mut self) -> Vec<ResponseEvent> {
        match self.active_block.take() {
            Some(ActiveBlock::Text { text }) => {
                vec![ResponseEvent::OutputItemDone(
                    self.message_item(vec![ContentItem::OutputText { text }]),
                )]
            }
            Some(ActiveBlock::Thinking {
                id,
                text,
                signature,
            }) => {
                let signature = (!signature.is_empty()).then_some(signature);
                vec![
                    ResponseEvent::ReasoningSummaryDone {
                        item_id: id.to_string(),
                        text: text.clone(),
                        summary_index: 0,
                    },
                    ResponseEvent::OutputItemDone(reasoning_item(id, text, signature)),
                ]
            }
            Some(ActiveBlock::Tool {
                id,
                name,
                arguments,
                custom,
            }) => {
                if custom {
                    let input = custom_tool_input(&arguments);
                    let item_id = prefixed_id("ctc", &id);
                    vec![
                        ResponseEvent::ToolCallInputDelta {
                            item_id: item_id.to_string(),
                            call_id: Some(id.clone()),
                            delta: input.clone(),
                        },
                        ResponseEvent::OutputItemDone(ResponseItem::CustomToolCall {
                            id: Some(item_id),
                            status: Some("completed".to_string()),
                            call_id: id,
                            name,
                            namespace: None,
                            input,
                            provider_metadata: None,
                            internal_chat_message_metadata_passthrough: None,
                        }),
                    ]
                } else {
                    vec![ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                        id: Some(prefixed_id("fc", &id)),
                        name,
                        namespace: None,
                        arguments,
                        call_id: id,
                        provider_metadata: None,
                        internal_chat_message_metadata_passthrough: None,
                    })]
                }
            }
            None => Vec::new(),
        }
    }

    fn apply_message_delta(&mut self, delta: AnthropicMessageDelta, usage: Option<AnthropicUsage>) {
        if let Some(stop_reason) = delta.stop_reason.filter(|reason| !reason.is_empty()) {
            self.stop_reason = Some(stop_reason);
        }
        self.apply_usage(usage);
    }

    fn apply_usage(&mut self, usage: Option<AnthropicUsage>) {
        if let Some(usage) = usage {
            self.usage.update(usage);
        }
    }

    fn completed_event(&self) -> ResponseEvent {
        ResponseEvent::Completed {
            response_id: self.response_id.clone(),
            token_usage: Some(self.usage.token_usage()),
            end_turn: (self.stop_reason.as_deref() == Some("max_tokens")).then_some(false),
        }
    }

    fn message_item(&self, content: Vec<ContentItem>) -> ResponseItem {
        ResponseItem::Message {
            id: Some(self.message_id.clone()),
            role: "assistant".to_string(),
            content,
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }
}

impl Default for AnthropicStreamTranslator {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
enum ActiveBlock {
    Text {
        text: String,
    },
    Thinking {
        id: ResponseItemId,
        text: String,
        signature: String,
    },
    Tool {
        id: String,
        name: String,
        arguments: String,
        custom: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicStreamEvent {
    MessageStart {
        message: AnthropicMessageStart,
    },
    ContentBlockStart {
        #[serde(rename = "index")]
        _index: i64,
        content_block: AnthropicContentBlockStart,
    },
    ContentBlockDelta {
        #[serde(rename = "index")]
        _index: i64,
        delta: AnthropicContentBlockDelta,
    },
    ContentBlockStop {
        #[serde(rename = "index")]
        _index: i64,
    },
    MessageDelta {
        delta: AnthropicMessageDelta,
        #[serde(default)]
        usage: Option<AnthropicUsage>,
    },
    MessageStop {},
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlockStart {
    Text {},
    Thinking {
        #[serde(default)]
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlockDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageStart {
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<i64>,
    #[serde(default)]
    cache_read_input_tokens: Option<i64>,
}

#[derive(Debug, Default)]
struct AnthropicAccumulatedUsage {
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_input_tokens: i64,
    cache_read_input_tokens: i64,
}

impl AnthropicAccumulatedUsage {
    fn update(&mut self, usage: AnthropicUsage) {
        self.input_tokens = usage.input_tokens.unwrap_or(self.input_tokens);
        self.output_tokens = usage.output_tokens.unwrap_or(self.output_tokens);
        self.cache_creation_input_tokens = usage
            .cache_creation_input_tokens
            .unwrap_or(self.cache_creation_input_tokens);
        self.cache_read_input_tokens = usage
            .cache_read_input_tokens
            .unwrap_or(self.cache_read_input_tokens);
    }

    fn token_usage(&self) -> TokenUsage {
        let input_tokens =
            self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens;
        TokenUsage {
            input_tokens,
            cached_input_tokens: self.cache_read_input_tokens,
            cache_write_input_tokens: self.cache_creation_input_tokens,
            output_tokens: self.output_tokens,
            reasoning_output_tokens: 0,
            total_tokens: input_tokens + self.output_tokens,
        }
    }
}

fn reasoning_item(id: ResponseItemId, text: String, signature: Option<String>) -> ResponseItem {
    let summary = (!text.is_empty())
        .then_some(ReasoningItemReasoningSummary::SummaryText { text })
        .into_iter()
        .collect();
    ResponseItem::Reasoning {
        id: Some(id),
        summary,
        content: None,
        encrypted_content: signature,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn prefixed_id(prefix: &str, id: &str) -> ResponseItemId {
    ResponseItemId::from_server(format!("{prefix}_{id}"))
}

fn custom_tool_input(arguments: &str) -> String {
    let source = if arguments.is_empty() {
        "{}"
    } else {
        arguments
    };
    match serde_json::from_str::<Value>(source) {
        Ok(Value::String(input)) => input,
        Ok(Value::Object(object)) => {
            if let Some(input) = object.get("input").and_then(Value::as_str) {
                input.to_string()
            } else {
                Value::Object(object).to_string()
            }
        }
        Ok(Value::Null | Value::Bool(false)) => "{}".to_string(),
        Ok(input) => input.to_string(),
        Err(_) => arguments.to_string(),
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
