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

    pub(super) fn stats(&self) -> history_cell::ToolCallSummaryStats {
        self.cell.stats()
    }

    pub(super) fn start(
        &mut self,
        id: String,
        label: String,
        preview: Option<history_cell::ToolCallSummaryPreview>,
    ) {
        self.cell.start_call_with_preview(id, label, preview);
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
            self.record_tool_call_start_with_preview(id, label, Self::tool_call_preview(&item));
        }
    }

    pub(super) fn handle_tool_summary_completed_now(&mut self, item: ThreadItem) {
        if !self.optimized_tool_call_rendering() {
            return;
        }
        if let Some((id, label, outcome)) = Self::tool_call_item_outcome(&item) {
            self.record_tool_call_completion_with_preview(
                id,
                label,
                outcome,
                Self::tool_call_preview(&item),
            );
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

    pub(super) fn flush_tool_call_summary(&mut self) {
        self.tool_call_summary = None;
    }

    pub(super) fn flush_tool_call_summary_into_history(&mut self) -> bool {
        let Some(summary) = self.tool_call_summary.take() else {
            return false;
        };
        if summary.has_calls() {
            self.add_boxed_history(Box::new(history_cell::ToolCallCountSummaryCell::new(
                summary.stats(),
            )));
            true
        } else {
            false
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
            ThreadItem::WebSearch(item) => (
                item.id.clone(),
                format!("Searched the web for \"{}\"", item.query),
            ),
            ThreadItem::FileChange { id, .. } => (id.clone(), "apply patch".to_string()),
            ThreadItem::ImageView { id, .. } => (id.clone(), "view image".to_string()),
            ThreadItem::ImageGeneration(item) => (item.id.clone(), "generate image".to_string()),
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
                output: aggregated_output.clone(),
            }),
            ThreadItem::FileChange { changes, .. } => {
                let change = changes.first()?;
                let diff_lines = change.diff.lines().map(str::to_string).collect::<Vec<_>>();
                let added = diff_lines
                    .iter()
                    .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
                    .count();
                let removed = diff_lines
                    .iter()
                    .filter(|line| line.starts_with('-') && !line.starts_with("---"))
                    .count();
                Some(history_cell::ToolCallSummaryPreview::FileChange {
                    path: change.path.clone(),
                    added,
                    removed,
                    diff_lines,
                })
            }
            _ => None,
        }
    }
}

fn bound_label(label: String) -> String {
    label.chars().take(MAX_LABEL_CHARS).collect()
}
