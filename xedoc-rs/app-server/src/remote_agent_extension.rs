//! Built-in remote-agent model tools.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use xedoc_app_server_protocol::DynamicToolCallParams;
use xedoc_app_server_protocol::DynamicToolFunctionSpec;
use xedoc_app_server_protocol::DynamicToolNamespaceTool;
use xedoc_app_server_protocol::DynamicToolSpec;
use xedoc_app_server_protocol::RemoteSessionControlParams;
use xedoc_app_server_protocol::RemoteSessionControlResponse;
use xedoc_app_server_protocol::ServerRequestPayload;
use xedoc_core::XedocThread;
use xedoc_protocol::ThreadId;

use crate::dynamic_tools::submit_unavailable_response;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::remote_agent_package::RemoteAgentPackage;
use crate::remote_agent_package::installed_remote_agent_package;
use crate::session_script_host::SessionScriptHost;
use crate::session_script_registry::SessionScriptRegistry;

pub(crate) const BUILTIN_REMOTE_AGENT_EXTENSION_ID: &str = "xedoc.remote-agent";
const BUILTIN_REMOTE_AGENT_EXTENSION_NAME: &str = "Xedoc remote agent";
const BUILTIN_REMOTE_AGENT_NAMESPACE: &str = "remote";
const BUILTIN_REMOTE_AGENT_PAYLOAD_VERSION: &str = "xedoc.remote-agent/v1";
const REMOTE_AGENT_READY_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 30);
const REMOTE_AGENT_READY_POLL: Duration = Duration::from_millis(/*millis*/ 25);
const REMOTE_AGENT_DISPATCH_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 3605);
const REMOTE_AGENT_BINDING_CAPABILITY_NAME: &str = "extension.capability";
const REMOTE_TOOL_NAMES: &[&str] = &[
    "remote_hosts_list",
    "remote_hosts_discover",
    "remote_host_pair",
    "remote_pairing_requests",
    "remote_pairing_approve",
    "remote_pairing_reject",
    "remote_host_grants_list",
    "remote_host_grant_set",
    "remote_host_suspend",
    "remote_host_revoke",
    "remote_host_remove",
    "remote_host_rotate",
    "remote_workspaces_list",
    "remote_sessions_list",
    "remote_sessions_search",
    "remote_session_start",
    "remote_session_resume",
    "remote_session_attach",
    "remote_session_read",
    "remote_session_send",
    "remote_session_steer",
    "remote_session_message",
    "remote_session_status",
    "remote_session_wait",
    "remote_session_cancel",
    "remote_session_detach",
    "remote_request_review",
    "remote_request_approve",
    "remote_request_reject",
];

#[derive(Clone)]
pub(crate) struct BuiltInRemoteAgentExtension {
    enabled: bool,
    outgoing: Arc<OutgoingMessageSender>,
    session_script_registry: SessionScriptRegistry,
    session_script_host: Arc<Mutex<SessionScriptHost>>,
    binding_capability_path: PathBuf,
    state: Arc<Mutex<RemoteAgentState>>,
}

#[derive(Clone)]
struct RemoteAgentProvider {
    connection_id: ConnectionId,
}

#[derive(Default)]
struct RemoteAgentState {
    providers: HashMap<(ThreadId, String), RemoteAgentProvider>,
    pending_threads: HashMap<ThreadId, u64>,
    launches: HashMap<ThreadId, JoinHandle<()>>,
    next_launch_generation: u64,
}

impl BuiltInRemoteAgentExtension {
    pub(crate) fn new(
        config: Arc<xedoc_core::config::Config>,
        outgoing: Arc<OutgoingMessageSender>,
        session_script_registry: SessionScriptRegistry,
        session_script_host: Arc<Mutex<SessionScriptHost>>,
    ) -> Self {
        Self {
            enabled: config.remote_agent.is_some() && !cfg!(windows),
            outgoing,
            session_script_registry,
            session_script_host,
            binding_capability_path: config
                .xedoc_home
                .join("remote-agent")
                .join(REMOTE_AGENT_BINDING_CAPABILITY_NAME)
                .to_path_buf(),
            state: Arc::new(Mutex::new(RemoteAgentState::default())),
        }
    }

    pub(crate) fn dynamic_tools(
        &self,
        config: &xedoc_core::config::Config,
        existing: &[DynamicToolSpec],
    ) -> Result<Vec<DynamicToolSpec>, String> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        reject_reserved_tool_conflicts(existing)?;
        remote_tools(config.model_provider.namespace_tools)
    }

    pub(crate) fn augment_dynamic_tools(
        &self,
        config: &xedoc_core::config::Config,
        mut existing: Vec<DynamicToolSpec>,
    ) -> Result<Vec<DynamicToolSpec>, String> {
        if !self.enabled {
            return Ok(existing);
        }
        reject_reserved_tool_conflicts(&existing)?;
        existing.extend(self.dynamic_tools(config, &existing)?);
        Ok(existing)
    }

    pub(crate) async fn start_for_thread(&self, thread_id: ThreadId) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let provider_key = (thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID.to_string());
        let existing_provider = {
            self.state
                .lock()
                .await
                .providers
                .get(&provider_key)
                .cloned()
        };
        if let Some(provider) = existing_provider {
            let registration = self
                .session_script_registry
                .ready_extension_registration(thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID)
                .await;
            if registration
                .is_some_and(|(connection_id, _)| connection_id == provider.connection_id)
            {
                return Ok(());
            }
            self.invalidate_provider(&provider_key, provider.connection_id)
                .await;
        }
        let mut state = self.state.lock().await;
        if state.pending_threads.contains_key(&thread_id) {
            return Ok(());
        }
        let launch_generation = state.next_launch_generation;
        state.next_launch_generation = state.next_launch_generation.wrapping_add(/*rhs*/ 1);
        state.pending_threads.insert(thread_id, launch_generation);
        drop(state);
        self.session_script_host
            .lock()
            .await
            .stop_extension(thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID)
            .await;
        let extension = self.clone();
        let handle = tokio::spawn(async move {
            extension
                .launch_and_wait(thread_id, launch_generation)
                .await;
        });
        let mut state = self.state.lock().await;
        if state.pending_threads.get(&thread_id) == Some(&launch_generation) {
            state.launches.insert(thread_id, handle);
        } else {
            handle.abort();
        }
        Ok(())
    }

    async fn launch_and_wait(&self, thread_id: ThreadId, launch_generation: u64) {
        let package = match installed_remote_agent_package() {
            Ok(package) => package,
            Err(error) => {
                self.finish_launch(thread_id, launch_generation).await;
                tracing::warn!(thread_id = %thread_id, %error, "failed to load built-in remote agent");
                return;
            }
        };
        let binding_token = match child_binding_token(&self.binding_capability_path, thread_id) {
            Ok(binding_token) => binding_token,
            Err(error) => {
                self.finish_launch(thread_id, launch_generation).await;
                tracing::warn!(thread_id = %thread_id, %error, "failed to prepare built-in remote agent");
                return;
            }
        };
        if let Err(error) = self
            .session_script_registry
            .grant_extension(
                BUILTIN_REMOTE_AGENT_EXTENSION_ID,
                thread_id,
                BUILTIN_REMOTE_AGENT_EXTENSION_NAME,
                &[],
            )
            .await
        {
            self.finish_launch(thread_id, launch_generation).await;
            tracing::warn!(thread_id = %thread_id, %error, "failed to grant built-in remote agent");
            return;
        }
        if let Err(error) = self
            .session_script_host
            .lock()
            .await
            .start_extension_argv_with_env(
                thread_id,
                BUILTIN_REMOTE_AGENT_EXTENSION_ID.to_string(),
                package.command_argv(),
                &[(
                    OsString::from("XEDOC_REMOTE_AGENT_BINDING_TOKEN"),
                    OsString::from(binding_token),
                )],
            )
            .await
        {
            let _ = self
                .session_script_registry
                .revoke_extension(BUILTIN_REMOTE_AGENT_EXTENSION_ID, thread_id)
                .await;
            self.finish_launch(thread_id, launch_generation).await;
            tracing::warn!(thread_id = %thread_id, %error, "failed to start built-in remote agent");
            return;
        }

        self.wait_for_ready_registration(thread_id, launch_generation)
            .await;
    }

    async fn wait_for_ready_registration(&self, thread_id: ThreadId, launch_generation: u64) {
        let deadline = Instant::now() + REMOTE_AGENT_READY_TIMEOUT;
        let (connection_id, _) = loop {
            if let Some(registration) = self
                .session_script_registry
                .ready_extension_registration(thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID)
                .await
            {
                break registration;
            }
            if Instant::now() >= deadline {
                let _ = self
                    .session_script_registry
                    .revoke_extension(BUILTIN_REMOTE_AGENT_EXTENSION_ID, thread_id)
                    .await;
                self.session_script_host
                    .lock()
                    .await
                    .stop_extension(thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID)
                    .await;
                self.finish_launch(thread_id, launch_generation).await;
                return;
            }
            tokio::time::sleep(REMOTE_AGENT_READY_POLL).await;
        };
        let mut state = self.state.lock().await;
        if state.pending_threads.get(&thread_id) == Some(&launch_generation) {
            state.pending_threads.remove(&thread_id);
            state.launches.remove(&thread_id);
            state.providers.insert(
                (thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID.to_string()),
                RemoteAgentProvider { connection_id },
            );
        }
    }

    pub(crate) async fn stop_thread(&self, thread_id: ThreadId) {
        let mut state = self.state.lock().await;
        state
            .providers
            .remove(&(thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID.to_string()));
        state.pending_threads.remove(&thread_id);
        let launch = state.launches.remove(&thread_id);
        drop(state);
        if let Some(launch) = launch {
            launch.abort();
        }
        let _ = self
            .session_script_registry
            .revoke_extension(BUILTIN_REMOTE_AGENT_EXTENSION_ID, thread_id)
            .await;
        self.session_script_host
            .lock()
            .await
            .stop_extension(thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID)
            .await;
    }

    async fn finish_launch(&self, thread_id: ThreadId, launch_generation: u64) {
        let mut state = self.state.lock().await;
        if state.pending_threads.get(&thread_id) == Some(&launch_generation) {
            state.pending_threads.remove(&thread_id);
            state.launches.remove(&thread_id);
        }
    }

    pub(crate) async fn dispatch(
        &self,
        params: DynamicToolCallParams,
        conversation: Arc<XedocThread>,
    ) -> bool {
        let Some(tool_name) = remote_tool_name(&params) else {
            return false;
        };
        let thread_id = match ThreadId::from_string(&params.thread_id) {
            Ok(thread_id) => thread_id,
            Err(_) => {
                submit_unavailable_response(
                    params.call_id,
                    "remote-agent call has an invalid thread id".to_string(),
                    conversation,
                )
                .await;
                return true;
            }
        };
        let provider = self.ready_provider(thread_id).await;
        let Some(provider) = provider else {
            submit_unavailable_response(
                params.call_id,
                format!("{tool_name} is unavailable; restart the local remote-agent broker"),
                conversation,
            )
            .await;
            return true;
        };
        let call_id = params.call_id.clone();
        let (request_id, receiver) = self
            .outgoing
            .send_request_to_connections(
                Some(&[provider.connection_id]),
                ServerRequestPayload::DynamicToolCall(params),
                Some(thread_id),
            )
            .await;
        let outgoing = Arc::clone(&self.outgoing);
        tokio::spawn(async move {
            match tokio::time::timeout(REMOTE_AGENT_DISPATCH_TIMEOUT, receiver).await {
                Ok(Ok(response)) => {
                    crate::dynamic_tools::on_targeted_call_response(
                        call_id,
                        response,
                        conversation,
                    )
                    .await;
                }
                Ok(Err(_)) => {
                    outgoing.cancel_request(&request_id).await;
                    submit_unavailable_response(
                        call_id,
                        "remote-agent operation failed; restart the local remote-agent broker"
                            .to_string(),
                        conversation,
                    )
                    .await;
                }
                Err(_) => {
                    outgoing.cancel_request(&request_id).await;
                    submit_unavailable_response(
                        call_id,
                        "remote-agent operation timed out; restart the local remote-agent broker"
                            .to_string(),
                        conversation,
                    )
                    .await;
                }
            }
        });
        true
    }

    pub(crate) async fn control(
        &self,
        root_thread_id: ThreadId,
        params: RemoteSessionControlParams,
    ) -> Result<RemoteSessionControlResponse, String> {
        let provider = self
            .ready_provider(root_thread_id)
            .await
            .ok_or_else(|| "remote-agent extension is unavailable".to_string())?;
        let (request_id, receiver) = self
            .outgoing
            .send_request_to_connections(
                Some(&[provider.connection_id]),
                ServerRequestPayload::RemoteSessionControl(params),
                Some(root_thread_id),
            )
            .await;
        match tokio::time::timeout(REMOTE_AGENT_DISPATCH_TIMEOUT, receiver).await {
            Ok(Ok(Ok(value))) => serde_json::from_value(value).map_err(|_| {
                "remote-agent extension returned an invalid control response".to_string()
            }),
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                self.outgoing.cancel_request(&request_id).await;
                Err("remote-agent control request failed".to_string())
            }
            Err(_) => {
                self.outgoing.cancel_request(&request_id).await;
                Err("remote-agent control request timed out".to_string())
            }
        }
    }

    async fn wait_for_provider(
        &self,
        provider_key: &(ThreadId, String),
    ) -> Option<RemoteAgentProvider> {
        let deadline = Instant::now() + REMOTE_AGENT_READY_TIMEOUT;
        loop {
            let state = self.state.lock().await;
            if let Some(provider) = state.providers.get(provider_key).cloned() {
                return Some(provider);
            }
            if !state.pending_threads.contains_key(&provider_key.0) || Instant::now() >= deadline {
                return None;
            }
            drop(state);
            tokio::time::sleep(REMOTE_AGENT_READY_POLL).await;
        }
    }

    async fn ready_provider(&self, thread_id: ThreadId) -> Option<RemoteAgentProvider> {
        let provider_key = (thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID.to_string());
        for _ in 0..2 {
            if self.start_for_thread(thread_id).await.is_err() {
                return None;
            }
            let Some(provider) = self.wait_for_provider(&provider_key).await else {
                return None;
            };
            if self
                .session_script_registry
                .ready_extension_registration(thread_id, BUILTIN_REMOTE_AGENT_EXTENSION_ID)
                .await
                .is_some_and(|(connection_id, _)| connection_id == provider.connection_id)
            {
                return Some(provider);
            }
            self.invalidate_provider(&provider_key, provider.connection_id)
                .await;
        }
        None
    }

    async fn invalidate_provider(&self, key: &(ThreadId, String), connection_id: ConnectionId) {
        let mut state = self.state.lock().await;
        if state
            .providers
            .get(key)
            .is_some_and(|provider| provider.connection_id == connection_id)
        {
            state.providers.remove(key);
        }
    }
}

fn declaration_digest(
    package: &RemoteAgentPackage,
    namespace_tools: bool,
) -> Result<String, String> {
    let tools = package
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            })
        })
        .collect::<Vec<_>>();
    let declaration = json!({
        "extensionId": BUILTIN_REMOTE_AGENT_EXTENSION_ID,
        "payloadVersion": BUILTIN_REMOTE_AGENT_PAYLOAD_VERSION,
        "payloadPackageVersion": package.payload_version,
        "payloadHash": package.payload_hash,
        "toolsSchemaVersion": package.tools_schema_version,
        "toolsHash": package.tools_hash,
        "packageVersion": package.package_version,
        "runtimeHash": package.runtime_hash,
        "namespaceTools": namespace_tools,
        "tools": tools,
        "dispatch": {
            "protocol": "xedoc.script/v1",
            "method": "item/tool/call",
        },
    });
    let bytes = serde_json::to_vec(&declaration).unwrap_or_default();
    Ok(hex_digest(&Sha256::digest(bytes)))
}

fn remote_tools(namespace_tools: bool) -> Result<Vec<DynamicToolSpec>, String> {
    let package = installed_remote_agent_package()?;
    let digest = declaration_digest(&package, namespace_tools)?;
    let tools = package
        .tools
        .iter()
        .map(|tool| {
            let display_name = if namespace_tools {
                tool.name.strip_prefix("remote_").unwrap_or(&tool.name)
            } else {
                &tool.name
            };
            DynamicToolFunctionSpec {
                name: display_name.to_string(),
                description: format!("{} (contract {digest})", tool.description),
                input_schema: tool.input_schema.clone(),
                defer_loading: false,
            }
        })
        .map(DynamicToolNamespaceTool::Function)
        .collect::<Vec<_>>();
    if namespace_tools {
        Ok(vec![DynamicToolSpec::Namespace(
            xedoc_protocol::dynamic_tools::DynamicToolNamespaceSpec {
                name: BUILTIN_REMOTE_AGENT_NAMESPACE.to_string(),
                description: format!(
                    "Local Xedoc remote-agent broker operations (contract {digest})."
                ),
                tools,
            },
        )])
    } else {
        Ok(tools
            .into_iter()
            .map(|tool| match tool {
                DynamicToolNamespaceTool::Function(tool) => DynamicToolSpec::Function(tool),
            })
            .collect())
    }
}

fn reject_reserved_tool_conflicts(tools: &[DynamicToolSpec]) -> Result<(), String> {
    for tool in tools {
        match tool {
            DynamicToolSpec::Function(function) if function.name.starts_with("remote_") => {
                return Err(format!(
                    "dynamic tool name is reserved by the built-in remote agent: {}",
                    function.name
                ));
            }
            DynamicToolSpec::Namespace(namespace)
                if namespace.name == BUILTIN_REMOTE_AGENT_NAMESPACE =>
            {
                return Err(
                    "dynamic tool namespace is reserved by the built-in remote agent: remote"
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn remote_tool_name(params: &DynamicToolCallParams) -> Option<String> {
    match (params.namespace.as_deref(), params.tool.as_str()) {
        (Some(BUILTIN_REMOTE_AGENT_NAMESPACE), tool)
            if REMOTE_TOOL_NAMES
                .iter()
                .any(|name| name.strip_prefix("remote_") == Some(tool)) =>
        {
            Some(format!("remote_{tool}"))
        }
        (None, tool) if REMOTE_TOOL_NAMES.contains(&tool) => Some(tool.to_string()),
        _ => None,
    }
}

pub(crate) fn remote_tool_requires_approval(tool_name: &str) -> bool {
    !matches!(
        tool_name,
        "remote_hosts_list"
            | "remote_workspaces_list"
            | "remote_sessions_list"
            | "remote_sessions_search"
            | "remote_session_read"
            | "remote_session_status"
            | "remote_session_wait"
    )
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn child_binding_token(path: &std::path::Path, thread_id: ThreadId) -> Result<String, String> {
    let binding_capability = fs::read_to_string(path)
        .map_err(|_| "built-in remote-agent binding capability is unavailable".to_string())?;
    let binding_capability = binding_capability.trim();
    if binding_capability.len() < 32 || binding_capability.len() > 128 {
        return Err("built-in remote-agent binding capability is invalid".to_string());
    }
    let mut hasher = Sha256::new();
    hasher.update(binding_capability.as_bytes());
    hasher.update([0]);
    hasher.update(thread_id.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(BUILTIN_REMOTE_AGENT_EXTENSION_ID.as_bytes());
    Ok(hex_digest(&hasher.finalize()))
}
