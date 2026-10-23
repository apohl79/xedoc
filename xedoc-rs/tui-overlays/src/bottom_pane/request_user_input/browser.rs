//! Browser controls backed by the terminal question state machine.

use super::AutoResolutionTiming;
use super::Focus;
use super::InputResult;
use super::OTHER_OPTION_DESCRIPTION;
use super::OTHER_OPTION_LABEL;
use super::RequestUserInputOverlay;
use std::time::Instant;

impl RequestUserInputOverlay {
    pub(super) fn browser_question_snapshot(&self) -> Option<serde_json::Value> {
        let question = self.current_question()?;
        let mut options = question
            .options
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|option| {
                serde_json::json!({
                    "label": option.label, "description": option.description,
                })
            })
            .collect::<Vec<_>>();
        if Self::other_option_enabled_for_question(question) {
            options.push(serde_json::json!({
                "label": OTHER_OPTION_LABEL, "description": OTHER_OPTION_DESCRIPTION,
            }));
        }
        let remaining_seconds = match self.auto_resolution_timing_at(Instant::now()) {
            AutoResolutionTiming::VisibleCountdown { remaining } => {
                Some(remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0))
            }
            AutoResolutionTiming::Due => Some(0),
            AutoResolutionTiming::Disabled | AutoResolutionTiming::HiddenGrace { .. } => None,
        };
        Some(serde_json::json!({
            "requestId": self.request.item_id,
            "index": self.current_index(), "total": self.question_count(),
            "question": question.question, "header": question.header,
            "options": options, "selected": self.selected_option_index(),
            "notes": self.composer.current_text_with_pending(),
            "confirming": self.confirm_unanswered_active(),
            "remainingSeconds": remaining_seconds,
        }))
    }

    pub(super) fn apply_browser_question(
        &mut self,
        request_id: &str,
        index: usize,
        action: &str,
        option: Option<usize>,
        text: Option<&str>,
    ) -> bool {
        if self.done
            || request_id != self.request.item_id
            || index != self.current_index()
            || text.is_some_and(|text| text.len() > 1_000_000)
        {
            return false;
        }
        if self.confirm_unanswered_active() {
            match action {
                "confirm" => self.submit_answers(),
                "back" => {
                    self.close_unanswered_confirmation();
                    if let Some(index) = self.first_unanswered_index() {
                        self.jump_to_question(index);
                    }
                }
                "cancel" => self.cancel_browser_question(),
                _ => return false,
            }
        } else {
            match action {
                "select" => {
                    let Some(option) = option.filter(|option| *option < self.options_len()) else {
                        return false;
                    };
                    if let Some(answer) = self.current_answer_mut() {
                        answer.options_state.selected_idx = Some(option);
                    }
                    self.focus = Focus::Options;
                    self.select_current_option(/*committed*/ true);
                }
                "notes" => {
                    let Some(text) = text else {
                        return false;
                    };
                    self.composer
                        .set_text_content(text.to_owned(), Vec::new(), Vec::new());
                    self.composer.move_cursor_to_end();
                    self.focus = Focus::Notes;
                    self.ensure_selected_for_notes();
                    self.save_current_draft();
                }
                "next" => {
                    let text = self.composer.current_text_with_pending();
                    self.handle_composer_input_result(InputResult::Submitted {
                        text,
                        text_elements: Vec::new(),
                    });
                }
                "previous" => self.move_question(/*next*/ false),
                "cancel" => self.cancel_browser_question(),
                _ => return false,
            }
        }
        if request_id == self.request.item_id {
            self.snooze_auto_resolution();
        }
        true
    }

    fn cancel_browser_question(&mut self) {
        self.focus = Focus::Options;
        <Self as super::BottomPaneView>::on_ctrl_c(self);
    }
}
