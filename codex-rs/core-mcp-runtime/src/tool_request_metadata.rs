//! MCP tool request metadata construction.

use codex_core_approval_policy::McpToolApprovalMetadata;
use serde_json::Value;

const MCP_TOOL_PLUGIN_ID_META_KEY: &str = "plugin_id";
const MCP_TOOL_THREAD_ID_META_KEY: &str = "threadId";

/// Merges transport metadata for an MCP tool request.
#[doc(hidden)]
pub fn build_mcp_tool_call_request_meta(
    turn_metadata_header: &str,
    turn_metadata: Option<Value>,
    metadata: Option<&McpToolApprovalMetadata>,
) -> Option<Value> {
    let mut request_meta = serde_json::Map::new();

    if let Some(turn_metadata) = turn_metadata {
        request_meta.insert(turn_metadata_header.to_string(), turn_metadata);
    }

    if let Some(plugin_id) = metadata.and_then(|metadata| metadata.plugin_id.as_ref()) {
        request_meta.insert(
            MCP_TOOL_PLUGIN_ID_META_KEY.to_string(),
            Value::String(plugin_id.clone()),
        );
    }

    (!request_meta.is_empty()).then_some(Value::Object(request_meta))
}

/// Adds the live thread identifier to an MCP request metadata object.
#[doc(hidden)]
pub fn with_mcp_tool_call_thread_id_meta(meta: Option<Value>, thread_id: &str) -> Option<Value> {
    match meta {
        Some(Value::Object(mut map)) => {
            map.insert(
                MCP_TOOL_THREAD_ID_META_KEY.to_string(),
                Value::String(thread_id.to_string()),
            );
            Some(Value::Object(map))
        }
        None => {
            let mut map = serde_json::Map::new();
            map.insert(
                MCP_TOOL_THREAD_ID_META_KEY.to_string(),
                Value::String(thread_id.to_string()),
            );
            Some(Value::Object(map))
        }
        other => other,
    }
}

#[cfg(test)]
#[path = "tool_request_metadata_tests.rs"]
mod tests;
