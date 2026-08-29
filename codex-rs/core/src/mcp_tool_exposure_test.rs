use std::collections::HashMap;
use std::sync::Arc;

use codex_mcp::ToolInfo;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use pretty_assertions::assert_eq;
use rmcp::model::JsonObject;
use rmcp::model::Meta;
use rmcp::model::Tool;

use super::*;

fn make_mcp_tool(
    server_name: &str,
    tool_name: &str,
    callable_namespace: &str,
    callable_name: &str,
) -> ToolInfo {
    ToolInfo {
        server_name: server_name.to_string(),
        supports_parallel_tool_calls: false,
        server_origin: None,
        callable_name: callable_name.to_string(),
        callable_namespace: callable_namespace.to_string(),
        namespace_description: None,
        tool: Tool::new(
            tool_name.to_string(),
            format!("Test tool: {tool_name}"),
            Arc::new(JsonObject::default()),
        ),
        connector_id: None,
        connector_name: None,
        plugin_display_names: Vec::new(),
    }
}

fn numbered_mcp_tools(count: usize) -> Vec<ToolInfo> {
    (0..count)
        .map(|index| {
            let tool_name = format!("tool_{index}");
            make_mcp_tool("rmcp", &tool_name, "mcp__rmcp", &tool_name)
        })
        .collect()
}

fn expected_runtimes(
    tools: &[ToolInfo],
    exposure: ToolExposure,
) -> HashMap<ToolName, ToolExposure> {
    tools
        .iter()
        .map(|tool| (tool.canonical_tool_name(), exposure))
        .collect()
}

fn runtimes_by_name(runtimes: &[Arc<dyn CoreToolRuntime>]) -> HashMap<ToolName, ToolExposure> {
    runtimes
        .iter()
        .map(|runtime| (runtime.tool_name(), runtime.exposure()))
        .collect()
}

fn with_visibility(mut tool: ToolInfo, visibility: &[&str]) -> ToolInfo {
    tool.tool.meta = Some(Meta(
        serde_json::json!({ "ui": { "visibility": visibility } })
            .as_object()
            .expect("metadata object")
            .clone(),
    ));
    tool
}

#[test]
fn directly_exposes_effective_tool_sets_when_search_is_unavailable() {
    let mcp_tools = numbered_mcp_tools(/*count*/ 2);

    let runtimes = build_mcp_tool_runtimes(&mcp_tools, /*search_tool_enabled*/ false);

    assert_eq!(
        runtimes_by_name(&runtimes),
        expected_runtimes(&mcp_tools, ToolExposure::Direct)
    );
}

#[test]
fn excludes_tools_hidden_from_model_exposure() {
    let visible_tool = make_mcp_tool("rmcp", "visible_tool", "mcp__rmcp", "visible_tool");
    let hidden_tool = with_visibility(
        make_mcp_tool("rmcp", "hidden_tool", "mcp__rmcp", "hidden_tool"),
        &["app"],
    );
    let empty_visibility_tool = with_visibility(
        make_mcp_tool(
            "rmcp",
            "empty_visibility_tool",
            "mcp__rmcp",
            "empty_visibility_tool",
        ),
        &[],
    );
    let mcp_tools = vec![visible_tool.clone(), hidden_tool, empty_visibility_tool];

    let runtimes = build_mcp_tool_runtimes(&mcp_tools, /*search_tool_enabled*/ false);

    assert_eq!(
        runtimes_by_name(&runtimes),
        expected_runtimes(&[visible_tool], ToolExposure::Direct)
    );
}

#[test]
fn defers_effective_tool_sets_when_search_is_available() {
    let mcp_tools = numbered_mcp_tools(/*count*/ 2);

    let runtimes = build_mcp_tool_runtimes(&mcp_tools, /*search_tool_enabled*/ true);

    assert_eq!(
        runtimes_by_name(&runtimes),
        expected_runtimes(&mcp_tools, ToolExposure::Deferred)
    );
}
