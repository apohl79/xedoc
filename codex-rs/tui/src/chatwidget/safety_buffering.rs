//! Safety-buffering status and retry UI for active turns.

use super::*;
use codex_app_server_protocol::ModelSafetyBufferingUpdatedNotification;

const SAFETY_BUFFERING_PROMPT_VIEW_ID: &str = "safety-buffering-prompt";
const SAFETY_BUFFERING_LEARN_MORE_URL: &str = "https://help.openai.com/en/articles/20001326";

const SAFETY_BUFFERING_HEADER: &str =
    "Our systems are thinking a bit more about this request before responding.";
const SAFETY_BUFFERING_MESSAGE_WITH_RETRY: &str = "Hang tight or retry with a faster model for a quicker response, though it may be less capable of handling complex requests.";
const SAFETY_BUFFERING_FOOTER: &str = "No action is required. Codex will keep waiting, and this menu will close when the response is ready.";

#[derive(Debug)]
struct ActiveSafetyBuffering {
    turn_id: String,
    last_prompt_had_retry: bool,
    agent_message_started: bool,
}

#[derive(Debug)]
struct AcceptedSafetyBufferingSteer {
    pending_steer_id: Option<u64>,
    input: Vec<UserInput>,
}

#[derive(Debug, Default)]
pub(super) struct SafetyBufferingState {
    submitted_turn: Option<(String, AppCommand)>,
    accepted_steers: Vec<AcceptedSafetyBufferingSteer>,
    active: Option<ActiveSafetyBuffering>,
}

impl ChatWidget {
    pub(crate) fn record_safety_buffering_turn(&mut self, turn_id: String, turn: &AppCommand) {
        self.safety_buffering.submitted_turn = Some((turn_id, turn.clone()));
        self.safety_buffering.accepted_steers.clear();
    }

    pub(crate) fn record_safety_buffering_steer(
        &mut self,
        turn_id: &str,
        pending_steer_id: Option<u64>,
        input: &[UserInput],
    ) {
        let Some((submitted_turn_id, AppCommand::UserTurn { .. })) =
            self.safety_buffering.submitted_turn.as_ref()
        else {
            return;
        };
        if submitted_turn_id != turn_id || input.is_empty() {
            return;
        }
        self.safety_buffering
            .accepted_steers
            .push(AcceptedSafetyBufferingSteer {
                pending_steer_id,
                input: input.to_vec(),
            });
    }

    pub(crate) fn record_safety_buffering_steer_canceled(&mut self, pending_steer_id: u64) {
        self.safety_buffering
            .accepted_steers
            .retain(|steer| steer.pending_steer_id != Some(pending_steer_id));
    }

    pub(crate) fn apply_safety_buffered_retry_input(
        &self,
        turn_id: &str,
        turn: &mut AppCommand,
        input_state: &mut ThreadInputState,
    ) -> usize {
        let Some((submitted_turn_id, AppCommand::UserTurn { items, .. })) =
            self.safety_buffering.submitted_turn.as_ref()
        else {
            return 0;
        };
        let AppCommand::UserTurn {
            items: retry_items, ..
        } = turn
        else {
            return 0;
        };
        if submitted_turn_id != turn_id {
            return 0;
        }
        *retry_items = items.clone();
        for steer in &self.safety_buffering.accepted_steers {
            retry_items.push(UserInput::Text {
                text: "\n".to_string(),
                text_elements: Vec::new(),
            });
            retry_items.extend(steer.input.iter().cloned());
        }
        let accepted_steer_count = self.safety_buffering.accepted_steers.len();
        let pending_steer_count = accepted_steer_count.min(input_state.pending_steers.len());
        input_state.pending_steers.drain(..pending_steer_count);
        input_state
            .pending_steer_history_records
            .drain(..pending_steer_count);
        input_state.pending_steer_ids.drain(..pending_steer_count);
        input_state
            .pending_steer_compare_keys
            .drain(..pending_steer_count);
        accepted_steer_count
    }

    pub(super) fn reset_safety_buffering_for_turn_start(&mut self) {
        self.bottom_pane
            .dismiss_view_by_id(SAFETY_BUFFERING_PROMPT_VIEW_ID);
        self.safety_buffering.active = None;
    }

    pub(crate) fn clear_safety_buffering(&mut self) {
        self.bottom_pane
            .dismiss_view_by_id(SAFETY_BUFFERING_PROMPT_VIEW_ID);
        self.safety_buffering = SafetyBufferingState::default();
    }

    pub(super) fn mark_safety_buffering_agent_message_started(&mut self) {
        if let Some(active) = self.safety_buffering.active.as_mut() {
            active.agent_message_started = true;
        }
    }

    pub(super) fn safety_buffering_is_waiting(&self) -> bool {
        self.safety_buffering
            .active
            .as_ref()
            .is_some_and(|active| !active.agent_message_started)
    }

    pub(crate) fn can_retry_safety_buffered_turn(&self, turn_id: &str) -> bool {
        self.turn_lifecycle.agent_turn_running
            && self
                .safety_buffering
                .active
                .as_ref()
                .is_some_and(|active| active.turn_id == turn_id && !active.agent_message_started)
    }

    pub(crate) fn prepare_safety_buffered_retry_submission(&mut self, prompt: UserMessage) {
        self.last_rendered_user_message_display = None;
        self.finalize_turn();
        self.safety_buffering_prompt = Some(prompt);
        self.input_queue.user_turn_pending_start = true;
    }

    pub(crate) fn commit_safety_buffered_retry_submission(&mut self, display: UserMessageDisplay) {
        self.on_user_message_display(display);
    }

    pub(crate) fn cancel_safety_buffered_retry_submission(&mut self) {
        self.input_queue.user_turn_pending_start = false;
        self.clear_safety_buffering();
    }

    pub(super) fn on_model_safety_buffering_updated(
        &mut self,
        notification: ModelSafetyBufferingUpdatedNotification,
        replay_kind: Option<ReplayKind>,
    ) {
        let ModelSafetyBufferingUpdatedNotification {
            turn_id,
            show_buffering_ui,
            faster_model,
            ..
        } = notification;
        if matches!(replay_kind, Some(ReplayKind::ResumeInitialMessages))
            || !self.turn_lifecycle.agent_turn_running
            || self.turn_lifecycle.last_turn_id.as_deref() != Some(turn_id.as_str())
        {
            return;
        }
        if !show_buffering_ui {
            if self
                .safety_buffering
                .active
                .as_ref()
                .is_some_and(|active| active.turn_id == turn_id)
            {
                self.bottom_pane
                    .dismiss_view_by_id(SAFETY_BUFFERING_PROMPT_VIEW_ID);
                self.safety_buffering.active = None;
                self.restore_reasoning_status_header();
            }
            return;
        }

        let retry_turn = if self.side_conversation_active() {
            None
        } else {
            self.safety_buffering
                .submitted_turn
                .as_ref()
                .filter(|(submitted_turn_id, _)| {
                    replay_kind.is_none() && submitted_turn_id == &turn_id
                })
                .map(|(_, turn)| turn.clone())
        };
        let thread_id = self.thread_id;
        let retry_prompt = self.safety_buffering_prompt.clone();
        let can_offer_retry = faster_model.is_some()
            && retry_turn.is_some()
            && retry_prompt.is_some()
            && thread_id.is_some();
        let previous_active = self
            .safety_buffering
            .active
            .as_ref()
            .filter(|active| active.turn_id == turn_id);
        let should_show_prompt =
            previous_active.is_none_or(|active| active.last_prompt_had_retry != can_offer_retry);
        let agent_message_started =
            previous_active.is_some_and(|active| active.agent_message_started);
        self.safety_buffering.active = Some(ActiveSafetyBuffering {
            turn_id: turn_id.clone(),
            last_prompt_had_retry: can_offer_retry,
            agent_message_started,
        });

        let status_details = if can_offer_retry {
            format!("{SAFETY_BUFFERING_HEADER} {SAFETY_BUFFERING_MESSAGE_WITH_RETRY}")
        } else {
            SAFETY_BUFFERING_HEADER.to_string()
        };
        self.bottom_pane.ensure_status_indicator();
        self.set_status(
            "Working".to_string(),
            Some(status_details),
            StatusDetailsCapitalization::Preserve,
            /*details_max_lines*/ 6,
        );

        if !should_show_prompt {
            return;
        }
        self.bottom_pane
            .dismiss_view_by_id(SAFETY_BUFFERING_PROMPT_VIEW_ID);

        let mut header = vec![Box::new(
            Paragraph::new(Line::from(SAFETY_BUFFERING_HEADER).bold()).wrap(Wrap { trim: false }),
        ) as Box<dyn Renderable>];
        if can_offer_retry {
            header.push(Box::new(
                Paragraph::new(Line::from(SAFETY_BUFFERING_MESSAGE_WITH_RETRY).dim())
                    .wrap(Wrap { trim: false }),
            ));
        }
        let header = ColumnRenderable::with(header);
        let mut items = Vec::new();
        if let (Some(faster_model), Some(turn), Some(prompt), Some(thread_id)) =
            (faster_model, retry_turn, retry_prompt, thread_id)
        {
            items.push(SelectionItem {
                name: "Retry with a faster model".to_string(),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::RetrySafetyBufferedTurn {
                        thread_id,
                        turn_id: turn_id.clone(),
                        model: faster_model.clone(),
                        turn: turn.clone(),
                        prompt: prompt.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        items.extend([
            SelectionItem {
                name: "Dismiss and keep waiting".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Learn more".to_string(),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::OpenUrlInBrowser {
                        url: SAFETY_BUFFERING_LEARN_MORE_URL.to_string(),
                    });
                })],
                ..Default::default()
            },
        ]);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(SAFETY_BUFFERING_PROMPT_VIEW_ID),
            header: Box::new(header),
            footer_note: Some(Line::from(SAFETY_BUFFERING_FOOTER).dim()),
            footer_hint: Some(Line::default()),
            items,
            ..Default::default()
        });
    }
}
