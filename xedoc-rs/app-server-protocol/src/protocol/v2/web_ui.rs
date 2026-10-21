//! Capability URLs for a browser attached to the native session controller.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Opens a loopback browser view of an active TUI session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadWebUiOpenParams {
    pub thread_id: String,
}

/// Local capability URLs for the browser and its owning TUI.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadWebUiOpenResponse {
    pub url: String,
    pub bridge_url: String,
}
