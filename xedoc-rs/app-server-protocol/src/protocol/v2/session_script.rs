use super::SessionExtensionMessageLevel;
use super::ThreadStatus;
use super::Turn;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use ts_rs::TS;

/// Identifies a script that is connected to one loaded root thread.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptIdentityParams {
    pub id: String,
    #[ts(optional = nullable)]
    pub name: Option<String>,
    #[ts(optional = nullable)]
    pub version: Option<String>,
}

/// Selects the bounded stream of events delivered to a session script.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptSubscriptionsParams {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub model_response_deltas: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub model_response_completed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_messages: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub turn_completed: bool,
    #[ts(optional = nullable)]
    pub prompts: Option<Vec<SessionScriptPromptKind>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub session_updates: bool,
}

/// A prompt class that a session script may observe.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum SessionScriptPromptKind {
    RequestUserInput,
    ExtensionInteraction,
    CommandExecutionApproval,
    FileChangeApproval,
    PermissionsApproval,
    McpElicitation,
}

/// A narrowly scoped action a session script may be granted for its thread.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum SessionScriptCapability {
    #[serde(rename = "userInput.send")]
    #[ts(rename = "userInput.send")]
    UserInputSend,
    #[serde(rename = "prompt.requestUserInput.respond")]
    #[ts(rename = "prompt.requestUserInput.respond")]
    PromptRequestUserInputRespond,
    #[serde(rename = "prompt.approval.respond")]
    #[ts(rename = "prompt.approval.respond")]
    PromptApprovalRespond,
}

/// Registers the initialized connection as a restricted session script.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptRegisterParams {
    pub thread_id: String,
    pub script: SessionScriptIdentityParams,
    pub subscriptions: SessionScriptSubscriptionsParams,
    #[ts(optional = nullable)]
    pub requested_capabilities: Option<Vec<SessionScriptCapability>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptRegisterResponse {
    pub registration_id: String,
    pub granted_capabilities: Vec<SessionScriptCapability>,
    pub snapshot: SessionScriptSnapshot,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptUnregisterParams {
    pub registration_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptUnregisterResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptReadParams {
    pub registration_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptReadResponse {
    pub snapshot: SessionScriptSnapshot,
}

/// Current, bounded state for a single session-script registration.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptSnapshot {
    #[ts(type = "number")]
    pub revision: u64,
    pub session: SessionScriptSession,
    pub thread: SessionScriptThread,
    pub turn: Option<Turn>,
    pub pending_prompts: Vec<SessionScriptPrompt>,
}

/// Stable session fields a script may cache until the next replacement update.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptSession {
    pub session_id: String,
    pub thread_id: String,
    pub title: Option<String>,
    pub project_name: Option<String>,
    pub project_root: Option<String>,
    pub cwd: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptThread {
    pub status: ThreadStatus,
    pub can_accept_direct_input: bool,
}

/// A bounded projection of a prompt that is open for this script's thread.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptPrompt {
    pub prompt_id: String,
    pub kind: SessionScriptPromptKind,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
    pub can_respond: bool,
    pub response_lease: Option<String>,
    pub request: SessionScriptPromptRequest,
}

/// Preserves the existing v2 request shape without making it actionable for observers.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptPromptRequest {
    pub method: String,
    pub params: JsonValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptRespondParams {
    pub registration_id: String,
    pub prompt_id: String,
    pub response_lease: String,
    pub response: SessionScriptPromptResponse,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(tag = "kind", rename_all = "camelCase", export_to = "v2/")]
pub enum SessionScriptPromptResponse {
    RequestUserInput {
        answers: HashMap<String, SessionScriptRequestUserInputAnswer>,
    },
    Approval {
        response: JsonValue,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptRequestUserInputAnswer {
    pub answers: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptRespondResponse {}

/// Posts a user-visible message from a registered session script.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptMessageParams {
    pub registration_id: String,
    pub level: SessionExtensionMessageLevel,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptMessageResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptUpdatedNotification {
    pub registration_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub session: SessionScriptSession,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptPromptOpenedNotification {
    pub registration_id: String,
    #[serde(flatten)]
    #[ts(flatten)]
    pub prompt: SessionScriptPrompt,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum SessionScriptPromptClosedReason {
    Answered,
    Cancelled,
    Expired,
    TurnEnded,
    ResponderDisconnected,
    Superseded,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptPromptClosedNotification {
    pub registration_id: String,
    pub prompt_id: String,
    pub reason: SessionScriptPromptClosedReason,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionScriptResyncRequiredNotification {
    pub registration_id: String,
    #[ts(type = "number")]
    pub revision: u64,
}
