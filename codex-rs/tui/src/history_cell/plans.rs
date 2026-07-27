//! Proposed-plan history cells.

use super::*;

/// Transient active-cell representation of the mutable tail of a proposed-plan stream.
///
/// The controller prepares the full styled plan lines because plan tails need the same header,
/// padding, and background treatment as committed `ProposedPlanStreamCell`s while remaining
/// preview-only during streaming.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StreamingPlanTailCell {
    lines: Vec<HyperlinkLine>,
    is_stream_continuation: bool,
}

impl StreamingPlanTailCell {
    pub(crate) fn new(lines: Vec<HyperlinkLine>, is_stream_continuation: bool) -> Self {
        Self {
            lines,
            is_stream_continuation,
        }
    }
}

impl HistoryCell for StreamingPlanTailCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        visible_lines(self.lines.clone())
    }

    fn display_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        self.lines.clone()
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(visible_lines(self.lines.clone()))
    }

    fn is_stream_continuation(&self) -> bool {
        self.is_stream_continuation
    }
}
/// Create a proposed-plan cell that snapshots the session cwd for later markdown rendering.
///
/// The plan body is stored as raw markdown so terminal resize reflow can render it again at the
/// current width. Callers should use `new_proposed_plan_stream` only for transient live streaming
/// cells, then consolidate to this source-backed cell when the plan is complete.
pub(crate) fn new_proposed_plan(plan_markdown: String, cwd: &Path) -> ProposedPlanCell {
    ProposedPlanCell {
        plan_markdown,
        cwd: cwd.to_path_buf(),
    }
}

/// Create a transient proposed-plan stream cell from already rendered lines.
///
/// Stream cells are display fragments, not source-backed history. They should be replaced by
/// `ProposedPlanCell` during consolidation before relying on resize reflow for finalized history.
pub(crate) fn new_proposed_plan_stream(
    lines: Vec<impl Into<HyperlinkLine>>,
    is_stream_continuation: bool,
) -> ProposedPlanStreamCell {
    ProposedPlanStreamCell {
        lines: lines.into_iter().map(Into::into).collect(),
        is_stream_continuation,
    }
}

/// Finalized proposed-plan history that can render itself again for a new width.
///
/// This is the source-backed counterpart to `ProposedPlanStreamCell`. It owns raw markdown and the
/// session cwd needed for stable local-link rendering during later transcript reflow.
#[derive(Debug)]
pub(crate) struct ProposedPlanCell {
    plan_markdown: String,
    /// Session cwd used to keep local file-link display aligned with live streamed plan rendering.
    cwd: PathBuf,
}

/// Transient proposed-plan history emitted while a plan is still streaming.
///
/// The lines are already rendered for the stream's current width. A finalized transcript should not
/// keep these cells after consolidation, because they cannot re-render their source on a later
/// terminal resize.
#[derive(Debug)]
pub(crate) struct ProposedPlanStreamCell {
    lines: Vec<HyperlinkLine>,
    is_stream_continuation: bool,
}

impl HistoryCell for ProposedPlanCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = vec![
            HyperlinkLine::new(vec!["• ".dim(), "Proposed Plan".bold()].into()),
            HyperlinkLine::new(Line::from(" ")),
        ];

        let mut plan_lines = vec![HyperlinkLine::new(Line::from(" "))];
        let plan_style = proposed_plan_style();
        let wrap_width = width.saturating_sub(4).max(1) as usize;
        let mut body = crate::markdown::render_markdown_agent_with_links_and_cwd(
            &self.plan_markdown,
            Some(wrap_width),
            Some(self.cwd.as_path()),
        );
        if body.is_empty() {
            body.push(HyperlinkLine::new(Line::from("(empty)".dim().italic())));
        }
        plan_lines.extend(prefix_hyperlink_lines(body, "  ".into(), "  ".into()));
        plan_lines.push(HyperlinkLine::new(Line::from(" ")));

        lines.extend(plan_lines.into_iter().map(|line| line.style(plan_style)));
        lines
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        raw_lines_from_source(&self.plan_markdown)
    }
}

impl HistoryCell for ProposedPlanStreamCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        visible_lines(self.lines.clone())
    }

    fn display_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        self.lines.clone()
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(visible_lines(self.lines.clone()))
    }

    fn is_stream_continuation(&self) -> bool {
        self.is_stream_continuation
    }
}
