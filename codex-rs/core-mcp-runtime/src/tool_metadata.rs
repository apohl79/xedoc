//! MCP tool metadata normalization shared by runtime consumers.

use std::collections::HashMap;

use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
use serde_json::Map;
use serde_json::Value;

const MCP_TOOL_OPENAI_OUTPUT_TEMPLATE_META_KEY: &str = "openai/outputTemplate";
const MCP_TOOL_UI_RESOURCE_URI_META_KEY: &str = "ui/resourceUri";

/// Returns file parameters only for the trusted Codex Apps server.
#[doc(hidden)]
pub fn openai_file_input_optional_fields_for_server(
    server: &str,
    openai_file_input_optional_fields: &HashMap<String, Vec<String>>,
) -> Option<HashMap<String, Vec<String>>> {
    (server == CODEX_APPS_MCP_SERVER_NAME)
        .then(|| openai_file_input_optional_fields.clone())
        .filter(|params| !params.is_empty())
}

/// Selects the supported resource URI metadata fields for a rendered MCP app.
#[doc(hidden)]
pub fn get_mcp_app_resource_uri(meta: Option<&Map<String, Value>>) -> Option<String> {
    meta.and_then(|meta| {
        meta.get("ui")
            .and_then(Value::as_object)
            .and_then(|ui| ui.get("resourceUri"))
            .and_then(Value::as_str)
            .or_else(|| {
                meta.get(MCP_TOOL_UI_RESOURCE_URI_META_KEY)
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                meta.get(MCP_TOOL_OPENAI_OUTPUT_TEMPLATE_META_KEY)
                    .and_then(Value::as_str)
            })
            .map(str::to_string)
    })
}

#[cfg(test)]
#[path = "tool_metadata_tests.rs"]
mod tests;
