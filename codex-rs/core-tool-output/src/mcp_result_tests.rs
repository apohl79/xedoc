use super::*;

use codex_protocol::mcp::CallToolResult;
use pretty_assertions::assert_eq;

const EVENT_MAX_BYTES: usize = 256;

#[test]
fn sanitizing_result_replaces_unsupported_image_and_audio_content() {
    let result = Ok(CallToolResult {
        content: vec![
            serde_json::json!({"type": "image", "data": "Zm9v", "mimeType": "image/png"}),
            serde_json::json!({"type": "audio", "data": "YmFy", "mimeType": "audio/wav"}),
            serde_json::json!({"type": "text", "text": "hello"}),
        ],
        structured_content: None,
        is_error: Some(false),
        meta: None,
    });

    assert_eq!(
        sanitize_mcp_tool_result_for_model(&[InputModality::Text], result),
        Ok(CallToolResult {
            content: vec![
                serde_json::json!({"type": "text", "text": "<image content omitted because you do not support image input>"}),
                serde_json::json!({"type": "text", "text": "<audio content omitted because you do not support audio input>"}),
                serde_json::json!({"type": "text", "text": "hello"}),
            ],
            structured_content: None,
            is_error: Some(false),
            meta: None,
        }),
    );
}

#[test]
fn sanitizing_result_preserves_supported_media() {
    let original = CallToolResult {
        content: vec![
            serde_json::json!({"type": "image", "data": "Zm9v", "mimeType": "image/png"}),
            serde_json::json!({"type": "audio", "data": "YmFy", "mimeType": "audio/wav"}),
        ],
        structured_content: Some(serde_json::json!({"x": 1})),
        is_error: Some(false),
        meta: Some(serde_json::json!({"k": "v"})),
    };

    assert_eq!(
        sanitize_mcp_tool_result_for_model(
            &[
                InputModality::Text,
                InputModality::Image,
                InputModality::Audio
            ],
            Ok(original.clone()),
        ),
        Ok(original),
    );
}

#[test]
fn truncating_event_result_preserves_small_result() {
    let original = CallToolResult {
        content: vec![serde_json::json!({"type": "text", "text": "hello"})],
        structured_content: Some(serde_json::json!({"x": 1})),
        is_error: Some(false),
        meta: Some(serde_json::json!({"k": "v"})),
    };

    assert_eq!(
        truncate_mcp_tool_result_for_event(&Ok(original.clone()), EVENT_MAX_BYTES),
        Ok(original),
    );
}

#[test]
fn truncating_event_result_collapses_oversized_result() {
    let result = Ok(CallToolResult {
        content: vec![serde_json::json!({"type": "text", "text": "long-message-".repeat(100)})],
        structured_content: Some(serde_json::json!({"structured": "value-".repeat(100)})),
        is_error: Some(false),
        meta: Some(serde_json::json!({"meta": "value-".repeat(100)})),
    });

    assert_eq!(
        truncate_mcp_tool_result_for_event(&result, EVENT_MAX_BYTES).map(|result| (
            result.structured_content,
            result.meta,
            result.is_error
        )),
        Ok((None, None, Some(false))),
    );
}

#[test]
fn truncating_event_error_bounds_oversized_message() {
    assert_eq!(
        truncate_mcp_tool_result_for_event(&Err("error-message-".repeat(100)), EVENT_MAX_BYTES)
            .map(|_| ())
            .map_err(|message| message.contains("truncated")),
        Err(true),
    );
}
