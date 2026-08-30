#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use anyhow::Result;
use codex_config::types::McpServerConfig;
use codex_config::types::McpServerTransportConfig;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::ev_tool_search_call;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::namespace_child_tool;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::stdio_server_bin;
use core_test_support::test_codex::configure_search_capable_model;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";

fn tool_names(body: &Value) -> Vec<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("name")
                        .or_else(|| tool.get("type"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tool_search_output_item(request: &ResponsesRequest, call_id: &str) -> Value {
    request.tool_search_output(call_id)
}

fn tool_search_output_tools(request: &ResponsesRequest, call_id: &str) -> Vec<Value> {
    tool_search_output_item(request, call_id)
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn tool_search_output_has_namespace_child(
    request: &ResponsesRequest,
    call_id: &str,
    namespace: &str,
    tool_name: &str,
) -> bool {
    let output = json!({
        "tools": tool_search_output_tools(request, call_id),
    });
    namespace_child_tool(&output, namespace, tool_name).is_some()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_tool_enabled_by_default_adds_tool_search() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_config(configure_search_capable_model);
    let test = builder.build(&server).await?;

    test.submit_turn_with_approval_and_permission_profile(
        "list tools",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let body = mock.single_request().body_json();
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools array should exist");
    let tool_search = tools
        .iter()
        .find(|tool| tool.get("type").and_then(Value::as_str) == Some(TOOL_SEARCH_TOOL_NAME))
        .cloned()
        .expect("tool_search should be present");

    assert_eq!(
        tool_search,
        json!({
            "type": "tool_search",
            "execution": "client",
            "description": tool_search["description"].as_str().expect("description should exist"),
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query for deferred tools."},
                    "limit": {"type": "number", "description": "Maximum number of tools to return. Defaults to 8."},
                },
                "required": ["query"],
                "additionalProperties": false,
            }
        })
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_search_returns_deferred_v1_multi_agent_tools() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let call_id = "tool-search-spawn-agent";
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_tool_search_call(
                    call_id,
                    &json!({
                        "query": "spawn agent",
                        "limit": 1,
                    }),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_config(configure_search_capable_model);
    let test = builder.build(&server).await?;
    test.submit_turn_with_approval_and_permission_profile(
        "Find the spawn agent tool",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);

    let first_request_body = requests[0].body_json();
    let first_request_tools = tool_names(&first_request_body);
    assert!(
        first_request_tools
            .iter()
            .any(|name| name == TOOL_SEARCH_TOOL_NAME),
        "first request should advertise tool_search: {first_request_tools:?}"
    );
    for tool_name in [
        "spawn_agent",
        "send_input",
        "resume_agent",
        "wait_agent",
        "close_agent",
    ] {
        assert!(
            !first_request_tools.iter().any(|name| name == tool_name),
            "v1 multi-agent tools should be hidden before search: {first_request_tools:?}"
        );
    }
    assert!(
        !first_request_body
            .to_string()
            .contains("### When to delegate vs. do the subtask yourself"),
        "deferred v1 multi-agent guidance should stay out of initial developer context"
    );

    let tools = tool_search_output_tools(&requests[1], call_id);
    assert!(
        !tools.iter().any(|tool| {
            tool.get("type").and_then(Value::as_str) == Some("function")
                && tool.get("name").and_then(Value::as_str) == Some("spawn_agent")
        }),
        "spawn_agent should be returned as a namespace child, not a flat function: {tools:?}"
    );
    assert!(
        tools.iter().any(|tool| {
            tool.get("type").and_then(Value::as_str) == Some("namespace")
                && tool.get("name").and_then(Value::as_str) == Some("multi_agent_v1")
        }),
        "expected tool_search to return multi_agent_v1 namespace: {tools:?}"
    );
    let output = tool_search_output_item(&requests[1], call_id);
    let spawn_agent = namespace_child_tool(&output, "multi_agent_v1", "spawn_agent")
        .expect("tool_search should return multi_agent_v1.spawn_agent");
    assert_eq!(
        spawn_agent.get("defer_loading").and_then(Value::as_bool),
        Some(true)
    );
    let description = spawn_agent
        .get("description")
        .and_then(Value::as_str)
        .expect("spawn_agent description should be present");
    assert!(description.contains(
        "Do not spawn sub-agents unless the user or applicable AGENTS.md/skill instructions explicitly ask for sub-agents, delegation, or parallel agent work."
    ));
    assert!(description.contains("### Designing delegated subtasks"));
    assert!(description.contains("### When to delegate vs. do the subtask yourself"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_search_returns_deferred_dynamic_tool_and_routes_follow_up_call() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let search_call_id = "tool-search-1";
    let dynamic_call_id = "dyn-search-call-1";
    let tool_name = "automation_update";
    let tool_description = "Create, update, view, or delete recurring automations.";
    let tool_args = json!({ "mode": "create" });
    let tool_call_arguments = serde_json::to_string(&tool_args)?;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_tool_search_call(
                    search_call_id,
                    &json!({
                        "query": "recurring automations",
                        "limit": 8,
                    }),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "function_call",
                        "call_id": dynamic_call_id,
                        "namespace": "codex_app",
                        "name": tool_name,
                        "arguments": tool_call_arguments,
                    }
                }),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-3"),
            ]),
        ],
    )
    .await;

    let input_schema = json!({
        "type": "object",
        "properties": {
            "mode": { "type": "string" },
        },
        "required": ["mode"],
        "additionalProperties": false,
    });
    let dynamic_tool = DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
        name: "codex_app".to_string(),
        description: "Automation tools.".to_string(),
        tools: vec![DynamicToolNamespaceTool::Function(
            DynamicToolFunctionSpec {
                name: tool_name.to_string(),
                description: tool_description.to_string(),
                input_schema: input_schema.clone(),
                defer_loading: true,
            },
        )],
    });

    let mut builder = test_codex().with_config(configure_search_capable_model);
    let base_test = builder.build(&server).await?;
    let new_thread = base_test
        .thread_manager
        .start_thread_with_tools(base_test.config.clone(), vec![dynamic_tool])
        .await?;
    let mut test = base_test;
    test.codex = new_thread.thread;
    test.session_configured = new_thread.session_configured;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Use the automation tool".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;

    let EventMsg::DynamicToolCallRequest(request) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::DynamicToolCallRequest(_))
    })
    .await
    else {
        unreachable!("event guard guarantees DynamicToolCallRequest");
    };
    assert_eq!(request.call_id, dynamic_call_id);
    assert_eq!(request.namespace.as_deref(), Some("codex_app"));
    assert_eq!(request.tool, tool_name);
    assert_eq!(request.arguments, tool_args);

    test.codex
        .submit(Op::DynamicToolResponse {
            id: request.call_id,
            response: DynamicToolResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: "dynamic-search-ok".to_string(),
                }],
                success: true,
            },
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);

    let first_request_body = requests[0].body_json();
    let first_request_tools = tool_names(&first_request_body);
    assert!(
        first_request_tools
            .iter()
            .any(|name| name == TOOL_SEARCH_TOOL_NAME),
        "first request should advertise tool_search: {first_request_tools:?}"
    );
    assert!(
        !first_request_tools.iter().any(|name| name == tool_name),
        "deferred dynamic tool should be hidden before search: {first_request_tools:?}"
    );

    let tools = tool_search_output_tools(&requests[1], search_call_id);
    assert_eq!(
        tools,
        vec![json!({
            "type": "namespace",
            "name": "codex_app",
            "description": "Automation tools.",
            "tools": [{
                "type": "function",
                "name": tool_name,
                "description": tool_description,
                "strict": false,
                "defer_loading": true,
                "parameters": input_schema,
            }],
        })]
    );

    let second_request_body = requests[1].body_json();
    let second_request_tools = tool_names(&second_request_body);
    assert!(
        !second_request_tools.iter().any(|name| name == tool_name),
        "follow-up request should rely on tool_search_output history, not tool injection: {second_request_tools:?}"
    );

    let output = requests[2]
        .function_call_output(dynamic_call_id)
        .get("output")
        .cloned()
        .expect("dynamic tool output should be present");
    let payload: FunctionCallOutputPayload = serde_json::from_value(output)?;
    assert_eq!(
        payload,
        FunctionCallOutputPayload::from_text("dynamic-search-ok".to_string())
    );

    let third_request_body = requests[2].body_json();
    let third_request_tools = tool_names(&third_request_body);
    assert!(
        !third_request_tools.iter().any(|name| name == tool_name),
        "post-tool follow-up should rely on tool_search_output history, not tool injection: {third_request_tools:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_search_indexes_only_enabled_mcp_tools() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let echo_call_id = "tool-search-echo";
    let image_call_id = "tool-search-image";
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_tool_search_call(
                    echo_call_id,
                    &json!({
                        "query": "Echo back the provided message and include environment data.",
                        "limit": 8,
                    }),
                ),
                ev_tool_search_call(
                    image_call_id,
                    &json!({
                        "query": "Return a single image content block.",
                        "limit": 8,
                    }),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let rmcp_test_server_bin = stdio_server_bin()?;
    let mut builder = test_codex()
        .with_config(configure_search_capable_model)
        .with_config(move |config| {
            let mut servers = config.mcp_servers.get().clone();
            servers.insert(
                "rmcp".to_string(),
                McpServerConfig {
                    auth: Default::default(),
                    transport: McpServerTransportConfig::Stdio {
                        command: rmcp_test_server_bin,
                        args: Vec::new(),
                        env: None,
                        env_vars: Vec::new(),
                        cwd: None,
                    },
                    environment_id: "local".to_string(),
                    enabled: true,
                    required: false,
                    disabled_reason: None,
                    startup_timeout_sec: Some(Duration::from_secs(10)),
                    tool_timeout_sec: None,
                    default_tools_approval_mode: None,
                    enabled_tools: Some(vec!["echo".to_string(), "image".to_string()]),
                    disabled_tools: Some(vec!["image".to_string()]),
                    scopes: None,
                    oauth: None,
                    oauth_resource: None,
                    supports_parallel_tool_calls: false,
                    tools: HashMap::new(),
                },
            );
            config
                .mcp_servers
                .set(servers)
                .expect("test mcp servers should accept any configuration");
        });
    let test = builder.build(&server).await?;
    wait_for_mcp_server(&test.codex, "rmcp").await?;

    test.submit_turn_with_approval_and_permission_profile(
        "Find the rmcp echo and image tools.",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);

    let first_request_tools = tool_names(&requests[0].body_json());
    assert!(
        first_request_tools
            .iter()
            .any(|name| name == TOOL_SEARCH_TOOL_NAME),
        "first request should advertise tool_search: {first_request_tools:?}"
    );
    assert!(
        !first_request_tools
            .iter()
            .any(|name| name == "mcp__rmcp__echo"),
        "MCP tools should be hidden before search in large-search mode: {first_request_tools:?}"
    );
    assert!(
        !first_request_tools.iter().any(|name| name == "mcp__rmcp"),
        "MCP namespace should be hidden before search in large-search mode: {first_request_tools:?}"
    );

    let echo_tools = tool_search_output_tools(&requests[1], echo_call_id);
    let echo_output = json!({ "tools": echo_tools });
    let rmcp_echo_tool = namespace_child_tool(&echo_output, "mcp__rmcp", "echo")
        .expect("tool_search should return rmcp echo as a namespace child tool");
    assert_eq!(
        rmcp_echo_tool.get("type").and_then(Value::as_str),
        Some("function")
    );

    let image_tools = tool_search_output_tools(&requests[1], image_call_id);
    let found_rmcp_image_tool = image_tools
        .iter()
        .filter(|tool| tool.get("name").and_then(Value::as_str) == Some("mcp__rmcp"))
        .flat_map(|namespace| namespace.get("tools").and_then(Value::as_array))
        .flatten()
        .any(|tool| tool.get("name").and_then(Value::as_str).is_some());
    assert!(
        !found_rmcp_image_tool,
        "disabled MCP tools should not be searchable: {image_tools:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_search_surfaced_mcp_tool_errors_are_returned_to_model() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let search_call_id = "tool-search-rmcp-echo";
    let tool_call_id = "rmcp-echo-error";
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_tool_search_call(
                    search_call_id,
                    &json!({
                        "query": "Echo back the provided message and include environment data.",
                        "limit": 8,
                    }),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_function_call_with_namespace(tool_call_id, "mcp__rmcp", "echo", "{}"),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-3"),
            ]),
        ],
    )
    .await;

    let rmcp_test_server_bin = stdio_server_bin()?;
    let mut builder = test_codex()
        .with_config(configure_search_capable_model)
        .with_config(move |config| {
            let mut servers = config.mcp_servers.get().clone();
            servers.insert(
                "rmcp".to_string(),
                McpServerConfig {
                    auth: Default::default(),
                    transport: McpServerTransportConfig::Stdio {
                        command: rmcp_test_server_bin,
                        args: Vec::new(),
                        env: None,
                        env_vars: Vec::new(),
                        cwd: None,
                    },
                    environment_id: "local".to_string(),
                    enabled: true,
                    required: false,
                    disabled_reason: None,
                    startup_timeout_sec: Some(Duration::from_secs(10)),
                    tool_timeout_sec: None,
                    default_tools_approval_mode: None,
                    enabled_tools: Some(vec!["echo".to_string()]),
                    disabled_tools: None,
                    scopes: None,
                    oauth: None,
                    oauth_resource: None,
                    supports_parallel_tool_calls: false,
                    tools: HashMap::new(),
                },
            );
            config
                .mcp_servers
                .set(servers)
                .expect("test mcp servers should accept any configuration");
        });
    let test = builder.build(&server).await?;
    wait_for_mcp_server(&test.codex, "rmcp").await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Find the rmcp echo tool and call it.".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;

    let EventMsg::McpToolCallEnd(end) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::McpToolCallEnd(_))
    })
    .await
    else {
        unreachable!("event guard guarantees McpToolCallEnd");
    };
    assert_eq!(end.call_id, tool_call_id);
    assert!(!end.is_success());
    let tool_error = end
        .result
        .as_ref()
        .expect_err("rmcp echo error should stay in the MCP result");
    assert!(
        tool_error.contains("tool call error:")
            && tool_error.contains("missing field")
            && tool_error.contains("message"),
        "MCP invocation should report the execution failure: {tool_error}"
    );

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);

    let first_request_tools = tool_names(&requests[0].body_json());
    assert!(
        first_request_tools
            .iter()
            .any(|name| name == TOOL_SEARCH_TOOL_NAME),
        "first request should advertise tool_search: {first_request_tools:?}"
    );
    assert!(
        !first_request_tools.iter().any(|name| name == "mcp__rmcp"),
        "deferred rmcp namespace should not be directly exposed before search: {first_request_tools:?}"
    );

    assert!(
        tool_search_output_has_namespace_child(&requests[1], search_call_id, "mcp__rmcp", "echo"),
        "tool_search should return the rmcp echo tool"
    );

    let output = requests[2].function_call_output(tool_call_id);
    let output_text = match output.get("output") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("unexpected MCP error output payload: {other:?}"),
    };
    assert!(
        output_text.contains("missing field") && output_text.contains("message"),
        "MCP error output should be model visible: {output_text}"
    );
    assert!(
        !output_text.contains("unsupported call"),
        "search-surfaced MCP calls should not fall through to unsupported call: {output_text}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_search_uses_mcp_server_instructions_as_namespace_description() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let search_call_id = "tool-search-echo";
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_tool_search_call(
                    search_call_id,
                    &json!({
                        "query": "Echo back the provided message and include environment data.",
                        "limit": 8,
                    }),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let rmcp_test_server_bin = stdio_server_bin()?;
    let mut builder = test_codex()
        .with_config(configure_search_capable_model)
        .with_config(move |config| {
            let mut servers = config.mcp_servers.get().clone();
            servers.insert(
                "rmcp".to_string(),
                McpServerConfig {
                    auth: Default::default(),
                    transport: McpServerTransportConfig::Stdio {
                        command: rmcp_test_server_bin,
                        args: Vec::new(),
                        env: None,
                        env_vars: Vec::new(),
                        cwd: None,
                    },
                    environment_id: "local".to_string(),
                    enabled: true,
                    required: false,
                    disabled_reason: None,
                    startup_timeout_sec: Some(Duration::from_secs(10)),
                    tool_timeout_sec: None,
                    default_tools_approval_mode: None,
                    enabled_tools: Some(vec!["echo".to_string()]),
                    disabled_tools: None,
                    scopes: None,
                    oauth: None,
                    oauth_resource: None,
                    supports_parallel_tool_calls: false,
                    tools: HashMap::new(),
                },
            );
            config
                .mcp_servers
                .set(servers)
                .expect("test mcp servers should accept any configuration");
        });
    let test = builder.build(&server).await?;
    wait_for_mcp_server(&test.codex, "rmcp").await?;

    test.submit_turn_with_approval_and_permission_profile(
        "Find the rmcp echo tool.",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);

    let tools = tool_search_output_tools(&requests[1], search_call_id);
    let rmcp_namespace = tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some("mcp__rmcp"))
        .expect("tool_search should return the rmcp namespace");
    assert_eq!(
        rmcp_namespace.get("description").and_then(Value::as_str),
        Some("Use these tools to exercise the rmcp test server.")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_search_matches_dynamic_tools_by_name_description_namespace_and_schema_terms()
-> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let query_cases = [
        ("tool-search-dynamic-name", "quasar_ping_beacon"),
        ("tool-search-dynamic-spaces", "quasar ping beacon"),
        ("tool-search-dynamic-description", "saffron metronome"),
        ("tool-search-dynamic-namespace", "orbit_ops"),
        ("tool-search-dynamic-schema", "chrono_spec"),
    ];
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(std::iter::once(ev_response_created("resp-1"))
                .chain(query_cases.into_iter().map(|(call_id, query)| {
                    ev_tool_search_call(
                        call_id,
                        &json!({
                            "query": query,
                            "limit": 8,
                        }),
                    )
                }))
                .chain(std::iter::once(ev_completed("resp-1")))
                .collect()),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let dynamic_tool = DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
        name: "orbit_ops".to_string(),
        description: "Orbital reminder operations.".to_string(),
        tools: vec![DynamicToolNamespaceTool::Function(
            DynamicToolFunctionSpec {
                name: "quasar_ping_beacon".to_string(),
                description: "Trigger the saffron metronome workflow for reminder follow-ups."
                    .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "chrono_spec": { "type": "string" },
                        "targetThreadId": { "type": "string" },
                    },
                    "required": ["chrono_spec"],
                    "additionalProperties": false,
                }),
                defer_loading: true,
            },
        )],
    });

    let mut builder = test_codex().with_config(configure_search_capable_model);
    let base_test = builder.build(&server).await?;
    let new_thread = base_test
        .thread_manager
        .start_thread_with_tools(base_test.config.clone(), vec![dynamic_tool])
        .await?;
    let mut test = base_test;
    test.codex = new_thread.thread;
    test.session_configured = new_thread.session_configured;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Search for the dynamic tool".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);

    for call_id in [
        "tool-search-dynamic-name",
        "tool-search-dynamic-spaces",
        "tool-search-dynamic-description",
        "tool-search-dynamic-namespace",
        "tool-search-dynamic-schema",
    ] {
        assert!(
            tool_search_output_has_namespace_child(
                &requests[1],
                call_id,
                "orbit_ops",
                "quasar_ping_beacon"
            ),
            "expected query {call_id} to surface the quasar_ping_beacon tool: {:?}",
            tool_search_output_tools(&requests[1], call_id)
        );
    }

    Ok(())
}
