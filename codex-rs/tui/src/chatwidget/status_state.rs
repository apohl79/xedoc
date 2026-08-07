//! Status indicator and terminal-title state for `ChatWidget`.

use crate::status_indicator_widget::STATUS_DETAILS_DEFAULT_MAX_LINES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StatusIndicatorState {
    pub(super) header: String,
    pub(super) details: Option<String>,
    pub(super) details_max_lines: usize,
}

impl StatusIndicatorState {
    pub(super) fn working() -> Self {
        Self {
            header: String::from("Working"),
            details: None,
            details_max_lines: STATUS_DETAILS_DEFAULT_MAX_LINES,
        }
    }

    pub(super) fn is_guardian_review(&self) -> bool {
        self.header == "Reviewing approval request" || self.header.starts_with("Reviewing ")
    }
}

/// Compact runtime states that can be rendered into the terminal title.
///
/// This is intentionally smaller than the full status-header vocabulary. The
/// title needs short, stable labels, so callers map richer lifecycle events
/// onto one of these buckets before rendering.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum TerminalTitleStatusKind {
    Working,
    WaitingForBackgroundTerminal,
    #[default]
    Thinking,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PendingGuardianReviewStatus {
    entries: Vec<PendingGuardianReviewStatusEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingGuardianReviewStatusEntry {
    id: String,
    detail: String,
}

impl PendingGuardianReviewStatus {
    pub(super) fn start_or_update(&mut self, id: String, detail: String) {
        if let Some(existing) = self.entries.iter_mut().find(|entry| entry.id == id) {
            existing.detail = detail;
        } else {
            self.entries
                .push(PendingGuardianReviewStatusEntry { id, detail });
        }
    }

    pub(super) fn finish(&mut self, id: &str) -> bool {
        let original_len = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.entries.len() != original_len
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // Guardian review status is derived from the full set of currently pending
    // review entries. The generic status cache on `ChatWidget` stores whichever
    // footer is currently rendered; this helper computes the guardian-specific
    // footer snapshot that should replace it while reviews remain in flight.
    pub(super) fn status_indicator_state(&self) -> Option<StatusIndicatorState> {
        let details = if self.entries.len() == 1 {
            self.entries.first().map(|entry| entry.detail.clone())
        } else if self.entries.is_empty() {
            None
        } else {
            let mut lines = self
                .entries
                .iter()
                .take(3)
                .map(|entry| format!("• {}", entry.detail))
                .collect::<Vec<_>>();
            let remaining = self.entries.len().saturating_sub(3);
            if remaining > 0 {
                lines.push(format!("+{remaining} more"));
            }
            Some(lines.join("\n"))
        };
        let details = details?;
        let header = if self.entries.len() == 1 {
            String::from("Reviewing approval request")
        } else {
            format!("Reviewing {} approval requests", self.entries.len())
        };
        let details_max_lines = if self.entries.len() == 1 { 1 } else { 4 };
        Some(StatusIndicatorState {
            header,
            details: Some(details),
            details_max_lines,
        })
    }
}

#[derive(Debug)]
pub(super) struct StatusState {
    pub(super) current_status: StatusIndicatorState,
    pub(super) pending_guardian_review_status: PendingGuardianReviewStatus,
    pub(super) terminal_title_status_kind: TerminalTitleStatusKind,
    pub(super) retry_status_header: Option<String>,
    pub(super) pending_status_indicator_restore: bool,
    /// Header of the compaction run that currently owns the status indicator, if any.
    ///
    /// Compaction can run for a long time while the rest of the turn pipeline keeps setting
    /// headers such as "Working". Remembering the active compaction header lets the widget keep
    /// compaction progress visible until the run reports completion or failure.
    active_compaction_header: Option<String>,
}

impl Default for StatusState {
    fn default() -> Self {
        Self {
            current_status: StatusIndicatorState::working(),
            pending_guardian_review_status: PendingGuardianReviewStatus::default(),
            terminal_title_status_kind: TerminalTitleStatusKind::Working,
            retry_status_header: None,
            pending_status_indicator_restore: false,
            active_compaction_header: None,
        }
    }
}

impl StatusState {
    pub(super) fn set_status(&mut self, status: StatusIndicatorState) {
        self.current_status = status;
    }

    /// Records that a compaction run owns the status indicator and should not be overwritten.
    pub(super) fn begin_compaction_status(&mut self, header: String) {
        self.active_compaction_header = Some(header);
    }

    /// Releases the compaction status so normal turn headers apply again.
    pub(super) fn end_compaction_status(&mut self) {
        self.active_compaction_header = None;
    }

    /// True when `header` would replace visible compaction progress.
    pub(super) fn compaction_status_blocks(&self, header: &str) -> bool {
        self.active_compaction_header
            .as_ref()
            .is_some_and(|active| active != header)
    }

    pub(super) fn take_retry_status_header(&mut self) -> Option<String> {
        self.retry_status_header.take()
    }

    pub(super) fn remember_retry_status_header(&mut self) {
        if self.retry_status_header.is_none() {
            self.retry_status_header = Some(self.current_status.header.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn guardian_status_aggregates_parallel_reviews() {
        let mut state = PendingGuardianReviewStatus::default();
        state.start_or_update("a".to_string(), "first".to_string());
        state.start_or_update("b".to_string(), "second".to_string());

        assert_eq!(
            state.status_indicator_state(),
            Some(StatusIndicatorState {
                header: "Reviewing 2 approval requests".to_string(),
                details: Some("• first\n• second".to_string()),
                details_max_lines: 4,
            })
        );
    }

    #[test]
    fn retry_status_header_is_taken_once() {
        let mut state = StatusState::default();
        state.current_status.header = "Thinking".to_string();

        state.remember_retry_status_header();

        assert_eq!(
            state.take_retry_status_header(),
            Some("Thinking".to_string())
        );
        assert_eq!(state.take_retry_status_header(), None);
    }
}
