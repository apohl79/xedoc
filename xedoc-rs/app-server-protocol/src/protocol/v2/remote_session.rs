use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use ts_rs::TS;

/// Root-thread scoped projection of a remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionSummary {
    pub remote_session_id: String,
    pub host_id: String,
    pub remote_thread_id: String,
    pub workspace_id: Option<String>,
    pub host_name: Option<String>,
    pub host_role: Option<RemoteSessionHostRole>,
    pub status: RemoteSessionStatus,
    pub active_turn_id: Option<String>,
    pub activity_summary: Option<String>,
    pub output_cursor: Option<String>,
}

/// Role advertised by a remote host.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum RemoteSessionHostRole {
    Coordinator,
    Managed,
}

/// Lifecycle state reported for a remote session.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum RemoteSessionStatus {
    Idle,
    Running,
    Completed,
    Failed,
    Cancelled,
    Expired,
    Detached,
}

/// Lists locally registered remote sessions for one root thread.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionListParams {
    pub thread_id: String,
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
    #[ts(optional = nullable)]
    pub limit: Option<u32>,
}

/// Registered remote sessions belonging to one root thread.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionListResponse {
    pub data: Vec<RemoteSessionSummary>,
    pub next_cursor: Option<String>,
}

/// Reads one locally registered remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionReadParams {
    pub thread_id: String,
    pub remote_session_id: String,
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
    #[ts(optional = nullable)]
    pub limit: Option<u32>,
}

/// Bounded output and summary for a remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionReadResponse {
    pub remote_session: RemoteSessionSummary,
    pub output: String,
}

/// Attaches the caller's root thread to a registered remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionAttachParams {
    pub thread_id: String,
    pub remote_session_id: String,
}

/// Sends input or steering to a registered remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionInputParams {
    pub thread_id: String,
    pub remote_session_id: String,
    pub message: String,
    #[ts(optional = nullable)]
    pub expected_turn_id: Option<String>,
}

/// Cancels the active turn of a registered remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionCancelParams {
    pub thread_id: String,
    pub remote_session_id: String,
    #[ts(optional = nullable)]
    pub expected_turn_id: Option<String>,
}

/// Detaches the caller's root thread from a registered remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionDetachParams {
    pub thread_id: String,
    pub remote_session_id: String,
}

/// Result of attaching to a remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionAttachResponse {
    pub remote_session: RemoteSessionSummary,
}

/// Result of sending input to a remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionInputResponse {
    pub remote_session: RemoteSessionSummary,
    pub output_delta: Option<String>,
}

/// Result of cancelling a remote session turn.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionCancelResponse {
    pub remote_session: RemoteSessionSummary,
}

/// Result of detaching a remote session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionDetachResponse {
    pub remote_session: RemoteSessionSummary,
}

/// Registers a remote identity from the host-managed remote-agent extension.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionRegisterParams {
    pub registration_id: String,
    pub host_id: String,
    pub remote_thread_id: String,
    #[ts(optional = nullable)]
    pub host_name: Option<String>,
}

/// Returns the opaque local session ID for a registered remote identity.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionRegisterResponse {
    pub remote_session: RemoteSessionSummary,
}

/// Updates a remote session from the host-managed remote-agent extension.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionUpdateParams {
    pub registration_id: String,
    pub remote_session_id: String,
    pub status: RemoteSessionStatus,
    #[ts(optional = nullable)]
    pub activity_summary: Option<String>,
    #[ts(optional = nullable)]
    pub output_delta: Option<String>,
    #[ts(optional = nullable)]
    pub output_cursor: Option<String>,
    #[ts(optional = nullable)]
    pub remote_turn_id: Option<String>,
    #[ts(optional = nullable)]
    pub workspace_id: Option<String>,
    #[ts(optional = nullable)]
    pub host_name: Option<String>,
    #[ts(optional = nullable)]
    pub host_role: Option<RemoteSessionHostRole>,
}

/// Empty successful response for a remote-session update.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionUpdateResponse {}

/// Requests one broker operation from the remote-agent extension.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionControlParams {
    pub action: RemoteSessionControlAction,
    pub host_id: String,
    pub thread_id: String,
    pub message: Option<String>,
    pub expected_turn_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// Fixed broker actions permitted through the remote-agent extension.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum RemoteSessionControlAction {
    Attach,
    Read,
    Send,
    Steer,
    Cancel,
    Detach,
}

/// Broker result returned by the remote-agent extension.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionControlResponse {
    pub success: bool,
    pub result: JsonValue,
}

/// Signals a change to a root-thread-scoped remote session projection.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RemoteSessionUpdatedNotification {
    pub thread_id: String,
    pub remote_session: RemoteSessionSummary,
    pub output_delta: Option<String>,
}
