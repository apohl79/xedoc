use super::ConnectionId;
use super::ConnectionRequestId;
use super::JSONRPCErrorError;
use super::ThreadId;
use super::ThreadReadParams;
use super::ThreadRequestProcessor;
use super::TurnItemsView;
use super::thread_lifecycle::EnsureConversationListenerResult;
use crate::error_code::invalid_request;
use crate::message_processor::ConnectionSessionState;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::remote_agent_extension::BUILTIN_REMOTE_AGENT_EXTENSION_ID;
use crate::remote_session_registry::RemoteSessionProjectionUpdate;
use crate::session_script_registry::SessionScriptRegistration;
use crate::session_script_registry::send_deliveries;
use std::path::Path;
use std::path::PathBuf;
use xedoc_app_server_protocol::ClientResponsePayload;
use xedoc_app_server_protocol::RemoteSessionAttachParams;
use xedoc_app_server_protocol::RemoteSessionAttachResponse;
use xedoc_app_server_protocol::RemoteSessionCancelParams;
use xedoc_app_server_protocol::RemoteSessionCancelResponse;
use xedoc_app_server_protocol::RemoteSessionControlAction;
use xedoc_app_server_protocol::RemoteSessionControlParams;
use xedoc_app_server_protocol::RemoteSessionDetachParams;
use xedoc_app_server_protocol::RemoteSessionDetachResponse;
use xedoc_app_server_protocol::RemoteSessionInputParams;
use xedoc_app_server_protocol::RemoteSessionInputResponse;
use xedoc_app_server_protocol::RemoteSessionListParams;
use xedoc_app_server_protocol::RemoteSessionListResponse;
use xedoc_app_server_protocol::RemoteSessionReadParams;
use xedoc_app_server_protocol::RemoteSessionReadResponse;
use xedoc_app_server_protocol::RemoteSessionRegisterParams;
use xedoc_app_server_protocol::RemoteSessionRegisterResponse;
use xedoc_app_server_protocol::RemoteSessionStatus;
use xedoc_app_server_protocol::RemoteSessionUpdateParams;
use xedoc_app_server_protocol::RemoteSessionUpdateResponse;
use xedoc_app_server_protocol::RemoteSessionUpdatedNotification;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::SessionScriptCapability;
use xedoc_app_server_protocol::SessionScriptMessageParams;
use xedoc_app_server_protocol::SessionScriptMessageResponse;
use xedoc_app_server_protocol::SessionScriptReadParams;
use xedoc_app_server_protocol::SessionScriptReadResponse;
use xedoc_app_server_protocol::SessionScriptRegisterParams;
use xedoc_app_server_protocol::SessionScriptRegisterResponse;
use xedoc_app_server_protocol::SessionScriptRespondParams;
use xedoc_app_server_protocol::SessionScriptRespondResponse;
use xedoc_app_server_protocol::SessionScriptSession;
use xedoc_app_server_protocol::SessionScriptSnapshot;
use xedoc_app_server_protocol::SessionScriptThread;
use xedoc_app_server_protocol::SessionScriptUnregisterParams;
use xedoc_app_server_protocol::SessionScriptUnregisterResponse;
use xedoc_config::ConfigLayerSource;
use xedoc_config::ConfigLayerStackOrdering;
use xedoc_git_utils::get_git_repo_root;
const MAX_REMOTE_SESSION_INPUT_BYTES: usize = 64 * 1024;

impl ThreadRequestProcessor {
    pub(crate) async fn session_script_registration(
        &self,
        connection_id: ConnectionId,
    ) -> Option<SessionScriptRegistration> {
        self.session_script_registry
            .registration(connection_id)
            .await
    }

    pub(crate) async fn script_register(
        &self,
        request_id: ConnectionRequestId,
        connection_id: ConnectionId,
        params: SessionScriptRegisterParams,
        session: &ConnectionSessionState,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let Some(scope) = session.session_script_scope() else {
            return Err(invalid_request(
                "session scripts must register through a host-managed script connection",
            ));
        };
        if params.script.id != scope.script_id() {
            return Err(invalid_request(
                "session script id does not match the host-managed connection",
            ));
        }
        if params.thread_id != scope.thread_id() {
            return Err(invalid_request(
                "session script thread id does not match the host-managed connection",
            ));
        }
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request(format!("thread is not loaded: {thread_id}")))?;
        if thread.config_snapshot().await.parent_thread_id.is_some() {
            return Err(invalid_request(
                "session scripts may register only for loaded root threads",
            ));
        }
        session.mark_session_script();
        let registration = self
            .session_script_registry
            .register(connection_id, thread_id, params)
            .await
            .map_err(|error| {
                session.clear_session_script();
                invalid_request(error)
            })?;
        match self
            .ensure_session_script_listener(thread_id, connection_id)
            .await
        {
            Ok(EnsureConversationListenerResult::Attached) => {}
            Ok(EnsureConversationListenerResult::ConnectionClosed) => {
                let _ = self
                    .session_script_registry
                    .unregister(connection_id, &registration.registration_id)
                    .await;
                session.clear_session_script();
                return Err(invalid_request(
                    "connection closed while registering a session script",
                ));
            }
            Err(error) => {
                let _ = self
                    .session_script_registry
                    .unregister(connection_id, &registration.registration_id)
                    .await;
                session.clear_session_script();
                return Err(error);
            }
        }

        let snapshot = match self
            .session_script_snapshot(connection_id, &registration.registration_id, thread_id)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = self
                    .session_script_registry
                    .unregister(connection_id, &registration.registration_id)
                    .await;
                self.thread_state_manager
                    .remove_session_script_connection(thread_id, connection_id)
                    .await;
                session.clear_session_script();
                return Err(error);
            }
        };
        let mut granted_capabilities = registration
            .capabilities
            .iter()
            .copied()
            .collect::<Vec<_>>();
        granted_capabilities.sort_by_key(session_script_capability_sort_key);
        let response = SessionScriptRegisterResponse {
            registration_id: registration.registration_id.clone(),
            granted_capabilities,
            snapshot,
        };
        self.outgoing.send_response(request_id, response).await;
        let resync_delivery = self
            .session_script_registry
            .activate(connection_id, &registration.registration_id)
            .await
            .map_err(invalid_request)?;
        if let Some(resync_delivery) = resync_delivery {
            send_deliveries(&self.outgoing, vec![resync_delivery]).await;
        }
        Ok(None)
    }

    pub(crate) async fn script_unregister(
        &self,
        connection_id: ConnectionId,
        params: SessionScriptUnregisterParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let registration = self
            .session_script_registry
            .registration(connection_id)
            .await;
        let Some(registration) = registration else {
            return Ok(Some(SessionScriptUnregisterResponse {}.into()));
        };
        let deliveries = self
            .session_script_registry
            .unregister(connection_id, &params.registration_id)
            .await
            .map_err(invalid_request)?;
        self.thread_state_manager
            .remove_session_script_connection(registration.thread_id, connection_id)
            .await;
        send_deliveries(&self.outgoing, deliveries).await;
        Ok(Some(SessionScriptUnregisterResponse {}.into()))
    }

    pub(crate) async fn script_read(
        &self,
        connection_id: ConnectionId,
        params: SessionScriptReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let registration = self
            .session_script_registry
            .registration(connection_id)
            .await
            .ok_or_else(|| invalid_request("no session script is registered on this connection"))?;
        let snapshot = self
            .session_script_snapshot(
                connection_id,
                &params.registration_id,
                registration.thread_id,
            )
            .await?;
        Ok(Some(SessionScriptReadResponse { snapshot }.into()))
    }

    pub(crate) async fn script_respond(
        &self,
        connection_id: ConnectionId,
        params: SessionScriptRespondParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let deliveries = self
            .session_script_registry
            .respond(connection_id, params)
            .await
            .map_err(invalid_request)?;
        send_deliveries(&self.outgoing, deliveries).await;
        Ok(Some(SessionScriptRespondResponse {}.into()))
    }

    pub(crate) async fn script_message(
        &self,
        connection_id: ConnectionId,
        params: SessionScriptMessageParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let notification = self
            .session_script_registry
            .message(connection_id, params)
            .await
            .map_err(invalid_request)?;
        self.outgoing
            .send_server_notification(ServerNotification::SessionExtensionMessage(notification))
            .await;
        Ok(Some(SessionScriptMessageResponse {}.into()))
    }

    pub(crate) async fn script_remote_session_register(
        &self,
        connection_id: ConnectionId,
        params: RemoteSessionRegisterParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let registration = self
            .remote_agent_registration(connection_id, &params.registration_id)
            .await?;
        let remote_session = self
            .remote_session_registry
            .register(
                registration.thread_id,
                params.host_id,
                params.remote_thread_id,
                params.host_name,
            )
            .await
            .map_err(invalid_request)?;
        self.send_remote_session_updated(
            registration.thread_id,
            RemoteSessionProjectionUpdate {
                summary: remote_session.clone(),
                output_delta: None,
            },
        )
        .await;
        Ok(Some(
            RemoteSessionRegisterResponse { remote_session }.into(),
        ))
    }

    pub(crate) async fn script_remote_session_update(
        &self,
        connection_id: ConnectionId,
        params: RemoteSessionUpdateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let registration = self
            .remote_agent_registration(connection_id, &params.registration_id)
            .await?;
        let update = self
            .remote_session_registry
            .update(registration.thread_id, params)
            .await
            .map_err(invalid_request)?;
        let running = update.summary.status == RemoteSessionStatus::Running;
        self.send_remote_session_updated(registration.thread_id, update)
            .await;
        if running {
            self.ensure_remote_activity_timer(registration.thread_id)
                .await;
        }
        Ok(Some(RemoteSessionUpdateResponse {}.into()))
    }

    pub(crate) async fn remote_session_list(
        &self,
        params: RemoteSessionListParams,
    ) -> Result<RemoteSessionListResponse, JSONRPCErrorError> {
        let root_thread_id = self.remote_session_root_thread(&params.thread_id).await?;
        let page = self
            .remote_session_registry
            .list(root_thread_id, params.cursor.as_deref(), params.limit)
            .await
            .map_err(invalid_request)?;
        Ok(RemoteSessionListResponse {
            data: page.data,
            next_cursor: page.next_cursor,
        })
    }

    pub(crate) async fn remote_session_read(
        &self,
        params: RemoteSessionReadParams,
    ) -> Result<RemoteSessionReadResponse, JSONRPCErrorError> {
        let root_thread_id = self.remote_session_root_thread(&params.thread_id).await?;
        let remote_session = self
            .remote_session_registry
            .read(root_thread_id, &params.remote_session_id)
            .await
            .map_err(invalid_request)?;
        let update = self
            .control_remote_session(
                root_thread_id,
                &params.remote_session_id,
                RemoteSessionControlParams {
                    action: RemoteSessionControlAction::Read,
                    host_id: remote_session.host_id,
                    thread_id: remote_session.remote_thread_id,
                    message: None,
                    expected_turn_id: None,
                    cursor: params.cursor,
                    limit: params.limit,
                },
            )
            .await?;
        Ok(RemoteSessionReadResponse {
            remote_session: update.summary,
            output: update.output_delta.unwrap_or_default(),
        })
    }

    pub(crate) async fn remote_session_attach(
        &self,
        params: RemoteSessionAttachParams,
    ) -> Result<RemoteSessionAttachResponse, JSONRPCErrorError> {
        let root_thread_id = self.remote_session_root_thread(&params.thread_id).await?;
        let remote_session = self
            .remote_session_summary(root_thread_id, &params.remote_session_id)
            .await?;
        let update = self
            .control_remote_session(
                root_thread_id,
                &params.remote_session_id,
                RemoteSessionControlParams {
                    action: RemoteSessionControlAction::Attach,
                    host_id: remote_session.host_id,
                    thread_id: remote_session.remote_thread_id,
                    message: None,
                    expected_turn_id: None,
                    cursor: None,
                    limit: None,
                },
            )
            .await?;
        Ok(RemoteSessionAttachResponse {
            remote_session: update.summary,
        })
    }

    pub(crate) async fn remote_session_input(
        &self,
        params: RemoteSessionInputParams,
    ) -> Result<RemoteSessionInputResponse, JSONRPCErrorError> {
        validate_remote_session_message(&params.message)?;
        let root_thread_id = self.remote_session_root_thread(&params.thread_id).await?;
        let remote_session = self
            .remote_session_summary(root_thread_id, &params.remote_session_id)
            .await?;
        if remote_session.status == RemoteSessionStatus::Detached {
            return Err(invalid_request(
                "remote session is detached; attach it before sending input",
            ));
        }
        let (action, expected_turn_id) = if remote_session.status == RemoteSessionStatus::Running {
            (
                RemoteSessionControlAction::Steer,
                Some(
                    params
                        .expected_turn_id
                        .or(remote_session.active_turn_id)
                        .ok_or_else(|| {
                            invalid_request("remote session has no active turn to steer")
                        })?,
                ),
            )
        } else {
            if params.expected_turn_id.is_some() {
                return Err(invalid_request(
                    "expected turn id is only valid while steering a remote session",
                ));
            }
            (RemoteSessionControlAction::Send, None)
        };
        let update = self
            .control_remote_session(
                root_thread_id,
                &params.remote_session_id,
                RemoteSessionControlParams {
                    action,
                    host_id: remote_session.host_id,
                    thread_id: remote_session.remote_thread_id,
                    message: Some(params.message),
                    expected_turn_id,
                    cursor: None,
                    limit: None,
                },
            )
            .await?;
        Ok(RemoteSessionInputResponse {
            remote_session: update.summary,
            output_delta: update.output_delta,
        })
    }

    pub(crate) async fn remote_session_cancel(
        &self,
        params: RemoteSessionCancelParams,
    ) -> Result<RemoteSessionCancelResponse, JSONRPCErrorError> {
        let root_thread_id = self.remote_session_root_thread(&params.thread_id).await?;
        let remote_session = self
            .remote_session_summary(root_thread_id, &params.remote_session_id)
            .await?;
        let expected_turn_id = params
            .expected_turn_id
            .or(remote_session.active_turn_id)
            .ok_or_else(|| invalid_request("remote session has no active turn to cancel"))?;
        let update = self
            .control_remote_session(
                root_thread_id,
                &params.remote_session_id,
                RemoteSessionControlParams {
                    action: RemoteSessionControlAction::Cancel,
                    host_id: remote_session.host_id,
                    thread_id: remote_session.remote_thread_id,
                    message: None,
                    expected_turn_id: Some(expected_turn_id),
                    cursor: None,
                    limit: None,
                },
            )
            .await?;
        Ok(RemoteSessionCancelResponse {
            remote_session: update.summary,
        })
    }

    pub(crate) async fn remote_session_detach(
        &self,
        params: RemoteSessionDetachParams,
    ) -> Result<RemoteSessionDetachResponse, JSONRPCErrorError> {
        let root_thread_id = self.remote_session_root_thread(&params.thread_id).await?;
        let remote_session = self
            .remote_session_summary(root_thread_id, &params.remote_session_id)
            .await?;
        let update = self
            .control_remote_session(
                root_thread_id,
                &params.remote_session_id,
                RemoteSessionControlParams {
                    action: RemoteSessionControlAction::Detach,
                    host_id: remote_session.host_id,
                    thread_id: remote_session.remote_thread_id,
                    message: None,
                    expected_turn_id: None,
                    cursor: None,
                    limit: None,
                },
            )
            .await?;
        Ok(RemoteSessionDetachResponse {
            remote_session: update.summary,
        })
    }

    async fn remote_agent_registration(
        &self,
        connection_id: ConnectionId,
        registration_id: &str,
    ) -> Result<SessionScriptRegistration, JSONRPCErrorError> {
        let registration = self
            .session_script_registry
            .registration(connection_id)
            .await
            .ok_or_else(|| invalid_request("no session script is registered on this connection"))?;
        if registration.registration_id != registration_id {
            return Err(invalid_request(
                "registration does not belong to this connection",
            ));
        }
        if !registration.is_extension(BUILTIN_REMOTE_AGENT_EXTENSION_ID) {
            return Err(invalid_request(
                "remote session updates require the built-in remote-agent extension",
            ));
        }
        Ok(registration)
    }

    async fn remote_session_root_thread(
        &self,
        thread_id: &str,
    ) -> Result<ThreadId, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(thread_id)
            .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request("remote sessions require a loaded root thread"))?;
        if thread.config_snapshot().await.parent_thread_id.is_some() {
            return Err(invalid_request(
                "remote sessions are scoped to a root thread",
            ));
        }
        Ok(thread_id)
    }

    async fn remote_session_summary(
        &self,
        root_thread_id: ThreadId,
        remote_session_id: &str,
    ) -> Result<xedoc_app_server_protocol::RemoteSessionSummary, JSONRPCErrorError> {
        self.remote_session_registry
            .read(root_thread_id, remote_session_id)
            .await
            .map_err(invalid_request)
    }

    async fn control_remote_session(
        &self,
        root_thread_id: ThreadId,
        remote_session_id: &str,
        params: RemoteSessionControlParams,
    ) -> Result<RemoteSessionProjectionUpdate, JSONRPCErrorError> {
        self.session_extension_manager
            .start_remote_for_thread(root_thread_id)
            .await
            .map_err(invalid_request)?;
        let action = params.action;
        let response = self
            .session_extension_manager
            .control_remote_session(root_thread_id, params)
            .await
            .map_err(invalid_request)?;
        if !response.success {
            let message = response
                .result
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("remote-agent control operation failed");
            return Err(invalid_request(message));
        }
        let update = self
            .remote_session_registry
            .apply_control_result(root_thread_id, remote_session_id, action, &response.result)
            .await
            .map_err(invalid_request)?;
        if action != RemoteSessionControlAction::Read {
            self.send_remote_session_updated(
                root_thread_id,
                RemoteSessionProjectionUpdate {
                    summary: update.summary.clone(),
                    output_delta: update.output_delta.clone(),
                },
            )
            .await;
        }
        if update.summary.status == RemoteSessionStatus::Running {
            self.ensure_remote_activity_timer(root_thread_id).await;
        }
        Ok(update)
    }

    pub(super) async fn send_remote_session_updated(
        &self,
        root_thread_id: ThreadId,
        update: RemoteSessionProjectionUpdate,
    ) {
        let connection_ids = self
            .thread_state_manager
            .subscribed_connection_ids(root_thread_id)
            .await;
        let outgoing = ThreadScopedOutgoingMessageSender::new(
            self.outgoing.clone(),
            connection_ids,
            root_thread_id,
        );
        outgoing
            .send_server_notification(ServerNotification::RemoteSessionUpdated(
                RemoteSessionUpdatedNotification {
                    thread_id: root_thread_id.to_string(),
                    remote_session: update.summary,
                    output_delta: update.output_delta,
                },
            ))
            .await;
    }

    async fn session_script_snapshot(
        &self,
        connection_id: ConnectionId,
        registration_id: &str,
        thread_id: ThreadId,
    ) -> Result<SessionScriptSnapshot, JSONRPCErrorError> {
        let thread = self
            .thread_read_response_inner(ThreadReadParams {
                thread_id: thread_id.to_string(),
                include_turns: false,
            })
            .await?
            .thread;
        let active_turn = {
            let thread_state = self.thread_state_manager.thread_state(thread_id).await;
            let mut active_turn = thread_state.lock().await.latest_turn_snapshot();
            if let Some(turn) = active_turn.as_mut() {
                turn.items.clear();
                turn.items_view = TurnItemsView::NotLoaded;
            }
            active_turn
        };
        let session = session_script_session(
            thread.session_id.clone(),
            thread.id.clone(),
            thread.name.clone(),
            thread.cwd.as_path(),
            &self.config.config_layer_stack,
        );
        self.session_script_registry
            .snapshot_for(
                connection_id,
                registration_id,
                SessionScriptSnapshot {
                    revision: 0,
                    session,
                    thread: SessionScriptThread {
                        status: thread.status,
                        can_accept_direct_input: thread.can_accept_direct_input.unwrap_or(false),
                    },
                    turn: active_turn,
                    pending_prompts: Vec::new(),
                },
            )
            .await
            .map_err(invalid_request)
    }
}

fn validate_remote_session_message(message: &str) -> Result<(), JSONRPCErrorError> {
    if message.trim().is_empty() || message.len() > MAX_REMOTE_SESSION_INPUT_BYTES {
        return Err(invalid_request("remote session input is invalid"));
    }
    Ok(())
}

fn session_script_capability_sort_key(capability: &SessionScriptCapability) -> u8 {
    match capability {
        SessionScriptCapability::UserInputSend => 0,
        SessionScriptCapability::PromptRequestUserInputRespond => 1,
        SessionScriptCapability::PromptApprovalRespond => 2,
    }
}

pub(super) fn session_script_session(
    session_id: String,
    thread_id: String,
    title: Option<String>,
    cwd: &Path,
    config_layer_stack: &xedoc_config::ConfigLayerStack,
) -> SessionScriptSession {
    let project_root = get_git_repo_root(cwd).or_else(|| {
        config_layer_stack
            .get_layers(
                ConfigLayerStackOrdering::LowestPrecedenceFirst,
                /*include_disabled*/ true,
            )
            .iter()
            .find_map(|layer| match &layer.name {
                ConfigLayerSource::Project { dot_xedoc_folder } => {
                    dot_xedoc_folder.as_path().parent().map(PathBuf::from)
                }
                _ => None,
            })
    });
    let project_name = project_root
        .as_deref()
        .or(Some(cwd))
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string());
    SessionScriptSession {
        session_id,
        thread_id,
        title,
        project_name,
        project_root: project_root.map(|path| path.display().to_string()),
        cwd: cwd.display().to_string(),
    }
}
