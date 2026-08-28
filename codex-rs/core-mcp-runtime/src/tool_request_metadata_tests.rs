use super::*;

use pretty_assertions::assert_eq;

const TURN_METADATA_HEADER: &str = "x-codex-turn-metadata";

fn turn_metadata() -> Value {
    serde_json::json!({
        "model": "gpt-test",
        "reasoning_effort": "medium",
        "turn_started_at_unix_ms": 1_700_000_000_123_i64,
    })
}

fn approval_metadata() -> McpToolApprovalMetadata {
    McpToolApprovalMetadata {
        annotations: None,
        connector_id: Some("calendar".to_string()),
        link_id: None,
        connector_name: Some("Calendar".to_string()),
        connector_description: Some("Manage events".to_string()),
        connected_account_email: None,
        plugin_id: None,
        tool_title: Some("Create Event".to_string()),
        tool_description: Some("Create a calendar event.".to_string()),
        mcp_app_resource_uri: None,
        codex_apps_meta: None,
        openai_file_input_optional_fields: None,
    }
}

#[test]
fn custom_mcp_tool_request_meta_includes_turn_metadata() {
    let turn_metadata = turn_metadata();

    assert_eq!(
        build_mcp_tool_call_request_meta(
            TURN_METADATA_HEADER,
            Some(turn_metadata.clone()),
            "custom_server",
            "call-custom",
            /*metadata*/ None,
        ),
        Some(serde_json::json!({
            TURN_METADATA_HEADER: turn_metadata,
        }))
    );
}

#[test]
fn codex_apps_tool_request_meta_merges_existing_app_metadata() {
    let turn_metadata = turn_metadata();
    let mut metadata = approval_metadata();
    metadata.codex_apps_meta = Some(
        serde_json::json!({
            "resource_uri": "connector://calendar/tools/calendar_create_event",
            "contains_mcp_source": true,
            "connector_id": "calendar",
        })
        .as_object()
        .cloned()
        .expect("_codex_apps metadata should be an object"),
    );

    assert_eq!(
        build_mcp_tool_call_request_meta(
            TURN_METADATA_HEADER,
            Some(turn_metadata.clone()),
            CODEX_APPS_MCP_SERVER_NAME,
            "call_abc123xyz789",
            Some(&metadata),
        ),
        Some(serde_json::json!({
            TURN_METADATA_HEADER: turn_metadata,
            MCP_TOOL_CODEX_APPS_META_KEY: {
                "call_id": "call_abc123xyz789",
                "resource_uri": "connector://calendar/tools/calendar_create_event",
                "contains_mcp_source": true,
                "connector_id": "calendar",
            },
        }))
    );
}

#[test]
fn codex_apps_tool_request_meta_includes_call_id_without_app_metadata() {
    let turn_metadata = turn_metadata();

    assert_eq!(
        build_mcp_tool_call_request_meta(
            TURN_METADATA_HEADER,
            Some(turn_metadata.clone()),
            CODEX_APPS_MCP_SERVER_NAME,
            "call_abc123xyz789",
            /*metadata*/ None,
        ),
        Some(serde_json::json!({
            TURN_METADATA_HEADER: turn_metadata,
            MCP_TOOL_CODEX_APPS_META_KEY: {
                "call_id": "call_abc123xyz789",
            },
        }))
    );
}

#[test]
fn plugin_mcp_tool_request_meta_includes_plugin_id() {
    let turn_metadata = turn_metadata();
    let mut metadata = approval_metadata();
    metadata.plugin_id = Some("sample@test".to_string());

    assert_eq!(
        build_mcp_tool_call_request_meta(
            TURN_METADATA_HEADER,
            Some(turn_metadata.clone()),
            "sample",
            "call-plugin",
            Some(&metadata),
        ),
        Some(serde_json::json!({
            TURN_METADATA_HEADER: turn_metadata,
            MCP_TOOL_PLUGIN_ID_META_KEY: "sample@test",
        }))
    );
}

#[test]
fn mcp_tool_call_thread_id_meta_is_added_to_request_meta() {
    assert_eq!(
        with_mcp_tool_call_thread_id_meta(
            Some(serde_json::json!({
                "source": "test-client",
                "threadId": "stale-thread",
            })),
            "thread-live",
        ),
        Some(serde_json::json!({
            "source": "test-client",
            "threadId": "thread-live",
        }))
    );

    assert_eq!(
        with_mcp_tool_call_thread_id_meta(/*meta*/ None, "thread-live"),
        Some(serde_json::json!({
            "threadId": "thread-live",
        }))
    );

    assert_eq!(
        with_mcp_tool_call_thread_id_meta(Some(serde_json::json!("invalid-meta")), "thread-live"),
        Some(serde_json::json!("invalid-meta"))
    );
}
