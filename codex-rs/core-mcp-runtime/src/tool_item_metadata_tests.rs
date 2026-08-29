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
    }
}

#[test]
fn mcp_tool_call_item_metadata_projects_resource_uri_and_plugin_id() {
    let mut metadata = approval_metadata(Some("calendar"), Some("Calendar"), Some("Create event"));
    metadata.mcp_app_resource_uri = Some("ui://widget/create.html".to_string());
    metadata.plugin_id = Some("sample@openai-curated".to_string());

    assert_eq!(
        McpToolCallItemMetadata::from_tool_metadata(Some(&metadata)),
        McpToolCallItemMetadata {
            connector_id: None,
            link_id: None,
            mcp_app_resource_uri: Some("ui://widget/create.html".to_string()),
            app_name: None,
            action_name: None,
            plugin_id: Some("sample@openai-curated".to_string()),
        }
    );
}
