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
use crate::session_script_registry::SessionScriptRegistration;
use crate::session_script_registry::send_deliveries;
use std::path::Path;
use std::path::PathBuf;
use xedoc_app_server_protocol::ClientResponsePayload;
use xedoc_app_server_protocol::SessionScriptCapability;
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
