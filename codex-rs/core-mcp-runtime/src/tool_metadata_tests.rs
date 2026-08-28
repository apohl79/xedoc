use super::*;

use pretty_assertions::assert_eq;

#[test]
fn mcp_app_resource_uri_reads_known_tool_meta_keys() {
    let nested = serde_json::json!({
        "ui": {
            "resourceUri": "ui://widget/nested.html",
        },
    });
    assert_eq!(
        get_mcp_app_resource_uri(nested.as_object()),
        Some("ui://widget/nested.html".to_string())
    );

    let flat = serde_json::json!({
        "ui/resourceUri": "ui://widget/flat.html",
    });
    assert_eq!(
        get_mcp_app_resource_uri(flat.as_object()),
        Some("ui://widget/flat.html".to_string())
    );

    let output_template = serde_json::json!({
        "openai/outputTemplate": "ui://widget/output-template.html",
    });
    assert_eq!(
        get_mcp_app_resource_uri(output_template.as_object()),
        Some("ui://widget/output-template.html".to_string())
    );
}

#[test]
fn openai_file_params_are_only_honored_for_codex_apps() {
    let params = HashMap::from([("file".to_string(), Vec::new())]);

    assert_eq!(
        openai_file_input_optional_fields_for_server(CODEX_APPS_MCP_SERVER_NAME, &params),
        Some(params.clone())
    );
    assert_eq!(
        openai_file_input_optional_fields_for_server("minimaltest", &params),
        None
    );
}
