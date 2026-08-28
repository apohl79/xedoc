use super::*;

use pretty_assertions::assert_eq;

fn approval_metadata(
    connector_id: Option<&str>,
    connector_name: Option<&str>,
    tool_title: Option<&str>,
) -> McpToolApprovalMetadata {
    McpToolApprovalMetadata {
        annotations: None,
        connector_id: connector_id.map(str::to_string),
        link_id: None,
        connector_name: connector_name.map(str::to_string),
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: tool_title.map(str::to_string),
        tool_description: None,
        mcp_app_resource_uri: None,
        codex_apps_meta: None,
        openai_file_input_optional_fields: None,
    }
}

#[test]
fn mcp_tool_call_item_metadata_only_trusts_codex_apps_identity() {
    let mut metadata = approval_metadata(
        Some("asdk_app_0123456789abcdef0123456789abcdef"),
        Some("Calendar"),
        Some("Create a calendar event"),
    );
    metadata.link_id = Some("link_fedcba9876543210fedcba9876543210".to_string());
    metadata.codex_apps_meta = Some(
        serde_json::json!({
            "resource_uri": "/asdk_app_0123456789abcdef0123456789abcdef/link_fedcba9876543210fedcba9876543210/create_event",
        })
        .as_object()
        .cloned()
        .expect("_codex_apps metadata should be an object"),
    );

    assert_eq!(
        McpToolCallItemMetadata::from_tool_metadata(CODEX_APPS_MCP_SERVER_NAME, Some(&metadata),),
        McpToolCallItemMetadata {
            connector_id: Some("asdk_app_0123456789abcdef0123456789abcdef".to_string()),
            link_id: Some("link_fedcba9876543210fedcba9876543210".to_string()),
            mcp_app_resource_uri: None,
            app_name: Some("Calendar".to_string()),
            action_name: Some("create_event".to_string()),
            plugin_id: None,
        }
    );
    assert_eq!(
        McpToolCallItemMetadata::from_tool_metadata("custom_server", Some(&metadata)),
        McpToolCallItemMetadata {
            connector_id: None,
            link_id: None,
            mcp_app_resource_uri: None,
            app_name: None,
            action_name: None,
            plugin_id: None,
        }
    );
}
