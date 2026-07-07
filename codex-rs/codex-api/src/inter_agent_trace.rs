//! Env-gated request and stream trace summaries.

use codex_client::RequestBody;
use http::Method;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const TRACE_ENV: &str = "CODEX_INTER_AGENT_TRACE";
const MAX_ENCRYPTED_VALUES: usize = 8;

pub(crate) fn log_request(method: &Method, path: &str, body: Option<&RequestBody>) {
    if !trace_enabled() {
        return;
    }

    let Some(body_summary) = body.and_then(summarize_request_body) else {
        append_trace(json!({
            "event": "request",
            "method": method.as_str(),
            "path": path,
        }));
        return;
    };

    append_trace(json!({
        "event": "request",
        "method": method.as_str(),
        "path": path,
        "body": body_summary,
    }));
}

pub(crate) fn log_websocket_request_text(request_text: &str) {
    if !trace_enabled() {
        return;
    }

    let body = serde_json::from_str::<Value>(request_text)
        .map(|value| summarize_json_body(&value))
        .unwrap_or_else(|_| {
            json!({
                "unparsed_len": request_text.len(),
            })
        });

    append_trace(json!({
        "event": "websocket_request",
        "path": "responses",
        "body": body,
    }));
}

pub(crate) fn log_stream_event(transport: &str, data: &str) {
    if !trace_enabled() {
        return;
    }

    let event = serde_json::from_str::<Value>(data)
        .map(|value| summarize_stream_value(transport, &value))
        .unwrap_or_else(|_| {
            json!({
                "event": "stream_event",
                "transport": transport,
                "unparsed_len": data.len(),
            })
        });

    append_trace(event);
}

fn summarize_request_body(body: &RequestBody) -> Option<Value> {
    match body {
        RequestBody::Json(value) => Some(summarize_json_body(value)),
        RequestBody::EncodedJson(body) => serde_json::from_slice::<Value>(body.as_bytes())
            .ok()
            .map(|value| summarize_json_body(&value)),
        RequestBody::Raw(raw) => Some(json!({ "raw_len": raw.len() })),
    }
}

fn summarize_json_body(value: &Value) -> Value {
    let mut summary = Map::new();
    if let Some(object) = value.as_object() {
        summary.insert(
            "keys".to_string(),
            Value::Array(object.keys().cloned().map(Value::String).collect()),
        );
    }

    let agent_messages = agent_message_summaries(value);
    if !agent_messages.is_empty() {
        summary.insert("agent_messages".to_string(), Value::Array(agent_messages));
    }

    let encrypted_values = encrypted_content_summaries(value);
    if !encrypted_values.is_empty() {
        summary.insert(
            "encrypted_content".to_string(),
            Value::Array(encrypted_values),
        );
    }

    if let Some(input_len) = value.get("input").and_then(Value::as_array).map(Vec::len) {
        summary.insert("input_len".to_string(), json!(input_len));
    }

    Value::Object(summary)
}

fn summarize_stream_value(transport: &str, value: &Value) -> Value {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut summary = Map::from_iter([
        (
            "event".to_string(),
            Value::String("stream_event".to_string()),
        ),
        (
            "transport".to_string(),
            Value::String(transport.to_string()),
        ),
        ("type".to_string(), Value::String(kind.to_string())),
    ]);

    if let Some(item) = value.get("item")
        && let Some(item_summary) = summarize_stream_item(item)
    {
        summary.insert("item".to_string(), item_summary);
    }

    if let Some(delta) = value.get("delta").and_then(Value::as_str) {
        summary.insert("delta".to_string(), summarize_string(delta));
    }

    if let Some(response_id) = value
        .get("response")
        .and_then(|response| response.get("id"))
        .and_then(Value::as_str)
    {
        summary.insert(
            "response_id".to_string(),
            Value::String(response_id.to_string()),
        );
    }

    Value::Object(summary)
}

fn summarize_stream_item(item: &Value) -> Option<Value> {
    let item_type = item.get("type").and_then(Value::as_str)?;
    let mut summary = Map::from_iter([("type".to_string(), Value::String(item_type.to_string()))]);

    for key in ["id", "call_id", "name", "author", "recipient"] {
        if let Some(value) = item.get(key).and_then(Value::as_str) {
            summary.insert(key.to_string(), Value::String(value.to_string()));
        }
    }

    if let Some(arguments) = item.get("arguments") {
        summary.insert("arguments".to_string(), summarize_arguments(arguments));
    }

    if let Some(content) = item.get("content") {
        let encrypted_values = encrypted_content_summaries(content);
        if !encrypted_values.is_empty() {
            summary.insert(
                "encrypted_content".to_string(),
                Value::Array(encrypted_values),
            );
        }
    }

    Some(Value::Object(summary))
}

fn summarize_arguments(arguments: &Value) -> Value {
    match arguments {
        Value::String(arguments) => serde_json::from_str::<Value>(arguments)
            .map(|value| summarize_tool_arguments(&value))
            .unwrap_or_else(|_| summarize_string(arguments)),
        value => summarize_tool_arguments(value),
    }
}

fn summarize_tool_arguments(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return summarize_json_value(value);
    };
    let mut summary = Map::new();
    for (key, value) in object {
        summary.insert(key.clone(), summarize_json_value(value));
    }
    Value::Object(summary)
}

fn summarize_json_value(value: &Value) -> Value {
    match value {
        Value::String(value) => summarize_string(value),
        Value::Number(value) => Value::Number(value.clone()),
        Value::Bool(value) => Value::Bool(*value),
        Value::Null => Value::Null,
        Value::Array(items) => json!({ "array_len": items.len() }),
        Value::Object(object) => {
            json!({ "object_keys": object.keys().cloned().collect::<Vec<_>>() })
        }
    }
}

fn agent_message_summaries(value: &Value) -> Vec<Value> {
    value
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|items| items.iter())
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("agent_message"))
        .map(summarize_agent_message)
        .collect()
}

fn summarize_agent_message(item: &Value) -> Value {
    let mut summary = Map::new();
    for key in ["author", "recipient"] {
        if let Some(value) = item.get(key).and_then(Value::as_str) {
            summary.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
    let encrypted_values = encrypted_content_summaries(item);
    if !encrypted_values.is_empty() {
        summary.insert(
            "encrypted_content".to_string(),
            Value::Array(encrypted_values),
        );
    }
    Value::Object(summary)
}

fn encrypted_content_summaries(value: &Value) -> Vec<Value> {
    let mut summaries = Vec::new();
    collect_encrypted_content(value, &mut summaries);
    summaries
}

fn collect_encrypted_content(value: &Value, summaries: &mut Vec<Value>) {
    if summaries.len() >= MAX_ENCRYPTED_VALUES {
        return;
    }

    match value {
        Value::Object(object) => {
            if let Some(content) = object.get("encrypted_content").and_then(Value::as_str) {
                summaries.push(summarize_string(content));
            }
            for value in object.values() {
                collect_encrypted_content(value, summaries);
                if summaries.len() >= MAX_ENCRYPTED_VALUES {
                    break;
                }
            }
        }
        Value::Array(items) => {
            for value in items {
                collect_encrypted_content(value, summaries);
                if summaries.len() >= MAX_ENCRYPTED_VALUES {
                    break;
                }
            }
        }
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn summarize_string(value: &str) -> Value {
    json!({
        "len": value.len(),
        "starts_gAAAAAB": value.starts_with("gAAAAAB"),
    })
}

fn append_trace(mut event: Value) {
    let Some(path) = env::var_os(TRACE_ENV) else {
        return;
    };
    if let Some(object) = event.as_object_mut() {
        object.insert("ts_ms".to_string(), json!(timestamp_ms()));
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    if let Ok(line) = serde_json::to_string(&event) {
        let _ = writeln!(file, "{line}");
    }
}

fn trace_enabled() -> bool {
    env::var_os(TRACE_ENV).is_some()
}

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
