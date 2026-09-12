//! Per-turn state and event classification for optimized tool-call rendering.

use super::*;
use xedoc_app_server_protocol::CommandExecutionSource;
use xedoc_app_server_protocol::ThreadItem;

const MAX_LABEL_CHARS: usize = 80;

#[derive(Debug, Default)]
pub(super) struct ToolCallSummaryState {
    cell: history_cell::ToolCallSummaryCell,
    persisted_stats: history_cell::ToolCallSummaryStats,
}

impl ToolCallSummaryState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn has_calls(&self) -> bool {
        self.cell.has_calls()
    }

    pub(super) fn start(
        &mut self,
        id: String,
        label: String,
        preview: Option<history_cell::ToolCallSummaryPreview>,
    ) {
        self.cell.start_call_with_preview(id, label, preview);
    }

    pub(super) fn record_file_change_stats(
        &mut self,
        id: &str,
        stats: history_cell::FileChangeStats,
    ) {
        self.cell.record_file_change_stats(id, stats);
    }

    pub(super) fn complete(
        &mut self,
        id: String,
        label: String,
        outcome: history_cell::ToolCallSummaryOutcome,
        preview: Option<history_cell::ToolCallSummaryPreview>,
    ) {
        self.cell
            .complete_call_with_preview(id, label, outcome, preview);
    }

    pub(super) fn cell(&self) -> &history_cell::ToolCallSummaryCell {
        &self.cell
    }

    pub(super) fn mark_in_progress_failed(&mut self) {
        self.cell.mark_in_progress_failed();
    }

    pub(super) fn append_command_output(&mut self, call_id: &str, delta: &str) -> bool {
        self.cell.append_command_output(call_id, delta)
    }

    pub(super) fn mark_history_summary_emitted(&mut self) {
        self.persisted_stats = self.cell.stats();
    }

    pub(super) fn unpersisted_stats(&self) -> history_cell::ToolCallSummaryStats {
        let stats = self.cell.stats();
        history_cell::ToolCallSummaryStats {
            total: stats.total.saturating_sub(self.persisted_stats.total),
            files_edited: stats
                .files_edited
                .saturating_sub(self.persisted_stats.files_edited),
            total_added: stats
                .total_added
                .saturating_sub(self.persisted_stats.total_added),
            total_removed: stats
                .total_removed
                .saturating_sub(self.persisted_stats.total_removed),
            web_searches: stats
                .web_searches
                .saturating_sub(self.persisted_stats.web_searches),
            web_pages_fetched: stats
                .web_pages_fetched
                .saturating_sub(self.persisted_stats.web_pages_fetched),
        }
    }
}

impl ChatWidget {
    pub fn tool_call_rendering_mode(&self) -> xedoc_config::types::ToolCallRenderingMode {
        self.config.tui_tool_call_rendering
    }

    pub fn set_tool_call_rendering_mode_and_notify(
        &mut self,
        mode: xedoc_config::types::ToolCallRenderingMode,
    ) {
        self.config.tui_tool_call_rendering = mode;
        let notice = match mode {
            xedoc_config::types::ToolCallRenderingMode::Normal => "Tool rendering mode: normal.",
            xedoc_config::types::ToolCallRenderingMode::Optimized => {
                "Tool rendering mode: optimized."
            }
        };
        let notice = if self.tool_call_rendering_mode_for_turn.is_some() {
            format!("{notice} (applies next turn)")
        } else {
            notice.to_string()
        };
        self.add_info_message(notice, /*hint*/ None);
    }

    pub(super) fn handle_tool_summary_started_now(&mut self, item: ThreadItem) {
        if !self.optimized_tool_call_rendering() {
            return;
        }
        if let Some((id, label)) = Self::tool_call_item_label(&item) {
            let file_change_stats = Self::tool_call_file_change_stats(&item);
            self.record_tool_call_start_with_preview(
                id.clone(),
                label,
                Self::tool_call_preview(&item),
            );
            if let Some(stats) = file_change_stats {
                self.record_tool_call_file_change_stats(&id, stats);
            }
        }
    }

    pub(super) fn handle_tool_summary_completed_now(&mut self, item: ThreadItem) {
        if !self.optimized_tool_call_rendering() {
            return;
        }
        if let Some((id, label, outcome)) = Self::tool_call_item_outcome(&item) {
            let file_change_stats = Self::tool_call_file_change_stats(&item);
            self.record_tool_call_completion_with_preview(
                id.clone(),
                label,
                outcome,
                Self::tool_call_preview(&item),
            );
            if let Some(stats) = file_change_stats {
                self.record_tool_call_file_change_stats(&id, stats);
            }
        }
    }

    pub(super) fn optimized_tool_call_rendering(&self) -> bool {
        let mode = self
            .tool_call_rendering_mode_for_turn
            .unwrap_or(self.config.tui_tool_call_rendering);
        matches!(mode, xedoc_config::types::ToolCallRenderingMode::Optimized)
    }

    pub(super) fn reset_tool_call_summary(&mut self) {
        self.tool_call_summary = None;
    }

    pub(super) fn record_tool_call_start(&mut self, id: String, label: String) {
        self.record_tool_call_start_with_preview(id, label, None);
    }

    fn record_tool_call_start_with_preview(
        &mut self,
        id: String,
        label: String,
        preview: Option<history_cell::ToolCallSummaryPreview>,
    ) {
        self.tool_call_summary
            .get_or_insert_with(ToolCallSummaryState::new)
            .start(id, label, preview);
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(super) fn record_tool_call_completion(
        &mut self,
        id: String,
        label: String,
        outcome: history_cell::ToolCallSummaryOutcome,
    ) {
        self.record_tool_call_completion_with_preview(id, label, outcome, None);
    }

    pub(super) fn append_tool_call_command_output(&mut self, call_id: &str, delta: &str) {
        if let Some(summary) = self.tool_call_summary.as_mut()
            && summary.append_command_output(call_id, delta)
        {
            self.bump_active_cell_revision();
            self.request_redraw();
        }
    }

    fn record_tool_call_completion_with_preview(
        &mut self,
        id: String,
        label: String,
        outcome: history_cell::ToolCallSummaryOutcome,
        preview: Option<history_cell::ToolCallSummaryPreview>,
    ) {
        self.tool_call_summary
            .get_or_insert_with(ToolCallSummaryState::new)
            .complete(id, label, outcome, preview);
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    fn record_tool_call_file_change_stats(
        &mut self,
        id: &str,
        stats: history_cell::FileChangeStats,
    ) {
        if let Some(summary) = self.tool_call_summary.as_mut() {
            summary.record_file_change_stats(id, stats);
        }
    }

    pub(super) fn flush_tool_call_summary(&mut self) {
        self.tool_call_summary = None;
    }

    pub(super) fn flush_tool_call_summary_into_history(&mut self) -> bool {
        let stats = self
            .tool_call_summary
            .as_ref()
            .and_then(|summary| summary.has_calls().then(|| summary.unpersisted_stats()));
        let Some(stats) = stats else {
            return false;
        };
        if stats.files_edited == 0 && stats.web_searches == 0 && stats.web_pages_fetched == 0 {
            return false;
        }
        self.add_boxed_history(Box::new(history_cell::ToolCallCountSummaryCell::new(stats)));
        self.transcript.needs_final_message_separator = true;
        self.transcript.had_work_activity = true;
        if let Some(summary) = self.tool_call_summary.as_mut() {
            summary.mark_history_summary_emitted();
        }
        true
    }

    pub(super) fn fail_tool_call_summary(&mut self) {
        if let Some(summary) = self.tool_call_summary.as_mut() {
            summary.mark_in_progress_failed();
        }
        self.flush_tool_call_summary();
    }

    pub(super) fn tool_call_item_label(item: &ThreadItem) -> Option<(String, String)> {
        let (id, label): (String, String) = match item {
            ThreadItem::CommandExecution {
                id,
                command,
                source,
                ..
            } if *source != CommandExecutionSource::UserShell => (id.clone(), command.clone()),
            ThreadItem::McpToolCall {
                id, server, tool, ..
            } => (id.clone(), format!("{server}/{tool}")),
            ThreadItem::WebSearch(item) => (item.id.clone(), Self::web_search_label(&item.query)),
            ThreadItem::FileChange { id, .. } => (id.clone(), "apply patch".to_string()),
            ThreadItem::ImageView { id, .. } => (id.clone(), "view image".to_string()),
            ThreadItem::ImageGeneration(item) => (item.id.clone(), "generate image".to_string()),
            ThreadItem::CollabAgentToolCall {
                tool: xedoc_app_server_protocol::CollabAgentTool::Wait,
                ..
            } => return None,
            ThreadItem::CollabAgentToolCall { id, tool, .. } => (id.clone(), format!("{tool:?}")),
            ThreadItem::DynamicToolCall {
                id,
                namespace,
                tool,
                arguments,
                ..
            } => {
                let label = if tool == "web_fetch" {
                    arguments
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .map_or_else(|| "Read webpage".to_string(), |url| format!("Read {url}"))
                } else {
                    namespace.as_deref().map_or_else(
                        || format!("Called {tool}"),
                        |namespace| format!("Called {namespace}/{tool}"),
                    )
                };
                (id.clone(), label)
            }
            _ => return None,
        };
        Some((id, bound_label(label)))
    }

    pub(super) fn web_search_label(query: &str) -> String {
        const PREFIX: &str = "Searched the web for \"";

        let query = query.trim();
        if query.is_empty() {
            return "Searched the web".to_string();
        }

        let max_query_chars = MAX_LABEL_CHARS.saturating_sub(PREFIX.chars().count() + 1);
        if query.chars().count() <= max_query_chars {
            return format!("{PREFIX}{query}\"");
        }

        let truncated = query
            .chars()
            .take(max_query_chars.saturating_sub(1))
            .collect::<String>();
        format!("{PREFIX}{truncated}…\"")
    }

    pub(super) fn tool_call_item_outcome(
        item: &ThreadItem,
    ) -> Option<(String, String, history_cell::ToolCallSummaryOutcome)> {
        let (id, label) = Self::tool_call_item_label(item)?;
        let outcome = match item {
            ThreadItem::CommandExecution {
                status, exit_code, ..
            } => {
                if matches!(
                    status,
                    xedoc_app_server_protocol::CommandExecutionStatus::Completed
                ) && exit_code.unwrap_or_default() == 0
                {
                    history_cell::ToolCallSummaryOutcome::Succeeded
                } else {
                    history_cell::ToolCallSummaryOutcome::Failed
                }
            }
            ThreadItem::McpToolCall { error, status, .. } => {
                if error.is_none()
                    && matches!(
                        status,
                        xedoc_app_server_protocol::McpToolCallStatus::Completed
                    )
                {
                    history_cell::ToolCallSummaryOutcome::Succeeded
                } else {
                    history_cell::ToolCallSummaryOutcome::Failed
                }
            }
            ThreadItem::FileChange { status, .. } => {
                if matches!(
                    status,
                    xedoc_app_server_protocol::PatchApplyStatus::Completed
                ) {
                    history_cell::ToolCallSummaryOutcome::Succeeded
                } else {
                    history_cell::ToolCallSummaryOutcome::Failed
                }
            }
            ThreadItem::ImageGeneration(item) => {
                if item.status.eq_ignore_ascii_case("completed") {
                    history_cell::ToolCallSummaryOutcome::Succeeded
                } else {
                    history_cell::ToolCallSummaryOutcome::Failed
                }
            }
            ThreadItem::DynamicToolCall {
                success, status, ..
            } => {
                if success.unwrap_or(matches!(
                    status,
                    xedoc_app_server_protocol::DynamicToolCallStatus::Completed
                )) {
                    history_cell::ToolCallSummaryOutcome::Succeeded
                } else {
                    history_cell::ToolCallSummaryOutcome::Failed
                }
            }
            ThreadItem::CollabAgentToolCall { status, .. } => {
                if matches!(
                    status,
                    xedoc_app_server_protocol::CollabAgentToolCallStatus::Completed
                ) {
                    history_cell::ToolCallSummaryOutcome::Succeeded
                } else {
                    history_cell::ToolCallSummaryOutcome::Failed
                }
            }
            ThreadItem::WebSearch(_) | ThreadItem::ImageView { .. } => {
                history_cell::ToolCallSummaryOutcome::Succeeded
            }
            _ => return None,
        };
        Some((id, label, outcome))
    }

    fn tool_call_preview(item: &ThreadItem) -> Option<history_cell::ToolCallSummaryPreview> {
        match item {
            ThreadItem::CommandExecution {
                command,
                aggregated_output,
                ..
            } => Some(history_cell::ToolCallSummaryPreview::Command {
                command: xedoc_tui_transcript::exec_command::strip_bash_lc_and_escape(
                    &split_command_string(command),
                ),
                output: Some(aggregated_output.clone().unwrap_or_default()),
            }),
            ThreadItem::FileChange { changes, .. } => {
                let change = changes.first()?;
                let (added, removed) = diff_line_counts(&change.kind, &change.diff);
                let (unified_diff, omitted_diff_lines) =
                    xedoc_tui_transcript::diff_render::truncate_unified_diff_preview(
                        &change.diff,
                        /*max_lines*/ 3,
                    )
                    .unwrap_or_default();
                Some(history_cell::ToolCallSummaryPreview::FileChange {
                    path: change.path.clone(),
                    added,
                    removed,
                    unified_diff,
                    omitted_diff_lines,
                })
            }
            _ => None,
        }
    }

    fn tool_call_file_change_stats(item: &ThreadItem) -> Option<history_cell::FileChangeStats> {
        let ThreadItem::FileChange { changes, .. } = item else {
            return None;
        };
        Some(
            changes
                .iter()
                .fold(history_cell::FileChangeStats::default(), |stats, change| {
                    let (added, removed) = diff_line_counts(&change.kind, &change.diff);
                    history_cell::FileChangeStats {
                        files_edited: stats.files_edited.saturating_add(1),
                        total_added: stats.total_added.saturating_add(added),
                        total_removed: stats.total_removed.saturating_add(removed),
                    }
                }),
        )
    }
}

fn diff_line_counts(
    kind: &xedoc_app_server_protocol::PatchChangeKind,
    diff: &str,
) -> (usize, usize) {
    match kind {
        xedoc_app_server_protocol::PatchChangeKind::Add => (diff.lines().count(), 0),
        xedoc_app_server_protocol::PatchChangeKind::Delete => (0, diff.lines().count()),
        xedoc_app_server_protocol::PatchChangeKind::Update { .. } => {
            diff.lines().fold((0, 0), |(added, removed), line| {
                if line.starts_with('+') && !line.starts_with("+++") {
                    (added.saturating_add(1), removed)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    (added, removed.saturating_add(1))
                } else {
                    (added, removed)
                }
            })
        }
    }
}

fn bound_label(label: String) -> String {
    label.chars().take(MAX_LABEL_CHARS).collect()
}
