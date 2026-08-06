use std::sync::Arc;
use std::sync::OnceLock;

use crate::Prompt;
use crate::client::ModelClient;
use crate::client_common::ResponseEvent;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::session::session::Session;
use crate::session::turn::get_last_assistant_message_from_turn;
use crate::session::turn_context::TurnContext;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;
use codex_rollout_trace::InferenceTraceContext;
use codex_utils_output_truncation::approx_token_count;
use futures::FutureExt;
use futures::StreamExt;
use futures::stream;
use tracing::debug;

use codex_protocol::protocol::COMPACTION_PROGRESS_PREFIX;

const MAX_PARALLEL_COMPACTION_REQUESTS: usize = 4;

/// A history chunk and its coarse model-visible token estimate.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HistoryChunk {
    pub(crate) items: Vec<ResponseItem>,
    pub(crate) estimated_tokens: i64,
}

/// Returns chunks that fit within the per-request input budget while keeping tool call/output
/// pairs together whenever possible.
pub(crate) fn chunk_history(items: &[ResponseItem], item_budget: i64) -> Vec<HistoryChunk> {
    let budget = item_budget.max(1);
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut current_tokens = 0_i64;

    for item in items {
        let item_tokens = crate::context_manager::estimate_item_token_count(item);
        let would_exceed = !current.is_empty()
            && current_tokens.saturating_add(item_tokens) > budget
            && !is_matching_tool_output(current.last(), item);
        if would_exceed {
            chunks.push(HistoryChunk {
                items: std::mem::take(&mut current),
                estimated_tokens: current_tokens,
            });
            current_tokens = 0;
        }
        current.push(item.clone());
        current_tokens = current_tokens.saturating_add(item_tokens);
    }

    if !current.is_empty() {
        chunks.push(HistoryChunk {
            items: current,
            estimated_tokens: current_tokens,
        });
    }
    chunks
}

/// Summarizes an oversized history using bounded parallel map requests followed by sequential
/// reduction layers. Only independent requests in one layer run concurrently; each reduction
/// layer consumes the completed summaries from the previous layer.
pub(crate) async fn summarize_history(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    history: Vec<ResponseItem>,
    compaction_prompt: ResponseItem,
    base_instructions: BaseInstructions,
    responses_metadata: CodexResponsesMetadata,
    turn_state: Arc<OnceLock<String>>,
) -> CodexResult<String> {
    let item_budget = compaction_item_budget(&turn_context, &base_instructions, &compaction_prompt)
        .ok_or(CodexErr::ContextWindowExceeded)?;
    let map_chunks = chunk_history(&history, item_budget);
    send_progress(
        &sess,
        &turn_context,
        format!("planning {} history chunks", map_chunks.len()),
    )
    .await;
    debug!(
        chunks = map_chunks.len(),
        item_budget, "starting hierarchical compaction map phase"
    );

    let model_client = sess.services.model_client.load_full().as_ref().clone();
    let map_total = map_chunks.len();
    let mut map_results = stream::iter(map_chunks.into_iter().enumerate().map(|(index, chunk)| {
        summarize_chunk(
            model_client.clone(),
            turn_context.clone(),
            chunk.items,
            compaction_prompt.clone(),
            base_instructions.clone(),
            responses_metadata.clone(),
            turn_state.clone(),
        )
        .map(move |result| (index, result))
    }))
    .buffer_unordered(MAX_PARALLEL_COMPACTION_REQUESTS);

    send_progress(&sess, &turn_context, format!("map 0/{map_total}")).await;
    let mut summaries = Vec::with_capacity(map_total);
    let mut completed = 0;
    while let Some((index, result)) = map_results.next().await {
        let summary = result?;
        summaries.push((index, summary));
        completed += 1;
        send_progress(&sess, &turn_context, format!("map {completed}/{map_total}")).await;
    }
    summaries.sort_by_key(|(index, _)| *index);
    let mut summaries: Vec<ResponseItem> = summaries
        .into_iter()
        .map(|(_, summary)| summary_message(summary))
        .collect();

    let mut layer = 0;
    while summaries.len() > 1 {
        layer += 1;
        let reduction_chunks = chunk_history(&summaries, item_budget);
        send_progress(
            &sess,
            &turn_context,
            format!("reduce layer {layer} ({} groups)", reduction_chunks.len()),
        )
        .await;
        let mut reduction_results = stream::iter(reduction_chunks.into_iter().enumerate().map(
            |(index, chunk)| {
                summarize_chunk(
                    model_client.clone(),
                    turn_context.clone(),
                    chunk.items,
                    compaction_prompt.clone(),
                    base_instructions.clone(),
                    responses_metadata.clone(),
                    turn_state.clone(),
                )
                .map(move |result| (index, result))
            },
        ))
        .buffer_unordered(MAX_PARALLEL_COMPACTION_REQUESTS);
        let mut reduced = Vec::new();
        while let Some((index, result)) = reduction_results.next().await {
            reduced.push((index, result?));
        }
        reduced.sort_by_key(|(index, _)| *index);
        summaries = reduced
            .into_iter()
            .map(|(_, summary)| summary_message(summary))
            .collect();
        send_progress(
            &sess,
            &turn_context,
            format!(
                "reduce layer {layer} complete ({} summaries remain)",
                summaries.len()
            ),
        )
        .await;
    }

    let summary = summaries
        .into_iter()
        .next()
        .and_then(|item| get_last_assistant_message_from_turn(&[item]))
        .ok_or_else(|| CodexErr::Stream("compaction produced no summary".to_string(), None))?;
    send_progress(&sess, &turn_context, "complete".to_string()).await;
    Ok(summary)
}

fn compaction_item_budget(
    turn_context: &TurnContext,
    base_instructions: &BaseInstructions,
    compaction_prompt: &ResponseItem,
) -> Option<i64> {
    let query_budget = turn_context.model_info.auto_compact_token_limit()?;
    let base_tokens =
        i64::try_from(approx_token_count(&base_instructions.text)).unwrap_or(i64::MAX);
    let prompt_tokens = crate::context_manager::estimate_item_token_count(compaction_prompt);
    query_budget
        .checked_sub(base_tokens.saturating_add(prompt_tokens))
        .filter(|budget| *budget > 0)
}

async fn summarize_chunk(
    model_client: ModelClient,
    turn_context: Arc<TurnContext>,
    mut items: Vec<ResponseItem>,
    compaction_prompt: ResponseItem,
    base_instructions: BaseInstructions,
    responses_metadata: CodexResponsesMetadata,
    turn_state: Arc<OnceLock<String>>,
) -> CodexResult<String> {
    items.push(compaction_prompt);
    let prompt = Prompt {
        input: items,
        base_instructions,
        ..Default::default()
    };
    let mut client_session = model_client.new_session_with_turn_state(turn_state);
    let mut stream = client_session
        .stream(
            &prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort.clone(),
            turn_context.reasoning_summary,
            turn_context.config.service_tier.clone(),
            &responses_metadata,
            &InferenceTraceContext::disabled(),
        )
        .await?;
    let mut output_items = Vec::new();
    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::OutputItemDone(item) => output_items.push(item),
            ResponseEvent::Completed { .. } => {
                return get_last_assistant_message_from_turn(&output_items).ok_or_else(|| {
                    CodexErr::Stream("compaction produced no assistant summary".to_string(), None)
                });
            }
            ResponseEvent::Created
            | ResponseEvent::SafetyBuffering(_)
            | ResponseEvent::OutputItemAdded(_)
            | ResponseEvent::ServerModel(_)
            | ResponseEvent::ModelVerifications(_)
            | ResponseEvent::TurnModerationMetadata(_)
            | ResponseEvent::ServerReasoningIncluded(_)
            | ResponseEvent::OutputTextDelta(_)
            | ResponseEvent::ToolCallInputDelta { .. }
            | ResponseEvent::ReasoningSummaryDelta { .. }
            | ResponseEvent::ReasoningSummaryDone { .. }
            | ResponseEvent::ReasoningContentDelta { .. }
            | ResponseEvent::ReasoningSummaryPartAdded { .. }
            | ResponseEvent::RateLimits(_)
            | ResponseEvent::ModelsEtag(_) => {}
        }
    }
    Err(CodexErr::Stream(
        "stream closed before response.completed".to_string(),
        None,
    ))
}

fn summary_message(summary: String) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText { text: summary }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

async fn send_progress(sess: &Session, turn_context: &TurnContext, details: String) {
    sess.send_event(
        turn_context,
        EventMsg::Warning(WarningEvent {
            message: format!("{COMPACTION_PROGRESS_PREFIX} {details}"),
        }),
    )
    .await;
}

fn is_matching_tool_output(previous: Option<&ResponseItem>, current: &ResponseItem) -> bool {
    let Some(previous_call_id) = previous.and_then(tool_call_id) else {
        return false;
    };
    output_call_id(current).is_some_and(|call_id| call_id == previous_call_id)
}

fn tool_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. } => Some(call_id),
        ResponseItem::ToolSearchCall { call_id, .. } => call_id.as_deref(),
        _ => None,
    }
}

fn output_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id),
        ResponseItem::ToolSearchOutput { call_id, .. } => call_id.as_deref(),
        _ => None,
    }
}

#[cfg(test)]
#[path = "compact_hierarchical_tests.rs"]
mod tests;
