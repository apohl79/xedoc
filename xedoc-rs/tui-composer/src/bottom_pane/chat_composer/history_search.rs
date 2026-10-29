//! Composer-side Ctrl+R reverse history search state and rendering helpers.
//!
//! The persistent and local history stores live in `chat_composer_history`, but the composer owns
//! the active search session because it has to snapshot/restore the editable draft, preview matches
//! in the textarea, and render the footer prompt while the footer line is acting as the search
//! input.
//!
//! This module is responsible for the UI-facing lifecycle of a search session: recognizing the
//! keys that enter and drive search mode, keeping the footer query separate from the textarea
//! preview, restoring the original draft on cancellation or misses, and translating history search
//! results into composer-visible state. It deliberately does not decide which history entries
//! match, how duplicate results are skipped, or when persistent history should be fetched; those
//! traversal invariants stay with `ChatComposerHistory`.
//!
//! A search session starts idle with an empty footer query, so opening Ctrl+R never previews the
//! latest history entry by itself. Typing a query restarts traversal from newest to oldest,
//! repeated Ctrl+R/Up and Ctrl+S/Down move between unique matches, `Enter` accepts the current
//! preview as an editable draft, and `Esc` or Ctrl+C restores the exact draft that existed before
//! search started.

use crate::city_lights::CityLightsStylize;
use std::ops::Range;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

use super::super::chat_composer_history::HistorySearchDirection;
use super::super::chat_composer_history::HistorySearchResult;
use super::super::footer::reset_mode_after_activity;
use super::ActivePopup;
use super::ChatComposer;
use super::ComposerDraft;
use super::InputResult;
use crate::app_event::AppEvent;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::KeyBindingListExt;
use crate::key_hint::has_ctrl_or_alt;
use crate::ui_consts::FOOTER_INDENT_COLS;

/// Active composer-owned state for one Ctrl+R search interaction.
///
/// The session is created only by [`ChatComposer::begin_history_search`] and is cleared only by
/// accepting, canceling, or replacing the search mode. It stores the original draft separately from
/// the footer query so transient previews never destroy the user's in-progress composer content.
#[derive(Clone, Debug)]
pub(super) struct HistorySearchSession {
    /// Draft to restore when search is canceled or a query has no match.
    original_draft: ComposerDraft,
    /// Footer-owned query text typed while Ctrl+R search is active.
    query: String,
    /// User-visible search status used to choose footer hints and composer preview behavior.
    status: HistorySearchStatus,
}

/// User-visible phase of the active Ctrl+R search session.
///
/// Search keeps the footer query and the composer preview separate: `Idle` leaves the original
/// draft untouched, `Searching` waits for persistent history, `Match` previews a found entry, and
/// `NoMatch` restores the original draft while leaving the search input open for more typing.
#[derive(Clone, Debug)]
enum HistorySearchStatus {
    Idle,
    Searching,
    Match,
    NoMatch,
}

impl ChatComposer {
    /// Returns whether a key event should open reverse history search or step to an older match.
    ///
    /// The check accepts both normal Ctrl+R reports and the raw control character variant that
    /// some terminals emit. Callers should only use this before generic text handling; treating the
    /// raw control character as ordinary input would insert an invisible byte into the search query
    /// or composer draft.
    pub(super) fn is_history_search_key(key_event: &KeyEvent, bindings: &[KeyBinding]) -> bool {
        bindings.is_pressed(*key_event)
    }

    fn is_history_search_forward_key(key_event: &KeyEvent, bindings: &[KeyBinding]) -> bool {
        bindings.is_pressed(*key_event)
    }

    /// Opens footer-owned reverse history search without previewing history yet.
    ///
    /// Entering search mode first flushes pending paste-burst text, then snapshots the full
    /// composer draft, clears any file/search popup state, and resets history traversal. The first
    /// visible match is produced only after the footer query becomes non-empty, which keeps Ctrl+R
    /// from replacing an empty composer with the latest prompt before the user has searched for
    /// anything.
    pub(super) fn begin_history_search(&mut self) -> (InputResult, bool) {
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }
        self.draft.paste_burst.clear_window_after_non_char();

        if self.popups.current_file_query.is_some() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
            self.popups.current_file_query = None;
        }
        self.popups.active = ActivePopup::None;
        self.attachments.clear_remote_image_selection();
        self.history_search = Some(HistorySearchSession {
            original_draft: self.snapshot_draft(),
            query: String::new(),
            status: HistorySearchStatus::Idle,
        });
        self.history.reset_search();
        (InputResult::None, true)
    }

    /// Handles every key while the footer is acting as the history search input.
    ///
    /// The method consumes search-mode keys before normal composer editing sees them. It guarantees
    /// that `Esc` and Ctrl+C restore the original draft, `Enter` only accepts an actual match, plain
    /// characters edit the footer query, and navigation keys delegate traversal to
    /// `ChatComposerHistory`. Calling this when no search session exists is harmless for ignored
    /// keys but would make query-edit branches no-op, so route here only after
    /// `history_search.is_some()` has been established.
    pub(super) fn handle_history_search_key(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if key_event.kind == KeyEventKind::Release {
            return (InputResult::None, false);
        }

        if Self::is_history_search_key(&key_event, &self.history_search_previous_keys)
            || matches!(key_event.code, KeyCode::Up)
        {
            let result = self.history_search_in_direction(HistorySearchDirection::Older);
            return (result, true);
        }

        if Self::is_history_search_forward_key(&key_event, &self.history_search_next_keys)
            || matches!(key_event.code, KeyCode::Down)
        {
            let result = self.history_search_in_direction(HistorySearchDirection::Newer);
            return (result, true);
        }

        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.cancel_history_search();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'c') => {
                self.cancel_history_search();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char('\u{0003}'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.cancel_history_search();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if self
                    .history_search
                    .as_ref()
                    .is_some_and(|search| matches!(search.status, HistorySearchStatus::Match))
                {
                    self.history_search = None;
                    self.history.reset_search();
                    self.footer.mode = reset_mode_after_activity(self.footer.mode);
                    self.move_cursor_to_end();
                }
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                if let Some(search) = self.history_search.as_ref() {
                    let mut query = search.query.clone();
                    query.pop();
                    self.update_history_search_query(query);
                }
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.update_history_search_query(String::new());
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
                ..
            } if !has_ctrl_or_alt(modifiers) => {
                if let Some(search) = self.history_search.as_ref() {
                    let mut query = search.query.clone();
                    query.push(ch);
                    self.update_history_search_query(query);
                }
                (InputResult::None, true)
            }
            _ => (InputResult::None, true),
        }
    }

    fn history_search_in_direction(&mut self, direction: HistorySearchDirection) -> InputResult {
        let Some((query, original_draft)) = self
            .history_search
            .as_ref()
            .map(|search| (search.query.clone(), search.original_draft.clone()))
        else {
            return InputResult::None;
        };
        if query.is_empty() {
            self.history.reset_search();
            if let Some(search) = self.history_search.as_mut() {
                search.status = HistorySearchStatus::Idle;
            }
            self.restore_draft(original_draft);
            return InputResult::None;
        }
        let result = self.history.search(
            &query,
            direction,
            /*restart*/ false,
            &self.app_event_tx,
        );
        self.apply_history_search_result(result);
        InputResult::None
    }

    fn update_history_search_query(&mut self, query: String) {
        let Some(original_draft) = self
            .history_search
            .as_ref()
            .map(|search| search.original_draft.clone())
        else {
            return;
        };
        if let Some(search) = self.history_search.as_mut() {
            search.query = query.clone();
            search.status = HistorySearchStatus::Searching;
        }
        self.restore_draft(original_draft);
        if query.is_empty() {
            self.history.reset_search();
            if let Some(search) = self.history_search.as_mut() {
                search.status = HistorySearchStatus::Idle;
            }
            return;
        }
        let result = self.history.search(
            &query,
            HistorySearchDirection::Older,
            /*restart*/ true,
            &self.app_event_tx,
        );
        self.apply_history_search_result(result);
    }

    /// Cancels active history search and restores the draft from before search mode opened.
    ///
    /// This clears normal history navigation as well as search traversal because previewing a match
    /// temporarily updates the shared history cursor. Callers that handle global cancellation, such
    /// as Ctrl+C, should use the boolean result to consume the key without also clearing the
    /// restored draft or triggering quit/interrupt behavior.
    pub fn cancel_history_search(&mut self) -> bool {
        let Some(search) = self.history_search.take() else {
            return false;
        };
        self.history.reset_navigation();
        self.footer.mode = reset_mode_after_activity(self.footer.mode);
        self.restore_draft(search.original_draft);
        true
    }

    /// Applies a traversal result to the composer preview and search status.
    ///
    /// `Found` previews the matching entry, `Pending` keeps the footer in a waiting state while an
    /// async persistent entry lookup is outstanding, `AtBoundary` preserves the current match, and
    /// `NotFound` restores the original draft while keeping the query available for further edits,
    /// and `Unavailable` does the same without claiming there was no match. Treating `AtBoundary`
    /// like `NotFound` would produce the visible "no match" flicker at the end of a one-result
    /// search and desynchronize Up/Down counts.
    pub(super) fn apply_history_search_result(&mut self, result: HistorySearchResult) {
        match result {
            HistorySearchResult::Found(entry) => {
                if let Some(search) = self.history_search.as_mut() {
                    search.status = HistorySearchStatus::Match;
                }
                self.apply_history_entry(entry);
            }
            HistorySearchResult::Pending => {
                if let Some(search) = self.history_search.as_mut() {
                    search.status = HistorySearchStatus::Searching;
                }
            }
            HistorySearchResult::AtBoundary => {
                if let Some(search) = self.history_search.as_mut() {
                    search.status = HistorySearchStatus::Match;
                }
            }
            result @ (HistorySearchResult::NotFound | HistorySearchResult::Unavailable) => {
                let original_draft = self
                    .history_search
                    .as_ref()
                    .map(|search| search.original_draft.clone());
                if let Some(search) = self.history_search.as_mut() {
                    search.status = if matches!(result, HistorySearchResult::NotFound) {
                        HistorySearchStatus::NoMatch
                    } else {
                        HistorySearchStatus::Idle
                    };
                }
                if let Some(original_draft) = original_draft {
                    self.restore_draft(original_draft);
                }
            }
        }
    }

    /// Builds the footer line shown while reverse history search is active.
    ///
    /// The footer displays the query as the editable field and uses the status to decide whether
    /// to show searching, match actions, or no-match feedback. The line is intentionally separate
    /// from cursor placement so rendering can fall back to normal footer layout if a small terminal
    /// cannot allocate a distinct hint row.
    pub(super) fn history_search_footer_line(&self) -> Option<Line<'static>> {
        let search = self.history_search.as_ref()?;
        let mut line = Line::from(vec![
            "reverse-i-search: ".dim(),
            search.query.clone().cl_cyan(),
        ]);
        match search.status {
            HistorySearchStatus::Idle => {}
            HistorySearchStatus::Searching => line.push_span("  searching".dim()),
            HistorySearchStatus::Match => {
                line.push_span("  ".dim());
                line.push_span(Self::history_search_action_key_span(KeyCode::Enter));
                line.push_span(" accept".dim());
                line.push_span(" · ".dim());
                line.push_span(Self::history_search_action_key_span(KeyCode::Esc));
                line.push_span(" cancel".dim());
            }
            HistorySearchStatus::NoMatch => line.push_span("  no match".cl_red()),
        }
        Some(line)
    }

    fn history_search_action_key_span(key: KeyCode) -> Span<'static> {
        Span::from(key_hint::plain(key)).cl_cyan().bold().not_dim()
    }

    /// Returns byte ranges that should be highlighted in the current composer preview.
    ///
    /// Highlights are only exposed while a matched history entry is being previewed. Once the user
    /// accepts with `Enter`, the search session is cleared and this returns an empty set so the
    /// accepted text becomes an ordinary editable draft again.
    pub(super) fn history_search_highlight_ranges(&self) -> Vec<Range<usize>> {
        let Some(search) = self.history_search.as_ref() else {
            return Vec::new();
        };
        if !matches!(search.status, HistorySearchStatus::Match) || search.query.is_empty() {
            return Vec::new();
        }
        Self::case_insensitive_match_ranges(self.draft.textarea.text(), &search.query)
    }

    fn case_insensitive_match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
        if query.is_empty() {
            return Vec::new();
        }

        let query_lower = query
            .chars()
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if query_lower.is_empty() {
            return Vec::new();
        }

        let mut folded = String::new();
        let mut folded_spans: Vec<(Range<usize>, Range<usize>)> = Vec::new();
        for (original_start, ch) in text.char_indices() {
            let original_range = original_start..original_start + ch.len_utf8();
            for lower in ch.to_lowercase() {
                let folded_start = folded.len();
                folded.push(lower);
                folded_spans.push((folded_start..folded.len(), original_range.clone()));
            }
        }

        let mut ranges = Vec::new();
        let mut search_from = 0;
        while search_from <= folded.len()
            && let Some(relative_start) = folded[search_from..].find(&query_lower)
        {
            let folded_start = search_from + relative_start;
            let folded_end = folded_start + query_lower.len();
            if let Some((_, first_original)) = folded_spans.iter().find(|(folded_range, _)| {
                folded_range.end > folded_start && folded_range.start < folded_end
            }) {
                let original_end = folded_spans
                    .iter()
                    .rev()
                    .find(|(folded_range, _)| {
                        folded_range.end > folded_start && folded_range.start < folded_end
                    })
                    .map(|(_, original_range)| original_range.end)
                    .unwrap_or(first_original.end);
                ranges.push(first_original.start..original_end);
            }
            search_from = folded_end;
        }
        ranges
    }

    /// Returns the screen cursor position for the footer query when search mode is active.
    ///
    /// The cursor tracks the end of the footer query rather than the textarea preview. If the
    /// footer area is collapsed or too narrow, the x coordinate is clamped inside the hint rect so
    /// terminal backends do not receive an off-screen cursor position.
    pub(super) fn history_search_cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let search = self.history_search.as_ref()?;
        let [_, _, _, popup_rect] = self.layout_areas(area);
        if popup_rect.is_empty() {
            return None;
        }

        let footer_props = self.footer_props();
        let footer_hint_height = self.footer_hint_height(&footer_props);
        let persistent_status_line_height =
            u16::from(self.persistent_status_line(&footer_props).is_some());
        let [_, hint_rect] = Self::footer_content_rects(
            popup_rect,
            footer_hint_height,
            persistent_status_line_height,
        );
        if hint_rect.is_empty() {
            return None;
        }

        let prompt_width = Line::from("reverse-i-search: ").width() as u16;
        let query_width = Line::from(search.query.clone()).width() as u16;
        let desired_x = hint_rect
            .x
            .saturating_add(FOOTER_INDENT_COLS as u16)
            .saturating_add(prompt_width)
            .saturating_add(query_width);
        let max_x = hint_rect
            .x
            .saturating_add(hint_rect.width.saturating_sub(1));
        Some((desired_x.min(max_x), hint_rect.y))
    }
}
