use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Reads the approved session-extension commands for one loaded root thread.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionExtensionListParams {
    pub thread_id: String,
}

/// One approved plugin extension slash command.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionExtensionCommand {
    pub extension_id: String,
    pub name: String,
    pub description: String,
}

/// Approved slash commands for one loaded root thread.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionExtensionListResponse {
    pub commands: Vec<SessionExtensionCommand>,
}

/// Invokes one approved session-extension slash command.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionExtensionCommandInvokeParams {
    pub thread_id: String,
    pub extension_id: String,
    pub command: String,
    pub arguments: Vec<String>,
}

/// Acknowledges that an approved session-extension command was dispatched.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionExtensionCommandInvokeResponse {}

/// Announces the replacement command set after approval, activation, or disabling.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SessionExtensionCommandsUpdatedNotification {
    pub thread_id: String,
    pub commands: Vec<SessionExtensionCommand>,
}
