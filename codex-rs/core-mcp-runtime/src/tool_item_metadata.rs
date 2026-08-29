//! MCP tool-call item metadata projection.

use codex_core_approval_policy::McpToolApprovalMetadata;

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
    pub fn from_tool_metadata(metadata: Option<&McpToolApprovalMetadata>) -> Self {
        Self {
            connector_id: None,
            link_id: None,
            mcp_app_resource_uri: metadata
                .and_then(|metadata| metadata.mcp_app_resource_uri.clone()),
            app_name: None,
            action_name: None,
            plugin_id: metadata.and_then(|metadata| metadata.plugin_id.clone()),
        }
    }
}

#[cfg(test)]
#[path = "tool_item_metadata_tests.rs"]
mod tests;
