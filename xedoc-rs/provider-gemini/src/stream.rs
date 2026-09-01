use serde::Deserialize;
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use xedoc_api::ResponseEvent;
use xedoc_protocol::ResponseItemId;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::protocol::TokenUsage;

use crate::GeminiThoughtSignatureStore;

pub struct GeminiStreamTranslator {
    response_id: String,
    message_id: ResponseItemId,
    thought_signatures: Arc<GeminiThoughtSignatureStore>,
    started: bool,
    text_started: bool,
    completed: bool,
    output_seen: bool,
    text: String,
    no_output_diagnostic: Option<String>,
    finish_reason: Option<String>,
    usage: TokenUsage,
}

impl GeminiStreamTranslator {
    pub fn new(thought_signatures: Arc<GeminiThoughtSignatureStore>) -> Self {
        Self {
            response_id: ResponseItemId::new("resp").to_string(),
            message_id: ResponseItemId::new("msg"),
            thought_signatures,
            started: false,
            text_started: false,
            completed: false,
            output_seen: false,
            text: String::new(),
            no_output_diagnostic: None,
            finish_reason: None,
            usage: TokenUsage::default(),
        }
    }

    pub fn translate_json(&mut self, data: &str) -> Result<Vec<ResponseEvent>, serde_json::Error> {
        let chunk = serde_json::from_str(data)?;
        Ok(self.translate(chunk))
    }

    pub fn complete(&mut self) -> Result<Vec<ResponseEvent>, GeminiStreamError> {
        if self.completed {
            return Ok(Vec::new());
        }
        self.completed = true;
        if !self.output_seen {
            return Err(GeminiStreamError::NoVisibleOutput(
                self.no_output_diagnostic
                    .clone()
                    .unwrap_or_else(|| "Gemini returned no visible output.".to_string()),
            ));
        }

        let mut events = Vec::new();
        self.start(&mut events);
        if self.text_started {
            events.push(ResponseEvent::OutputItemDone(self.message_item(vec![
                ContentItem::OutputText {
                    text: self.text.clone(),
                },
            ])));
        }
        events.push(ResponseEvent::Completed {
            response_id: self.response_id.clone(),
            token_usage: Some(self.usage.clone()),
            end_turn: (self.finish_reason.as_deref() == Some("MAX_TOKENS")).then_some(false),
        });
        Ok(events)
    }

    fn translate(&mut self, chunk: GeminiStreamChunk) -> Vec<ResponseEvent> {
        if self.completed {
            return Vec::new();
        }

        let mut events = Vec::new();
        self.start(&mut events);
        if self.no_output_diagnostic.is_none() {
            self.no_output_diagnostic = no_output_diagnostic(&chunk);
        }
        if let Some(usage) = chunk.usage_metadata {
            self.usage = usage.into();
        }

        let Some(candidate) = chunk.candidates.into_iter().next() else {
            return events;
        };
        if let Some(finish_reason) = candidate
            .finish_reason
            .filter(|finish_reason| !finish_reason.is_empty())
        {
            self.finish_reason = Some(finish_reason);
        }

        let parts = candidate
            .content
            .map(|content| content.parts)
            .unwrap_or_default();
        let text = parts
            .iter()
            .filter_map(|part| part.text.as_deref())
            .collect::<String>();
        if !text.is_empty() {
            self.output_seen = true;
            self.text.push_str(&text);
            if !self.text_started {
                self.text_started = true;
                events.push(ResponseEvent::OutputItemAdded(
                    self.message_item(Vec::new()),
                ));
            }
            events.push(ResponseEvent::OutputTextDelta(text));
        }
        for part in parts {
            if let Some(call) = part.function_call {
                self.output_seen = true;
                events.extend(self.function_call_events(call, part.thought_signature));
            }
        }
        events
    }

    fn start(&mut self, events: &mut Vec<ResponseEvent>) {
        if !self.started {
            self.started = true;
            events.push(ResponseEvent::Created);
        }
    }

    fn function_call_events(
        &self,
        call: GeminiStreamFunctionCall,
        part_thought_signature: Option<String>,
    ) -> Vec<ResponseEvent> {
        let GeminiStreamFunctionCall {
            name,
            args,
            thought_signature,
        } = call;
        let call_id = ResponseItemId::new("call").to_string();
        let name = name.unwrap_or_else(|| "tool".to_string());
        let args = args.unwrap_or_else(empty_object);
        let arguments = serde_json::to_string(&args).unwrap_or_default();
        let thought_signature = match part_thought_signature {
            Some(signature) => signature,
            None => thought_signature.unwrap_or_default(),
        };
        self.thought_signatures
            .remember(&call_id, &thought_signature);

        if name == "apply_patch" {
            let input = args
                .get("input")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| arguments.clone());
            let item_id = prefixed_id("ctc", &call_id);
            vec![
                ResponseEvent::OutputItemAdded(ResponseItem::CustomToolCall {
                    id: Some(item_id.clone()),
                    status: Some("in_progress".to_string()),
                    call_id: call_id.clone(),
                    name: name.clone(),
                    namespace: None,
                    input: String::new(),
                    internal_chat_message_metadata_passthrough: None,
                }),
                ResponseEvent::ToolCallInputDelta {
                    item_id: item_id.to_string(),
                    call_id: Some(call_id.clone()),
                    delta: input.clone(),
                },
                ResponseEvent::OutputItemDone(ResponseItem::CustomToolCall {
                    id: Some(item_id),
                    status: Some("completed".to_string()),
                    call_id,
                    name,
                    namespace: None,
                    input,
                    internal_chat_message_metadata_passthrough: None,
                }),
            ]
        } else {
            let item_id = prefixed_id("fc", &call_id);
            vec![
                ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall {
                    id: Some(item_id.clone()),
                    name: name.clone(),
                    namespace: None,
                    arguments: String::new(),
                    call_id: call_id.clone(),
                    internal_chat_message_metadata_passthrough: None,
                }),
                ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                    id: Some(item_id),
                    name,
                    namespace: None,
                    arguments,
                    call_id,
                    internal_chat_message_metadata_passthrough: None,
                }),
            ]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiStreamError {
    NoVisibleOutput(String),
}

impl fmt::Display for GeminiStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoVisibleOutput(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for GeminiStreamError {}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiStreamChunk {
    #[serde(default)]
    candidates: Vec<GeminiStreamCandidate>,
    prompt_feedback: Option<GeminiPromptFeedback>,
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiStreamCandidate {
    content: Option<GeminiStreamContent>,
    finish_reason: Option<String>,
    finish_message: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct GeminiStreamContent {
    #[serde(default)]
    parts: Vec<GeminiStreamPart>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiStreamPart {
    text: Option<String>,
    function_call: Option<GeminiStreamFunctionCall>,
    thought_signature: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiStreamFunctionCall {
    name: Option<String>,
    args: Option<Value>,
    thought_signature: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPromptFeedback {
    block_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    #[serde(default)]
    prompt_token_count: i64,
    #[serde(default)]
    candidates_token_count: i64,
    #[serde(default)]
    cached_content_token_count: i64,
    #[serde(default)]
    thoughts_token_count: i64,
}

impl From<GeminiUsageMetadata> for TokenUsage {
    fn from(usage: GeminiUsageMetadata) -> Self {
        Self {
            input_tokens: usage.prompt_token_count,
            cached_input_tokens: usage.cached_content_token_count,
            cache_write_input_tokens: 0,
            output_tokens: usage.candidates_token_count,
            reasoning_output_tokens: usage.thoughts_token_count,
            total_tokens: usage.prompt_token_count + usage.candidates_token_count,
        }
    }
}

fn no_output_diagnostic(chunk: &GeminiStreamChunk) -> Option<String> {
    if let Some(block_reason) = chunk
        .prompt_feedback
        .as_ref()
        .and_then(|feedback| feedback.block_reason.as_deref())
        .filter(|reason| !reason.is_empty())
    {
        return Some(format!("Gemini blocked the prompt: {block_reason}."));
    }
    let candidate = chunk.candidates.first()?;
    candidate
        .finish_reason
        .as_deref()
        .filter(|reason| !reason.is_empty())
        .map(|reason| {
            match candidate
                .finish_message
                .as_deref()
                .filter(|message| !message.is_empty())
            {
                Some(message) => {
                    format!("Gemini returned no visible output: {reason} ({message}).")
                }
                None => format!("Gemini returned no visible output: {reason}."),
            }
        })
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

fn prefixed_id(prefix: &str, id: &str) -> ResponseItemId {
    ResponseItemId::from_server(format!("{prefix}_{id}"))
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
