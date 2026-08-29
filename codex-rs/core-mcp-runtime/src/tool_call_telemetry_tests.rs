use super::*;

use pretty_assertions::assert_eq;
use tracing::Instrument;
use tracing::Level;
use tracing::field::Empty;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_test::internal::MockWriter;

fn metric_call_tool_result(
    is_error: bool,
    structured_content: Option<serde_json::Value>,
) -> CallToolResult {
    CallToolResult {
        content: Vec::new(),
        structured_content,
        is_error: Some(is_error),
        meta: None,
    }
}

#[test]
fn mcp_call_metric_tags_include_server_name() {
    assert_eq!(
        mcp_call_metric_tags(
            "error",
            "docs server",
            "search docs",
            Some("connector/docs"),
            Some("Docs connector"),
        ),
        vec![
            ("status", "error".to_string()),
            ("server", "docs_server".to_string()),
            ("tool", "search_docs".to_string()),
            ("connector_id", "connector/docs".to_string()),
            ("connector_name", "Docs_connector".to_string()),
        ],
    );
}

#[test]
fn mcp_call_metric_outcome_distinguishes_request_and_tool_errors() {
    assert_eq!(
        mcp_call_metric_outcome(&Ok(metric_call_tool_result(
            /*is_error*/ false, /*structured_content*/ None,
        )),),
        McpCallMetricOutcome {
            status: "ok",
            error_type: None,
            error_code: None,
        }
    );
    assert_eq!(
        mcp_call_metric_outcome(&Ok(metric_call_tool_result(
            /*is_error*/ true,
            Some(serde_json::json!({"error_code": "RATE_LIMITED"})),
        )),),
        McpCallMetricOutcome {
            status: "error",
            error_type: Some(MCP_CALL_ERROR_TYPE_TOOL_RESULT),
            error_code: Some("RATE_LIMITED".to_string()),
        }
    );
    assert_eq!(
        mcp_call_metric_outcome(&Err("connection closed".to_string())),
        McpCallMetricOutcome {
            status: "error",
            error_type: Some(MCP_CALL_ERROR_TYPE_MCP_REQUEST),
            error_code: Some(MCP_CALL_ERROR_CODE_UNKNOWN.to_string()),
        }
    );
}

#[test]
fn mcp_call_metric_outcome_reports_server_tool_error_codes() {
    let result = Ok(metric_call_tool_result(
        /*is_error*/ true,
        Some(serde_json::json!({"error_code": "arbitrary-user-value"})),
    ));

    assert_eq!(
        mcp_call_metric_outcome(&result),
        McpCallMetricOutcome {
            status: "error",
            error_type: Some(MCP_CALL_ERROR_TYPE_TOOL_RESULT),
            error_code: Some("arbitrary-user-value".to_string()),
        }
    );
}

#[test]
fn mcp_call_metric_outcome_bounds_and_sanitizes_error_code() {
    let raw_error_code = format!("BAD CODE {}", "x".repeat(300));
    let result = Ok(metric_call_tool_result(
        /*is_error*/ true,
        Some(serde_json::json!({"error_code": raw_error_code})),
    ));

    assert_eq!(
        mcp_call_metric_outcome(&result),
        McpCallMetricOutcome {
            status: "error",
            error_type: Some(MCP_CALL_ERROR_TYPE_TOOL_RESULT),
            error_code: Some(format!("BAD_CODE_{}", "x".repeat(247))),
        }
    );
}

async fn mcp_result_telemetry_span_logs(meta: Option<serde_json::Value>) -> String {
    let buffer: &'static std::sync::Mutex<Vec<u8>> =
        Box::leak(Box::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_level(true)
        .with_ansi(false)
        .with_max_level(Level::TRACE)
        .with_span_events(FmtSpan::FULL)
        .with_writer(MockWriter::new(buffer))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let result = Ok(CallToolResult {
        content: Vec::new(),
        structured_content: None,
        is_error: None,
        meta,
    });
    let span = tracing::info_span!(
        "mcp.tools.call",
        codex.mcp.target.id = Empty,
        codex.mcp.server_user_flow.triggered = Empty,
    );

    async {
        record_mcp_result_span_telemetry(&tracing::Span::current(), &result);
    }
    .instrument(span)
    .await;

    String::from_utf8(buffer.lock().expect("buffer lock").clone()).expect("utf8 logs")
}

#[tokio::test]
async fn mcp_result_telemetry_records_allowlisted_span_fields() {
    let logs = mcp_result_telemetry_span_logs(Some(serde_json::json!({
        "codex/telemetry": {
            "span": {
                "target_id": "com.apple.reminders",
                "did_trigger_server_user_flow": false,
                "not_promoted_sentinel_key": "not_promoted_sentinel_value",
            },
        },
    })))
    .await;

    assert!(
        logs.contains("codex.mcp.target.id=\"com.apple.reminders\"")
            && logs.contains("codex.mcp.server_user_flow.triggered=false"),
        "missing MCP result telemetry span fields\nlogs:\n{logs}"
    );
    assert!(
        !logs.contains("not_promoted_sentinel_key")
            && !logs.contains("not_promoted_sentinel_value"),
        "unknown MCP result telemetry keys should be ignored\nlogs:\n{logs}"
    );
}

#[tokio::test]
async fn mcp_result_telemetry_ignores_invalid_and_missing_values() {
    let invalid_logs = mcp_result_telemetry_span_logs(Some(serde_json::json!({
        "codex/telemetry": {
            "span": {
                "target_id": 123,
                "did_trigger_server_user_flow": "false",
            },
        },
    })))
    .await;
    assert!(
        !invalid_logs.contains("codex.mcp.target.id=")
            && !invalid_logs.contains("codex.mcp.server_user_flow.triggered="),
        "invalid MCP result telemetry values should be ignored\nlogs:\n{invalid_logs}"
    );

    let missing_logs = mcp_result_telemetry_span_logs(Some(serde_json::json!({
        "codex/telemetry": {},
    })))
    .await;
    assert!(
        !missing_logs.contains("codex.mcp.target.id=")
            && !missing_logs.contains("codex.mcp.server_user_flow.triggered="),
        "missing MCP result telemetry span object should be ignored\nlogs:\n{missing_logs}"
    );

    let no_meta_logs = mcp_result_telemetry_span_logs(/*meta*/ None).await;
    assert!(
        !no_meta_logs.contains("codex.mcp.target.id=")
            && !no_meta_logs.contains("codex.mcp.server_user_flow.triggered="),
        "missing MCP result metadata should be ignored\nlogs:\n{no_meta_logs}"
    );
}

#[tokio::test]
async fn mcp_result_telemetry_truncates_long_target_id() {
    let truncated = "x".repeat(MCP_RESULT_TELEMETRY_TARGET_ID_MAX_CHARS);
    let target_id = format!("{truncated}tail");
    let logs = mcp_result_telemetry_span_logs(Some(serde_json::json!({
        "codex/telemetry": {
            "span": {
                "target_id": target_id,
            },
        },
    })))
    .await;

    assert!(
        logs.contains(&format!("codex.mcp.target.id=\"{truncated}\"")) && !logs.contains("tail"),
        "long MCP result telemetry target_id should be truncated\nlogs:\n{logs}"
    );
}

#[test]
fn truncates_strings_on_char_boundaries() {
    let prefix = "á".repeat(MCP_RESULT_TELEMETRY_TARGET_ID_MAX_CHARS);
    let value = format!("{prefix}tail");
    let truncated = truncate_str_to_char_boundary(&value, MCP_RESULT_TELEMETRY_TARGET_ID_MAX_CHARS);

    assert_eq!(truncated, prefix);
    assert_eq!(
        truncate_str_to_char_boundary("short", MCP_RESULT_TELEMETRY_TARGET_ID_MAX_CHARS),
        "short"
    );
}

#[tokio::test]
async fn mcp_tool_call_span_records_expected_fields() {
    let buffer: &'static std::sync::Mutex<Vec<u8>> =
        Box::leak(Box::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_level(true)
        .with_ansi(false)
        .with_max_level(Level::TRACE)
        .with_span_events(FmtSpan::FULL)
        .with_writer(MockWriter::new(buffer))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    async {}
        .instrument(mcp_tool_call_span(
            &"conversation-123",
            &"session-123",
            "turn-123",
            McpToolCallSpanFields {
                server_name: "rmcp",
                tool_name: "echo",
                call_id: "call-123",
                server_origin: Some("https://example.com:8443/mcp"),
                connector_id: Some("calendar"),
                connector_name: Some("Calendar"),
            },
        ))
        .await;

    let logs = String::from_utf8(buffer.lock().expect("buffer lock").clone()).expect("utf8 logs");
    assert!(
        logs.contains("mcp.tools.call{otel.kind=\"client\"")
            && logs.contains("rpc.system=\"jsonrpc\"")
            && logs.contains("rpc.method=\"tools/call\"")
            && logs.contains("mcp.server.name=\"rmcp\"")
            && logs.contains("mcp.server.origin=\"https://example.com:8443/mcp\"")
            && logs.contains("mcp.transport=\"streamable_http\"")
            && logs.contains("mcp.connector.id=\"calendar\"")
            && logs.contains("mcp.connector.name=\"Calendar\"")
            && logs.contains("tool.name=\"echo\"")
            && logs.contains("tool.call_id=\"call-123\"")
            && logs.contains("server.address=\"example.com\"")
            && logs.contains("server.port=8443")
            && logs.contains("conversation.id=\"conversation-123\"")
            && logs.contains("session.id=\"session-123\"")
            && logs.contains("turn.id=\"turn-123\""),
        "missing MCP tool span fields\nlogs:\n{logs}"
    );
}

#[tokio::test]
async fn mcp_tool_call_span_records_error_type_and_error_code() {
    let buffer: &'static std::sync::Mutex<Vec<u8>> =
        Box::leak(Box::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_level(true)
        .with_ansi(false)
        .with_max_level(Level::TRACE)
        .with_span_events(FmtSpan::FULL)
        .with_writer(MockWriter::new(buffer))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let result = Ok(CallToolResult {
        content: Vec::new(),
        structured_content: Some(serde_json::json!({"error_code": "RATE_LIMITED"})),
        is_error: Some(true),
        meta: None,
    });
    let span = mcp_tool_call_span(
        &"conversation-123",
        &"session-123",
        "turn-123",
        McpToolCallSpanFields {
            server_name: "codex_apps",
            tool_name: "calendar_search",
            call_id: "call-123",
            server_origin: Some("https://chatgpt.com/api/codex/ps/mcp"),
            connector_id: Some("calendar"),
            connector_name: Some("Calendar"),
        },
    );

    async {
        record_mcp_result_span_telemetry(&tracing::Span::current(), &result);
    }
    .instrument(span)
    .await;

    let logs = String::from_utf8(buffer.lock().expect("buffer lock").clone()).expect("utf8 logs");
    assert!(
        logs.contains("error.type=\"tool_result\"")
            && logs.contains("codex.mcp.error.code=\"RATE_LIMITED\""),
        "missing MCP tool error span fields\nlogs:\n{logs}"
    );
}
