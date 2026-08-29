use std::sync::Arc;

use codex_mcp::ToolInfo as McpToolInfo;
use codex_mcp::tool_is_model_visible;
use codex_tools::ToolExposure;
use tracing::instrument;
use tracing::warn;

use crate::tools::handlers::McpHandler;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::override_tool_exposure;

#[instrument(level = "trace", skip_all)]
pub(crate) fn build_mcp_tool_runtimes(
    all_mcp_tools: &[McpToolInfo],
    search_tool_enabled: bool,
) -> Vec<Arc<dyn CoreToolRuntime>> {
    let exposure = if search_tool_enabled {
        ToolExposure::Deferred
    } else {
        ToolExposure::Direct
    };
    all_mcp_tools
        .iter()
        .filter(|tool| tool_is_model_visible(tool))
        .cloned()
        .filter_map(|tool| {
            let tool_name = tool.canonical_tool_name();
            match McpHandler::new(tool) {
                Ok(handler) => {
                    let handler: Arc<dyn CoreToolRuntime> = Arc::new(handler);
                    Some(override_tool_exposure(handler, exposure))
                }
                Err(err) => {
                    warn!("Skipping MCP tool `{tool_name}`: failed to build tool spec: {err}");
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "mcp_tool_exposure_test.rs"]
mod tests;
