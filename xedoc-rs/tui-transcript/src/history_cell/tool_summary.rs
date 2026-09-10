//! Compact per-turn tool-call summary history cell.

use super::*;
use crate::city_lights::CityLightsStylize;
use crate::diff_model::FileChange;
use crate::diff_render::create_diff_summary;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::highlight::highlight_bash_to_lines;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use xedoc_ansi_escape::ansi_escape_line;

const MAX_TRACKED_CALLS: usize = 512;
const MAX_LABEL_CHARS: usize = 80;
const MAX_COMMAND_OUTPUT_CHARS: usize = 16 * 1024;

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
        unified_diff: String,
        omitted_diff_lines: usize,
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
    last_preview_call_id: Option<String>,
    total: usize,
    succeeded: usize,
    failed: usize,
    in_progress: usize,
}

/// A persistent, compact marker separating model-output blocks around tool use.
#[derive(Debug)]
pub struct ToolCallCountSummaryCell {
    stats: ToolCallSummaryStats,
}

impl ToolCallCountSummaryCell {
    pub fn new(stats: ToolCallSummaryStats) -> Self {
        Self { stats }
    }
}

impl HistoryCell for ToolCallCountSummaryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let separator = "─".repeat(width as usize).dim();
        let mut summary = String::new();
        if self.stats.files_edited > 0 {
            let files = if self.stats.files_edited == 1 {
                "file"
            } else {
                "files"
            };
            summary.push_str(&format!("{} {files} edited.", self.stats.files_edited));
        }
        if self.stats.web_searches > 0 {
            let searches = if self.stats.web_searches == 1 {
                "search"
            } else {
                "searches"
            };
            summary.push_str(&format!(
                " {} web {searches} performed.",
                self.stats.web_searches
            ));
        }
        if self.stats.web_pages_fetched > 0 {
            let pages = if self.stats.web_pages_fetched == 1 {
                "page"
            } else {
                "pages"
            };
            summary.push_str(&format!(
                " {} web {pages} fetched.",
                self.stats.web_pages_fetched
            ));
        }
        vec![
            separator.clone().into(),
            vec!["• ".dim(), summary.dim()].into(),
            separator.into(),
        ]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(/*width*/ u16::MAX)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ToolCallSummaryStats {
    pub total: usize,
    pub files_edited: usize,
    pub web_searches: usize,
    pub web_pages_fetched: usize,
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
            last_preview_call_id: None,
            total: 0,
            succeeded: 0,
            failed: 0,
            in_progress: 0,
        }
    }

    pub fn has_calls(&self) -> bool {
        self.total > 0
    }

    pub fn stats(&self) -> ToolCallSummaryStats {
        let labels = self.labels.values();
        ToolCallSummaryStats {
            total: self.total,
            files_edited: labels
                .clone()
                .filter(|label| *label == "apply patch")
                .count(),
            web_searches: labels
                .clone()
                .filter(|label| {
                    *label == "web search" || label.starts_with("Searched the web for ")
                })
                .count(),
            web_pages_fetched: labels.filter(|label| label.starts_with("Read ")).count(),
        }
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
        self.last_preview_call_id = self.in_progress_order.back().cloned();
    }

    pub fn complete_call(
        &mut self,
        call_id: String,
        label: String,
        outcome: ToolCallSummaryOutcome,
    ) {
        self.complete_call_with_preview(call_id, label, outcome, None);
    }

    pub fn append_command_output(&mut self, call_id: &str, delta: &str) -> bool {
        let Some(ToolCallSummaryPreview::Command {
            output: Some(output),
            ..
        }) = self.previews.get_mut(call_id)
        else {
            return false;
        };
        output.push_str(delta);
        trim_to_tail(output, MAX_COMMAND_OUTPUT_CHARS);
        self.last_preview = self.previews.get(call_id).cloned();
        self.last_preview_call_id = Some(call_id.to_string());
        true
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
        self.last_preview_call_id = Some(call_id);
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
        let mut lines = match &self.last_preview {
            Some(ToolCallSummaryPreview::Command { command, output }) => {
                let mut highlighted = highlight_bash_to_lines(command);
                let first = highlighted
                    .drain(..1)
                    .next()
                    .unwrap_or_else(|| Line::from(self.display_label().to_string()));
                let verb = if self
                    .last_preview_call_id
                    .as_ref()
                    .and_then(|call_id| self.calls.get(call_id))
                    .is_some_and(|status| *status == ToolCallStatus::InProgress)
                {
                    action_verb("Running ")
                } else {
                    action_verb("Ran ")
                };
                let mut header = Line::from(vec!["• ".dim(), verb]);
                header.extend(first);
                let mut lines = vec![truncate_line_with_ellipsis_if_overflow(
                    header,
                    usize::from(width),
                )];
                if let Some(output) = output {
                    let output_lines = output
                        .split_terminator('\n')
                        .map(|output_line| {
                            let output_line = output_line
                                .trim_end_matches('\r')
                                .rsplit('\r')
                                .next()
                                .unwrap_or_default();
                            ansi_escape_line(output_line)
                                .to_string()
                                .trim_end()
                                .to_string()
                        })
                        .collect::<Vec<_>>();
                    let omitted = output_lines.len().saturating_sub(3);
                    if omitted > 0 {
                        lines.push(format!("  ... {omitted} more lines").dim().into());
                    }
                    for output_line in output_lines.iter().skip(omitted) {
                        let (visible, _, _) =
                            take_prefix_by_width(output_line, usize::from(width).saturating_sub(2));
                        lines.push(vec!["  ".dim(), visible.dim()].into());
                    }
                }
                lines
            }
            Some(ToolCallSummaryPreview::FileChange {
                path,
                added,
                removed,
                unified_diff,
                omitted_diff_lines,
            }) => {
                let mut lines = vec![Line::from(vec![
                    "• ".dim(),
                    action_verb("Edited "),
                    path.clone().cl_cyan(),
                    " ".into(),
                    format!("+{added}").cl_green(),
                    " ".into(),
                    format!("-{removed}").cl_red(),
                ])];
                let changes = HashMap::from([(
                    PathBuf::from(path),
                    FileChange::Update {
                        unified_diff: unified_diff.clone(),
                        move_path: None,
                    },
                )]);
                let mut rendered = create_diff_summary(
                    &changes,
                    Path::new(""),
                    usize::from(width).saturating_sub(2),
                );
                rendered.remove(0);
                let omitted = omitted_diff_lines.saturating_add(rendered.len().saturating_sub(3));
                lines.extend(rendered.into_iter().take(3));
                if omitted > 0 {
                    lines.push(format!("  ... {omitted} more lines").dim().into());
                }
                lines
            }
            None => vec![action_label_line(self.display_label())],
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
        Some(user_message_style())
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(u16::MAX)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }
}

fn action_label_line(label: &str) -> Line<'static> {
    let mut line = Line::from("• ".dim());
    if let Some((action, detail)) = label.split_once(' ')
        && action.chars().next().is_some_and(char::is_uppercase)
    {
        line.push_span(action_verb(action));
        line.push_span(format!(" {detail}"));
    } else {
        line.push_span(action_verb("Ran "));
        line.push_span(label.to_string());
    }
    line
}

fn action_verb(action: impl Into<String>) -> Span<'static> {
    action.into().white().bold()
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

fn trim_to_tail(text: &mut String, max_chars: usize) {
    let char_count = text.chars().count();
    if char_count > max_chars {
        *text = text
            .chars()
            .skip(char_count.saturating_sub(max_chars))
            .collect();
    }
}

#[cfg(test)]
#[path = "tool_summary_tests.rs"]
mod tests;
