//! Metrics and tracing helpers for MCP tool calls.

use std::fmt::Display;
use std::time::Duration;

use codex_mcp::MCP_TOOL_CODEX_APPS_META_KEY;
use codex_otel::SessionTelemetry;
use codex_otel::sanitize_metric_tag_value;
use codex_protocol::mcp::CallToolResult;
use serde_json::Value as JsonValue;
use tracing::Span;
use tracing::field::Empty;
use url::Url;

const MCP_CALL_COUNT_METRIC: &str = "codex.mcp.call";
const MCP_CALL_DURATION_METRIC: &str = "codex.mcp.call.duration_ms";
const MCP_CALL_ERROR_COUNT_METRIC: &str = "codex.mcp.call.error";
const MCP_CALL_ERROR_TYPE_MCP_REQUEST: &str = "mcp_request";
const MCP_CALL_ERROR_TYPE_TOOL_RESULT: &str = "tool_result";
const MCP_CALL_ERROR_CODE_UNKNOWN: &str = "unknown";
const MCP_CALL_ERROR_CODE_MAX_CHARS: usize = 256;
const MCP_CALL_ERROR_TYPE_SPAN_ATTR: &str = "error.type";
const MCP_CALL_ERROR_CODE_SPAN_ATTR: &str = "codex.mcp.error.code";
const MCP_RESULT_TELEMETRY_META_KEY: &str = "codex/telemetry";
const MCP_RESULT_TELEMETRY_SPAN_KEY: &str = "span";
const MCP_RESULT_TELEMETRY_TARGET_ID_KEY: &str = "target_id";
const MCP_RESULT_TELEMETRY_DID_TRIGGER_SERVER_USER_FLOW_KEY: &str = "did_trigger_server_user_flow";
const MCP_RESULT_TELEMETRY_TARGET_ID_SPAN_ATTR: &str = "codex.mcp.target.id";
const MCP_RESULT_TELEMETRY_SERVER_USER_FLOW_SPAN_ATTR: &str =
    "codex.mcp.server_user_flow.triggered";
const MCP_RESULT_TELEMETRY_TARGET_ID_MAX_CHARS: usize = 256;

/// Classifies the outcome of an MCP tool call for telemetry.
#[derive(Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct McpCallMetricOutcome {
    status: &'static str,
    error_type: Option<&'static str>,
    error_code: Option<String>,
}

/// Attributes used to construct one MCP tool-call tracing span.
#[doc(hidden)]
pub struct McpToolCallSpanFields<'a> {
    /// MCP server name.
    pub server_name: &'a str,
    /// MCP tool name.
    pub tool_name: &'a str,
    /// Model tool-call identifier.
    pub call_id: &'a str,
    /// Server origin, when available.
    pub server_origin: Option<&'a str>,
    /// Connector identifier, when available.
    pub connector_id: Option<&'a str>,
    /// Connector display name, when available.
    pub connector_name: Option<&'a str>,
}

impl McpCallMetricOutcome {
    /// Creates an outcome with a successful or generic status.
    #[doc(hidden)]
    pub fn from_status(status: &'static str) -> Self {
        Self {
            status,
            error_type: None,
            error_code: None,
        }
    }
}

/// Records metrics for one MCP tool call.
#[doc(hidden)]
pub fn emit_mcp_call_metrics(
    session_telemetry: &SessionTelemetry,
    outcome: &McpCallMetricOutcome,
    server_name: &str,
    tool_name: &str,
    connector_id: Option<&str>,
    connector_name: Option<&str>,
    duration: Option<Duration>,
) {
    let tags = mcp_call_metric_tags(
        outcome.status,
        server_name,
        tool_name,
        connector_id,
        connector_name,
    );
    let tag_refs: Vec<(&str, &str)> = tags
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    session_telemetry.counter(MCP_CALL_COUNT_METRIC, /*inc*/ 1, &tag_refs);
    if let Some(duration) = duration {
        session_telemetry.record_duration(MCP_CALL_DURATION_METRIC, duration, &tag_refs);
    }

    let (Some(error_type), Some(error_code)) = (outcome.error_type, outcome.error_code.as_deref())
    else {
        return;
    };
    let mut error_tags = tags;
    error_tags.push(("error_type", sanitize_metric_tag_value(error_type)));
    error_tags.push(("error_code", error_code.to_string()));
    let error_tag_refs: Vec<(&str, &str)> = error_tags
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    session_telemetry.counter(MCP_CALL_ERROR_COUNT_METRIC, /*inc*/ 1, &error_tag_refs);
}

fn mcp_call_metric_tags(
    status: &str,
    server_name: &str,
    tool_name: &str,
    connector_id: Option<&str>,
    connector_name: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut tags = vec![
        ("status", sanitize_metric_tag_value(status)),
        ("server", sanitize_metric_tag_value(server_name)),
        ("tool", sanitize_metric_tag_value(tool_name)),
    ];
    if let Some(connector_id) = connector_id.filter(|connector_id| !connector_id.is_empty()) {
        tags.push(("connector_id", sanitize_metric_tag_value(connector_id)));
    }
    if let Some(connector_name) = connector_name.filter(|connector_name| !connector_name.is_empty())
    {
        tags.push(("connector_name", sanitize_metric_tag_value(connector_name)));
    }
    tags
}

/// Derives the metric outcome from a completed MCP call.
#[doc(hidden)]
pub fn mcp_call_metric_outcome(result: &Result<CallToolResult, String>) -> McpCallMetricOutcome {
    match result {
        Ok(result) if result.is_error.unwrap_or(false) => {
            let error_code = result
                .structured_content
                .as_ref()
                .and_then(JsonValue::as_object)
                .and_then(|structured_content| structured_content.get("error_code"))
                .and_then(JsonValue::as_str)
                .filter(|error_code| !error_code.is_empty())
                .or_else(|| {
                    result
                        .meta
                        .as_ref()
                        .and_then(JsonValue::as_object)
                        .and_then(|meta| meta.get(MCP_TOOL_CODEX_APPS_META_KEY))
                        .and_then(JsonValue::as_object)
                        .and_then(|codex_apps| codex_apps.get("connector_auth_failure"))
                        .and_then(JsonValue::as_object)
                        .filter(|auth_failure| {
                            auth_failure
                                .get("is_auth_failure")
                                .and_then(JsonValue::as_bool)
                                == Some(true)
                        })
                        .and_then(|auth_failure| auth_failure.get("error_code"))
                        .and_then(JsonValue::as_str)
                        .filter(|error_code| !error_code.is_empty())
                });
            let error_code: String = error_code
                .unwrap_or(MCP_CALL_ERROR_CODE_UNKNOWN)
                .chars()
                .take(MCP_CALL_ERROR_CODE_MAX_CHARS)
                .collect();
            McpCallMetricOutcome {
                status: "error",
                error_type: Some(MCP_CALL_ERROR_TYPE_TOOL_RESULT),
                error_code: Some(sanitize_metric_tag_value(&error_code)),
            }
        }
        Ok(_) => McpCallMetricOutcome::from_status("ok"),
        Err(_) => McpCallMetricOutcome {
            status: "error",
            error_type: Some(MCP_CALL_ERROR_TYPE_MCP_REQUEST),
            error_code: Some(MCP_CALL_ERROR_CODE_UNKNOWN.to_string()),
        },
    }
}

/// Adds MCP-call error fields to the active tracing span.
#[doc(hidden)]
pub fn record_mcp_call_outcome_span_telemetry(
    span: &Span,
    result: &Result<CallToolResult, String>,
) {
    let outcome = mcp_call_metric_outcome(result);
    let (Some(error_type), Some(error_code)) = (outcome.error_type, outcome.error_code) else {
        return;
    };
    span.record(MCP_CALL_ERROR_TYPE_SPAN_ATTR, error_type);
    span.record(MCP_CALL_ERROR_CODE_SPAN_ATTR, error_code);
}

/// Records MCP result telemetry fields on a tool-call tracing span.
#[doc(hidden)]
pub fn record_mcp_result_span_telemetry(span: &Span, result: &Result<CallToolResult, String>) {
    record_mcp_call_outcome_span_telemetry(span, result);

    let Some(span_telemetry) = result
        .as_ref()
        .ok()
        .and_then(|result| result.meta.as_ref())
        .and_then(JsonValue::as_object)
        .and_then(|meta| meta.get(MCP_RESULT_TELEMETRY_META_KEY))
        .and_then(JsonValue::as_object)
        .and_then(|telemetry| telemetry.get(MCP_RESULT_TELEMETRY_SPAN_KEY))
        .and_then(JsonValue::as_object)
    else {
        return;
    };

    if let Some(target_id) = span_telemetry
        .get(MCP_RESULT_TELEMETRY_TARGET_ID_KEY)
        .and_then(JsonValue::as_str)
        .filter(|target_id| !target_id.is_empty())
    {
        span.record(
            MCP_RESULT_TELEMETRY_TARGET_ID_SPAN_ATTR,
            truncate_str_to_char_boundary(target_id, MCP_RESULT_TELEMETRY_TARGET_ID_MAX_CHARS),
        );
    }

    if let Some(did_trigger_server_user_flow) = span_telemetry
        .get(MCP_RESULT_TELEMETRY_DID_TRIGGER_SERVER_USER_FLOW_KEY)
        .and_then(JsonValue::as_bool)
    {
        span.record(
            MCP_RESULT_TELEMETRY_SERVER_USER_FLOW_SPAN_ATTR,
            did_trigger_server_user_flow,
        );
    }
}

/// Starts a tracing span for one MCP tool call.
#[doc(hidden)]
pub fn mcp_tool_call_span(
    conversation_id: &impl Display,
    session_id: &impl Display,
    turn_id: &str,
    fields: McpToolCallSpanFields<'_>,
) -> Span {
    let transport = match fields.server_origin {
        Some("stdio") => "stdio",
        Some("in_process") => "in_process",
        Some(_) => "streamable_http",
        None => "",
    };
    let span = tracing::info_span!(
        "mcp.tools.call",
        otel.kind = "client",
        rpc.system = "jsonrpc",
        rpc.method = "tools/call",
        mcp.server.name = fields.server_name,
        mcp.server.origin = fields.server_origin.unwrap_or(""),
        mcp.transport = transport,
        mcp.connector.id = fields.connector_id.unwrap_or(""),
        mcp.connector.name = fields.connector_name.unwrap_or(""),
        tool.name = fields.tool_name,
        tool.call_id = fields.call_id,
        conversation.id = %conversation_id,
        session.id = %session_id,
        turn.id = turn_id,
        server.address = Empty,
        server.port = Empty,
        codex.mcp.target.id = Empty,
        codex.mcp.server_user_flow.triggered = Empty,
        error.type = Empty,
        codex.mcp.error.code = Empty,
    );
    record_server_fields(&span, fields.server_origin);
    span
}

fn record_server_fields(span: &Span, url: Option<&str>) {
    let Some(url) = url else {
        return;
    };
    let Ok(parsed) = Url::parse(url) else {
        return;
    };
    if let Some(host) = parsed.host_str() {
        span.record("server.address", host);
    }
    if let Some(port) = parsed.port_or_known_default() {
        span.record("server.port", port as i64);
    }
}

fn truncate_str_to_char_boundary(value: &str, max_chars: usize) -> &str {
    match value.char_indices().nth(max_chars) {
        Some((index, _)) => &value[..index],
        None => value,
    }
}

#[cfg(test)]
#[path = "tool_call_telemetry_tests.rs"]
mod tests;
