#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use anyhow::Result;
use codex_login::CodexAuth;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::ev_tool_search_call;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::namespace_child_tool;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::stdio_server_bin;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_mcp_server;
use tempfile::TempDir;
use wiremock::MockServer;

const SAMPLE_PLUGIN_CONFIG_NAME: &str = "sample@test";
const SAMPLE_PLUGIN_DISPLAY_NAME: &str = "sample";
const SAMPLE_PLUGIN_DESCRIPTION: &str = "inspect sample data";
const SAMPLE_PLUGIN_MCP_NAMESPACE: &str = "mcp__sample";
const PLUGIN_MCP_SEARCH_CALL_ID: &str = "plugin-mcp-search";

fn sample_plugin_root(home: &TempDir) -> std::path::PathBuf {
    home.path().join("plugins/cache/test/sample/local")
}

fn write_sample_plugin_manifest_and_config(home: &TempDir) -> std::path::PathBuf {
    let plugin_root = sample_plugin_root(home);
    std::fs::create_dir_all(plugin_root.join(".codex-plugin")).expect("create plugin manifest dir");
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        format!(
            r#"{{"name":"{SAMPLE_PLUGIN_DISPLAY_NAME}","description":"{SAMPLE_PLUGIN_DESCRIPTION}"}}"#
        ),
    )
    .expect("write plugin manifest");
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "[features]\nplugins = true\n\n[plugins.\"{SAMPLE_PLUGIN_CONFIG_NAME}\"]\nenabled = true\n"
        ),
    )
    .expect("write config");
    plugin_root
}

fn write_plugin_skill_plugin(home: &TempDir) -> std::path::PathBuf {
    let plugin_root = write_sample_plugin_manifest_and_config(home);
    let skill_dir = plugin_root.join("skills/sample-search");
    std::fs::create_dir_all(skill_dir.as_path()).expect("create plugin skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: inspect sample data\n---\n\n# body\n",
    )
    .expect("write plugin skill");
    skill_dir.join("SKILL.md")
}

fn write_plugin_mcp_plugin(home: &TempDir, command: &str) {
    let plugin_root = write_sample_plugin_manifest_and_config(home);
    std::fs::write(
        plugin_root.join(".mcp.json"),
        format!(
            r#"{{
  "mcpServers": {{
    "sample": {{
      "command": "{command}",
      "cwd": ".",
      "startup_timeout_sec": 60.0
    }}
  }}
}}"#
        ),
    )
    .expect("write plugin mcp config");
}

async fn mount_plugin_tool_search_turn(server: &MockServer) -> ResponseMock {
    mount_sse_sequence(
        server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_tool_search_call(
                    PLUGIN_MCP_SEARCH_CALL_ID,
                    &serde_json::json!({"query": "echo"}),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
        ],
    )
    .await
}

fn assert_plugin_provenance(tool: &serde_json::Value) {
    let description = tool
        .get("description")
        .and_then(serde_json::Value::as_str)
        .expect("plugin tool description should be present");
    assert!(
        description.contains("This tool is part of plugin `sample`."),
        "expected plugin provenance in tool description: {description:?}"
    );
}

fn searched_plugin_mcp_tool(request: &ResponsesRequest) -> Option<serde_json::Value> {
    let mcp_output = request.tool_search_output(PLUGIN_MCP_SEARCH_CALL_ID);
    namespace_child_tool(&mcp_output, SAMPLE_PLUGIN_MCP_NAMESPACE, "echo").cloned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_plugin_mentions_expose_plugin_mcp_tools() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mock = mount_plugin_tool_search_turn(&server).await;

    let codex_home = Arc::new(TempDir::new()?);
    let rmcp_test_server_bin = match stdio_server_bin() {
        Ok(bin) => bin,
        Err(err) => {
            eprintln!("test_stdio_server binary not available, skipping test: {err}");
            return Ok(());
        }
    };
    write_plugin_skill_plugin(codex_home.as_ref());
    write_plugin_mcp_plugin(codex_home.as_ref(), &rmcp_test_server_bin);

    let mut builder = test_codex()
        .with_home(codex_home)
        .with_auth(CodexAuth::from_api_key("Test API Key"));
    let test_codex = builder
        .build(&server)
        .await
        .expect("create new conversation");
    let codex = Arc::clone(&test_codex.codex);
    wait_for_mcp_server(&codex, "sample").await?;

    codex
        .submit(Op::UserInput {
            items: vec![codex_protocol::user_input::UserInput::Mention {
                name: "sample".into(),
                path: format!("plugin://{SAMPLE_PLUGIN_CONFIG_NAME}"),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = mock.requests();
    let request = &requests[0];
    let developer_messages = request.message_input_texts("developer");
    assert!(
        developer_messages
            .iter()
            .any(|text| text.contains("Skills from this plugin")),
        "expected plugin skills guidance: {developer_messages:?}"
    );
    assert!(
        developer_messages
            .iter()
            .any(|text| text.contains("MCP servers from this plugin")),
        "expected visible plugin MCP guidance: {developer_messages:?}"
    );
    let echo_tool =
        searched_plugin_mcp_tool(&requests[1]).expect("plugin MCP tool should be searchable");
    assert_plugin_provenance(&echo_tool);

    Ok(())
}
