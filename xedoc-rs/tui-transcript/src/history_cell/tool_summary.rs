//! Compact per-turn tool-call summary history cell.

use super::*;
use std::collections::HashMap;
use std::collections::VecDeque;

const MAX_TRACKED_CALLS: usize = 512;
const MAX_LABEL_CHARS: usize = 80;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallSummaryOutcome {
    Succeeded,
    Failed,
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
    in_progress_order: VecDeque<String>,
    capped: bool,
    current_label: Option<String>,
    last_label: Option<String>,
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
            in_progress_order: VecDeque::new(),
            capped: false,
            current_label: None,
            last_label: None,
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
        if self.calls.contains_key(&call_id) {
            return;
        }
        if self.calls.len() >= MAX_TRACKED_CALLS {
            self.capped = true;
            return;
        }
        self.labels
            .insert(call_id.clone(), bound_label(label.clone()));
        self.calls
            .insert(call_id.clone(), ToolCallStatus::InProgress);
        self.in_progress_order.push_back(call_id);
        self.total = self.total.saturating_add(1);
        self.in_progress = self.in_progress.saturating_add(1);
        self.current_label = Some(bound_label(label));
    }

    pub fn complete_call(
        &mut self,
        call_id: String,
        label: String,
        outcome: ToolCallSummaryOutcome,
    ) {
        if !self.calls.contains_key(&call_id) {
            if self.capped {
                return;
            }
            self.start_call(call_id.clone(), label.clone());
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
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let label = self.display_label();
        let label_width = usize::from(width).saturating_sub("• Tool: ".len()).max(1);
        let (label, _, _) = take_prefix_by_width(label, label_width);
        vec![
            Line::from(format!("• Tool: {label}")),
            Line::from(vec![
                format!(
                    "  Calls: {} · {} succeeded · {} failed · {} in progress{}",
                    self.total,
                    self.succeeded,
                    self.failed,
                    self.in_progress,
                    if self.capped { " · truncated" } else { "" }
                )
                .dim(),
            ]),
        ]
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
