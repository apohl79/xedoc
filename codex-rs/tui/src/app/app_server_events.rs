//! App-server event stream handling for the TUI app.

use super::App;
use super::app_server_event_targets::ServerNotificationThreadTarget;
use super::app_server_event_targets::server_notification_thread_target;
use super::app_server_event_targets::server_request_thread_id;
use super::thread_events::ThreadEventAttachment;
use crate::AppServerTarget;
use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::status_account_display_from_auth_mode;
use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStatus;
use std::time::Duration;

const LOCAL_DAEMON_RECONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const LOCAL_DAEMON_RECONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(100);

impl App {
    pub(super) fn refresh_mcp_startup_expected_servers_from_config(&mut self) {
        let enabled_config_mcp_servers: Vec<String> = self
            .config
            .mcp_servers
            .get()
            .iter()
            .filter_map(|(name, server)| server.enabled.then_some(name.clone()))
            .collect();
        self.chat_widget
            .set_mcp_startup_expected_servers(enabled_config_mcp_servers);
    }

    pub(super) async fn handle_app_server_event(
        &mut self,
        app_server_client: &AppServerSession,
        event: AppServerEvent,
    ) {
        match event {
            AppServerEvent::Lagged { skipped } => {
                tracing::warn!(
                    skipped,
                    "app-server event consumer lagged; dropping ignored events"
                );
                self.refresh_mcp_startup_expected_servers_from_config();
                self.chat_widget.finish_mcp_startup_after_lag();
            }
            AppServerEvent::ServerNotification(notification) => {
                self.handle_server_notification_event(app_server_client, notification)
                    .await;
            }
            AppServerEvent::ServerRequest(request) => {
                self.handle_server_request_event(app_server_client, request)
                    .await;
            }
            AppServerEvent::Disconnected { message } => {
                tracing::warn!("app-server event stream disconnected: {message}");
                self.chat_widget.add_error_message(message.clone());
                self.app_event_tx.send(AppEvent::FatalExitRequest(message));
            }
        }
    }

    pub(super) async fn handle_app_server_disconnected(
        &mut self,
        app_server_client: &mut AppServerSession,
        message: String,
    ) {
        tracing::warn!("app-server event stream disconnected: {message}");
        if let Err(recovery_error) = self
            .recover_local_daemon_connection(app_server_client)
            .await
        {
            tracing::warn!(
                "failed to recover local app-server daemon connection: {recovery_error}"
            );
            self.chat_widget.add_error_message(message.clone());
            self.app_event_tx.send(AppEvent::FatalExitRequest(message));
        }
    }

    async fn recover_local_daemon_connection(
        &mut self,
        app_server_client: &mut AppServerSession,
    ) -> Result<(), String> {
        let endpoint = match &self.app_server_target {
            AppServerTarget::LocalDaemon { endpoint } => endpoint.clone(),
            _ => return Err("disconnected app server is not the local daemon".to_string()),
        };

        let deadline = tokio::time::Instant::now() + LOCAL_DAEMON_RECONNECT_TIMEOUT;
        let mut last_error = None;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, async {
                app_server_client
                    .reconnect_remote(endpoint.clone())
                    .await
                    .map_err(|err| format!("{err:#}"))?;
                self.restore_local_daemon_threads(app_server_client).await
            })
            .await
            {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(err)) => last_error = Some(err),
                Err(_) => break,
            }

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            tokio::time::sleep(LOCAL_DAEMON_RECONNECT_RETRY_INTERVAL.min(remaining)).await;
        }

        let detail = last_error.unwrap_or_else(|| "connection attempt timed out".to_string());
        Err(format!(
            "local app-server daemon did not recover the TUI session within {} seconds: {detail}",
            LOCAL_DAEMON_RECONNECT_TIMEOUT.as_secs()
        ))
    }

    async fn restore_local_daemon_threads(
        &mut self,
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
            let started = app_server_client
                .resume_thread(self.config.clone(), thread_id, self.resume_model_settings())
                .await
                .map_err(|err| format!("failed to resume thread {thread_id}: {err:#}"))?;
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
            if let Some(turn) = active_turn_id.and_then(|active_turn_id| {
                started
                    .turns
                    .iter()
                    .find(|turn| {
                        turn.id == active_turn_id && !matches!(turn.status, TurnStatus::InProgress)
                    })
                    .cloned()
            }) {
                self.handle_server_notification_event(
                    app_server_client,
                    ServerNotification::TurnCompleted(TurnCompletedNotification {
                        thread_id: thread_id.to_string(),
                        turn,
                    }),
                )
                .await;
            }
            if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                let mut store = channel.store.lock().await;
                store.set_session(session, started.turns);
            }
        }
        self.refresh_pending_thread_approvals().await;
        Ok(())
    }

    async fn handle_server_notification_event(
        &mut self,
        app_server_client: &AppServerSession,
        notification: ServerNotification,
    ) {
        match &notification {
            ServerNotification::ServerRequestResolved(notification) => {
                if let Some(request) = self
                    .pending_app_server_requests
                    .resolve_notification(&notification.request_id)
                {
                    self.chat_widget.dismiss_app_server_request(&request);
                }
            }
            ServerNotification::McpServerStatusUpdated(_) => {
                self.refresh_mcp_startup_expected_servers_from_config();
            }
            ServerNotification::AccountRateLimitsUpdated(notification) => {
                self.chat_widget
                    .on_rolling_rate_limit_snapshot(notification.rate_limits.clone());
                return;
            }
            ServerNotification::AccountUpdated(notification) => {
                let has_codex_backend_auth = matches!(
                    notification.auth_mode,
                    Some(
                        AuthMode::Chatgpt
                            | AuthMode::ChatgptAuthTokens
                            | AuthMode::AgentIdentity
                            | AuthMode::PersonalAccessToken
                    )
                );
                self.chat_widget.update_account_state(
                    status_account_display_from_auth_mode(
                        notification.auth_mode,
                        notification.plan_type,
                    ),
                    notification.plan_type,
                    notification
                        .auth_mode
                        .is_some_and(AuthMode::has_chatgpt_account),
                    has_codex_backend_auth,
                );
                return;
            }
            ServerNotification::ExternalAgentConfigImportCompleted(notification) => {
                let should_report_completion =
                    app_server_client.consume_external_agent_config_import_completion();
                if let Err(err) = self.refresh_in_memory_config_from_disk().await {
                    tracing::warn!(
                        error = %err,
                        "failed to refresh config after external agent config import"
                    );
                }
                let cwd = self.chat_widget.config_ref().cwd.to_path_buf();
                self.chat_widget.refresh_plugin_mentions();
                self.chat_widget.submit_op(AppCommand::reload_user_config());
                self.fetch_plugins_list(app_server_client, cwd);
                if should_report_completion {
                    self.chat_widget.add_plain_history_lines(
                        crate::external_agent_config_migration_flow::external_agent_config_migration_finished_lines(notification),
                    );
                }
                return;
            }
            _ => {}
        }

        match server_notification_thread_target(&notification) {
            ServerNotificationThreadTarget::Thread(thread_id) => {
                let result = if self.primary_thread_id == Some(thread_id)
                    || self.primary_thread_id.is_none()
                {
                    self.enqueue_primary_thread_notification(thread_id, notification)
                        .await
                } else {
                    self.enqueue_thread_notification(thread_id, notification)
                        .await
                };

                if let Err(err) = result {
                    tracing::warn!("failed to enqueue app-server notification: {err}");
                }
                return;
            }
            ServerNotificationThreadTarget::InvalidThreadId(thread_id) => {
                tracing::warn!(
                    thread_id,
                    "ignoring app-server notification with invalid thread_id"
                );
                return;
            }
            ServerNotificationThreadTarget::AppScoped => {
                tracing::debug!(
                    "ignoring app-scoped MCP startup notification without a TUI app-level target"
                );
                return;
            }
            ServerNotificationThreadTarget::Global => {}
        }

        self.chat_widget
            .handle_server_notification(notification, /*replay_kind*/ None);
    }

    async fn handle_server_request_event(
        &mut self,
        app_server_client: &AppServerSession,
        request: ServerRequest,
    ) {
        if let Some(unsupported) = self
            .pending_app_server_requests
            .note_server_request(&request)
        {
            tracing::warn!(
                request_id = ?unsupported.request_id,
                message = unsupported.message,
                "rejecting unsupported app-server request"
            );
            self.chat_widget
                .add_error_message(unsupported.message.clone());
            if let Err(err) = self
                .reject_app_server_request(
                    app_server_client,
                    unsupported.request_id,
                    unsupported.message,
                )
                .await
            {
                tracing::warn!("{err}");
            }
            return;
        }

        let Some(thread_id) = server_request_thread_id(&request) else {
            tracing::warn!("ignoring threadless app-server request");
            return;
        };

        let result =
            if self.primary_thread_id == Some(thread_id) || self.primary_thread_id.is_none() {
                self.enqueue_primary_thread_request(thread_id, request)
                    .await
            } else {
                self.enqueue_thread_request(thread_id, request).await
            };
        if let Err(err) = result {
            tracing::warn!("failed to enqueue app-server request: {err}");
        }
    }
}
