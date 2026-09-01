use base64::Engine;
use pretty_assertions::assert_eq;
use serde_json::json;
use xedoc_api::Reasoning;
use xedoc_api::ResponsesApiRequest;
use xedoc_api::create_text_param_for_request;
use xedoc_protocol::ResponseItemId;
use xedoc_protocol::config_types::ReasoningSummary;
use xedoc_protocol::models::AgentMessageInputContent;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::FunctionCallOutputPayload;
use xedoc_protocol::models::ReasoningItemReasoningSummary;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::openai_models::ReasoningEffort;

use super::translate_request;
use crate::AnthropicContentBlock;
use crate::AnthropicMessage;
use crate::AnthropicMessagesRequest;
use crate::AnthropicOutputConfig;
use crate::AnthropicRole;
use crate::AnthropicSystemBlock;
use crate::AnthropicThinking;
use crate::AnthropicTool;
use crate::AnthropicToolChoice;

fn request(model: &str, input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: model.to_string(),
        instructions: String::new(),
        input,
        tools: None,
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream: true,
        stream_options: None,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

fn message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn translates_alias_instructions_images_and_structured_output() {
    let mut input = message("user", "describe");
    if let ResponseItem::Message { content, .. } = &mut input {
        content.push(ContentItem::InputImage {
            image_url: "data:image/png;base64,aGVsbG8=".to_string(),
            detail: None,
        });
        content.push(ContentItem::InputImage {
            image_url: "data:image/png;base64,".to_string(),
            detail: None,
        });
    }
    let mut request = request("opus-4.8[1m]", vec![input]);
    request.instructions = "Be precise.".to_string();
    request.text = create_text_param_for_request(None, &Some(json!({"type": "object"})), true);

    let actual = serde_json::to_value(translate_request(&request).unwrap()).unwrap();

    assert_eq!(
        actual,
        json!({
            "model": "claude-opus-4-8",
            "max_tokens": 8192,
            "stream": true,
            "system": [{"type": "text", "text": "Be precise."}],
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "describe",
                    },
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": "aGVsbG8=",
                        }
                    },
                    {
                        "type": "image",
                        "source": {
                            "type": "url",
                            "url": "data:image/png;base64,",
                        }
                    },
                ]
            }],
            "tool_choice": {"type": "auto"},
            "output_config": {
                "format": {
                    "type": "json_schema",
                    "schema": {"type": "object"},
                    "name": "codex_output_schema",
                }
            },
        })
    );
}

#[test]
fn translates_adaptive_thinking_and_filters_tools() {
    let mut request = request("fable", vec![message("user", "edit")]);
    request.reasoning = Some(Reasoning {
        effort: Some(ReasoningEffort::High),
        summary: Some(ReasoningSummary::Auto),
        context: None,
    });
    request.tools = Some(vec![
        json!({
            "type": "function",
            "name": "exec_command",
            "description": "Run command",
            "parameters": {"type": "object", "properties": {}}
        }),
        json!({
            "type": "custom",
            "name": "apply_patch",
            "description": "Apply patch"
        }),
        json!({"type": "web_search", "external_web_access": true}),
    ]);
    request.tool_choice = "required".to_string();

    let actual = translate_request(&request).unwrap();

    assert_eq!(
        actual,
        AnthropicMessagesRequest {
            model: "claude-fable-5".to_string(),
            max_tokens: 65536,
            stream: true,
            system: Vec::new(),
            messages: vec![AnthropicMessage {
                role: AnthropicRole::User,
                content: vec![AnthropicContentBlock::Text {
                    text: "edit".to_string(),
                }],
            }],
            tools: vec![
                AnthropicTool {
                    name: "exec_command".to_string(),
                    description: "Run command".to_string(),
                    input_schema: json!({"type": "object", "properties": {}}),
                },
                AnthropicTool {
                    name: "apply_patch".to_string(),
                    description: "Apply patch".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "input": {
                                "type": "string",
                                "description": "Raw input for the freeform tool."
                            }
                        },
                        "required": ["input"],
                        "additionalProperties": false
                    }),
                },
            ],
            tool_choice: Some(AnthropicToolChoice::Any {
                disable_parallel_tool_use: false,
            }),
            thinking: None,
            output_config: Some(AnthropicOutputConfig {
                effort: Some("high".to_string()),
                format: None,
            }),
        }
    );
}

#[test]
fn replays_reasoning_tool_calls_and_parallel_results() {
    let input = vec![
        message("user", "Use both tools."),
        ResponseItem::Reasoning {
            id: None,
            summary: vec![ReasoningItemReasoningSummary::SummaryText {
                text: "I need both tools.".to_string(),
            }],
            content: None,
            encrypted_content: Some("sig_1".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCall {
            id: Some(ResponseItemId::from_server("call_1".to_string())),
            name: "one".to_string(),
            namespace: None,
            arguments: "{\"value\":1}".to_string(),
            call_id: String::new(),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCall {
            id: None,
            name: "two".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            call_id: "call_2".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "call_1".to_string(),
            output: FunctionCallOutputPayload::from_text("one".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "call_2".to_string(),
            output: FunctionCallOutputPayload::from_text("two".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
    ];

    let actual = translate_request(&request("claude-opus-5", input)).unwrap();

    assert_eq!(
        actual.messages,
        vec![
            AnthropicMessage {
                role: AnthropicRole::User,
                content: vec![AnthropicContentBlock::Text {
                    text: "Use both tools.".to_string(),
                }],
            },
            AnthropicMessage {
                role: AnthropicRole::Assistant,
                content: vec![
                    AnthropicContentBlock::Thinking {
                        thinking: "I need both tools.".to_string(),
                        signature: Some("sig_1".to_string()),
                    },
                    AnthropicContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "one".to_string(),
                        input: json!({"value": 1}),
                    },
                    AnthropicContentBlock::ToolUse {
                        id: "call_2".to_string(),
                        name: "two".to_string(),
                        input: json!({}),
                    },
                ],
            },
            AnthropicMessage {
                role: AnthropicRole::User,
                content: vec![
                    AnthropicContentBlock::ToolResult {
                        tool_use_id: "call_1".to_string(),
                        content: "one".to_string(),
                    },
                    AnthropicContentBlock::ToolResult {
                        tool_use_id: "call_2".to_string(),
                        content: "two".to_string(),
                    },
                ],
            },
        ]
    );
}

#[test]
fn preserves_positional_developer_and_custom_tool_history() {
    let patch = "*** Begin Patch\n*** End Patch";
    let input = vec![
        message("developer", "Leading contract"),
        message("user", "run it"),
        ResponseItem::CustomToolCall {
            id: None,
            status: None,
            call_id: "call_patch".to_string(),
            name: "apply_patch".to_string(),
            namespace: None,
            input: patch.to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::CustomToolCallOutput {
            id: None,
            call_id: "call_patch".to_string(),
            name: Some("apply_patch".to_string()),
            output: FunctionCallOutputPayload::from_text("Success".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
        message("developer", "<turn_aborted>interrupted"),
        message("user", "next"),
    ];

    let actual = translate_request(&request("opus", input)).unwrap();

    assert_eq!(
        actual,
        AnthropicMessagesRequest {
            model: "claude-opus-4-8".to_string(),
            max_tokens: 8192,
            stream: true,
            system: vec![AnthropicSystemBlock::Text {
                text: "Leading contract".to_string(),
            }],
            messages: vec![
                AnthropicMessage {
                    role: AnthropicRole::User,
                    content: vec![AnthropicContentBlock::Text {
                        text: "run it".to_string(),
                    }],
                },
                AnthropicMessage {
                    role: AnthropicRole::Assistant,
                    content: vec![AnthropicContentBlock::ToolUse {
                        id: "call_patch".to_string(),
                        name: "apply_patch".to_string(),
                        input: json!({"input": patch}),
                    }],
                },
                AnthropicMessage {
                    role: AnthropicRole::User,
                    content: vec![AnthropicContentBlock::ToolResult {
                        tool_use_id: "call_patch".to_string(),
                        content: "Success".to_string(),
                    }],
                },
                AnthropicMessage {
                    role: AnthropicRole::User,
                    content: vec![AnthropicContentBlock::Text {
                        text: "<turn_aborted>interrupted".to_string(),
                    }],
                },
                AnthropicMessage {
                    role: AnthropicRole::User,
                    content: vec![AnthropicContentBlock::Text {
                        text: "next".to_string(),
                    }],
                },
            ],
            tools: Vec::new(),
            tool_choice: Some(AnthropicToolChoice::Auto {
                disable_parallel_tool_use: false,
            }),
            thinking: None,
            output_config: None,
        }
    );
}

#[test]
fn masks_fernet_agent_content_and_appends_continuation() {
    let mut raw = vec![0_u8; 57];
    raw[0] = 0x80;
    let token = base64::engine::general_purpose::URL_SAFE.encode(raw);
    let input = vec![
        message("user", "start"),
        ResponseItem::AgentMessage {
            id: None,
            author: "/root/child".to_string(),
            recipient: "/root".to_string(),
            content: vec![
                AgentMessageInputContent::InputText {
                    text: "Message Type: MESSAGE\nPayload:\n".to_string(),
                },
                AgentMessageInputContent::EncryptedContent {
                    encrypted_content: token,
                },
            ],
            internal_chat_message_metadata_passthrough: None,
        },
    ];

    let actual = translate_request(&request("opus", input)).unwrap();

    assert_eq!(
        actual.messages,
        vec![
            AnthropicMessage {
                role: AnthropicRole::User,
                content: vec![AnthropicContentBlock::Text {
                    text: "start".to_string(),
                }],
            },
            AnthropicMessage {
                role: AnthropicRole::Assistant,
                content: vec![AnthropicContentBlock::Text {
                    text: "Message Type: MESSAGE\nPayload:\n[encrypted inter-agent content]"
                        .to_string(),
                }],
            },
            AnthropicMessage {
                role: AnthropicRole::User,
                content: vec![AnthropicContentBlock::Text {
                    text: "Continue.".to_string(),
                }],
            },
        ]
    );
}

#[test]
fn downgrades_forced_tool_choice_for_new_fable_models() {
    let mut request = request("claude-melon-lp-eap", vec![message("user", "Weather?")]);
    request.reasoning = Some(Reasoning {
        effort: Some(ReasoningEffort::High),
        summary: None,
        context: None,
    });
    request.tool_choice = "required".to_string();
    request.parallel_tool_calls = false;

    let actual = translate_request(&request).unwrap();

    assert_eq!(
        actual,
        AnthropicMessagesRequest {
            model: "claude-melon-lp-eap".to_string(),
            max_tokens: 28672,
            stream: true,
            system: vec![AnthropicSystemBlock::Text {
                text: "You must respond by calling one of the available tools.".to_string(),
            }],
            messages: vec![AnthropicMessage {
                role: AnthropicRole::User,
                content: vec![AnthropicContentBlock::Text {
                    text: "Weather?".to_string(),
                }],
            }],
            tools: Vec::new(),
            tool_choice: Some(AnthropicToolChoice::Auto {
                disable_parallel_tool_use: true,
            }),
            thinking: Some(AnthropicThinking::Enabled {
                budget_tokens: 24576,
                display: None,
            }),
            output_config: None,
        }
    );
}

#[test]
fn keeps_default_output_limit_for_unknown_fixed_effort() {
    let mut request = request("haiku", vec![message("user", "Analyze")]);
    request.reasoning = Some(Reasoning {
        effort: Some(ReasoningEffort::Ultra),
        summary: None,
        context: None,
    });

    let actual = translate_request(&request).unwrap();

    assert_eq!(
        (actual.max_tokens, actual.thinking),
        (
            8192,
            Some(AnthropicThinking::Enabled {
                budget_tokens: 8192,
                display: None,
            }),
        )
    );
}
