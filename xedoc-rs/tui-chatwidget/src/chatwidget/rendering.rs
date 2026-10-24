//! Render composition for the main chat widget surface.

use super::*;

impl ChatWidget {
    /// Expose the active shared question to native browser controls.
    pub fn browser_question(&self) -> Option<serde_json::Value> {
        self.bottom_pane.browser_question()
    }

    /// Reconcile browser question actions with the terminal view state.
    pub fn handle_browser_question(
        &mut self,
        request_id: &str,
        index: usize,
        action: &str,
        option: Option<usize>,
        text: Option<&str>,
    ) {
        self.bottom_pane
            .handle_browser_question(request_id, index, action, option, text);
    }

    /// Exposes shared input state to a browser without duplicating composer behavior.
    pub fn browser_draft(&self) -> crate::bottom_pane::chat_composer::ComposerDraftSnapshot {
        let mut draft = self.bottom_pane.composer_draft_snapshot();
        if draft
            .pending_pastes
            .iter()
            .any(|(anchor, _)| anchor.starts_with("[Doc #"))
        {
            for (anchor, text) in &draft.pending_pastes {
                if !anchor.starts_with("[Doc #") {
                    draft.text = draft.text.replace(anchor, text);
                }
            }
        } else {
            draft.text = self.bottom_pane.composer_text_with_pending();
        }
        draft
    }

    /// Reconciles a browser edit using the external-editor attachment rules.
    pub fn apply_browser_draft(&mut self, text: String) {
        if self.bottom_pane.composer_input_enabled() {
            self.apply_external_edit(text);
        }
    }

    /// Adds a file reference using the composer's shared submission placeholders.
    pub fn attach_browser_document(&mut self, anchor: String, reference: String) {
        if !self.bottom_pane.composer_input_enabled() {
            return;
        }
        let draft = self.browser_draft();
        let mut pending = draft.pending_pastes;
        let separator = if draft.text.is_empty() || draft.text.ends_with(char::is_whitespace) {
            ""
        } else {
            " "
        };
        let text = format!("{}{separator}{anchor} ", draft.text);
        self.apply_browser_draft(text.clone());
        pending.push((anchor, reference));
        self.bottom_pane.set_composer_pending_pastes(pending);
        self.apply_browser_draft(text);
    }

    /// Returns composer layout and metadata for the native browser surface.
    pub fn browser_composer_context(
        &self,
        width: u16,
    ) -> (
        u16,
        u16,
        bool,
        Option<String>,
        Option<u64>,
        Option<ratatui::text::Line<'static>>,
        String,
        Option<String>,
    ) {
        let (router, elapsed, model, effort) = self.bottom_pane.browser_context();
        (
            self.bottom_pane.browser_composer_height(width),
            self.bottom_pane.browser_popup_height(width),
            self.bottom_pane.browser_has_dialog(),
            router.map(str::to_owned),
            elapsed,
            self.bottom_pane.browser_status_line_styled(),
            model.unwrap_or_else(|| self.current_model()).to_owned(),
            if model.is_some() {
                effort.map(str::to_owned)
            } else {
                self.current_reasoning_effort()
                    .map(|effort| effort.as_str().to_owned())
            },
        )
    }

    pub fn as_renderable(&self) -> RenderableItem<'_> {
        let active_cell_renderable = match &self.transcript.active_cell {
            Some(cell) => RenderableItem::Owned(Box::new(TranscriptAreaRenderable {
                child: cell.as_ref(),
                top: 1,
            })),
            None => RenderableItem::Owned(Box::new(())),
        };
        let tool_summary_renderable = match &self.tool_call_summary {
            Some(summary)
                if self.turn_lifecycle.agent_turn_running
                    && self.optimized_tool_call_rendering()
                    && summary.has_calls() =>
            {
                RenderableItem::Owned(Box::new(TranscriptAreaRenderable {
                    child: summary.cell(),
                    top: 1,
                }))
            }
            _ => RenderableItem::Owned(Box::new(())),
        };
        self.renderable_with_active_cell(active_cell_renderable, tool_summary_renderable)
    }

    /// Exposes the same active history cell without imposing terminal layout.
    pub fn browser_active_cell(&self) -> Option<&dyn HistoryCell> {
        self.transcript.active_cell.as_deref()
    }

    /// Exposes the live assistant or plan source for browser-native rendering.
    pub fn browser_stream_content(&self) -> Option<BrowserStreamContent<'_>> {
        self.stream_controller
            .as_ref()
            .map(|controller| BrowserStreamContent::Assistant(controller.source()))
            .or_else(|| {
                self.plan_stream_controller
                    .as_ref()
                    .map(|controller| BrowserStreamContent::Plan(controller.source()))
            })
    }

    /// Exposes the live optimized tool summary for browser-native rendering.
    pub fn browser_tool_summary(&self) -> Option<&dyn HistoryCell> {
        self.tool_call_summary
            .as_ref()
            .filter(|summary| {
                self.turn_lifecycle.agent_turn_running
                    && self.optimized_tool_call_rendering()
                    && summary.has_calls()
            })
            .map(|summary| summary.cell() as &dyn HistoryCell)
    }

    /// Keeps shared activity surfaces while the browser lays out the active cell.
    pub fn browser_renderable(&self) -> RenderableItem<'_> {
        self.renderable_with_active_cell(
            RenderableItem::Owned(Box::new(())),
            RenderableItem::Owned(Box::new(())),
        )
    }

    fn renderable_with_active_cell<'a>(
        &'a self,
        active_cell_renderable: RenderableItem<'a>,
        tool_summary_renderable: RenderableItem<'a>,
    ) -> RenderableItem<'a> {
        let active_hook_cell_renderable = match &self.active_hook_cell {
            Some(cell) if cell.should_render() => {
                RenderableItem::Owned(Box::new(TranscriptAreaRenderable {
                    child: cell,
                    top: 1,
                }))
            }
            _ => RenderableItem::Owned(Box::new(())),
        };
        let mut flex = FlexRenderable::new();
        flex.push(/*flex*/ 1, active_cell_renderable);
        flex.push(/*flex*/ 0, tool_summary_renderable);
        flex.push(/*flex*/ 0, active_hook_cell_renderable);
        flex.push(
            /*flex*/ 0,
            self.bottom_pane.as_renderable().inset(Insets::tlbr(
                /*top*/ 1, /*left*/ 0, /*bottom*/ 0, /*right*/ 0,
            )),
        );
        RenderableItem::Owned(Box::new(flex))
    }

    pub fn note_rendered_width(&self, width: u16) {
        self.last_rendered_width.set(Some(width as usize));
    }

    pub fn history_wrap_width(&self, width: u16) -> u16 {
        width.max(1)
    }
}

struct TranscriptAreaRenderable<'a> {
    child: &'a dyn HistoryCell,
    top: u16,
}

impl Renderable for TranscriptAreaRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let area = self.child_area(area);
        let lines = self.child.display_lines(area.width);
        let background_style = self.child.display_background_style();
        Clear.render(area, buf);
        if let Some(style) = background_style {
            buf.set_style(area, style);
        }
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let y = if area.height == 0 {
            0
        } else {
            let overflow = paragraph
                .line_count(area.width)
                .saturating_sub(usize::from(area.height));
            u16::try_from(overflow).unwrap_or(u16::MAX)
        };
        paragraph.scroll((y, 0)).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let child_width = width.max(1);
        HistoryCell::desired_height(self.child, child_width) + self.top
    }
}

impl TranscriptAreaRenderable<'_> {
    fn child_area(&self, area: Rect) -> Rect {
        let y = area.y.saturating_add(self.top);
        let height = area.height.saturating_sub(self.top);
        Rect::new(area.x, y, area.width.max(1), height)
    }
}

impl Renderable for ChatWidget {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.as_renderable().render(area, buf);
        self.note_rendered_width(area.width);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.as_renderable().desired_height(width)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.as_renderable().cursor_pos(area)
    }

    fn cursor_style(&self, area: Rect) -> crossterm::cursor::SetCursorStyle {
        self.as_renderable().cursor_style(area)
    }
}
