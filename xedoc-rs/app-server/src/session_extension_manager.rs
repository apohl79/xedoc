//! Session-scoped discovery, approval, and one-shot invocation of plugin extensions.

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;
use xedoc_app_server_protocol::ExtensionInteractionAction;
use xedoc_app_server_protocol::ExtensionInteractionDetail;
use xedoc_app_server_protocol::ExtensionInteractionOutcome;
use xedoc_app_server_protocol::ExtensionInteractionRequestParams;
use xedoc_app_server_protocol::ExtensionInteractionRequestResponse;
use xedoc_app_server_protocol::ExtensionInteractionSurface;
use xedoc_app_server_protocol::JSONRPCErrorError;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ServerRequestPayload;
use xedoc_app_server_protocol::SessionExtensionCommand;
use xedoc_app_server_protocol::SessionExtensionCommandInvokeParams;
use xedoc_app_server_protocol::SessionExtensionCommandInvokeResponse;
use xedoc_app_server_protocol::SessionExtensionCommandsUpdatedNotification;
use xedoc_app_server_protocol::SessionExtensionListParams;
use xedoc_app_server_protocol::SessionExtensionListResponse;
use xedoc_app_server_protocol::SessionExtensionMessageLevel;
use xedoc_app_server_protocol::SessionExtensionMessageNotification;
use xedoc_app_server_protocol::SessionScriptPromptClosedReason;
use xedoc_app_server_protocol::SessionScriptPromptKind;
use xedoc_app_server_protocol::SessionScriptPromptRequest;
use xedoc_app_server_protocol::WarningNotification;
use xedoc_core::ThreadManager;
use xedoc_core::config::Config;
use xedoc_plugin::LoadedPlugin;
use xedoc_plugin::manifest::PluginManifestExtension;
use xedoc_protocol::ThreadId;
use xedoc_script_protocol::Extension;
use xedoc_script_protocol::FormField;
use xedoc_script_protocol::Interaction;
use xedoc_script_protocol::InteractionResponse;
use xedoc_script_protocol::InteractionSurface;
use xedoc_script_protocol::Method;
use xedoc_script_protocol::OpaqueId;
use xedoc_script_protocol::ProtocolVersion;
use xedoc_script_protocol::RequestId;
use xedoc_script_protocol::ResponseOutcome;
use xedoc_script_protocol::ScriptInvoker;
use xedoc_script_protocol::ScriptMessageLevel;
use xedoc_script_protocol::ScriptRequest;
use xedoc_script_protocol::ScriptResult;

use crate::error_code::invalid_request;
use crate::outgoing_message::ClientRequestResult;
use crate::outgoing_message::OutgoingMessageSender;
use crate::session_script_host::SessionScriptHost;
use crate::session_script_registry::SessionScriptRegistry;
use crate::session_script_registry::send_deliveries;
use crate::thread_state::ThreadStateManager;

const SESSION_EXTENSION_INVOCATION_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 300);
const SESSION_EXTENSION_REQUEST_ATTEMPT_TIMEOUT: Duration = SESSION_EXTENSION_INVOCATION_TIMEOUT;
const APPROVAL_ACTION_SESSION: &str = "approve-session";
const APPROVAL_ACTION_ALWAYS: &str = "approve-always";
const APPROVAL_ACTION_DENY: &str = "deny";
const SESSION_EXTENSION_INTERACTION_TURN_PREFIX: &str = "session-extension:";
const PERSISTENT_GRANTS_FILE: &str = "session-extension-grants.json";
const MAX_PLUGIN_MANIFEST_EVIDENCE_BYTES: u64 = 64 * 1024;

/// Manages extension activation independently for each root thread.
#[derive(Clone)]
pub(crate) struct SessionExtensionManager {
    inner: Arc<SessionExtensionManagerInner>,
}

struct SessionExtensionManagerInner {
    config: Arc<Config>,
    thread_manager: Arc<ThreadManager>,
    thread_state_manager: ThreadStateManager,
    outgoing: Arc<OutgoingMessageSender>,
    session_script_registry: SessionScriptRegistry,
    session_script_host: Arc<Mutex<SessionScriptHost>>,
    grants_path: PathBuf,
    persistent_grants: Mutex<Vec<PersistentGrant>>,
    threads: Mutex<HashMap<ThreadId, SessionExtensionThreadState>>,
}

#[derive(Default)]
struct SessionExtensionThreadState {
    extensions: HashMap<String, SessionExtensionDescriptor>,
    approved_extension_ids: HashSet<String>,
    enabled_extension_ids: HashSet<String>,
}

#[derive(Clone)]
struct SessionExtensionDescriptor {
    id: String,
    manifest_extension_id: String,
    plugin_id: String,
    plugin_display_name: String,
    entrypoint: PathBuf,
    requested_capabilities: Vec<String>,
    commands: Vec<SessionExtensionCommand>,
    declaration_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistentGrant {
    plugin_id: String,
    extension_id: String,
    declaration_digest: String,
}

#[derive(Clone, Copy)]
enum ApprovalDecision {
    Session,
    Always,
    Deny,
}

impl SessionExtensionManager {
    pub(crate) fn new(
        config: Arc<Config>,
        thread_manager: Arc<ThreadManager>,
        thread_state_manager: ThreadStateManager,
        outgoing: Arc<OutgoingMessageSender>,
        session_script_registry: SessionScriptRegistry,
        session_script_host: Arc<Mutex<SessionScriptHost>>,
    ) -> Self {
        let grants_path = config
            .xedoc_home
            .join(PERSISTENT_GRANTS_FILE)
            .into_path_buf();
        let persistent_grants = read_persistent_grants(&grants_path);
        Self {
            inner: Arc::new(SessionExtensionManagerInner {
                config,
                thread_manager,
                thread_state_manager,
                outgoing,
                session_script_registry,
                session_script_host,
                grants_path,
                persistent_grants: Mutex::new(persistent_grants),
                threads: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Discovers manifest declarations and begins the approval flow for one root thread.
    pub(crate) async fn start_for_thread(&self, thread_id: ThreadId) {
        {
            let threads = self.inner.threads.lock().await;
            if threads.contains_key(&thread_id) {
                return;
            }
        }

        let descriptors = self.discover_extensions().await;
        {
            let mut threads = self.inner.threads.lock().await;
            if threads.contains_key(&thread_id) {
                return;
            }
            threads.insert(
                thread_id,
                SessionExtensionThreadState {
                    extensions: descriptors
                        .into_iter()
                        .map(|descriptor| (descriptor.id.clone(), descriptor))
                        .collect(),
                    approved_extension_ids: HashSet::new(),
                    enabled_extension_ids: HashSet::new(),
                },
            );
        }

        let manager = self.clone();
        tokio::spawn(async move {
            manager.activate_discovered_extensions(thread_id).await;
        });
    }

    /// Cancels the per-thread lifecycle state when the root thread is removed.
    pub(crate) async fn stop_thread(&self, thread_id: ThreadId) {
        let enabled_extensions = self
            .inner
            .threads
            .lock()
            .await
            .remove(&thread_id)
            .map(|state| state.enabled_extension_ids.into_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for extension_id in enabled_extensions {
            self.stop_extension_script(thread_id, &extension_id).await;
        }
    }

    pub(crate) async fn list(
        &self,
        params: SessionExtensionListParams,
    ) -> Result<SessionExtensionListResponse, JSONRPCErrorError> {
        let thread_id = parse_thread_id(&params.thread_id)?;
        Ok(SessionExtensionListResponse {
            commands: self.commands_for_thread(thread_id).await,
        })
    }

    pub(crate) async fn invoke(
        &self,
        params: SessionExtensionCommandInvokeParams,
    ) -> Result<SessionExtensionCommandInvokeResponse, JSONRPCErrorError> {
        let thread_id = parse_thread_id(&params.thread_id)?;
        let descriptor = {
            let threads = self.inner.threads.lock().await;
            let Some(state) = threads.get(&thread_id) else {
                return Err(invalid_request(
                    "session extension state is not available for this thread",
                ));
            };
            if !state.approved_extension_ids.contains(&params.extension_id) {
                return Err(invalid_request(
                    "session extension is not approved for this thread",
                ));
            }
            let Some(descriptor) = state.extensions.get(&params.extension_id) else {
                return Err(invalid_request(
                    "session extension is not declared for this thread",
                ));
            };
            if !descriptor
                .commands
                .iter()
                .any(|command| command.name == params.command)
            {
                return Err(invalid_request(
                    "session extension command is not declared by this extension",
                ));
            }
            descriptor.clone()
        };

        if params
            .arguments
            .first()
            .is_some_and(|argument| argument == "off")
        {
            let command_result = self
                .invoke_extension_command(
                    thread_id,
                    descriptor.clone(),
                    params.command.clone(),
                    params.arguments.clone(),
                )
                .await;
            self.disable(thread_id, &descriptor.id).await;
            command_result.map_err(invalid_request)?;
            return Ok(SessionExtensionCommandInvokeResponse {});
        }
        if params
            .arguments
            .first()
            .is_some_and(|argument| argument == "on")
        {
            self.invoke_extension_command(
                thread_id,
                descriptor.clone(),
                params.command,
                params.arguments,
            )
            .await
            .map_err(invalid_request)?;
            self.enable(thread_id, descriptor).await?;
            return Ok(SessionExtensionCommandInvokeResponse {});
        }
        if params
            .arguments
            .first()
            .is_some_and(|argument| argument == "restart")
        {
            self.invoke_extension_command(
                thread_id,
                descriptor.clone(),
                params.command,
                params.arguments,
            )
            .await
            .map_err(invalid_request)?;
            self.disable(thread_id, &descriptor.id).await;
            self.enable(thread_id, descriptor).await?;
            return Ok(SessionExtensionCommandInvokeResponse {});
        }

        let manager = self.clone();
        tokio::spawn(async move {
            if let Err(error) = manager
                .invoke_extension_command(thread_id, descriptor, params.command, params.arguments)
                .await
            {
                warn!(thread_id = %thread_id, %error, "session extension command failed");
                manager
                    .inner
                    .outgoing
                    .send_server_notification(ServerNotification::Warning(WarningNotification {
                        thread_id: Some(thread_id.to_string()),
                        message: format!("Session extension command failed: {error}"),
                    }))
                    .await;
            }
        });
        Ok(SessionExtensionCommandInvokeResponse {})
    }

    async fn activate_discovered_extensions(&self, thread_id: ThreadId) {
        let descriptors = {
            let threads = self.inner.threads.lock().await;
            threads
                .get(&thread_id)
                .map(|state| state.extensions.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        for descriptor in descriptors {
            let decision = if self.has_persistent_grant(&descriptor).await {
                Some(ApprovalDecision::Always)
            } else {
                self.request_approval(thread_id, &descriptor).await
            };
            match decision {
                Some(ApprovalDecision::Session | ApprovalDecision::Always) => {
                    if let Some(state) = self.inner.threads.lock().await.get_mut(&thread_id) {
                        state.approved_extension_ids.insert(descriptor.id.clone());
                    }
                    if matches!(decision, Some(ApprovalDecision::Always)) {
                        self.persist_grant(&descriptor).await;
                    }
                    if let Err(error) = self.activate_extension(thread_id, descriptor.clone()).await
                    {
                        warn!(
                            thread_id = %thread_id,
                            extension_id = descriptor.id,
                            %error,
                            "session extension setup failed"
                        );
                    }
                }
                Some(ApprovalDecision::Deny) | None => {}
            }
        }
    }

    async fn activate_extension(
        &self,
        thread_id: ThreadId,
        descriptor: SessionExtensionDescriptor,
    ) -> Result<(), String> {
        let outcome = self
            .invoke_script(
                thread_id,
                &descriptor,
                Method::ExtensionSetupOpen,
                json!({}),
            )
            .await?;
        self.drive_outcome(thread_id, descriptor.clone(), outcome)
            .await?;
        self.notify_commands(thread_id).await;
        Ok(())
    }

    async fn invoke_extension_command(
        &self,
        thread_id: ThreadId,
        descriptor: SessionExtensionDescriptor,
        command: String,
        arguments: Vec<String>,
    ) -> Result<(), String> {
        let outcome = self
            .invoke_script(
                thread_id,
                &descriptor,
                Method::ExtensionCommandInvoke,
                json!({
                    "command": command,
                    "arguments": arguments,
                }),
            )
            .await?;
        self.drive_outcome(thread_id, descriptor, outcome).await
    }

    async fn drive_outcome(
        &self,
        thread_id: ThreadId,
        descriptor: SessionExtensionDescriptor,
        mut outcome: ResponseOutcome,
    ) -> Result<(), String> {
        loop {
            match outcome {
                ResponseOutcome::Error { error } => return Err(error.message),
                ResponseOutcome::Result {
                    result: ScriptResult::Complete { summary },
                } => {
                    if let Some(message) = summary {
                        self.inner
                            .outgoing
                            .send_server_notification(ServerNotification::Warning(
                                WarningNotification {
                                    thread_id: Some(thread_id.to_string()),
                                    message,
                                },
                            ))
                            .await;
                    }
                    return Ok(());
                }
                ResponseOutcome::Result {
                    result: ScriptResult::Message { level, message },
                } => {
                    let level = match level {
                        ScriptMessageLevel::Info => SessionExtensionMessageLevel::Info,
                        ScriptMessageLevel::Warning => SessionExtensionMessageLevel::Warning,
                        ScriptMessageLevel::Error => SessionExtensionMessageLevel::Error,
                    };
                    self.inner
                        .outgoing
                        .send_server_notification(ServerNotification::SessionExtensionMessage(
                            SessionExtensionMessageNotification {
                                thread_id: thread_id.to_string(),
                                extension_name: descriptor.manifest_extension_id.clone(),
                                level,
                                message,
                            },
                        ))
                        .await;
                    return Ok(());
                }
                ResponseOutcome::Result {
                    result: ScriptResult::Interaction { interaction },
                } => {
                    validate_session_extension_interaction(&interaction)?;
                    let response = self
                        .request_extension_interaction(thread_id, &descriptor, interaction)
                        .await?;
                    outcome = self
                        .invoke_script(
                            thread_id,
                            &descriptor,
                            Method::InteractionRespond,
                            serde_json::to_value(interaction_response_from_request(response))
                                .map_err(|error| error.to_string())?,
                        )
                        .await?;
                }
                ResponseOutcome::Result {
                    result: ScriptResult::Route { .. },
                } => {
                    return Err(
                        "session extension returned a routing decision for a non-routing method"
                            .to_string(),
                    );
                }
                ResponseOutcome::Result {
                    result: ScriptResult::ClassifierRequest { .. },
                } => {
                    return Err(
                        "session extension requested model classification for a non-routing method"
                            .to_string(),
                    );
                }
                ResponseOutcome::Result {
                    result: ScriptResult::State { .. },
                } => {
                    return Err(
                        "session extension returned router state for a non-routing method"
                            .to_string(),
                    );
                }
            }
        }
    }

    async fn request_approval(
        &self,
        thread_id: ThreadId,
        descriptor: &SessionExtensionDescriptor,
    ) -> Option<ApprovalDecision> {
        let request_id = format!("session-extension-approval:{}", descriptor.id);
        let response = self
            .request_interaction(
                thread_id,
                &request_id,
                "session-extension-approval",
                &request_id,
                "approval",
                /*state_revision*/ None,
                ExtensionInteractionSurface::Confirmation {
                    title: format!(
                        "Allow “{}” in this session?",
                        descriptor.plugin_display_name
                    ),
                    body:
                        "This plugin extension will run its declared local script for this session."
                            .to_string(),
                    details: vec![
                        ExtensionInteractionDetail {
                            label: "Plugin".to_string(),
                            value: descriptor.plugin_id.clone(),
                        },
                        ExtensionInteractionDetail {
                            label: "Commands".to_string(),
                            value: descriptor
                                .commands
                                .iter()
                                .map(|command| format!("/{}", command.name))
                                .collect::<Vec<_>>()
                                .join(", "),
                        },
                        ExtensionInteractionDetail {
                            label: "Requested access".to_string(),
                            value: requested_access_summary(&descriptor.requested_capabilities),
                        },
                    ],
                    sections: Vec::new(),
                    actions: vec![
                        ExtensionInteractionAction {
                            id: APPROVAL_ACTION_SESSION.to_string(),
                            opens: None,
                            host_action: None,
                            label: Some("Approve for this session".to_string()),
                            key_bindings: vec!["enter".to_string()],
                            context: None,
                            value: None,
                        },
                        ExtensionInteractionAction {
                            id: APPROVAL_ACTION_ALWAYS.to_string(),
                            opens: None,
                            host_action: None,
                            label: Some("Approve always".to_string()),
                            key_bindings: Vec::new(),
                            context: None,
                            value: None,
                        },
                        ExtensionInteractionAction {
                            id: APPROVAL_ACTION_DENY.to_string(),
                            opens: None,
                            host_action: None,
                            label: Some("Deny".to_string()),
                            key_bindings: Vec::new(),
                            context: None,
                            value: None,
                        },
                    ],
                    override_form: None,
                },
            )
            .await
            .ok()?;
        match response.action.as_ref().map(|action| action.id.as_str()) {
            Some(APPROVAL_ACTION_SESSION)
                if response.outcome == ExtensionInteractionOutcome::Accepted =>
            {
                Some(ApprovalDecision::Session)
            }
            Some(APPROVAL_ACTION_ALWAYS)
                if response.outcome == ExtensionInteractionOutcome::Accepted =>
            {
                Some(ApprovalDecision::Always)
            }
            _ => Some(ApprovalDecision::Deny),
        }
    }

    async fn request_extension_interaction(
        &self,
        thread_id: ThreadId,
        descriptor: &SessionExtensionDescriptor,
        interaction: Interaction,
    ) -> Result<ExtensionInteractionRequestResponse, String> {
        let interaction_id = interaction.id.as_str().to_string();
        let continuation = interaction.continuation.as_str().to_string();
        let state_revision = interaction
            .state_revision
            .as_ref()
            .map(|revision| revision.as_str().to_string());
        let surface = serde_json::from_value(
            serde_json::to_value(interaction.surface).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        self.request_interaction(
            thread_id,
            &format!("session-extension:{}", descriptor.id),
            &descriptor.id,
            &interaction_id,
            &continuation,
            state_revision,
            surface,
        )
        .await
    }

    async fn request_interaction(
        &self,
        thread_id: ThreadId,
        request_id: &str,
        extension_id: &str,
        interaction_id: &str,
        continuation: &str,
        state_revision: Option<String>,
        surface: ExtensionInteractionSurface,
    ) -> Result<ExtensionInteractionRequestResponse, String> {
        let params = ExtensionInteractionRequestParams {
            thread_id: thread_id.to_string(),
            turn_id: format!("{SESSION_EXTENSION_INTERACTION_TURN_PREFIX}{thread_id}"),
            request_id: request_id.to_string(),
            extension_id: extension_id.to_string(),
            interaction_id: interaction_id.to_string(),
            continuation: continuation.to_string(),
            state_revision,
            expires_at: i64::MAX,
            surface,
        };
        let script_prompt = self
            .inner
            .session_script_registry
            .open_approval_prompt(
                &self.inner.outgoing,
                thread_id,
                SessionScriptPromptKind::ExtensionInteraction,
                Some(params.turn_id.clone()),
                Some(params.request_id.clone()),
                SessionScriptPromptRequest {
                    method: "item/extensionInteraction/request".to_string(),
                    params: serde_json::to_value(&params).unwrap_or(serde_json::Value::Null),
                },
            )
            .await;
        if let (Some(response_receiver), Some(response_timeout)) = (
            script_prompt.response_receiver,
            script_prompt.response_timeout,
        ) {
            match timeout(response_timeout, response_receiver).await {
                Ok(Ok(response)) => {
                    self.inner
                        .session_script_registry
                        .close_prompt(
                            &self.inner.outgoing,
                            &script_prompt.prompt_id,
                            SessionScriptPromptClosedReason::Answered,
                        )
                        .await;
                    return serde_json::from_value(response).map_err(|error| error.to_string());
                }
                Ok(Err(_)) => {
                    return Err("approval responder closed the interaction".to_string());
                }
                Err(_) => {
                    self.inner
                        .session_script_registry
                        .close_prompt(
                            &self.inner.outgoing,
                            &script_prompt.prompt_id,
                            SessionScriptPromptClosedReason::Expired,
                        )
                        .await;
                }
            }
        }

        let deadline = Instant::now() + SESSION_EXTENSION_INVOCATION_TIMEOUT;
        let mut last_connection_ids = None;
        let result = loop {
            let connection_ids = self
                .inner
                .thread_state_manager
                .subscribed_connection_ids(thread_id)
                .await;
            if connection_ids.is_empty() {
                if Instant::now() >= deadline {
                    break Err("no client is subscribed to the extension thread".to_string());
                }
                sleep(Duration::from_millis(100)).await;
                continue;
            }
            let target_connection_id = connection_ids[0];
            let (request_id, receiver) = self
                .inner
                .outgoing
                .send_request_to_connections(
                    Some(std::slice::from_ref(&target_connection_id)),
                    ServerRequestPayload::ExtensionInteractionRequest(params.clone()),
                    Some(thread_id),
                )
                .await;
            let remaining = deadline.saturating_duration_since(Instant::now());
            match timeout(
                SESSION_EXTENSION_REQUEST_ATTEMPT_TIMEOUT.min(remaining),
                deserialize_interaction_response(receiver),
            )
            .await
            {
                Ok(result) => break result,
                Err(_) => {
                    self.inner.outgoing.cancel_request(&request_id).await;
                    let current_connection_ids = self
                        .inner
                        .thread_state_manager
                        .subscribed_connection_ids(thread_id)
                        .await;
                    if current_connection_ids == connection_ids
                        && last_connection_ids.as_ref() == Some(&connection_ids)
                    {
                        break Err("extension interaction timed out".to_string());
                    }
                    last_connection_ids = Some(current_connection_ids);
                    if Instant::now() >= deadline {
                        break Err("extension interaction timed out".to_string());
                    }
                }
            }
        };
        self.inner
            .session_script_registry
            .close_prompt(
                &self.inner.outgoing,
                &script_prompt.prompt_id,
                SessionScriptPromptClosedReason::Answered,
            )
            .await;
        result
    }

    async fn invoke_script(
        &self,
        thread_id: ThreadId,
        descriptor: &SessionExtensionDescriptor,
        method: Method,
        params: serde_json::Value,
    ) -> Result<ResponseOutcome, String> {
        let extension_enabled = self
            .inner
            .threads
            .lock()
            .await
            .get(&thread_id)
            .is_some_and(|state| state.enabled_extension_ids.contains(&descriptor.id));
        let cwd = match self.inner.thread_manager.get_thread(thread_id).await {
            Ok(thread) => thread
                .config_snapshot()
                .await
                .cwd()
                .as_path()
                .display()
                .to_string(),
            Err(_) => String::new(),
        };
        let request = ScriptRequest {
            protocol: ProtocolVersion::v1(),
            request_id: RequestId::new(OpaqueId::new(Uuid::now_v7().to_string())),
            extension: Extension::SessionExtension,
            method,
            context: json!({
                "extensionId": descriptor.manifest_extension_id,
                "pluginId": descriptor.plugin_id,
                "threadId": thread_id.to_string(),
                "session": {
                    "threadId": thread_id.to_string(),
                    "cwd": cwd,
                    "extensionEnabled": extension_enabled,
                },
            }),
            params,
        };
        ScriptInvoker::new(vec![OsString::from(descriptor.entrypoint.as_os_str())])
            .invoke(
                &request,
                SESSION_EXTENSION_INVOCATION_TIMEOUT,
                CancellationToken::new(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn disable(&self, thread_id: ThreadId, extension_id: &str) {
        if let Some(state) = self.inner.threads.lock().await.get_mut(&thread_id) {
            state.enabled_extension_ids.remove(extension_id);
        }
        self.stop_extension_script(thread_id, extension_id).await;
        self.notify_commands(thread_id).await;
    }

    async fn enable(
        &self,
        thread_id: ThreadId,
        descriptor: SessionExtensionDescriptor,
    ) -> Result<(), JSONRPCErrorError> {
        let already_enabled = self
            .inner
            .threads
            .lock()
            .await
            .get(&thread_id)
            .is_some_and(|state| state.enabled_extension_ids.contains(&descriptor.id));
        if !already_enabled {
            self.inner
                .session_script_registry
                .grant_extension(
                    &descriptor.id,
                    thread_id,
                    &descriptor.manifest_extension_id,
                    &descriptor.requested_capabilities,
                )
                .await
                .map_err(invalid_request)?;
            let start_result = self
                .inner
                .session_script_host
                .lock()
                .await
                .start_extension(
                    thread_id,
                    descriptor.id.clone(),
                    descriptor.entrypoint.clone(),
                )
                .await;
            if let Err(error) = start_result {
                self.stop_extension_script(thread_id, &descriptor.id).await;
                return Err(invalid_request(error.to_string()));
            }
            let thread_exists = {
                let mut threads = self.inner.threads.lock().await;
                threads
                    .get_mut(&thread_id)
                    .map(|state| state.enabled_extension_ids.insert(descriptor.id.clone()))
                    .is_some()
            };
            if !thread_exists {
                self.stop_extension_script(thread_id, &descriptor.id).await;
                return Ok(());
            }
            self.notify_commands(thread_id).await;
        }
        Ok(())
    }

    async fn stop_extension_script(&self, thread_id: ThreadId, extension_id: &str) {
        let deliveries = self
            .inner
            .session_script_registry
            .revoke_extension(extension_id, thread_id)
            .await;
        send_deliveries(&self.inner.outgoing, deliveries).await;
        self.inner
            .session_script_host
            .lock()
            .await
            .stop_extension(thread_id, extension_id)
            .await;
    }

    async fn commands_for_thread(&self, thread_id: ThreadId) -> Vec<SessionExtensionCommand> {
        let threads = self.inner.threads.lock().await;
        let Some(state) = threads.get(&thread_id) else {
            return Vec::new();
        };
        let mut commands = state
            .approved_extension_ids
            .iter()
            .filter_map(|extension_id| state.extensions.get(extension_id))
            .flat_map(|extension| extension.commands.clone())
            .collect::<Vec<_>>();
        commands.sort_by(|left, right| left.name.cmp(&right.name));
        commands
    }

    async fn notify_commands(&self, thread_id: ThreadId) {
        self.inner
            .outgoing
            .send_server_notification(ServerNotification::SessionExtensionCommandsUpdated(
                SessionExtensionCommandsUpdatedNotification {
                    thread_id: thread_id.to_string(),
                    commands: self.commands_for_thread(thread_id).await,
                },
            ))
            .await;
    }

    async fn has_persistent_grant(&self, descriptor: &SessionExtensionDescriptor) -> bool {
        self.inner
            .persistent_grants
            .lock()
            .await
            .iter()
            .any(|grant| {
                grant.plugin_id == descriptor.plugin_id
                    && grant.extension_id == descriptor.id
                    && grant.declaration_digest == descriptor.declaration_digest
            })
    }

    async fn persist_grant(&self, descriptor: &SessionExtensionDescriptor) {
        let grants = {
            let mut grants = self.inner.persistent_grants.lock().await;
            if !grants.iter().any(|grant| {
                grant.plugin_id == descriptor.plugin_id
                    && grant.extension_id == descriptor.id
                    && grant.declaration_digest == descriptor.declaration_digest
            }) {
                grants.retain(|grant| {
                    grant.plugin_id != descriptor.plugin_id || grant.extension_id != descriptor.id
                });
                grants.push(PersistentGrant {
                    plugin_id: descriptor.plugin_id.clone(),
                    extension_id: descriptor.id.clone(),
                    declaration_digest: descriptor.declaration_digest.clone(),
                });
            }
            grants.clone()
        };
        if let Err(error) = write_persistent_grants(&self.inner.grants_path, &grants) {
            warn!(%error, "failed to persist session extension approval");
        }
    }

    async fn discover_extensions(&self) -> Vec<SessionExtensionDescriptor> {
        let outcome = self
            .inner
            .thread_manager
            .plugins_manager()
            .plugins_for_config(&self.inner.config.plugins_config_input())
            .await;
        let descriptors = outcome
            .plugins()
            .iter()
            .filter(|plugin| plugin.is_active())
            .flat_map(|plugin| {
                plugin.extensions.iter().filter_map(|extension| {
                    descriptor_from_manifest(plugin, extension)
                        .map_err(|()| {
                            warn!(
                                plugin = plugin.display_name(),
                                extension_id = extension.id,
                                "skipping malformed session extension declaration"
                            );
                        })
                        .ok()
                })
            })
            .collect::<Vec<_>>();
        let extension_id_counts =
            descriptors
                .iter()
                .fold(HashMap::<String, usize>::new(), |mut counts, descriptor| {
                    *counts.entry(descriptor.id.clone()).or_default() += 1;
                    counts
                });
        let command_name_counts = descriptors.iter().flat_map(|descriptor| {
            descriptor
                .commands
                .iter()
                .map(|command| command.name.clone())
        });
        let command_name_counts =
            command_name_counts.fold(HashMap::<String, usize>::new(), |mut counts, name| {
                *counts.entry(name).or_default() += 1;
                counts
            });
        descriptors
            .into_iter()
            .filter(|descriptor| {
                let has_collision = extension_id_counts.get(&descriptor.id) != Some(&1)
                    || descriptor.commands.iter().any(|command| {
                        command_name_counts.get(&command.name) != Some(&1)
                            || is_reserved_slash_command(&command.name)
                    });
                if has_collision {
                    warn!(
                        extension_id = descriptor.id,
                        "skipping colliding session extension declaration"
                    );
                }
                !has_collision
            })
            .collect()
    }
}

fn descriptor_from_manifest(
    plugin: &LoadedPlugin<xedoc_config::McpServerConfig>,
    extension: &PluginManifestExtension<xedoc_utils_absolute_path::AbsolutePathBuf>,
) -> Result<SessionExtensionDescriptor, ()> {
    if extension.id.trim().is_empty()
        || extension.commands.is_empty()
        || extension
            .commands
            .iter()
            .any(|command| command.name.trim().is_empty() || command.description.trim().is_empty())
    {
        return Err(());
    }
    let plugin_id = plugin
        .plugin_namespace
        .clone()
        .unwrap_or_else(|| plugin.config_name.clone());
    let id = format!("{plugin_id}:{}", extension.id);
    let manifest_evidence = bounded_manifest_evidence(plugin)?;
    let commands = extension
        .commands
        .iter()
        .map(|command| SessionExtensionCommand {
            extension_id: id.clone(),
            name: command.name.clone(),
            description: command.description.clone(),
        })
        .collect::<Vec<_>>();
    let declaration = json!({
        "pluginId": plugin_id,
        "extensionId": extension.id,
        "entrypoint": extension.entrypoint.as_path().display().to_string(),
        "commands": extension.commands.iter().map(|command| {
            json!({"name": command.name, "description": command.description})
        }).collect::<Vec<_>>(),
        "requestedCapabilities": extension.requested_capabilities,
        "manifestEvidence": manifest_evidence,
    });
    let declaration_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&declaration).map_err(|_| ())?,)
    );
    Ok(SessionExtensionDescriptor {
        id,
        manifest_extension_id: extension.id.clone(),
        plugin_id,
        plugin_display_name: plugin.display_name().to_string(),
        entrypoint: extension.entrypoint.as_path().to_path_buf(),
        requested_capabilities: extension.requested_capabilities.clone(),
        commands,
        declaration_digest,
    })
}

fn bounded_manifest_evidence(
    plugin: &LoadedPlugin<xedoc_config::McpServerConfig>,
) -> Result<serde_json::Value, ()> {
    const MANIFEST_PATHS: [&str; 4] = [
        ".xedoc-plugin/plugin.json",
        ".codex-plugin/plugin.json",
        ".claude-plugin/plugin.json",
        ".cursor-plugin/plugin.json",
    ];
    let manifest_path = MANIFEST_PATHS
        .iter()
        .map(|path| plugin.root.as_path().join(path))
        .find(|path| path.is_file())
        .ok_or(())?;
    let mut bytes = Vec::new();
    std::fs::File::open(manifest_path)
        .map_err(|_| ())?
        .take(MAX_PLUGIN_MANIFEST_EVIDENCE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    let truncated = bytes.len() as u64 > MAX_PLUGIN_MANIFEST_EVIDENCE_BYTES;
    if truncated {
        bytes.truncate(MAX_PLUGIN_MANIFEST_EVIDENCE_BYTES as usize);
    }
    let version = (!truncated)
        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .flatten()
        .and_then(|manifest| {
            manifest
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    Ok(json!({
        "rawManifestDigest": format!("{:x}", Sha256::digest(&bytes)),
        "rawManifestLength": bytes.len(),
        "rawManifestTruncated": truncated,
        "version": version,
    }))
}

fn parse_thread_id(thread_id: &str) -> Result<ThreadId, JSONRPCErrorError> {
    ThreadId::from_string(thread_id)
        .map_err(|error| invalid_request(format!("invalid thread id: {error}")))
}

fn requested_access_summary(capabilities: &[String]) -> String {
    if capabilities.is_empty() {
        return "No session capabilities requested".to_string();
    }
    capabilities.join(", ")
}

fn interaction_response_from_request(
    response: ExtensionInteractionRequestResponse,
) -> InteractionResponse {
    InteractionResponse {
        continuation: OpaqueId::new(response.continuation),
        interaction_id: OpaqueId::new(response.interaction_id),
        state_revision: response.state_revision.map(OpaqueId::new),
        outcome: match response.outcome {
            ExtensionInteractionOutcome::Accepted => {
                xedoc_script_protocol::InteractionOutcome::Accepted
            }
            ExtensionInteractionOutcome::Cancelled => {
                xedoc_script_protocol::InteractionOutcome::Cancelled
            }
            ExtensionInteractionOutcome::Dismissed => {
                xedoc_script_protocol::InteractionOutcome::Dismissed
            }
        },
        action: response
            .action
            .map(|action| xedoc_script_protocol::SelectedAction {
                id: OpaqueId::new(action.id),
            }),
        values: response.values,
    }
}

async fn deserialize_interaction_response(
    receiver: tokio::sync::oneshot::Receiver<ClientRequestResult>,
) -> Result<ExtensionInteractionRequestResponse, String> {
    match receiver.await {
        Ok(Ok(value)) => serde_json::from_value(value).map_err(|error| error.to_string()),
        Ok(Err(error)) => Err(error.message),
        Err(error) => Err(error.to_string()),
    }
}

fn validate_session_extension_interaction(interaction: &Interaction) -> Result<(), String> {
    match &interaction.surface {
        InteractionSurface::Menu(menu) => {
            if menu
                .items
                .iter()
                .any(|item| item.action.host_action.is_some())
            {
                return Err("session extension requested a model-router host action".to_string());
            }
        }
        InteractionSurface::Form(form) => {
            validate_form_fields(&form.fields)?;
            if form.submit.host_action.is_some()
                || form
                    .cancel
                    .as_ref()
                    .is_some_and(|action| action.host_action.is_some())
                || form.fields.iter().any(|field| {
                    matches!(
                        field,
                        FormField::Action {
                            action,
                            ..
                        } if action.host_action.is_some()
                    )
                })
            {
                return Err("session extension requested a model-router host action".to_string());
            }
        }
        InteractionSurface::Confirmation(confirmation) => {
            if confirmation
                .actions
                .iter()
                .any(|action| action.host_action.is_some())
            {
                return Err("session extension requested a model-router host action".to_string());
            }
            if let Some(override_form) = &confirmation.override_form {
                validate_form_fields(&override_form.fields)?;
                if override_form.submit.host_action.is_some()
                    || override_form
                        .cancel
                        .as_ref()
                        .is_some_and(|action| action.host_action.is_some())
                    || override_form.fields.iter().any(|field| {
                        matches!(
                            field,
                            FormField::Action {
                                action,
                                ..
                            } if action.host_action.is_some()
                        )
                    })
                {
                    return Err(
                        "session extension requested a model-router host action".to_string()
                    );
                }
            }
        }
        InteractionSurface::Notice(_) => {}
    }
    Ok(())
}

fn validate_form_fields(fields: &[FormField]) -> Result<(), String> {
    if fields.iter().any(|field| {
        matches!(
            field,
            FormField::Text {
                sensitive: true,
                value,
                ..
            } if !value.is_empty()
        )
    }) {
        return Err("sensitive text fields must not include a default value".to_string());
    }
    Ok(())
}

fn read_persistent_grants(path: &PathBuf) -> Vec<PersistentGrant> {
    std::fs::read(path)
        .ok()
        .and_then(|contents| serde_json::from_slice(&contents).ok())
        .unwrap_or_default()
}

fn write_persistent_grants(path: &PathBuf, grants: &[PersistentGrant]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        serde_json::to_vec(grants).map_err(std::io::Error::other)?,
    )
}

fn is_reserved_slash_command(name: &str) -> bool {
    matches!(
        name,
        "model"
            | "model-manager"
            | "ide"
            | "permissions"
            | "keymap"
            | "vim"
            | "experimental"
            | "skills"
            | "hooks"
            | "review"
            | "rename"
            | "new"
            | "archive"
            | "delete"
            | "resume"
            | "fork"
            | "init"
            | "compact"
            | "plan"
            | "goal"
            | "agent"
            | "side"
            | "btw"
            | "copy"
            | "raw"
            | "tool-rendering"
            | "diff"
            | "mention"
            | "status"
            | "debug-config"
            | "title"
            | "statusline"
            | "theme"
            | "mcp"
            | "plugins"
            | "logout"
            | "quit"
            | "exit"
            | "rollout"
            | "ps"
            | "stop"
            | "clean"
            | "clear"
            | "personality"
            | "model-router"
            | "token-usage-optimizer"
            | "test-approval"
            | "multi-agents"
            | "subagents"
    )
}
