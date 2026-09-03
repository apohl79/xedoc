use pretty_assertions::assert_eq;
use serde_json::json;
use xedoc_api::Reasoning;
use xedoc_api::ResponsesApiRequest;
use xedoc_api::create_text_param_for_request;
use xedoc_protocol::ResponseItemId;
use xedoc_protocol::models::AgentMessageInputContent;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::FunctionCallOutputPayload;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::openai_models::ReasoningEffort;

use super::translate_request;
use crate::GeminiThoughtSignatureStore;

fn request(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "gemini-3.6-flash".to_string(),
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

fn message(role: &str, content: Vec<ContentItem>) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content,
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn text(text: &str) -> ContentItem {
    ContentItem::InputText {
        text: text.to_string(),
    }
}

#[test]
fn translates_instructions_images_and_structured_output() {
    let mut request = request(vec![
        message("developer", vec![text("Developer guidance.")]),
        message(
            "user",
            vec![
                text("Read this."),
                ContentItem::InputImage {
                    image_url: "data:image/png;base64,YQ==".to_string(),
                    detail: None,
                },
                ContentItem::InputImage {
                    image_url: "https://example.test/image.png".to_string(),
                    detail: None,
                },
            ],
        ),
    ]);
    request.instructions = "Use available tools.".to_string();
    request.text = create_text_param_for_request(None, &Some(json!({"type": "object"})), true);

    let actual = serde_json::to_value(
        translate_request(&request, &GeminiThoughtSignatureStore::new())
            .expect("translate request"),
    )
    .expect("serialize request");

    assert_eq!(
        actual,
        json!({
            "contents": [{
                "role": "user",
                "parts": [
                    {"text": "Read this."},
                    {"inlineData": {"mimeType": "image/png", "data": "YQ=="}}
                ]
            }],
            "systemInstruction": {
                "parts": [{
                    "text": "Use available tools.\n\nDeveloper guidance."
                }]
            },
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseJsonSchema": {"type": "object"}
            }
        })
    );
}

#[test]
fn translates_tool_history_schemas_and_thought_signatures() {
    let signatures = GeminiThoughtSignatureStore::new();
    signatures.remember("call_weather", "opaque-signature");
    let mut request = request(vec![
        message("user", vec![text("Check weather.")]),
        ResponseItem::FunctionCall {
            id: None,
            name: "weather".to_string(),
            namespace: None,
            arguments: r#"{"city":"Berlin"}"#.to_string(),
            call_id: "call_weather".to_string(),
            provider_metadata: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "call_weather".to_string(),
            output: FunctionCallOutputPayload::from_text("sunny".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::CustomToolCall {
            id: Some(ResponseItemId::from_server("call_patch".to_string())),
            status: None,
            call_id: String::new(),
            name: "apply_patch".to_string(),
            namespace: None,
            input: "*** Begin Patch".to_string(),
            provider_metadata: None,
            internal_chat_message_metadata_passthrough: None,
        },
    ]);
    request.tool_choice = "required".to_string();
    request.tools = Some(vec![
        json!({
            "type": "function",
            "name": "weather",
            "description": "Returns weather.",
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "city": {
                        "type": "object",
                        "additionalProperties": false
                    }
                }
            }
        }),
        json!({
            "type": "custom",
            "name": "apply_patch",
            "description": "Apply a patch."
        }),
        json!({"type": "web_search", "name": "search"}),
    ]);

    let actual =
        serde_json::to_value(translate_request(&request, &signatures).expect("translate request"))
            .expect("serialize request");

    assert_eq!(
        actual,
        json!({
            "contents": [
                {"role": "user", "parts": [{"text": "Check weather."}]},
                {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "weather",
                            "args": {"city": "Berlin"}
                        },
                        "thoughtSignature": "opaque-signature"
                    }]
                },
                {
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": "weather",
                            "response": {"output": "sunny"}
                        }
                    }]
                },
                {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "apply_patch",
                            "args": {"input": "*** Begin Patch"}
                        }
                    }]
                }
            ],
            "tools": [{
                "functionDeclarations": [
                    {
                        "name": "weather",
                        "description": "Returns weather.",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "city": {"type": "object"}
                            }
                        }
                    },
                    {
                        "name": "apply_patch",
                        "description": "Apply a patch.",
                        "parameters": {
                            "type": "object",
                            "properties": {"input": {"type": "string"}},
                            "required": ["input"]
                        }
                    }
                ]
            }],
            "toolConfig": {
                "functionCallingConfig": {"mode": "ANY"}
            }
        })
    );
}

#[test]
fn preserves_agent_message_order_and_merges_adjacent_roles() {
    let request = request(vec![
        ResponseItem::AgentMessage {
            id: None,
            author: "parent".to_string(),
            recipient: "child".to_string(),
            content: vec![AgentMessageInputContent::InputText {
                text: "Review the implementation.".to_string(),
            }],
            internal_chat_message_metadata_passthrough: None,
        },
        message("user", vec![text("Focus on tests.")]),
        message("assistant", vec![text("First pass complete.")]),
        message("model", vec![text("Continuing.")]),
    ]);

    let actual = serde_json::to_value(
        translate_request(&request, &GeminiThoughtSignatureStore::new())
            .expect("translate request"),
    )
    .expect("serialize request");

    assert_eq!(
        actual,
        json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [
                        {"text": "Review the implementation."},
                        {"text": "Focus on tests."}
                    ]
                },
                {
                    "role": "model",
                    "parts": [
                        {"text": "First pass complete."},
                        {"text": "Continuing."}
                    ]
                }
            ]
        })
    );
}

#[test]
fn maps_reasoning_effort_and_tool_choice_decisions() {
    let cases = [
        (ReasoningEffort::Minimal, "none", Some(("LOW", "NONE"))),
        (ReasoningEffort::Low, "required", Some(("LOW", "ANY"))),
        (ReasoningEffort::Medium, "auto", Some(("MEDIUM", ""))),
        (ReasoningEffort::High, "auto", Some(("HIGH", ""))),
        (ReasoningEffort::Ultra, "auto", None),
    ];
    let actual = cases
        .iter()
        .map(|(effort, tool_choice, _)| {
            let mut request = request(vec![message("user", vec![text("Work.")])]);
            request.reasoning = Some(Reasoning {
                effort: Some(effort.clone()),
                summary: None,
                context: None,
            });
            request.tool_choice = tool_choice.to_string();
            serde_json::to_value(
                translate_request(&request, &GeminiThoughtSignatureStore::new())
                    .expect("translate request"),
            )
            .expect("serialize request")
        })
        .collect::<Vec<_>>();
    let expected = cases
        .into_iter()
        .map(|(_, _, expected)| match expected {
            Some((level, mode)) if !mode.is_empty() => json!({
                "contents": [{"role": "user", "parts": [{"text": "Work."}]}],
                "toolConfig": {"functionCallingConfig": {"mode": mode}},
                "generationConfig": {"thinkingConfig": {"thinkingLevel": level}}
            }),
            Some((level, _)) => json!({
                "contents": [{"role": "user", "parts": [{"text": "Work."}]}],
                "generationConfig": {"thinkingConfig": {"thinkingLevel": level}}
            }),
            None => json!({
                "contents": [{"role": "user", "parts": [{"text": "Work."}]}]
            }),
        })
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
}

#[test]
fn drops_empty_unsupported_and_malformed_inputs() {
    let request = request(vec![
        message(
            "user",
            vec![
                text(""),
                ContentItem::InputImage {
                    image_url: "data:image/png;base64,".to_string(),
                    detail: None,
                },
                ContentItem::InputAudio {
                    audio_url: "data:audio/wav;base64,aQ==".to_string(),
                },
            ],
        ),
        ResponseItem::FunctionCall {
            id: None,
            name: "broken".to_string(),
            namespace: None,
            arguments: "{".to_string(),
            call_id: "call_broken".to_string(),
            provider_metadata: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Other,
    ]);

    let actual = serde_json::to_value(
        translate_request(&request, &GeminiThoughtSignatureStore::new())
            .expect("translate request"),
    )
    .expect("serialize request");

    assert_eq!(
        actual,
        json!({
            "contents": [{
                "role": "model",
                "parts": [{
                    "functionCall": {
                        "name": "broken",
                        "args": {}
                    }
                }]
            }]
        })
    );
}
