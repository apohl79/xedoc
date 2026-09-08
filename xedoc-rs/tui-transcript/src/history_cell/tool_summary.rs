//! Compact per-turn tool-call summary history cell.

use super::*;
use crate::city_lights::CL_SESSION_TITLE_BG;
use crate::city_lights::CityLightsStylize;
use crate::render::highlight::highlight_bash_to_lines;
use crate::terminal_palette::rgb_color;
use std::collections::HashMap;
use std::collections::VecDeque;

const MAX_TRACKED_CALLS: usize = 512;
const MAX_LABEL_CHARS: usize = 80;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallSummaryOutcome {
    Succeeded,
    Failed,
}

#[derive(Clone, Debug)]
pub enum ToolCallSummaryPreview {
    Command {
        command: String,
        output: Option<String>,
    },
    FileChange {
        path: String,
        added: usize,
        removed: usize,
        diff_lines: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolCallStatus {
    InProgress,
    Succeeded,
    Failed,
}

/// A bounded, mutable two-row summary of the tools used during one agent turn.
#[derive(Debug)]
pub struct ToolCallSummaryCell {
    calls: HashMap<String, ToolCallStatus>,
    labels: HashMap<String, String>,
    previews: HashMap<String, ToolCallSummaryPreview>,
    in_progress_order: VecDeque<String>,
    capped: bool,
    current_label: Option<String>,
    last_label: Option<String>,
    last_preview: Option<ToolCallSummaryPreview>,
    total: usize,
    succeeded: usize,
    failed: usize,
    in_progress: usize,
}

impl ToolCallSummaryCell {
    pub fn new() -> Self {
        Self {
            calls: HashMap::new(),
            labels: HashMap::new(),
            previews: HashMap::new(),
            in_progress_order: VecDeque::new(),
            capped: false,
            current_label: None,
            last_label: None,
            last_preview: None,
            total: 0,
            succeeded: 0,
            failed: 0,
            in_progress: 0,
        }
    }

    pub fn has_calls(&self) -> bool {
        self.total > 0
    }

    pub fn start_call(&mut self, call_id: String, label: String) {
        self.start_call_with_preview(call_id, label, None);
    }

    pub fn start_call_with_preview(
        &mut self,
        call_id: String,
        label: String,
        preview: Option<ToolCallSummaryPreview>,
    ) {
        if self.calls.contains_key(&call_id) {
            return;
        }
        if self.calls.len() >= MAX_TRACKED_CALLS {
            self.capped = true;
            return;
        }
        self.labels
            .insert(call_id.clone(), bound_label(label.clone()));
        if let Some(preview) = preview {
            self.previews.insert(call_id.clone(), preview);
        }
        self.calls
            .insert(call_id.clone(), ToolCallStatus::InProgress);
        self.in_progress_order.push_back(call_id);
        self.total = self.total.saturating_add(1);
        self.in_progress = self.in_progress.saturating_add(1);
        self.current_label = Some(bound_label(label));
        self.last_preview = self
            .previews
            .get(
                self.in_progress_order
                    .back()
                    .expect("newly started tool call must be tracked"),
            )
            .cloned();
    }

    pub fn complete_call(
        &mut self,
        call_id: String,
        label: String,
        outcome: ToolCallSummaryOutcome,
    ) {
        self.complete_call_with_preview(call_id, label, outcome, None);
    }

    pub fn complete_call_with_preview(
        &mut self,
        call_id: String,
        label: String,
        outcome: ToolCallSummaryOutcome,
        preview: Option<ToolCallSummaryPreview>,
    ) {
        if !self.calls.contains_key(&call_id) {
            if self.capped {
                return;
            }
            self.start_call_with_preview(call_id.clone(), label.clone(), preview.clone());
        }
        let Some(status) = self.calls.get_mut(&call_id) else {
            return;
        };
        if *status != ToolCallStatus::InProgress {
            return;
        }
        *status = match outcome {
            ToolCallSummaryOutcome::Succeeded => ToolCallStatus::Succeeded,
            ToolCallSummaryOutcome::Failed => ToolCallStatus::Failed,
        };
        if let Some(preview) = preview {
            self.previews.insert(call_id.clone(), preview);
        }
        self.in_progress_order.retain(|id| id != &call_id);
        self.in_progress = self.in_progress.saturating_sub(1);
        match outcome {
            ToolCallSummaryOutcome::Succeeded => self.succeeded = self.succeeded.saturating_add(1),
            ToolCallSummaryOutcome::Failed => self.failed = self.failed.saturating_add(1),
        }
        self.current_label = self
            .in_progress_order
            .back()
            .and_then(|id| self.labels.get(id).cloned());
        self.last_label = Some(bound_label(label));
        self.last_preview = self.previews.get(&call_id).cloned();
    }

    pub fn display_label(&self) -> &str {
        self.current_label
            .as_deref()
            .or(self.last_label.as_deref())
            .unwrap_or("none")
    }

    pub fn mark_in_progress_failed(&mut self) {
        let mut failed = 0;
        for status in self.calls.values_mut() {
            if *status == ToolCallStatus::InProgress {
                *status = ToolCallStatus::Failed;
                failed += 1;
            }
        }
        self.in_progress = self.in_progress.saturating_sub(failed);
        self.failed = self.failed.saturating_add(failed);
        if let Some(label) = self
            .in_progress_order
            .back()
            .and_then(|id| self.labels.get(id))
            .cloned()
        {
            self.last_label = Some(label);
        }
        self.current_label = None;
        self.in_progress_order.clear();
    }
}

impl Default for ToolCallSummaryCell {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryCell for ToolCallSummaryCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = match &self.last_preview {
            Some(ToolCallSummaryPreview::Command { command, output }) => {
                let mut highlighted = highlight_bash_to_lines(command);
                let first = highlighted
                    .drain(..1)
                    .next()
                    .unwrap_or_else(|| Line::from(self.display_label().to_string()));
                let mut header = Line::from(vec!["• ".dim(), "Ran ".bold()]);
                header.extend(first);
                let mut lines = vec![header];
                for line in highlighted {
                    let mut continuation = Line::from("  ".dim());
                    continuation.extend(line);
                    lines.push(continuation);
                }
                if let Some(output) = output {
                    let output_lines = output.lines().collect::<Vec<_>>();
                    for output_line in output_lines.iter().take(3) {
                        lines.push(vec!["  ".dim(), (*output_line).to_string().dim()].into());
                    }
                    let omitted = output_lines.len().saturating_sub(3);
                    if omitted > 0 {
                        lines.push(format!("  ... {omitted} more lines").dim().into());
                    }
                }
                lines
            }
            Some(ToolCallSummaryPreview::FileChange {
                path,
                added,
                removed,
                diff_lines,
            }) => {
                let mut lines = vec![Line::from(vec![
                    "• ".dim(),
                    "Edited ".bold(),
                    path.clone().cl_cyan(),
                    " ".into(),
                    format!("+{added}").cl_green(),
                    " ".into(),
                    format!("-{removed}").cl_red(),
                ])];
                for diff_line in diff_lines.iter().take(3) {
                    let styled = if diff_line.starts_with('+') {
                        diff_line.clone().green()
                    } else if diff_line.starts_with('-') {
                        diff_line.clone().red()
                    } else {
                        diff_line.clone().dim()
                    };
                    lines.push(vec!["  ".dim(), styled].into());
                }
                let omitted = diff_lines.len().saturating_sub(3);
                if omitted > 0 {
                    lines.push(format!("  ... {omitted} more lines").dim().into());
                }
                lines
            }
            None => vec![Line::from(vec![
                "• ".dim(),
                "Ran ".bold(),
                self.display_label().to_string().into(),
            ])],
        };
        lines.insert(0, Line::default());
        lines.push("".into());
        lines.push(
            format!(
                "  Calls: {} · {} succeeded · {} failed · {} in progress{}",
                self.total,
                self.succeeded,
                self.failed,
                self.in_progress,
                if self.capped { " · truncated" } else { "" }
            )
            .dim()
            .into(),
        );
        lines.push(Line::default());
        lines
    }

    fn display_background_style(&self) -> Option<Style> {
        Some(Style::default().bg(rgb_color(CL_SESSION_TITLE_BG)))
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(u16::MAX)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }
}

fn bound_label(label: String) -> String {
    let label = label
        .chars()
        .map(|character| {
            if character.is_control() || (character.is_whitespace() && character != ' ') {
                ' '
            } else {
                character
            }
        })
        .take(MAX_LABEL_CHARS)
        .collect::<String>();
    if label.trim().is_empty() {
        "unnamed".to_string()
    } else {
        label
    }
}

#[cfg(test)]
#[path = "tool_summary_tests.rs"]
mod tests;
