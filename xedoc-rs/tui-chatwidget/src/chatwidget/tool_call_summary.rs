//! Per-turn state and event classification for optimized tool-call rendering.

use super::*;
use xedoc_app_server_protocol::CommandExecutionSource;
use xedoc_app_server_protocol::ThreadItem;

const MAX_LABEL_CHARS: usize = 80;

#[derive(Debug, Default)]
pub(super) struct ToolCallSummaryState {
    cell: history_cell::ToolCallSummaryCell,
}

impl ToolCallSummaryState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn has_calls(&self) -> bool {
        self.cell.has_calls()
    }

    pub(super) fn start(&mut self, id: String, label: String) {
        self.cell.start_call(id, label);
    }

    pub(super) fn complete(
        &mut self,
        id: String,
        label: String,
        outcome: history_cell::ToolCallSummaryOutcome,
    ) {
        self.cell.complete_call(id, label, outcome);
    }

    pub(super) fn cell(&self) -> &history_cell::ToolCallSummaryCell {
        &self.cell
    }

    pub(super) fn into_cell(self) -> history_cell::ToolCallSummaryCell {
        self.cell
    }

    pub(super) fn mark_in_progress_failed(&mut self) {
        self.cell.mark_in_progress_failed();
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
            self.record_tool_call_start(id, label);
        }
    }

    pub(super) fn handle_tool_summary_completed_now(&mut self, item: ThreadItem) {
        if !self.optimized_tool_call_rendering() {
            return;
        }
        if let Some((id, label, outcome)) = Self::tool_call_item_outcome(&item) {
            self.record_tool_call_completion(id, label, outcome);
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
        self.tool_call_summary
            .get_or_insert_with(ToolCallSummaryState::new)
            .start(id, label);
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(super) fn record_tool_call_completion(
        &mut self,
        id: String,
        label: String,
        outcome: history_cell::ToolCallSummaryOutcome,
    ) {
        self.tool_call_summary
            .get_or_insert_with(ToolCallSummaryState::new)
            .complete(id, label, outcome);
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(super) fn flush_tool_call_summary(&mut self) {
        let Some(summary) = self.tool_call_summary.take() else {
            return;
        };
        if summary.has_calls() {
            self.add_boxed_history(Box::new(summary.into_cell()));
        }
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
            ThreadItem::WebSearch(item) => (item.id.clone(), "web search".to_string()),
            ThreadItem::FileChange { id, .. } => (id.clone(), "apply patch".to_string()),
            ThreadItem::ImageView { id, .. } => (id.clone(), "view image".to_string()),
            ThreadItem::ImageGeneration(item) => (item.id.clone(), "generate image".to_string()),
            ThreadItem::CollabAgentToolCall { id, tool, .. } => (id.clone(), format!("{tool:?}")),
            ThreadItem::DynamicToolCall {
                id,
                namespace,
                tool,
                ..
            } => (
                id.clone(),
                namespace
                    .as_deref()
                    .map_or_else(|| tool.clone(), |namespace| format!("{namespace}/{tool}")),
            ),
            _ => return None,
        };
        Some((id, bound_label(label)))
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
}

fn bound_label(label: String) -> String {
    label.chars().take(MAX_LABEL_CHARS).collect()
}
