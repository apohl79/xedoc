//! MCP tool-call item metadata projection.

use codex_core_approval_policy::McpToolApprovalMetadata;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;

const MCP_TOOL_RESOURCE_URI_META_KEY: &str = "resource_uri";

/// Identity metadata attached to one MCP tool-call lifecycle item.
#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct McpToolCallItemMetadata {
    /// Trusted connector identifier, when available.
    pub connector_id: Option<String>,
    /// Trusted connector link identifier, when available.
    pub link_id: Option<String>,
    /// MCP app resource URI exposed to the client.
    pub mcp_app_resource_uri: Option<String>,
    /// Trusted connector display name, when available.
    pub app_name: Option<String>,
    /// Action name derived from the trusted resource URI.
    pub action_name: Option<String>,
    /// Plugin identifier associated with the MCP server.
    pub plugin_id: Option<String>,
}

impl McpToolCallItemMetadata {
    /// Projects approved MCP tool metadata into lifecycle-item identity fields.
    #[doc(hidden)]
    pub fn from_tool_metadata(server: &str, metadata: Option<&McpToolApprovalMetadata>) -> Self {
        let trusted_mcp_app_metadata = if server == CODEX_APPS_MCP_SERVER_NAME {
            metadata
        } else {
            None
        };
        Self {
            connector_id: trusted_mcp_app_metadata
                .and_then(|metadata| metadata.connector_id.clone()),
            link_id: trusted_mcp_app_metadata.and_then(|metadata| metadata.link_id.clone()),
            mcp_app_resource_uri: metadata
                .and_then(|metadata| metadata.mcp_app_resource_uri.clone()),
            app_name: trusted_mcp_app_metadata.and_then(|metadata| metadata.connector_name.clone()),
            action_name: trusted_mcp_app_metadata
                .and_then(|metadata| metadata.codex_apps_meta.as_ref())
                .and_then(|meta| meta.get(MCP_TOOL_RESOURCE_URI_META_KEY))
                .and_then(serde_json::Value::as_str)
                .and_then(|resource_uri| resource_uri.trim_matches('/').rsplit('/').next())
                .filter(|action_name| !action_name.is_empty())
                .map(str::to_string),
            plugin_id: metadata.and_then(|metadata| metadata.plugin_id.clone()),
        }
    }
}

#[cfg(test)]
#[path = "tool_item_metadata_tests.rs"]
mod tests;
