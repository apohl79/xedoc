//! App-server event stream handling for the TUI app.

use super::App;
use super::app_server_event_targets::ServerNotificationThreadTarget;
use super::app_server_event_targets::server_notification_thread_target;
use super::app_server_event_targets::server_request_thread_id;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::status_account_display_from_auth_mode;
use xedoc_app_server_client::AppServerEvent;
use xedoc_app_server_protocol::AuthMode;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ServerRequest;

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
                self.handle_server_notification_event(notification).await;
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

    pub(super) async fn handle_server_notification_event(
        &mut self,
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
                let has_xedoc_backend_auth = matches!(
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
                    has_xedoc_backend_auth,
                );
                return;
            }
            ServerNotification::RemoteSessionUpdated(notification) => {
                let remote_session_id = super::agent_navigation::RemoteSessionId::new(
                    notification.remote_session.remote_session_id.clone(),
                );
                self.upsert_remote_session_picker_entry(notification.remote_session.clone());
                self.sync_active_remote_session_running();
                if self.active_remote_session.as_ref() == Some(&remote_session_id)
                    && let Some(output_delta) = notification
                        .output_delta
                        .as_deref()
                        .filter(|output_delta| !output_delta.trim().is_empty())
                {
                    self.chat_widget.add_plain_history_lines(
                        output_delta
                            .lines()
                            .map(|line| line.to_string().into())
                            .collect(),
                    );
                }
                self.sync_active_agent_display();
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
