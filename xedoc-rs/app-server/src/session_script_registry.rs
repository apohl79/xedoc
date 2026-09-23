use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use uuid::Uuid;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::SessionExtensionMessageNotification;
use xedoc_app_server_protocol::SessionScriptCapability;
use xedoc_app_server_protocol::SessionScriptIdentityParams;
use xedoc_app_server_protocol::SessionScriptMessageParams;
use xedoc_app_server_protocol::SessionScriptPrompt;
use xedoc_app_server_protocol::SessionScriptPromptClosedNotification;
use xedoc_app_server_protocol::SessionScriptPromptClosedReason;
use xedoc_app_server_protocol::SessionScriptPromptKind;
use xedoc_app_server_protocol::SessionScriptPromptOpenedNotification;
use xedoc_app_server_protocol::SessionScriptPromptRequest;
use xedoc_app_server_protocol::SessionScriptPromptResponse;
use xedoc_app_server_protocol::SessionScriptRegisterParams;
use xedoc_app_server_protocol::SessionScriptRequestUserInputAnswer;
use xedoc_app_server_protocol::SessionScriptRespondParams;
use xedoc_app_server_protocol::SessionScriptResyncRequiredNotification;
use xedoc_app_server_protocol::SessionScriptSession;
use xedoc_app_server_protocol::SessionScriptSnapshot;
use xedoc_app_server_protocol::SessionScriptSubscriptionsParams;
use xedoc_app_server_protocol::SessionScriptUpdatedNotification;
use xedoc_app_server_protocol::ToolRequestUserInputAnswer;
use xedoc_app_server_protocol::ToolRequestUserInputParams;
use xedoc_app_server_protocol::ToolRequestUserInputResponse;
use xedoc_config::SessionScriptCapabilityToml;
use xedoc_config::SessionScriptConfigToml;
use xedoc_config::SessionScriptSubscriptionToml;
use xedoc_protocol::ThreadId;

/// Connection-oriented state for restricted app-server session scripts.
///
/// This registry deliberately keeps scripts out of ordinary thread subscriptions:
/// selected notifications are delivered directly from the script-aware event
/// paths, while the thread-state manager retains only their liveness interest.
#[derive(Clone)]
pub(crate) struct SessionScriptRegistry {
    state: Arc<Mutex<SessionScriptRegistryState>>,
    policies_by_script_id: Arc<HashMap<String, SessionScriptPolicy>>,
    extension_policies_by_script_id:
        Arc<Mutex<HashMap<String, HashMap<ThreadId, SessionScriptPolicy>>>>,
    thread_removed_tx: broadcast::Sender<ThreadId>,
}

#[derive(Default)]
struct SessionScriptRegistryState {
    registrations_by_connection: HashMap<ConnectionId, SessionScriptRegistration>,
    connections_by_thread: HashMap<ThreadId, HashSet<ConnectionId>>,
    request_user_input_responder_by_thread: HashMap<ThreadId, ConnectionId>,
    approval_responder_by_thread: HashMap<ThreadId, ConnectionId>,
    prompts_by_id: HashMap<String, SessionScriptPromptState>,
}

#[derive(Clone)]
struct SessionScriptPolicy {
    extension_name: String,
    capabilities: HashSet<SessionScriptCapability>,
    subscriptions: SessionScriptSubscriptionPolicy,
    response_timeout: Duration,
}

#[derive(Clone, Default)]
struct SessionScriptSubscriptionPolicy {
    model_response_deltas: bool,
    model_response_completed: bool,
    user_messages: bool,
    turn_completed: bool,
    file_changes: bool,
    session_updates: bool,
    prompts: HashSet<SessionScriptPromptKind>,
}

#[derive(Clone)]
pub(crate) struct SessionScriptRegistration {
    pub(crate) registration_id: String,
    pub(crate) thread_id: ThreadId,
    extension_name: String,
    pub(crate) subscriptions: SessionScriptSubscriptionsParams,
    pub(crate) capabilities: HashSet<SessionScriptCapability>,
    revision: u64,
    ready: bool,
    resync_required: bool,
    session: Option<SessionScriptSession>,
    response_timeout: Duration,
    identity: SessionScriptIdentityParams,
}

struct SessionScriptPromptState {
    prompt_id: String,
    kind: SessionScriptPromptKind,
    thread_id: ThreadId,
    turn_id: Option<String>,
    item_id: Option<String>,
    request: SessionScriptPromptRequest,
    responder_connection_id: Option<ConnectionId>,
    response_lease: Option<String>,
    response_tx: Option<SessionScriptPromptResponseSender>,
    request_user_input_params: Option<ToolRequestUserInputParams>,
}

enum SessionScriptPromptResponseSender {
    RequestUserInput(oneshot::Sender<ToolRequestUserInputResponse>),
    Approval(oneshot::Sender<serde_json::Value>),
}

pub(crate) struct OpenRequestUserInputPrompt {
    pub(crate) prompt_id: String,
    pub(crate) response_receiver: Option<oneshot::Receiver<ToolRequestUserInputResponse>>,
    pub(crate) response_timeout: Option<Duration>,
}

pub(crate) struct OpenApprovalPrompt {
    pub(crate) prompt_id: String,
    pub(crate) response_receiver: Option<oneshot::Receiver<serde_json::Value>>,
}

impl Default for SessionScriptRegistry {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl SessionScriptRegistry {
    pub(crate) fn new(configured_scripts: &[SessionScriptConfigToml]) -> Self {
        let configured_script_id_counts = configured_scripts
            .iter()
            .filter(|script| !script.id.trim().is_empty() && script.command_argv().is_some())
            .fold(HashMap::<&str, usize>::new(), |mut counts, script| {
                *counts.entry(&script.id).or_default() += 1;
                counts
            });
        let policies_by_script_id = configured_scripts
            .iter()
            .filter(|script| configured_script_id_counts.get(script.id.as_str()) == Some(&1))
            .map(|script| {
                let subscriptions = script.subscriptions.iter().copied().fold(
                    SessionScriptSubscriptionPolicy::default(),
                    |mut policy, subscription| {
                        match subscription {
                            SessionScriptSubscriptionToml::ModelResponseDeltas => {
                                policy.model_response_deltas = true;
                            }
                            SessionScriptSubscriptionToml::ModelResponseCompleted => {
                                policy.model_response_completed = true;
                            }
                            SessionScriptSubscriptionToml::UserMessages => {
                                policy.user_messages = true;
                            }
                            SessionScriptSubscriptionToml::TurnCompleted => {
                                policy.turn_completed = true;
                            }
                            SessionScriptSubscriptionToml::FileChanges => {
                                policy.file_changes = true;
                            }
                            SessionScriptSubscriptionToml::SessionUpdates => {
                                policy.session_updates = true;
                            }
                            SessionScriptSubscriptionToml::PromptRequestUserInput => {
                                policy
                                    .prompts
                                    .insert(SessionScriptPromptKind::RequestUserInput);
                            }
                            SessionScriptSubscriptionToml::PromptExtensionInteraction => {
                                policy
                                    .prompts
                                    .insert(SessionScriptPromptKind::ExtensionInteraction);
                            }
                            SessionScriptSubscriptionToml::PromptCommandExecutionApproval => {
                                policy
                                    .prompts
                                    .insert(SessionScriptPromptKind::CommandExecutionApproval);
                            }
                            SessionScriptSubscriptionToml::PromptFileChangeApproval => {
                                policy
                                    .prompts
                                    .insert(SessionScriptPromptKind::FileChangeApproval);
                            }
                            SessionScriptSubscriptionToml::PromptPermissionsApproval => {
                                policy
                                    .prompts
                                    .insert(SessionScriptPromptKind::PermissionsApproval);
                            }
                            SessionScriptSubscriptionToml::PromptMcpElicitation => {
                                policy
                                    .prompts
                                    .insert(SessionScriptPromptKind::McpElicitation);
                            }
                        }
                        policy
                    },
                );
                (
                    script.id.clone(),
                    SessionScriptPolicy {
                        extension_name: script.id.clone(),
                        capabilities: script
                            .capabilities
                            .iter()
                            .copied()
                            .map(session_script_capability_from_config)
                            .collect(),
                        subscriptions,
                        response_timeout: script.response_timeout(),
                    },
                )
            })
            .collect();
        let (thread_removed_tx, _) = broadcast::channel(32);
        Self {
            state: Arc::new(Mutex::new(SessionScriptRegistryState::default())),
            policies_by_script_id: Arc::new(policies_by_script_id),
            extension_policies_by_script_id: Arc::new(Mutex::new(HashMap::new())),
            thread_removed_tx,
        }
    }

    pub(crate) async fn grant_extension(
        &self,
        extension_id: &str,
        thread_id: ThreadId,
        extension_name: &str,
        requested_capabilities: &[String],
    ) -> Result<(), String> {
        if self.policies_by_script_id.contains_key(extension_id) {
            return Err(
                "extension script id conflicts with a configured session script".to_string(),
            );
        }
        self.extension_policies_by_script_id
            .lock()
            .await
            .entry(extension_id.to_string())
            .or_default()
            .insert(
                thread_id,
                extension_policy_from_requested_capabilities(
                    extension_name,
                    requested_capabilities,
                ),
            );
        Ok(())
    }

    pub(crate) async fn revoke_extension(
        &self,
        extension_id: &str,
        thread_id: ThreadId,
    ) -> Vec<ScriptNotificationDelivery> {
        let mut extension_policies = self.extension_policies_by_script_id.lock().await;
        if let Some(policies_by_thread) = extension_policies.get_mut(extension_id) {
            policies_by_thread.remove(&thread_id);
            if policies_by_thread.is_empty() {
                extension_policies.remove(extension_id);
            }
        }
        drop(extension_policies);
        let mut state = self.state.lock().await;
        let connection_ids = state
            .registrations_by_connection
            .iter()
            .filter_map(|(connection_id, registration)| {
                (registration.identity.id == extension_id && registration.thread_id == thread_id)
                    .then_some(*connection_id)
            })
            .collect::<Vec<_>>();
        connection_ids
            .into_iter()
            .flat_map(|connection_id| {
                remove_registration(
                    &mut state,
                    connection_id,
                    SessionScriptPromptClosedReason::ResponderDisconnected,
                )
            })
            .collect()
    }

    pub(crate) async fn register(
        &self,
        connection_id: ConnectionId,
        thread_id: ThreadId,
        params: SessionScriptRegisterParams,
    ) -> Result<SessionScriptRegistration, String> {
        let policy = self
            .extension_policies_by_script_id
            .lock()
            .await
            .get(params.script.id.as_str())
            .and_then(|policies_by_thread| policies_by_thread.get(&thread_id))
            .cloned()
            .or_else(|| {
                self.policies_by_script_id
                    .get(params.script.id.as_str())
                    .cloned()
            })
            .ok_or_else(|| "script is not enabled by host policy".to_string())?;
        let mut state = self.state.lock().await;
        if state
            .registrations_by_connection
            .contains_key(&connection_id)
        {
            return Err("a session script is already registered on this connection".to_string());
        }
        let subscriptions = &params.subscriptions;
        if (subscriptions.model_response_deltas && !policy.subscriptions.model_response_deltas)
            || (subscriptions.model_response_completed
                && !policy.subscriptions.model_response_completed)
            || (subscriptions.user_messages && !policy.subscriptions.user_messages)
            || (subscriptions.turn_completed && !policy.subscriptions.turn_completed)
            || (subscriptions.file_changes && !policy.subscriptions.file_changes)
            || (subscriptions.session_updates && !policy.subscriptions.session_updates)
            || subscriptions.prompts.as_ref().is_some_and(|prompts| {
                prompts
                    .iter()
                    .any(|prompt| !policy.subscriptions.prompts.contains(prompt))
            })
        {
            return Err("requested subscriptions are not enabled by host policy".to_string());
        }

        let requested_capabilities = params.requested_capabilities.unwrap_or_default();
        let capabilities = requested_capabilities
            .into_iter()
            .filter(|capability| policy.capabilities.contains(capability))
            .collect::<HashSet<_>>();
        if capabilities.contains(&SessionScriptCapability::PromptRequestUserInputRespond)
            && state
                .request_user_input_responder_by_thread
                .contains_key(&thread_id)
        {
            return Err(
                "another registered script already owns requestUserInput response rights for this thread"
                    .to_string(),
            );
        }
        if capabilities.contains(&SessionScriptCapability::PromptApprovalRespond)
            && state.approval_responder_by_thread.contains_key(&thread_id)
        {
            return Err(
                "another registered script already owns approval response rights for this thread"
                    .to_string(),
            );
        }

        let registration = SessionScriptRegistration {
            registration_id: Uuid::now_v7().to_string(),
            thread_id,
            extension_name: policy.extension_name,
            subscriptions: params.subscriptions,
            capabilities,
            revision: 0,
            ready: false,
            resync_required: false,
            session: None,
            response_timeout: policy.response_timeout,
            identity: params.script,
        };
        if registration
            .capabilities
            .contains(&SessionScriptCapability::PromptRequestUserInputRespond)
        {
            state
                .request_user_input_responder_by_thread
                .insert(thread_id, connection_id);
        }
        if registration
            .capabilities
            .contains(&SessionScriptCapability::PromptApprovalRespond)
        {
            state
                .approval_responder_by_thread
                .insert(thread_id, connection_id);
        }
        state
            .connections_by_thread
            .entry(thread_id)
            .or_default()
            .insert(connection_id);
        state
            .registrations_by_connection
            .insert(connection_id, registration.clone());
        Ok(registration)
    }

    pub(crate) async fn unregister(
        &self,
        connection_id: ConnectionId,
        registration_id: &str,
    ) -> Result<Vec<ScriptNotificationDelivery>, String> {
        let mut state = self.state.lock().await;
        let Some(registration) = state.registrations_by_connection.get(&connection_id) else {
            return Err("no session script is registered on this connection".to_string());
        };
        if registration.registration_id != registration_id {
            return Err("registration does not belong to this connection".to_string());
        }
        Ok(remove_registration(
            &mut state,
            connection_id,
            SessionScriptPromptClosedReason::ResponderDisconnected,
        ))
    }

    pub(crate) async fn remove_connection(
        &self,
        connection_id: ConnectionId,
    ) -> Vec<ScriptNotificationDelivery> {
        let mut state = self.state.lock().await;
        if !state
            .registrations_by_connection
            .contains_key(&connection_id)
        {
            return Vec::new();
        }
        remove_registration(
            &mut state,
            connection_id,
            SessionScriptPromptClosedReason::ResponderDisconnected,
        )
    }

    pub(crate) async fn registration(
        &self,
        connection_id: ConnectionId,
    ) -> Option<SessionScriptRegistration> {
        self.state
            .lock()
            .await
            .registrations_by_connection
            .get(&connection_id)
            .cloned()
    }

    pub(crate) async fn message(
        &self,
        connection_id: ConnectionId,
        params: SessionScriptMessageParams,
    ) -> Result<SessionExtensionMessageNotification, String> {
        if params.message.trim().is_empty() {
            return Err("session script message must not be empty".to_string());
        }
        let state = self.state.lock().await;
        let registration = state
            .registrations_by_connection
            .get(&connection_id)
            .ok_or_else(|| "no session script is registered on this connection".to_string())?;
        if registration.registration_id != params.registration_id {
            return Err("registration does not belong to this connection".to_string());
        }
        Ok(SessionExtensionMessageNotification {
            thread_id: registration.thread_id.to_string(),
            extension_name: registration.extension_name.clone(),
            level: params.level,
            message: params.message,
        })
    }

    pub(crate) async fn snapshot_for(
        &self,
        connection_id: ConnectionId,
        registration_id: &str,
        mut snapshot: SessionScriptSnapshot,
    ) -> Result<SessionScriptSnapshot, String> {
        let mut state = self.state.lock().await;
        let registration = {
            let registration = state
                .registrations_by_connection
                .get_mut(&connection_id)
                .ok_or_else(|| "no session script is registered on this connection".to_string())?;
            if registration.registration_id != registration_id {
                return Err("registration does not belong to this connection".to_string());
            }
            registration.revision = registration.revision.saturating_add(1);
            registration.session = Some(snapshot.session.clone());
            registration.clone()
        };
        snapshot.revision = registration.revision;
        snapshot.pending_prompts = state
            .prompts_by_id
            .values()
            .filter(|prompt| prompt.thread_id == registration.thread_id)
            .filter(|prompt| should_receive_prompt(&registration, connection_id, prompt))
            .map(|prompt| prompt_projection(prompt, connection_id))
            .collect();
        Ok(snapshot)
    }

    pub(crate) async fn activate(
        &self,
        connection_id: ConnectionId,
        registration_id: &str,
    ) -> Result<Option<ScriptNotificationDelivery>, String> {
        let mut state = self.state.lock().await;
        let registration = state
            .registrations_by_connection
            .get_mut(&connection_id)
            .ok_or_else(|| "no session script is registered on this connection".to_string())?;
        if registration.registration_id != registration_id {
            return Err("registration does not belong to this connection".to_string());
        }
        registration.ready = true;
        Ok(registration.resync_required.then(|| {
            registration.resync_required = false;
            ScriptNotificationDelivery {
                connection_id,
                notification: ServerNotification::ScriptResyncRequired(
                    SessionScriptResyncRequiredNotification {
                        registration_id: registration.registration_id.clone(),
                        revision: registration.revision,
                    },
                ),
            }
        }))
    }

    pub(crate) async fn publish_model_response_delta(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        notification: ServerNotification,
    ) {
        self.publish(outgoing, thread_id, notification, |subscriptions| {
            subscriptions.model_response_deltas
        })
        .await;
    }

    pub(crate) async fn publish_model_response_completed(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        notification: ServerNotification,
    ) {
        self.publish(outgoing, thread_id, notification, |subscriptions| {
            subscriptions.model_response_completed
        })
        .await;
    }

    pub(crate) async fn publish_user_message(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        notification: ServerNotification,
    ) {
        self.publish(outgoing, thread_id, notification, |subscriptions| {
            subscriptions.user_messages
        })
        .await;
    }

    pub(crate) async fn publish_turn_started(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        notification: ServerNotification,
    ) {
        self.publish(outgoing, thread_id, notification, |_| true)
            .await;
    }

    pub(crate) async fn publish_turn_completed(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        notification: ServerNotification,
    ) {
        self.publish(outgoing, thread_id, notification, |subscriptions| {
            subscriptions.turn_completed
        })
        .await;
    }

    pub(crate) async fn publish_file_change(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        notification: ServerNotification,
    ) {
        self.publish(outgoing, thread_id, notification, |subscriptions| {
            subscriptions.file_changes
        })
        .await;
    }

    pub(crate) async fn publish_session_updated(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        session: SessionScriptSession,
    ) {
        let deliveries = {
            let mut state = self.state.lock().await;
            state
                .registrations_by_connection
                .iter_mut()
                .filter(|(_, registration)| {
                    registration.thread_id == thread_id
                        && registration.subscriptions.session_updates
                })
                .filter_map(|(connection_id, registration)| {
                    if !registration.ready {
                        registration.resync_required = true;
                        return None;
                    }
                    let mut replacement_session = session.clone();
                    if replacement_session.title.is_none()
                        && let Some(current_session) = registration.session.as_ref()
                    {
                        replacement_session.title = current_session.title.clone();
                    }
                    registration.session = Some(replacement_session.clone());
                    registration.revision = registration.revision.saturating_add(1);
                    Some(ScriptNotificationDelivery {
                        connection_id: *connection_id,
                        notification: ServerNotification::ScriptSessionUpdated(
                            SessionScriptUpdatedNotification {
                                registration_id: registration.registration_id.clone(),
                                revision: registration.revision,
                                session: replacement_session,
                            },
                        ),
                    })
                })
                .collect::<Vec<_>>()
        };
        send_deliveries(outgoing, deliveries).await;
    }

    pub(crate) async fn publish_session_title_updated(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        title: Option<String>,
    ) {
        let deliveries = {
            let mut state = self.state.lock().await;
            state
                .registrations_by_connection
                .iter_mut()
                .filter(|(_, registration)| registration.thread_id == thread_id)
                .filter(|(_, registration)| registration.subscriptions.session_updates)
                .filter_map(|(connection_id, registration)| {
                    let Some(mut session) = registration.session.clone() else {
                        registration.resync_required = true;
                        return None;
                    };
                    session.title = title.clone();
                    registration.session = Some(session.clone());
                    if !registration.ready {
                        registration.resync_required = true;
                        return None;
                    }
                    registration.revision = registration.revision.saturating_add(1);
                    Some(ScriptNotificationDelivery {
                        connection_id: *connection_id,
                        notification: ServerNotification::ScriptSessionUpdated(
                            SessionScriptUpdatedNotification {
                                registration_id: registration.registration_id.clone(),
                                revision: registration.revision,
                                session,
                            },
                        ),
                    })
                })
                .collect::<Vec<_>>()
        };
        send_deliveries(outgoing, deliveries).await;
    }

    pub(crate) async fn open_request_user_input(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        params: ToolRequestUserInputParams,
    ) -> OpenRequestUserInputPrompt {
        let (prompt_id, response_receiver, response_timeout, deliveries) = {
            let mut state = self.state.lock().await;
            let responder_connection_id = state
                .request_user_input_responder_by_thread
                .get(&thread_id)
                .copied();
            let response_timeout = responder_connection_id.and_then(|connection_id| {
                state
                    .registrations_by_connection
                    .get(&connection_id)
                    .map(|registration| registration.response_timeout)
            });
            let (response_tx, response_receiver, response_lease) =
                if responder_connection_id.is_some() {
                    let (response_tx, response_receiver) = oneshot::channel();
                    (
                        Some(response_tx),
                        Some(response_receiver),
                        Some(Uuid::now_v7().to_string()),
                    )
                } else {
                    (None, None, None)
                };
            let prompt_id = Uuid::now_v7().to_string();
            let prompt = SessionScriptPromptState {
                prompt_id: prompt_id.clone(),
                kind: SessionScriptPromptKind::RequestUserInput,
                thread_id,
                turn_id: Some(params.turn_id.clone()),
                item_id: Some(params.item_id.clone()),
                request: SessionScriptPromptRequest {
                    method: "item/tool/requestUserInput".to_string(),
                    params: serde_json::to_value(&params).unwrap_or(serde_json::Value::Null),
                },
                responder_connection_id,
                response_lease,
                response_tx: response_tx.map(SessionScriptPromptResponseSender::RequestUserInput),
                request_user_input_params: Some(params),
            };
            let deliveries = prompt_open_deliveries(&mut state, &prompt);
            state.prompts_by_id.insert(prompt_id.clone(), prompt);
            (prompt_id, response_receiver, response_timeout, deliveries)
        };
        send_deliveries(outgoing, deliveries).await;
        OpenRequestUserInputPrompt {
            prompt_id,
            response_receiver,
            response_timeout,
        }
    }

    pub(crate) async fn open_observed_prompt(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        kind: SessionScriptPromptKind,
        turn_id: Option<String>,
        item_id: Option<String>,
        request: SessionScriptPromptRequest,
    ) -> String {
        let (prompt_id, deliveries) = {
            let mut state = self.state.lock().await;
            let prompt_id = Uuid::now_v7().to_string();
            let prompt = SessionScriptPromptState {
                prompt_id: prompt_id.clone(),
                kind,
                thread_id,
                turn_id,
                item_id,
                request,
                responder_connection_id: None,
                response_lease: None,
                response_tx: None,
                request_user_input_params: None,
            };
            let deliveries = prompt_open_deliveries(&mut state, &prompt);
            state.prompts_by_id.insert(prompt_id.clone(), prompt);
            (prompt_id, deliveries)
        };
        send_deliveries(outgoing, deliveries).await;
        prompt_id
    }

    pub(crate) async fn open_approval_prompt(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        kind: SessionScriptPromptKind,
        turn_id: Option<String>,
        item_id: Option<String>,
        request: SessionScriptPromptRequest,
    ) -> OpenApprovalPrompt {
        assert!(is_approval_prompt_kind(kind));
        let (prompt_id, response_receiver, deliveries) = {
            let mut state = self.state.lock().await;
            let responder_connection_id =
                state.approval_responder_by_thread.get(&thread_id).copied();
            let (response_tx, response_receiver, response_lease) =
                if responder_connection_id.is_some() {
                    let (response_tx, response_receiver) = oneshot::channel();
                    (
                        Some(SessionScriptPromptResponseSender::Approval(response_tx)),
                        Some(response_receiver),
                        Some(Uuid::now_v7().to_string()),
                    )
                } else {
                    (None, None, None)
                };
            let prompt_id = Uuid::now_v7().to_string();
            let prompt = SessionScriptPromptState {
                prompt_id: prompt_id.clone(),
                kind,
                thread_id,
                turn_id,
                item_id,
                request,
                responder_connection_id,
                response_lease,
                response_tx,
                request_user_input_params: None,
            };
            let deliveries = prompt_open_deliveries(&mut state, &prompt);
            state.prompts_by_id.insert(prompt_id.clone(), prompt);
            (prompt_id, response_receiver, deliveries)
        };
        send_deliveries(outgoing, deliveries).await;
        OpenApprovalPrompt {
            prompt_id,
            response_receiver,
        }
    }

    pub(crate) async fn respond(
        &self,
        connection_id: ConnectionId,
        params: SessionScriptRespondParams,
    ) -> Result<Vec<ScriptNotificationDelivery>, String> {
        let mut state = self.state.lock().await;
        let registration = state
            .registrations_by_connection
            .get(&connection_id)
            .ok_or_else(|| "no session script is registered on this connection".to_string())?;
        if registration.registration_id != params.registration_id {
            return Err("registration does not belong to this connection".to_string());
        }
        let prompt = state
            .prompts_by_id
            .get(&params.prompt_id)
            .ok_or_else(|| "prompt is no longer open".to_string())?;
        if prompt.thread_id != registration.thread_id
            || prompt.responder_connection_id != Some(connection_id)
            || prompt.response_lease.as_deref() != Some(params.response_lease.as_str())
        {
            return Err("response lease is not valid for this prompt".to_string());
        }
        match (prompt.kind, &params.response, &prompt.response_tx) {
            (
                SessionScriptPromptKind::RequestUserInput,
                SessionScriptPromptResponse::RequestUserInput { answers },
                Some(SessionScriptPromptResponseSender::RequestUserInput(_)),
            ) => {
                let request_user_input_params = prompt
                    .request_user_input_params
                    .as_ref()
                    .ok_or_else(|| "prompt is missing requestUserInput metadata".to_string())?;
                validate_request_user_input_answers(request_user_input_params, answers)?;
            }
            (
                kind,
                SessionScriptPromptResponse::Approval { .. },
                Some(SessionScriptPromptResponseSender::Approval(_)),
            ) if is_approval_prompt_kind(kind) => {}
            _ => return Err("response kind does not match the prompt".to_string()),
        }
        let mut prompt = state
            .prompts_by_id
            .remove(&params.prompt_id)
            .expect("validated prompt remains present");
        let response_tx = prompt
            .response_tx
            .take()
            .ok_or_else(|| "prompt is not delegated to a session script".to_string())?;
        match (prompt.kind, params.response, response_tx) {
            (
                SessionScriptPromptKind::RequestUserInput,
                SessionScriptPromptResponse::RequestUserInput { answers },
                SessionScriptPromptResponseSender::RequestUserInput(response_tx),
            ) => {
                let response = ToolRequestUserInputResponse {
                    answers: answers
                        .into_iter()
                        .map(|(id, answer)| {
                            (
                                id,
                                ToolRequestUserInputAnswer {
                                    answers: answer.answers,
                                },
                            )
                        })
                        .collect(),
                };
                response_tx.send(response).map_err(|_| {
                    "requestUserInput is no longer waiting for a response".to_string()
                })?;
            }
            (
                kind,
                SessionScriptPromptResponse::Approval { response },
                SessionScriptPromptResponseSender::Approval(response_tx),
            ) if is_approval_prompt_kind(kind) => {
                response_tx.send(response).map_err(|_| {
                    "approval prompt is no longer waiting for a response".to_string()
                })?;
            }
            _ => return Err("response kind does not match the prompt".to_string()),
        }
        Ok(prompt_closed_deliveries(
            &mut state,
            &prompt,
            SessionScriptPromptClosedReason::Answered,
        ))
    }

    pub(crate) async fn close_prompt(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        prompt_id: &str,
        reason: SessionScriptPromptClosedReason,
    ) -> bool {
        let deliveries = {
            let mut state = self.state.lock().await;
            state
                .prompts_by_id
                .remove(prompt_id)
                .map(|prompt| prompt_closed_deliveries(&mut state, &prompt, reason))
        };
        if let Some(deliveries) = deliveries {
            send_deliveries(outgoing, deliveries).await;
            true
        } else {
            false
        }
    }

    pub(crate) async fn close_prompts_for_turn(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        turn_id: &str,
        reason: SessionScriptPromptClosedReason,
    ) {
        let deliveries = {
            let mut state = self.state.lock().await;
            let prompt_ids = state
                .prompts_by_id
                .iter()
                .filter_map(|(prompt_id, prompt)| {
                    (prompt.thread_id == thread_id && prompt.turn_id.as_deref() == Some(turn_id))
                        .then_some(prompt_id.clone())
                })
                .collect::<Vec<_>>();
            let mut deliveries = Vec::new();
            for prompt_id in prompt_ids {
                if let Some(prompt) = state.prompts_by_id.remove(&prompt_id) {
                    deliveries.extend(prompt_closed_deliveries(
                        &mut state,
                        &prompt,
                        reason.clone(),
                    ));
                }
            }
            deliveries
        };
        send_deliveries(outgoing, deliveries).await;
    }

    pub(crate) async fn remove_thread(
        &self,
        thread_id: ThreadId,
    ) -> Vec<ScriptNotificationDelivery> {
        let deliveries = {
            let mut state = self.state.lock().await;
            let prompt_ids = state
                .prompts_by_id
                .iter()
                .filter_map(|(prompt_id, prompt)| {
                    (prompt.thread_id == thread_id).then_some(prompt_id.clone())
                })
                .collect::<Vec<_>>();
            let mut deliveries = Vec::new();
            for prompt_id in prompt_ids {
                if let Some(prompt) = state.prompts_by_id.remove(&prompt_id) {
                    deliveries.extend(prompt_closed_deliveries(
                        &mut state,
                        &prompt,
                        SessionScriptPromptClosedReason::TurnEnded,
                    ));
                }
            }
            let connection_ids = state
                .connections_by_thread
                .get(&thread_id)
                .into_iter()
                .flat_map(|connection_ids| connection_ids.iter().copied())
                .collect::<Vec<_>>();
            for connection_id in connection_ids {
                deliveries.extend(remove_registration(
                    &mut state,
                    connection_id,
                    SessionScriptPromptClosedReason::TurnEnded,
                ));
            }
            deliveries
        };
        let _ = self.thread_removed_tx.send(thread_id);
        deliveries
    }

    pub(crate) fn thread_removed_receiver(&self) -> broadcast::Receiver<ThreadId> {
        self.thread_removed_tx.subscribe()
    }

    async fn publish(
        &self,
        outgoing: &Arc<OutgoingMessageSender>,
        thread_id: ThreadId,
        notification: ServerNotification,
        subscribed: impl Fn(&SessionScriptSubscriptionsParams) -> bool,
    ) {
        let connection_ids = {
            let mut state = self.state.lock().await;
            state
                .registrations_by_connection
                .iter_mut()
                .filter(|(_, registration)| registration.thread_id == thread_id)
                .filter_map(|(connection_id, registration)| {
                    if !subscribed(&registration.subscriptions) {
                        return None;
                    }
                    if !registration.ready {
                        registration.resync_required = true;
                        return None;
                    }
                    Some(*connection_id)
                })
                .collect::<Vec<_>>()
        };
        if !connection_ids.is_empty() {
            outgoing
                .send_server_notification_to_connections(connection_ids.as_slice(), notification)
                .await;
        }
    }
}

fn session_script_capability_from_config(
    capability: SessionScriptCapabilityToml,
) -> SessionScriptCapability {
    match capability {
        SessionScriptCapabilityToml::UserInputSend => SessionScriptCapability::UserInputSend,
        SessionScriptCapabilityToml::PromptRequestUserInputRespond => {
            SessionScriptCapability::PromptRequestUserInputRespond
        }
        SessionScriptCapabilityToml::PromptApprovalRespond => {
            SessionScriptCapability::PromptApprovalRespond
        }
    }
}

fn extension_policy_from_requested_capabilities(
    extension_name: &str,
    requested_capabilities: &[String],
) -> SessionScriptPolicy {
    let mut policy = SessionScriptPolicy {
        extension_name: extension_name.to_string(),
        capabilities: HashSet::new(),
        subscriptions: SessionScriptSubscriptionPolicy::default(),
        response_timeout: Duration::from_secs(/*secs*/ 10),
    };
    for capability in requested_capabilities {
        match capability.as_str() {
            "session.observe" => {
                policy.subscriptions.model_response_deltas = true;
                policy.subscriptions.model_response_completed = true;
                policy.subscriptions.user_messages = true;
                policy.subscriptions.turn_completed = true;
                policy.subscriptions.file_changes = true;
                policy.subscriptions.session_updates = true;
            }
            "userInput.send" => {
                policy
                    .capabilities
                    .insert(SessionScriptCapability::UserInputSend);
            }
            "prompt.requestUserInput.respond" => {
                policy
                    .capabilities
                    .insert(SessionScriptCapability::PromptRequestUserInputRespond);
                policy
                    .subscriptions
                    .prompts
                    .insert(SessionScriptPromptKind::RequestUserInput);
            }
            "prompt.approval.respond" => {
                policy
                    .capabilities
                    .insert(SessionScriptCapability::PromptApprovalRespond);
                policy.subscriptions.prompts.extend([
                    SessionScriptPromptKind::ExtensionInteraction,
                    SessionScriptPromptKind::CommandExecutionApproval,
                    SessionScriptPromptKind::FileChangeApproval,
                    SessionScriptPromptKind::PermissionsApproval,
                ]);
            }
            _ => {}
        }
    }
    policy
}

fn validate_request_user_input_answers(
    params: &ToolRequestUserInputParams,
    answers: &HashMap<String, SessionScriptRequestUserInputAnswer>,
) -> Result<(), String> {
    if answers.len() != params.questions.len() {
        return Err("a response must include one answer entry for every question".to_string());
    }

    for question in &params.questions {
        let answer = answers
            .get(&question.id)
            .ok_or_else(|| format!("response is missing question id `{}`", question.id))?;
        let Some(options) = question.options.as_ref() else {
            continue;
        };
        let option_labels = options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<HashSet<_>>();
        let selected_options = answer
            .answers
            .iter()
            .filter(|answer| !answer.starts_with("user_note: "))
            .collect::<Vec<_>>();
        if selected_options.len() > 1 {
            return Err(format!(
                "response includes multiple selected options for question `{}`",
                question.id
            ));
        }
        if let Some(selected_option) = selected_options.first()
            && !option_labels.contains(selected_option.as_str())
        {
            return Err(format!(
                "response contains an unknown option for question `{}`",
                question.id
            ));
        }
    }
    if let Some(unknown_question_id) = answers.keys().find(|question_id| {
        !params
            .questions
            .iter()
            .any(|question| question.id.as_str() == question_id.as_str())
    }) {
        return Err(format!(
            "response contains unknown question id `{unknown_question_id}`"
        ));
    }
    Ok(())
}

pub(crate) struct ScriptNotificationDelivery {
    connection_id: ConnectionId,
    notification: ServerNotification,
}

pub(crate) async fn send_deliveries(
    outgoing: &Arc<OutgoingMessageSender>,
    deliveries: Vec<ScriptNotificationDelivery>,
) {
    for delivery in deliveries {
        outgoing
            .send_server_notification_to_connections(
                std::slice::from_ref(&delivery.connection_id),
                delivery.notification,
            )
            .await;
    }
}

fn remove_registration(
    state: &mut SessionScriptRegistryState,
    connection_id: ConnectionId,
    reason: SessionScriptPromptClosedReason,
) -> Vec<ScriptNotificationDelivery> {
    let Some(registration) = state.registrations_by_connection.remove(&connection_id) else {
        return Vec::new();
    };
    if state
        .request_user_input_responder_by_thread
        .get(&registration.thread_id)
        == Some(&connection_id)
    {
        state
            .request_user_input_responder_by_thread
            .remove(&registration.thread_id);
    }
    if state
        .approval_responder_by_thread
        .get(&registration.thread_id)
        == Some(&connection_id)
    {
        state
            .approval_responder_by_thread
            .remove(&registration.thread_id);
    }
    if let Some(connection_ids) = state.connections_by_thread.get_mut(&registration.thread_id) {
        connection_ids.remove(&connection_id);
        if connection_ids.is_empty() {
            state.connections_by_thread.remove(&registration.thread_id);
        }
    }

    let prompt_ids = state
        .prompts_by_id
        .iter()
        .filter_map(|(prompt_id, prompt)| {
            (prompt.responder_connection_id == Some(connection_id)).then_some(prompt_id.clone())
        })
        .collect::<Vec<_>>();
    let mut deliveries = Vec::new();
    for prompt_id in prompt_ids {
        if let Some(prompt) = state.prompts_by_id.remove(&prompt_id) {
            deliveries.extend(prompt_closed_deliveries(state, &prompt, reason.clone()));
        }
    }
    deliveries
}

fn is_approval_prompt_kind(kind: SessionScriptPromptKind) -> bool {
    matches!(
        kind,
        SessionScriptPromptKind::ExtensionInteraction
            | SessionScriptPromptKind::CommandExecutionApproval
            | SessionScriptPromptKind::FileChangeApproval
            | SessionScriptPromptKind::PermissionsApproval
    )
}

fn should_receive_prompt(
    registration: &SessionScriptRegistration,
    connection_id: ConnectionId,
    prompt: &SessionScriptPromptState,
) -> bool {
    registration
        .subscriptions
        .prompts
        .as_ref()
        .is_some_and(|kinds| kinds.contains(&prompt.kind))
        || prompt.responder_connection_id == Some(connection_id)
}

fn prompt_projection(
    prompt: &SessionScriptPromptState,
    connection_id: ConnectionId,
) -> SessionScriptPrompt {
    let can_respond = prompt.responder_connection_id == Some(connection_id);
    SessionScriptPrompt {
        prompt_id: prompt.prompt_id.clone(),
        kind: prompt.kind,
        thread_id: prompt.thread_id.to_string(),
        turn_id: prompt.turn_id.clone(),
        item_id: prompt.item_id.clone(),
        can_respond,
        response_lease: can_respond.then(|| {
            prompt
                .response_lease
                .clone()
                .expect("requestUserInput responder has a lease")
        }),
        request: prompt.request.clone(),
    }
}

fn prompt_open_deliveries(
    state: &mut SessionScriptRegistryState,
    prompt: &SessionScriptPromptState,
) -> Vec<ScriptNotificationDelivery> {
    state
        .registrations_by_connection
        .iter_mut()
        .filter(|(_, registration)| registration.thread_id == prompt.thread_id)
        .filter_map(|(connection_id, registration)| {
            if !should_receive_prompt(registration, *connection_id, prompt) {
                return None;
            }
            if !registration.ready {
                registration.resync_required = true;
                return None;
            }
            Some(ScriptNotificationDelivery {
                connection_id: *connection_id,
                notification: ServerNotification::ScriptPromptOpened(
                    SessionScriptPromptOpenedNotification {
                        registration_id: registration.registration_id.clone(),
                        prompt: prompt_projection(prompt, *connection_id),
                    },
                ),
            })
        })
        .collect()
}

fn prompt_closed_deliveries(
    state: &mut SessionScriptRegistryState,
    prompt: &SessionScriptPromptState,
    reason: SessionScriptPromptClosedReason,
) -> Vec<ScriptNotificationDelivery> {
    state
        .registrations_by_connection
        .iter_mut()
        .filter(|(_, registration)| registration.thread_id == prompt.thread_id)
        .filter_map(|(connection_id, registration)| {
            if !should_receive_prompt(registration, *connection_id, prompt) {
                return None;
            }
            if !registration.ready {
                registration.resync_required = true;
                return None;
            }
            Some(ScriptNotificationDelivery {
                connection_id: *connection_id,
                notification: ServerNotification::ScriptPromptClosed(
                    SessionScriptPromptClosedNotification {
                        registration_id: registration.registration_id.clone(),
                        prompt_id: prompt.prompt_id.clone(),
                        reason: reason.clone(),
                    },
                ),
            })
        })
        .collect()
}
