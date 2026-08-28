use super::*;
use codex_protocol::models::DEFAULT_IMAGE_DETAIL;
use codex_protocol::models::SearchToolCallParams;
use pretty_assertions::assert_eq;
use regex_lite::Regex;
use serde_json::json;

#[test]
fn custom_tool_calls_should_roundtrip_as_custom_outputs() {
    let payload = ToolPayload::Custom {
        input: "patch".to_string(),
    };
    let response = FunctionToolOutput::from_text("patched".to_string(), Some(true))
        .to_response_item("call-42", &payload);

    match response {
        ResponseInputItem::CustomToolCallOutput {
            call_id, output, ..
        } => {
            assert_eq!(call_id, "call-42");
            assert_eq!(output.content_items(), None);
            assert_eq!(output.body.to_text().as_deref(), Some("patched"));
            assert_eq!(output.success, Some(true));
        }
        other => panic!("expected CustomToolCallOutput, got {other:?}"),
    }
}

#[test]
fn function_payloads_remain_function_outputs() {
    let payload = ToolPayload::Function {
        arguments: "{}".to_string(),
    };
    let response = FunctionToolOutput::from_text("ok".to_string(), Some(true))
        .to_response_item("fn-1", &payload);

    match response {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            assert_eq!(call_id, "fn-1");
            assert_eq!(output.content_items(), None);
            assert_eq!(output.body.to_text().as_deref(), Some("ok"));
            assert_eq!(output.success, Some(true));
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn mcp_code_mode_result_serializes_full_call_tool_result() {
    let output = McpToolOutput {
        result: CallToolResult {
            content: vec![serde_json::json!({
                "type": "text",
                "text": "ignored",
            })],
            structured_content: Some(serde_json::json!({
                "threadId": "thread_123",
                "content": "done",
            })),
            is_error: Some(false),
            meta: Some(serde_json::json!({
                "source": "mcp",
            })),
        },
        tool_input: json!({}),
        wall_time: Duration::ZERO,
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Bytes(1024),
    };

    assert_eq!(
        output.code_mode_result(&ToolPayload::Function {
            arguments: "{}".to_string(),
        }),
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": "ignored",
            }],
            "structuredContent": {
                "threadId": "thread_123",
                "content": "done",
            },
            "isError": false,
            "_meta": {
                "source": "mcp",
            },
        })
    );
}

#[test]
fn mcp_tool_output_response_item_includes_wall_time() {
    let output = McpToolOutput {
        result: CallToolResult {
            content: vec![serde_json::json!({
                "type": "text",
                "text": "done",
            })],
            structured_content: None,
            is_error: Some(false),
            meta: None,
        },
        tool_input: json!({}),
        wall_time: Duration::from_millis(1250),
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Bytes(1024),
    };

    let response = output.to_response_item(
        "mcp-call-1",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );

    match response {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            assert_eq!(call_id, "mcp-call-1");
            assert_eq!(output.success, Some(true));
            let Some(text) = output.body.to_text() else {
                panic!("MCP output should serialize as text");
            };
            let Some(payload) = text.strip_prefix("Wall time: 1.2500 seconds\nOutput:\n") else {
                panic!("MCP output should include wall-time header: {text}");
            };
            let parsed: serde_json::Value = serde_json::from_str(payload).unwrap_or_else(|err| {
                panic!("MCP output should serialize JSON content: {err}");
            });
            assert_eq!(
                parsed,
                json!([{
                    "type": "text",
                    "text": "done",
                }])
            );
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn mcp_tool_output_response_item_truncates_large_structured_content() {
    let output = McpToolOutput {
        result: CallToolResult {
            content: vec![serde_json::json!({
                "type": "text",
                "text": "ignored when structured content is present",
            })],
            structured_content: Some(serde_json::json!({
                "items": "large structured value ".repeat(1_000),
            })),
            is_error: Some(false),
            meta: None,
        },
        tool_input: json!({}),
        wall_time: Duration::from_millis(1250),
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Bytes(128),
    };

    let response = output.to_response_item(
        "mcp-call-large",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );

    match response {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            let text = output
                .body
                .to_text()
                .expect("MCP output should serialize as text");
            assert_eq!(
                (
                    call_id,
                    output.success,
                    text.starts_with("Wall time: 1.2500 seconds\nOutput:\n"),
                    text.contains("chars truncated"),
                    text.contains("ignored when structured content is present")
                ),
                ("mcp-call-large".to_string(), Some(true), true, true, false)
            );
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn mcp_tool_output_response_item_preserves_content_items() {
    let output = McpToolOutput {
        result: CallToolResult {
            content: vec![serde_json::json!({
                "type": "image",
                "mimeType": "image/png",
                "data": "AAA",
            })],
            structured_content: None,
            is_error: Some(false),
            meta: None,
        },
        tool_input: json!({}),
        wall_time: Duration::from_millis(500),
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Bytes(1024),
    };

    let response = output.to_response_item(
        "mcp-call-2",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );

    match response {
        ResponseInputItem::FunctionCallOutput { output, .. } => {
            assert_eq!(
                (output.content_items(), output.body.to_text().as_deref()),
                (
                    Some(
                        vec![
                            FunctionCallOutputContentItem::InputText {
                                text: "Wall time: 0.5000 seconds\nOutput:".to_string(),
                            },
                            FunctionCallOutputContentItem::InputImage {
                                image_url: "data:image/png;base64,AAA".to_string(),
                                detail: Some(DEFAULT_IMAGE_DETAIL),
                            },
                        ]
                        .as_slice()
                    ),
                    Some("Wall time: 0.5000 seconds\nOutput:")
                )
            );
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn mcp_tool_output_code_mode_result_stays_raw_call_tool_result() {
    let large_content = "large structured value ".repeat(1_000);
    let output = McpToolOutput {
        result: CallToolResult {
            content: vec![serde_json::json!({
                "type": "text",
                "text": "ignored",
            })],
            structured_content: Some(serde_json::json!({
                "content": large_content,
            })),
            is_error: Some(false),
            meta: None,
        },
        tool_input: json!({}),
        wall_time: Duration::from_millis(1250),
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Bytes(64),
    };

    assert_eq!(
        output.code_mode_result(&ToolPayload::Function {
            arguments: "{}".to_string(),
        }),
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": "ignored",
            }],
            "structuredContent": {
                "content": "large structured value ".repeat(1_000),
            },
            "isError": false,
        })
    );
}

#[test]
fn custom_tool_calls_can_derive_text_from_content_items() {
    let payload = ToolPayload::Custom {
        input: "patch".to_string(),
    };
    let response = FunctionToolOutput::from_content(
        vec![
            FunctionCallOutputContentItem::InputText {
                text: "line 1".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,AAA".to_string(),
                detail: Some(DEFAULT_IMAGE_DETAIL),
            },
            FunctionCallOutputContentItem::InputText {
                text: "line 2".to_string(),
            },
        ],
        Some(true),
    )
    .to_response_item("call-99", &payload);

    match response {
        ResponseInputItem::CustomToolCallOutput {
            call_id, output, ..
        } => {
            let expected = vec![
                FunctionCallOutputContentItem::InputText {
                    text: "line 1".to_string(),
                },
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,AAA".to_string(),
                    detail: Some(DEFAULT_IMAGE_DETAIL),
                },
                FunctionCallOutputContentItem::InputText {
                    text: "line 2".to_string(),
                },
            ];
            assert_eq!(
                (
                    call_id,
                    output.content_items(),
                    output.body.to_text().as_deref(),
                    output.success
                ),
                (
                    "call-99".to_string(),
                    Some(expected.as_slice()),
                    Some("line 1\nline 2"),
                    Some(true)
                )
            );
        }
        other => panic!("expected CustomToolCallOutput, got {other:?}"),
    }
}

#[test]
fn tool_search_payloads_roundtrip_as_tool_search_outputs() {
    let payload = ToolPayload::ToolSearch {
        arguments: SearchToolCallParams {
            query: "calendar".to_string(),
            limit: None,
        },
    };
    let response = ToolSearchOutput {
        tools: vec![LoadableToolSpec::Function(codex_tools::ResponsesApiTool {
            name: "create_event".to_string(),
            description: String::new(),
            strict: false,
            defer_loading: Some(true),
            parameters: codex_tools::JsonSchema::object(
                /*properties*/ Default::default(),
                /*required*/ None,
                /*additional_properties*/ None,
            ),
            output_schema: None,
        })],
    }
    .to_response_item("search-1", &payload);

    match response {
        ResponseInputItem::ToolSearchOutput {
            call_id,
            status,
            execution,
            tools,
        } => assert_eq!(
            (call_id, status, execution, tools),
            (
                "search-1".to_string(),
                "completed".to_string(),
                "client".to_string(),
                vec![json!({
                    "type": "function",
                    "name": "create_event",
                    "description": "",
                    "strict": false,
                    "defer_loading": true,
                    "parameters": {
                        "type": "object",
                        "properties": {}
                    }
                })]
            )
        ),
        other => panic!("expected ToolSearchOutput, got {other:?}"),
    }
}

#[test]
fn log_preview_uses_content_items_when_plain_text_is_missing() {
    let output = FunctionToolOutput::from_content(
        vec![FunctionCallOutputContentItem::InputText {
            text: "preview".to_string(),
        }],
        Some(true),
    );

    assert_eq!(
        (
            output.log_preview(),
            function_call_output_content_items_to_text(&output.body)
        ),
        ("preview".to_string(), Some("preview".to_string()))
    );
}

#[test]
fn telemetry_preview_returns_original_within_limits() {
    assert_eq!(telemetry_preview("short output"), "short output");
}

#[test]
fn telemetry_preview_truncates_by_bytes() {
    let content = "x".repeat(TELEMETRY_PREVIEW_MAX_BYTES + 8);
    let preview = telemetry_preview(&content);

    assert_eq!(
        (
            preview.contains(TELEMETRY_PREVIEW_TRUNCATION_NOTICE),
            preview.len()
                <= TELEMETRY_PREVIEW_MAX_BYTES + TELEMETRY_PREVIEW_TRUNCATION_NOTICE.len() + 1,
        ),
        (true, true)
    );
}

#[test]
fn telemetry_preview_truncates_by_lines() {
    let content = (0..(TELEMETRY_PREVIEW_MAX_LINES + 5))
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let lines = telemetry_preview(&content).lines().collect::<Vec<_>>();

    assert_eq!(
        (lines.len() <= TELEMETRY_PREVIEW_MAX_LINES + 1, lines.last()),
        (true, Some(&TELEMETRY_PREVIEW_TRUNCATION_NOTICE))
    );
}

#[test]
fn exec_command_tool_output_formats_truncated_response() {
    let response = ExecCommandToolOutput {
        event_call_id: "call-42".to_string(),
        chunk_id: "abc123".to_string(),
        wall_time: Duration::from_millis(1250),
        raw_output: b"token one token two token three token four token five".to_vec(),
        truncation_policy: TruncationPolicy::Tokens(10_000),
        max_output_tokens: Some(4),
        process_id: None,
        exit_code: Some(0),
        original_token_count: Some(10),
        output_omitted_bytes: None,
        hook_command: None,
    }
    .to_response_item(
        "call-42",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );

    let ResponseInputItem::FunctionCallOutput { call_id, output } = response else {
        panic!("expected FunctionCallOutput");
    };
    let text = output
        .body
        .to_text()
        .expect("exec output should serialize as text");
    let pattern = Regex::new(
        r#"(?sx)
            ^Chunk\ ID:\ abc123
            \nWall\ time:\ \d+\.\d{4}\ seconds
            \nProcess\ exited\ with\ code\ 0
            \nOriginal\ token\ count:\ 10
            \nOutput:
            \n.*tokens\ truncated.*
            $"#,
    )
    .expect("test pattern should compile");

    assert_eq!(
        (call_id, output.success, pattern.is_match(&text)),
        ("call-42".to_string(), Some(true), true)
    );
}

#[test]
fn exec_command_tool_output_preserves_omission_metadata_when_truncated() {
    let marker = format_output_omission_marker(/*omitted_bytes*/ 123_456);
    let raw_output = format!(
        "HEAD-{}\n{marker}\nTAIL-{}",
        "a".repeat(/*n*/ 100),
        "z".repeat(/*n*/ 100)
    )
    .into_bytes();
    let response = ExecCommandToolOutput {
        event_call_id: "call-omitted".to_string(),
        chunk_id: "abc123".to_string(),
        wall_time: Duration::from_millis(/*millis*/ 1250),
        raw_output,
        truncation_policy: TruncationPolicy::Tokens(10_000),
        max_output_tokens: Some(4),
        process_id: None,
        exit_code: Some(0),
        original_token_count: Some(42_000),
        output_omitted_bytes: NonZeroUsize::new(/*n*/ 123_456),
        hook_command: None,
    }
    .to_response_item(
        "call-omitted",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );

    let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
        panic!("expected FunctionCallOutput");
    };
    let text = output
        .body
        .to_text()
        .expect("exec output should serialize as text");

    assert_eq!(
        (
            text.contains("Original token count: 42000"),
            text.contains("Warning: truncated output (original token count: 42000)"),
            text.matches(&marker).count(),
        ),
        (true, true, 1)
    );
}
