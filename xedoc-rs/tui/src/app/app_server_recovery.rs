//! Recovery of a lost remote app-server connection.
//!
//! A local daemon restart (or a transient network failure for an explicit remote target) closes
//! the TUI's WebSocket without a closing handshake. Instead of exiting, the TUI reconnects with a
//! bounded, backed-off retry loop and then restores every live thread through `thread/resume`.
//!
//! Connecting and restoring are deliberately timed separately: a freshly started app server can
//! take well over ten seconds to answer its first `thread/resume` (models refresh, MCP startup,
//! plugin sync), so the restore phase gets a much longer budget than a single connect attempt.

use super::App;
use super::session_lifecycle::ThreadAttachPresentation;
use super::thread_events::ThreadEventAttachment;
use crate::AppServerTarget;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::chatwidget::ThreadInputStateRestoreMode;
use crate::chatwidget::UserMessage;
use crate::tui;
use std::time::Duration;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ThreadStatus;
use xedoc_app_server_protocol::Turn;
use xedoc_app_server_protocol::TurnCompletedNotification;
use xedoc_app_server_protocol::TurnItemsView;
use xedoc_app_server_protocol::TurnStatus;
use xedoc_protocol::ThreadId;

const APP_SERVER_RECONNECT_ATTEMPTS: u8 = 5;
const APP_SERVER_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const APP_SERVER_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(8);
const APP_SERVER_THREAD_RESTORE_TIMEOUT: Duration = Duration::from_secs(120);

/// Prompt submitted on the user's behalf when the reconnected server reports that the turn which
/// was running at disconnect time did not survive the restart.
pub(super) const INTERRUPTED_TURN_CONTINUATION_PROMPT: &str = "The app server restarted and interrupted your \
previous turn. Continue where you left off; do not repeat work that already completed.";

fn thread_not_found_error(message: &str) -> bool {
    message.contains("no rollout found for thread id")
}

impl App {
    pub(super) async fn handle_app_server_disconnected(
        &mut self,
        tui: &mut tui::Tui,
        app_server_client: &mut AppServerSession,
        message: String,
    ) {
        tracing::warn!("app-server event stream disconnected: {message}");
        if !self.app_server_target.supports_thread_disconnect() {
            self.chat_widget.add_error_message(message.clone());
            self.app_event_tx.send(AppEvent::FatalExitRequest(message));
            return;
        }
        self.chat_widget.add_warning_message(format!(
            "Lost the app-server connection; reconnecting (up to {APP_SERVER_RECONNECT_ATTEMPTS} attempts)…"
        ));
        if let Err(err) = self.render_chat_widget_frame(tui) {
            tracing::debug!("failed to render reconnect notice: {err}");
        }
        match self
            .recover_app_server_connection(tui, app_server_client)
            .await
        {
            Ok(()) => {
                self.chat_widget.add_info_message(
                    "Reconnected to the app server.".to_string(),
                    /*hint*/ None,
                );
                tui.frame_requester().schedule_frame();
            }
            Err(recovery_error) => {
                tracing::warn!("failed to recover app-server connection: {recovery_error}");
                self.chat_widget
                    .add_error_message(format!("{message}\n{recovery_error}"));
                self.app_event_tx.send(AppEvent::FatalExitRequest(message));
            }
        }
    }

    async fn recover_app_server_connection(
        &mut self,
        tui: &mut tui::Tui,
        app_server_client: &mut AppServerSession,
    ) -> Result<(), String> {
        let endpoint = match &self.app_server_target {
            AppServerTarget::LocalDaemon { endpoint } | AppServerTarget::Remote { endpoint } => {
                endpoint.clone()
            }
            AppServerTarget::Embedded => {
                return Err("disconnected app server does not support reconnection".to_string());
            }
        };

        let mut backoff = APP_SERVER_RECONNECT_INITIAL_BACKOFF;
        let mut last_error = None;
        for attempt in 1..=APP_SERVER_RECONNECT_ATTEMPTS {
            let attempt_started = tokio::time::Instant::now();
            let attempt_result = match app_server_client.reconnect_remote(endpoint.clone()).await {
                Err(err) => Err(format!("connect failed: {err:#}")),
                Ok(()) => {
                    tracing::info!(
                        attempt,
                        elapsed_ms = attempt_started.elapsed().as_millis(),
                        "reconnected to app server; restoring threads"
                    );
                    match tokio::time::timeout(
                        APP_SERVER_THREAD_RESTORE_TIMEOUT,
                        self.restore_reconnected_threads(tui, app_server_client),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => Err(format!(
                            "thread restore timed out after {} seconds",
                            APP_SERVER_THREAD_RESTORE_TIMEOUT.as_secs()
                        )),
                    }
                }
            };

            match attempt_result {
                Ok(()) => {
                    tracing::info!(
                        attempt,
                        elapsed_ms = attempt_started.elapsed().as_millis(),
                        "app-server connection recovered"
                    );
                    return Ok(());
                }
                Err(err) => {
                    tracing::warn!(
                        attempt,
                        max_attempts = APP_SERVER_RECONNECT_ATTEMPTS,
                        "app-server reconnect attempt failed: {err}"
                    );
                    last_error = Some(err);
                }
            }

            if attempt < APP_SERVER_RECONNECT_ATTEMPTS {
                tracing::info!(
                    attempt,
                    backoff_ms = backoff.as_millis(),
                    "retrying app-server connection after backoff"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(APP_SERVER_RECONNECT_MAX_BACKOFF);
            }
        }

        let detail = last_error.unwrap_or_else(|| "no connection attempt was made".to_string());
        Err(format!(
            "app server did not recover the TUI session after {APP_SERVER_RECONNECT_ATTEMPTS} attempts: {detail}"
        ))
    }

    async fn restore_reconnected_threads(
        &mut self,
        tui: &mut tui::Tui,
        app_server_client: &mut AppServerSession,
    ) -> Result<(), String> {
        self.pending_app_server_requests.clear();
        let mut thread_ids: Vec<_> = self
            .thread_event_channels
            .iter()
            .filter_map(|(thread_id, channel)| {
                (channel.attachment() == ThreadEventAttachment::Live).then_some(*thread_id)
            })
            .collect();
        thread_ids.sort_by_key(|thread_id| Some(*thread_id) != self.primary_thread_id);

        for thread_id in thread_ids {
            let started = match app_server_client
                .resume_thread(self.config.clone(), thread_id, self.resume_model_settings())
                .await
            {
                Ok(started) => started,
                Err(err) => {
                    let err = format!("{err:#}");
                    let thread_has_turns = match self.thread_event_channels.get(&thread_id) {
                        Some(channel) => !channel.store.lock().await.snapshot().turns.is_empty(),
                        None => false,
                    };
                    if thread_not_found_error(&err)
                        && self.primary_thread_id == Some(thread_id)
                        && !thread_has_turns
                    {
                        // The thread never persisted a rollout (no turn was submitted before
                        // the restart), so nothing is lost by starting a fresh one.
                        tracing::info!(
                            %thread_id,
                            "restarted app server has no rollout for the empty primary thread; starting a fresh thread"
                        );
                        self.replace_empty_primary_thread_after_reconnect(tui, app_server_client)
                            .await?;
                        continue;
                    }
                    return Err(format!("failed to resume thread {thread_id}: {err}"));
                }
            };
            if started.blocks_direct_input {
                self.agent_navigation.mark_parent_owned(thread_id);
            }
            let session = started.session;
            if self.primary_thread_id == Some(thread_id) {
                self.primary_session_configured = Some(session.clone());
            }
            self.set_agent_model_metadata_from_session(&session);
            let active_turn_id = if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                let mut store = channel.store.lock().await;
                store.reset_after_app_server_reconnect();
                store.active_turn_id().map(ToOwned::to_owned)
            } else {
                None
            };
            let terminated_turn = match active_turn_id {
                Some(active_turn_id) => {
                    self.turn_terminated_by_reconnect(
                        app_server_client,
                        thread_id,
                        active_turn_id,
                        &started.turns,
                    )
                    .await
                }
                None => None,
            };
            let interrupted_by_restart = terminated_turn
                .as_ref()
                .is_some_and(|turn| matches!(turn.status, TurnStatus::Interrupted));
            if let Some(turn) = terminated_turn {
                self.handle_server_notification_event(ServerNotification::TurnCompleted(
                    TurnCompletedNotification {
                        thread_id: thread_id.to_string(),
                        turn,
                    },
                ))
                .await;
            }
            if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                let mut store = channel.store.lock().await;
                store.set_session(session, started.turns);
            }
            if interrupted_by_restart && self.active_thread_id == Some(thread_id) {
                // The synthesized completion travels through the active thread channel. Apply it
                // before submitting the continuation; otherwise the widget still believes the
                // old turn is running, treats the continuation as a steer, and later restores
                // it to the composer when the interruption lands.
                while let Some(event) = self
                    .active_thread_rx
                    .as_mut()
                    .and_then(|rx| rx.try_recv().ok())
                {
                    self.handle_thread_event_now(event);
                }
                self.continue_turn_interrupted_by_restart(thread_id);
            }
        }
        self.refresh_pending_thread_approvals().await;
        Ok(())
    }

    /// Returns the terminal state of the turn the TUI believed was running when the connection
    /// dropped, or `None` when the resumed server still reports it as in progress.
    ///
    /// A hard restart kills the sampler, and the server's turn projection can lag behind the
    /// canonical rollout after such a kill, so a turn missing from `turns` is treated as
    /// interrupted once `thread/read` confirms the thread is no longer active.
    async fn turn_terminated_by_reconnect(
        &self,
        app_server_client: &mut AppServerSession,
        thread_id: ThreadId,
        active_turn_id: String,
        turns: &[Turn],
    ) -> Option<Turn> {
        if let Some(turn) = turns.iter().find(|turn| turn.id == active_turn_id) {
            return (!matches!(turn.status, TurnStatus::InProgress)).then(|| turn.clone());
        }
        let thread_active = match app_server_client
            .thread_read(thread_id, /*include_turns*/ false)
            .await
        {
            Ok(thread) => matches!(thread.status, ThreadStatus::Active { .. }),
            Err(err) => {
                tracing::warn!(
                    %thread_id,
                    "failed to read thread status after reconnect; assuming the turn was interrupted: {err:#}"
                );
                false
            }
        };
        if thread_active {
            return None;
        }
        Some(Turn {
            id: active_turn_id,
            items: Vec::new(),
            items_view: TurnItemsView::NotLoaded,
            status: TurnStatus::Interrupted,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        })
    }

    /// Replaces a primary thread the restarted server cannot resume with a fresh thread while
    /// keeping the composer draft.
    async fn replace_empty_primary_thread_after_reconnect(
        &mut self,
        tui: &mut tui::Tui,
        app_server_client: &mut AppServerSession,
    ) -> Result<(), String> {
        let input_state = self.chat_widget.capture_thread_input_state();
        let started = app_server_client
            .start_thread_with_session_start_source(
                &self.config,
                /*session_start_source*/ None,
            )
            .await
            .map_err(|err| format!("failed to start a replacement thread: {err:#}"))?;
        self.replace_chat_widget_with_app_server_thread(
            tui,
            started,
            ThreadAttachPresentation::SessionLineage,
            /*initial_user_message*/ None,
        )
        .await
        .map_err(|err| format!("failed to attach the replacement thread: {err:#}"))?;
        self.chat_widget.restore_thread_input_state(
            input_state,
            ThreadInputStateRestoreMode {
                preserve_in_flight_turn: false,
            },
        );
        Ok(())
    }

    /// The restarted server marks the turn that was running at disconnect time as interrupted;
    /// submit an explicit continuation so the work resumes without manual intervention.
    fn continue_turn_interrupted_by_restart(&mut self, thread_id: ThreadId) {
        tracing::info!(%thread_id, "continuing turn interrupted by app-server restart");
        self.chat_widget.add_info_message(
            "The app-server restart interrupted the running turn; asking the agent to continue."
                .to_string(),
            /*hint*/ None,
        );
        self.chat_widget
            .submit_user_message_as_plain_user_turn(UserMessage {
                text: INTERRUPTED_TURN_CONTINUATION_PROMPT.to_string(),
                local_images: Vec::new(),
                remote_image_urls: Vec::new(),
                text_elements: Vec::new(),
                mention_bindings: Vec::new(),
            });
    }
}
