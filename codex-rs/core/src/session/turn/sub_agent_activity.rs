use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::WebSearchAction;
use std::collections::VecDeque;

const MAX_RECENT_ACTIVITY_ITEMS: usize = 8;
const MAX_ACTIVITY_ITEM_CHARS: usize = 512;

#[derive(Debug, Default)]
pub(super) struct RecentSubAgentActivity {
    items: VecDeque<String>,
    changed: bool,
}

impl RecentSubAgentActivity {
    pub(super) fn record_response_item(&mut self, item: &ResponseItem) {
        let Some(activity) = response_item_activity(item) else {
            return;
        };
        let activity = activity
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_ACTIVITY_ITEM_CHARS)
            .collect::<String>();
        if activity.is_empty() || self.items.back() == Some(&activity) {
            return;
        }
        self.items.push_back(activity);
        while self.items.len() > MAX_RECENT_ACTIVITY_ITEMS {
            self.items.pop_front();
        }
        self.changed = true;
    }

    pub(super) fn snapshot_if_changed(&mut self) -> Option<String> {
        if !self.changed {
            return None;
        }
        self.changed = false;
        Some(self.items.iter().cloned().collect::<Vec<_>>().join("\n"))
    }

    pub(super) fn retry(&mut self) {
        self.changed = !self.items.is_empty();
    }
}

fn response_item_activity(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::Message { role, content, .. } if role == "assistant" => {
            let text = content
                .iter()
                .filter_map(|content| match content {
                    ContentItem::OutputText { text } => Some(text.as_str()),
                    ContentItem::InputText { .. }
                    | ContentItem::InputImage { .. }
                    | ContentItem::InputAudio { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then(|| format!("Assistant: {text}"))
        }
        ResponseItem::AgentMessage { content, .. } => {
            let text = content
                .iter()
                .filter_map(|content| match content {
                    AgentMessageInputContent::InputText { text } => Some(text.as_str()),
                    AgentMessageInputContent::EncryptedContent { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then(|| format!("Agent message: {text}"))
        }
        ResponseItem::Reasoning {
            summary, content, ..
        } => {
            let summary = summary
                .iter()
                .map(|summary| match summary {
                    ReasoningItemReasoningSummary::SummaryText { text } => text.as_str(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            let text = if summary.trim().is_empty() {
                content
                    .iter()
                    .flatten()
                    .map(|content| match content {
                        ReasoningItemContent::ReasoningText { text }
                        | ReasoningItemContent::Text { text } => text.as_str(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                summary
            };
            (!text.trim().is_empty()).then(|| format!("Reasoning: {text}"))
        }
        ResponseItem::LocalShellCall { action, .. } => {
            let LocalShellAction::Exec(action) = action;
            Some(format!("Shell command: {}", action.command.join(" ")))
        }
        ResponseItem::FunctionCall {
            name,
            namespace,
            arguments,
            ..
        } => {
            let name = namespace
                .as_ref()
                .map(|namespace| format!("{namespace}/{name}"))
                .unwrap_or_else(|| name.clone());
            Some(format!("Tool {name}: {arguments}"))
        }
        ResponseItem::ToolSearchCall {
            execution,
            arguments,
            ..
        } => Some(format!("Tool search {execution}: {arguments}")),
        ResponseItem::CustomToolCall {
            name,
            namespace,
            input,
            ..
        } => {
            let name = namespace
                .as_ref()
                .map(|namespace| format!("{namespace}/{name}"))
                .unwrap_or_else(|| name.clone());
            Some(format!("Tool {name}: {input}"))
        }
        ResponseItem::WebSearchCall { action, .. } => match action {
            Some(WebSearchAction::Search { query, queries }) => {
                let queries = query
                    .iter()
                    .chain(queries.iter().flatten())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                Some(format!("Web search: {queries}"))
            }
            Some(WebSearchAction::OpenPage { url }) => Some(format!(
                "Opening web page: {}",
                url.as_deref().unwrap_or_default()
            )),
            Some(WebSearchAction::FindInPage { url, pattern }) => Some(format!(
                "Finding {} in {}",
                pattern.as_deref().unwrap_or_default(),
                url.as_deref().unwrap_or_default()
            )),
            Some(WebSearchAction::Other) | None => Some("Using web search".to_string()),
        },
        ResponseItem::ImageGenerationCall { revised_prompt, .. } => Some(
            revised_prompt
                .as_ref()
                .map(|prompt| format!("Generating image: {prompt}"))
                .unwrap_or_else(|| "Generating image".to_string()),
        ),
        ResponseItem::AdditionalTools { .. }
        | ResponseItem::Message { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::Other => None,
    }
}

#[cfg(test)]
#[path = "sub_agent_activity_tests.rs"]
mod tests;
