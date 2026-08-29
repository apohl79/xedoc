use super::*;
use crate::config::ConfigBuilder;
use crate::environment_selection::TurnEnvironmentState;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context;
use crate::session::tests::make_session_and_context_with_rx;
use crate::session::turn_context::TurnEnvironment;
use crate::state::ActiveTurn;
use crate::test_support::models_manager_with_provider;
use crate::tools::hook_names::HookToolName;
use codex_config::CONFIG_TOML_FILE;
use codex_config::config_toml::ConfigToml;
use codex_config::types::ApprovalsReviewer;
use codex_config::types::McpServerConfig;
use codex_config::types::McpServerToolConfig;
use codex_hooks::Hooks;
use codex_hooks::HooksConfig;
use codex_model_provider::create_model_provider;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::GranularApprovalConfig;
use codex_protocol::protocol::McpInvocation;
use codex_protocol::protocol::SessionSource;
use codex_rollout_trace::ThreadStartedTraceMetadata;
use codex_rollout_trace::ToolDispatchInvocation;
use codex_rollout_trace::ToolDispatchPayload;
use codex_rollout_trace::ToolDispatchRequester;
use codex_rollout_trace::replay_bundle;
use codex_utils_path_uri::PathUri;
use core_test_support::hooks::trusted_config_layer_stack;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;

fn annotations(
    read_only: Option<bool>,
    destructive: Option<bool>,
    open_world: Option<bool>,
) -> ToolAnnotations {
    ToolAnnotations::from_raw(
        /*title*/ None,
        read_only,
        destructive,
        /*idempotent_hint*/ None,
        open_world,
    )
}

fn approval_metadata(
    connector_id: Option<&str>,
    connector_name: Option<&str>,
    connector_description: Option<&str>,
    tool_title: Option<&str>,
    tool_description: Option<&str>,
) -> McpToolApprovalMetadata {
    McpToolApprovalMetadata {
        annotations: None,
        connector_id: connector_id.map(str::to_string),
        link_id: None,
        connector_name: connector_name.map(str::to_string),
        connector_description: connector_description.map(str::to_string),
        connected_account_email: None,
        plugin_id: None,
        tool_title: tool_title.map(str::to_string),
        tool_description: tool_description.map(str::to_string),
        mcp_app_resource_uri: None,
    }
}

fn write_sample_plugin_mcp(codex_home: &std::path::Path) {
    let plugin_root = codex_home.join("plugins/cache/test/sample/local");
    std::fs::create_dir_all(plugin_root.join(".codex-plugin")).expect("create plugin manifest dir");
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        r#"{
  "name": "sample"
}"#,
    )
    .expect("write plugin manifest");
    std::fs::write(
        plugin_root.join(".mcp.json"),
        r#"{
  "mcpServers": {
    "sample": {
      "type": "http",
      "url": "https://sample.example/mcp"
    }
  }
}"#,
    )
    .expect("write plugin mcp config");
}

#[tokio::test]
async fn execute_mcp_tool_call_records_replayable_correlation() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let (mut session, turn_context) = make_session_and_context().await;
    attach_trace_bundle(&mut session, &turn_context, temp.path())?;

    let dispatch_trace = session
        .services
        .rollout_thread_trace
        .start_tool_dispatch_trace(|| {
            Some(ToolDispatchInvocation {
                thread_id: session.thread_id.to_string(),
                codex_turn_id: turn_context.sub_id.clone(),
                tool_call_id: "mcp-call".to_string(),
                tool_name: "search".to_string(),
                tool_namespace: Some("mcp__docs__".to_string()),
                requester: ToolDispatchRequester::Model {
                    model_visible_call_id: "mcp-call".to_string(),
                },
                payload: ToolDispatchPayload::Function {
                    arguments: r#"{"query":"trace"}"#.to_string(),
                },
            })
        });
    assert!(dispatch_trace.is_enabled());
    let turn_context = Arc::new(turn_context);
    let step_context = StepContext::for_test(Arc::clone(&turn_context));

    let result = execute_mcp_tool_call(
        &session,
        step_context.as_ref(),
        "mcp-call",
        &McpInvocation {
            server: "docs".to_string(),
            tool: "search".to_string(),
            arguments: Some(serde_json::json!({ "query": "trace" })),
        },
        /*rewritten_arguments*/ None,
        /*metadata*/ None,
    )
    .await;
    assert!(
        result.is_err(),
        "the synthetic backend is absent; only trace emission matters",
    );

    let replayed = replay_bundle(single_bundle_dir(temp.path())?)?;
    assert!(
        replayed.tool_calls["mcp-call"].mcp_call_id.is_some(),
        "the real MCP execution path should emit a reducer-visible correlation ID",
    );

    Ok(())
}

fn install_mcp_permission_request_hook(
    session: &mut Session,
    turn_context: &TurnContext,
    matcher: &str,
    hook_output: &serde_json::Value,
) -> std::path::PathBuf {
    let script_path = turn_context
        .config
        .codex_home
        .join("mcp_permission_request_hook.py");
    let log_path = turn_context
        .config
        .codex_home
        .join("mcp_permission_request_hook_log.jsonl");
    let hook_output = hook_output.to_string();
    std::fs::create_dir_all(&turn_context.config.codex_home)
        .expect("create codex home for MCP permission hook");
    let script = format!(
        r#"import json
from pathlib import Path
import sys

payload = json.load(sys.stdin)
with Path(r"{log_path}").open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(payload) + "\n")

print({hook_output:?})
"#,
        log_path = log_path.display(),
        hook_output = hook_output,
    );

    std::fs::write(&script_path, script).expect("write MCP permission hook script");
    let python = if cfg!(windows) { "python" } else { "python3" };
    let script_path_arg = if cfg!(windows) {
        script_path.display().to_string()
    } else {
        format!(
            "'{}'",
            script_path.display().to_string().replace('\'', "'\\''")
        )
    };
    std::fs::write(
        turn_context.config.codex_home.join("hooks.json"),
        serde_json::json!({
            "hooks": {
                "PermissionRequest": [{
                    "matcher": matcher,
                    "hooks": [{
                        "type": "command",
                        "command": format!("{python} {script_path_arg}"),
                        "timeout_sec": 5,
                    }]
                }]
            }
        })
        .to_string(),
    )
    .expect("write hooks.json");
    let hook_list = codex_hooks::list_hooks(HooksConfig {
        feature_enabled: true,
        config_layer_stack: Some(turn_context.config.config_layer_stack.clone()),
        ..HooksConfig::default()
    });
    assert_eq!(hook_list.hooks.len(), 1);
    let trusted_config_layer_stack = trusted_config_layer_stack(
        &turn_context.config.config_layer_stack,
        &turn_context.config.codex_home,
        hook_list.hooks,
    );

    session
        .services
        .hooks
        .store(Arc::new(Hooks::new(HooksConfig {
            feature_enabled: true,
            config_layer_stack: Some(trusted_config_layer_stack),
            shell_program: (!cfg!(windows)).then_some("/bin/sh".to_string()),
            shell_args: if cfg!(windows) {
                Vec::new()
            } else {
                vec!["-c".to_string()]
            },
            ..HooksConfig::default()
        })));

    log_path.to_path_buf()
}

/// Attaches a replayable rollout bundle to one synthetic session under test.
fn attach_trace_bundle(
    session: &mut Session,
    turn_context: &TurnContext,
    root: &Path,
) -> anyhow::Result<()> {
    let rollout_thread_trace =
        codex_rollout_trace::ThreadTraceContext::start_root_in_root_for_test(
            root,
            ThreadStartedTraceMetadata {
                thread_id: session.thread_id.to_string(),
                agent_path: "/root".to_string(),
                task_name: None,
                nickname: None,
                agent_role: None,
                session_source: SessionSource::Exec,
                cwd: PathBuf::from("/workspace"),
                rollout_path: None,
                model: "gpt-test".to_string(),
                provider_name: "test-provider".to_string(),
                approval_policy: "never".to_string(),
                sandbox_policy: "danger-full-access".to_string(),
            },
        )?;
    rollout_thread_trace.record_codex_turn_started(turn_context.sub_id.as_str());
    session.services.rollout_thread_trace = rollout_thread_trace;
    Ok(())
}

/// Returns the sole bundle emitted under a temporary rollout trace root.
fn single_bundle_dir(root: &Path) -> anyhow::Result<PathBuf> {
    let mut entries = fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    assert_eq!(entries.len(), 1);
    Ok(entries.remove(0))
}

#[tokio::test]
async fn mcp_sandbox_cwd_uses_matching_server_environment_uri() -> anyhow::Result<()> {
    let (_, mut turn_context) = make_session_and_context().await;
    let secondary_cwd = PathUri::parse("file:///C:/remote/project")?;
    let environment = turn_context
        .environments
        .primary()
        .expect("primary environment")
        .environment
        .clone();
    turn_context
        .environments
        .environments
        .push(TurnEnvironmentState::Ready(TurnEnvironment::new(
            "remote".to_string(),
            environment,
            secondary_cwd.clone(),
            Vec::new(),
            /*shell*/ None,
        )));

    let step_context = StepContext::for_test(Arc::new(turn_context));
    let sandbox_cwd = sandbox_cwd_for_mcp_server(&step_context, "remote");

    assert_eq!(sandbox_cwd, Some(secondary_cwd));
    Ok(())
}

#[tokio::test]
async fn mcp_sandbox_cwd_is_none_for_unselected_server_environment() -> anyhow::Result<()> {
    let (_, turn_context) = make_session_and_context().await;

    let step_context = StepContext::for_test(Arc::new(turn_context));
    let sandbox_cwd = sandbox_cwd_for_mcp_server(&step_context, "remote");

    assert_eq!(sandbox_cwd, None);
    Ok(())
}

#[test]
fn guardian_mcp_review_request_includes_invocation_metadata() {
    let invocation = McpInvocation {
        server: "playwright".to_string(),
        tool: "browser_navigate".to_string(),
        arguments: Some(serde_json::json!({
            "url": "https://example.com",
        })),
    };

    let mut metadata = approval_metadata(
        Some("playwright"),
        Some("Playwright"),
        Some("Browser automation"),
        Some("Navigate"),
        Some("Open a page"),
    );
    metadata.connected_account_email = Some("owner@example.com".to_string());
    let request = build_guardian_mcp_tool_review_request("call-1", &invocation, Some(&metadata));

    assert_eq!(
        request,
        GuardianApprovalRequest::McpToolCall {
            id: "call-1".to_string(),
            server: "playwright".to_string(),
            tool_name: "browser_navigate".to_string(),
            arguments: Some(serde_json::json!({
                "url": "https://example.com",
            })),
            connector_id: Some("playwright".to_string()),
            connector_name: Some("Playwright".to_string()),
            connector_description: Some("Browser automation".to_string()),
            connected_account_email: Some("owner@example.com".to_string()),
            tool_title: Some("Navigate".to_string()),
            tool_description: Some("Open a page".to_string()),
            annotations: None,
        }
    );
}

#[test]
fn guardian_mcp_review_request_includes_annotations_when_present() {
    let invocation = McpInvocation {
        server: "custom_server".to_string(),
        tool: "dangerous_tool".to_string(),
        arguments: None,
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(Some(false), Some(true), Some(true))),
        connector_id: None,
        link_id: None,
        connector_name: None,
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: None,
        tool_description: None,
        mcp_app_resource_uri: None,
    };

    let request = build_guardian_mcp_tool_review_request("call-1", &invocation, Some(&metadata));

    assert_eq!(
        request,
        GuardianApprovalRequest::McpToolCall {
            id: "call-1".to_string(),
            server: "custom_server".to_string(),
            tool_name: "dangerous_tool".to_string(),
            arguments: None,
            connector_id: None,
            connector_name: None,
            connector_description: None,
            connected_account_email: None,
            tool_title: None,
            tool_description: None,
            annotations: Some(GuardianMcpAnnotations {
                destructive_hint: Some(true),
                open_world_hint: Some(true),
                read_only_hint: Some(false),
            }),
        }
    );
}

#[test]
fn guardian_mcp_review_request_ignores_untrusted_connected_account_email() {
    let invocation = McpInvocation {
        server: "custom_server".to_string(),
        tool: "dangerous_tool".to_string(),
        arguments: None,
    };
    let mut metadata = approval_metadata(
        /*connector_id*/ None, /*connector_name*/ None,
        /*connector_description*/ None, /*tool_title*/ None,
        /*tool_description*/ None,
    );
    metadata.connected_account_email = Some("spoofed@example.com".to_string());

    let request = build_guardian_mcp_tool_review_request("call-1", &invocation, Some(&metadata));

    assert_eq!(
        request,
        GuardianApprovalRequest::McpToolCall {
            id: "call-1".to_string(),
            server: "custom_server".to_string(),
            tool_name: "dangerous_tool".to_string(),
            arguments: None,
            connector_id: None,
            connector_name: None,
            connector_description: None,
            connected_account_email: None,
            tool_title: None,
            tool_description: None,
            annotations: None,
        }
    );
}

#[test]
fn guardian_review_decision_maps_to_mcp_tool_decision() {
    assert_eq!(
        mcp_tool_approval_decision_from_guardian(ReviewDecision::Approved),
        McpToolApprovalDecision::Accept
    );
    let denial = mcp_tool_approval_decision_from_guardian(ReviewDecision::denied(
        "This action was rejected due to unacceptable risk.\nReason: too risky\nThe agent must not attempt to achieve the same outcome",
    ));
    let McpToolApprovalDecision::Decline {
        message: Some(message),
    } = denial
    else {
        panic!("guardian denial should carry a rejection message");
    };
    assert!(message.contains("Reason: too risky"));
    assert!(message.contains("The agent must not attempt to achieve the same outcome"));
    let timeout = mcp_tool_approval_decision_from_guardian(ReviewDecision::TimedOut);
    let McpToolApprovalDecision::Decline {
        message: Some(message),
    } = timeout
    else {
        panic!("guardian timeout should carry a timeout message");
    };
    assert!(message.contains("did not finish before its deadline"));
    assert!(!message.contains("unacceptable risk"));
    assert_eq!(
        mcp_tool_approval_decision_from_guardian(ReviewDecision::Abort),
        McpToolApprovalDecision::Decline { message: None }
    );
}

#[tokio::test]
async fn persist_custom_mcp_tool_approval_writes_tool_override() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        "[mcp_servers.docs]\ncommand = \"docs-server\"\n",
    )
    .expect("seed config");
    let config = ConfigBuilder::default()
        .codex_home(tmp.path().to_path_buf())
        .build()
        .await
        .expect("load config");

    persist_custom_mcp_tool_approval(&config, "docs", "search")
        .await
        .expect("persist approval");

    let contents = std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).expect("read config");
    let parsed: ConfigToml = toml::from_str(&contents).expect("parse config");
    let tool = parsed
        .mcp_servers
        .get("docs")
        .and_then(|server| server.tools.get("search"))
        .expect("docs/search tool config exists");

    assert_eq!(
        tool,
        &McpServerToolConfig {
            approval_mode: Some(AppToolApproval::Approve),
        }
    );
    assert!(contents.contains("[mcp_servers.docs.tools.search]"));
}

#[tokio::test]
async fn custom_mcp_tool_approval_mode_uses_server_default_with_tool_override() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        r#"
[mcp_servers.docs]
command = "docs-server"
default_tools_approval_mode = "approve"

[mcp_servers.docs.tools.search]
approval_mode = "prompt"
"#,
    )
    .expect("seed config");
    let config = ConfigBuilder::default()
        .codex_home(tmp.path().to_path_buf())
        .build()
        .await
        .expect("load config");
    let (session, mut turn_context) = make_session_and_context().await;
    turn_context.config = Arc::new(config);

    assert_eq!(
        custom_mcp_tool_approval_mode(&session, &turn_context, "docs", "read").await,
        AppToolApproval::Approve
    );
    assert_eq!(
        custom_mcp_tool_approval_mode(&session, &turn_context, "docs", "search").await,
        AppToolApproval::Prompt
    );
    assert_eq!(
        custom_mcp_tool_approval_mode(&session, &turn_context, "unknown", "search").await,
        AppToolApproval::Auto
    );
}

#[tokio::test]
async fn custom_mcp_tool_approval_mode_uses_plugin_mcp_policy() {
    let (session, mut turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    write_sample_plugin_mcp(codex_home.as_path());
    std::fs::write(
        codex_home.join(CONFIG_TOML_FILE),
        r#"
[features]
plugins = true

[plugins."sample@test"]
enabled = true

[plugins."sample@test".mcp_servers.sample]
default_tools_approval_mode = "prompt"

[plugins."sample@test".mcp_servers.sample.tools.search]
approval_mode = "approve"
"#,
    )
    .expect("seed config");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .build()
        .await
        .expect("load config");
    turn_context.config = Arc::new(config);
    session.services.plugins_manager.clear_cache();

    assert_eq!(
        custom_mcp_tool_approval_mode(&session, &turn_context, "sample", "read").await,
        AppToolApproval::Prompt
    );
    assert_eq!(
        custom_mcp_tool_approval_mode(&session, &turn_context, "sample", "search").await,
        AppToolApproval::Approve
    );
}

#[tokio::test]
async fn custom_mcp_tool_approval_mode_uses_updated_plugin_mcp_policy_after_cache_warm() {
    let (session, mut turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    write_sample_plugin_mcp(codex_home.as_path());
    std::fs::write(
        codex_home.join(CONFIG_TOML_FILE),
        r#"
[features]
plugins = true

[plugins."sample@test"]
enabled = true
"#,
    )
    .expect("seed config");
    let initial_config = ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .build()
        .await
        .expect("load initial config");
    session
        .services
        .plugins_manager
        .plugins_for_config(&initial_config.plugins_config_input())
        .await;
    std::fs::write(
        codex_home.join(CONFIG_TOML_FILE),
        r#"
[features]
plugins = true

[plugins."sample@test"]
enabled = true

[plugins."sample@test".mcp_servers.sample.tools.search]
approval_mode = "approve"
"#,
    )
    .expect("update config");
    let updated_config = ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .build()
        .await
        .expect("load updated config");
    turn_context.config = Arc::new(updated_config);

    assert_eq!(
        custom_mcp_tool_approval_mode(&session, &turn_context, "sample", "search").await,
        AppToolApproval::Approve
    );
}

#[tokio::test]
async fn maybe_persist_mcp_tool_approval_reloads_session_config_for_custom_server() {
    let (session, mut turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    std::fs::write(
        codex_home.join(CONFIG_TOML_FILE),
        "[mcp_servers.docs]\ncommand = \"docs-server\"\n",
    )
    .expect("seed config");
    let config = crate::config_test_support::config_builder_without_managed_config()
        .codex_home(codex_home.clone().to_path_buf())
        .build()
        .await
        .expect("load config");
    turn_context.config = Arc::new(config);
    let key = McpToolApprovalKey {
        server: "docs".to_string(),
        connector_id: None,
        tool_name: "search".to_string(),
    };

    maybe_persist_mcp_tool_approval(&session, &turn_context, key.clone()).await;

    let config = session.get_config().await;
    let mcp_servers_toml = config
        .config_layer_stack
        .effective_config()
        .as_table()
        .and_then(|table| table.get("mcp_servers"))
        .cloned()
        .expect("mcp_servers table");
    let mcp_servers = HashMap::<String, McpServerConfig>::deserialize(mcp_servers_toml)
        .expect("deserialize MCP servers");
    let tool = mcp_servers
        .get("docs")
        .and_then(|server| server.tools.get("search"))
        .expect("docs/search tool config exists");

    assert_eq!(
        tool,
        &McpServerToolConfig {
            approval_mode: Some(AppToolApproval::Approve),
        }
    );
    assert_eq!(mcp_tool_approval_is_remembered(&session, &key).await, true);
}

#[tokio::test]
async fn maybe_persist_mcp_tool_approval_writes_plugin_mcp_policy() {
    let (session, mut turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    write_sample_plugin_mcp(codex_home.as_path());
    std::fs::write(
        codex_home.join(CONFIG_TOML_FILE),
        r#"
[features]
plugins = true

[plugins."sample@test"]
enabled = true
"#,
    )
    .expect("seed config");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .build()
        .await
        .expect("load config");
    turn_context.config = Arc::new(config);
    session.services.plugins_manager.clear_cache();
    let key = McpToolApprovalKey {
        server: "sample".to_string(),
        connector_id: None,
        tool_name: "search".to_string(),
    };

    maybe_persist_mcp_tool_approval(&session, &turn_context, key.clone()).await;

    let contents = std::fs::read_to_string(codex_home.join(CONFIG_TOML_FILE)).expect("read config");
    let parsed: ConfigToml = toml::from_str(&contents).expect("parse config");
    let tool = parsed
        .plugins
        .get("sample@test")
        .and_then(|plugin| plugin.mcp_servers.get("sample"))
        .and_then(|server| server.tools.get("search"))
        .expect("sample/search tool config exists");

    assert_eq!(
        tool,
        &McpServerToolConfig {
            approval_mode: Some(AppToolApproval::Approve),
        }
    );
    assert!(contents.contains(r#"[plugins."sample@test".mcp_servers.sample.tools.search]"#));
    assert_eq!(mcp_tool_approval_is_remembered(&session, &key).await, true);
}

#[tokio::test]
async fn maybe_persist_mcp_tool_approval_writes_project_config_for_project_server() {
    let (session, mut turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    let project_dir = tempdir().expect("tempdir");
    std::fs::write(project_dir.path().join(".git"), "gitdir: nowhere").expect("seed git marker");
    let project_codex_dir = project_dir.path().join(".codex");
    std::fs::create_dir_all(&project_codex_dir).expect("create project .codex dir");
    std::fs::write(
        project_codex_dir.join(CONFIG_TOML_FILE),
        "[mcp_servers.docs]\ncommand = \"docs-server\"\n",
    )
    .expect("seed project config");
    ConfigEditsBuilder::new(&codex_home)
        .set_project_trust_level(
            project_dir.path(),
            codex_protocol::config_types::TrustLevel::Trusted,
        )
        .apply()
        .await
        .expect("trust project");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .fallback_cwd(Some(project_dir.path().to_path_buf()))
        .build()
        .await
        .expect("load project config");
    turn_context.config = Arc::new(config);
    let key = McpToolApprovalKey {
        server: "docs".to_string(),
        connector_id: None,
        tool_name: "search".to_string(),
    };

    maybe_persist_mcp_tool_approval(&session, &turn_context, key.clone()).await;

    let contents = std::fs::read_to_string(project_codex_dir.join(CONFIG_TOML_FILE))
        .expect("read project config");
    let parsed: ConfigToml = toml::from_str(&contents).expect("parse project config");
    let tool = parsed
        .mcp_servers
        .get("docs")
        .and_then(|server| server.tools.get("search"))
        .expect("docs/search tool config exists");

    assert_eq!(
        tool,
        &McpServerToolConfig {
            approval_mode: Some(AppToolApproval::Approve),
        }
    );
    assert!(contents.contains("[mcp_servers.docs.tools.search]"));
    assert_eq!(mcp_tool_approval_is_remembered(&session, &key).await, true);
}

#[tokio::test]
async fn approve_mode_skips_when_annotations_do_not_require_approval() {
    let (session, turn_context) = make_session_and_context().await;
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let invocation = McpInvocation {
        server: "custom_server".to_string(),
        tool: "read_only_tool".to_string(),
        arguments: None,
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(
            Some(true),
            /*destructive*/ None,
            /*open_world*/ None,
        )),
        connector_id: None,
        link_id: None,
        connector_name: None,
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: Some("Read Only Tool".to_string()),
        tool_description: None,
        mcp_app_resource_uri: None,
    };

    let decision = maybe_request_mcp_tool_approval(
        &session,
        &StepContext::for_test(Arc::clone(&turn_context)),
        "call-1",
        &invocation,
        &HookToolName::new("mcp__test__tool"),
        Some(&metadata),
        AppToolApproval::Approve,
    )
    .await;

    assert_eq!(decision, None);
}

#[tokio::test]
async fn guardian_mode_skips_auto_when_annotations_do_not_require_approval() {
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let server = start_mock_server().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let (mut session, mut turn_context) = make_session_and_context().await;
    turn_context
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("test setup should allow updating approval policy");
    let mut config = (*turn_context.config).clone();
    config.model_provider.base_url = Some(format!("{}/v1", server.uri()));
    config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    let config = Arc::new(config);
    let models_manager = models_manager_with_provider(
        config.codex_home.to_path_buf(),
        Arc::clone(&session.services.auth_manager),
        config.model_provider.clone(),
    );
    session.services.models_manager = models_manager;
    turn_context.config = Arc::clone(&config);
    turn_context.provider = create_model_provider(
        config.model_provider.clone(),
        turn_context.auth_manager.clone(),
    );

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let invocation = McpInvocation {
        server: "custom_server".to_string(),
        tool: "read_only_tool".to_string(),
        arguments: None,
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(
            Some(true),
            /*destructive*/ None,
            /*open_world*/ None,
        )),
        connector_id: None,
        link_id: None,
        connector_name: None,
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: Some("Read Only Tool".to_string()),
        tool_description: None,
        mcp_app_resource_uri: None,
    };

    let decision = maybe_request_mcp_tool_approval(
        &session,
        &StepContext::for_test(Arc::clone(&turn_context)),
        "call-guardian",
        &invocation,
        &HookToolName::new("mcp__test__tool"),
        Some(&metadata),
        AppToolApproval::Auto,
    )
    .await;

    assert_eq!(decision, None);
}

#[tokio::test]
async fn permission_request_hook_allows_mcp_tool_call() {
    let (mut session, turn_context) = make_session_and_context().await;
    let log_path = install_mcp_permission_request_hook(
        &mut session,
        &turn_context,
        "mcp__memory__.*",
        &serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "allow" }
            }
        }),
    );
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let invocation = McpInvocation {
        server: "memory".to_string(),
        tool: "create_entities".to_string(),
        arguments: Some(serde_json::json!({
            "entities": [{
                "name": "Ada",
                "entityType": "person"
            }]
        })),
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(
            Some(false),
            Some(true),
            /*open_world*/ None,
        )),
        connector_id: None,
        link_id: None,
        connector_name: None,
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: Some("Create entities".to_string()),
        tool_description: None,
        mcp_app_resource_uri: None,
    };

    let decision = maybe_request_mcp_tool_approval(
        &session,
        &StepContext::for_test(Arc::clone(&turn_context)),
        "call-mcp-hook",
        &invocation,
        &HookToolName::new("mcp__memory__create_entities"),
        Some(&metadata),
        AppToolApproval::Auto,
    )
    .await;

    assert_eq!(decision, Some(McpToolApprovalDecision::Accept));
    let log = std::fs::read_to_string(log_path).expect("read MCP permission hook log");
    let inputs = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("parse hook input"))
        .collect::<Vec<_>>();
    #[allow(deprecated)]
    let turn_cwd = turn_context.cwd.clone();
    assert_eq!(
        inputs,
        vec![serde_json::json!({
            "session_id": session.session_id(),
            "turn_id": "turn_id",
            "cwd": turn_cwd,
            "transcript_path": null,
            "model": turn_context.model_info.slug,
            "permission_mode": "default",
            "tool_name": "mcp__memory__create_entities",
            "hook_event_name": "PermissionRequest",
            "tool_input": {
                "entities": [{
                    "name": "Ada",
                    "entityType": "person"
                }]
            }
        })]
    );
}

#[tokio::test]
async fn permission_request_hook_uses_hook_tool_name_without_metadata() {
    let (mut session, turn_context) = make_session_and_context().await;
    let log_path = install_mcp_permission_request_hook(
        &mut session,
        &turn_context,
        "mcp__memory__.*",
        &serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "allow" }
            }
        }),
    );
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let invocation = McpInvocation {
        server: "memory".to_string(),
        tool: "create_entities".to_string(),
        arguments: Some(serde_json::json!({ "entities": [] })),
    };

    let decision = maybe_request_mcp_tool_approval(
        &session,
        &StepContext::for_test(Arc::clone(&turn_context)),
        "call-mcp-hook-no-metadata",
        &invocation,
        &HookToolName::new("mcp__memory__create_entities"),
        /*metadata*/ None,
        AppToolApproval::Auto,
    )
    .await;

    assert_eq!(decision, Some(McpToolApprovalDecision::Accept));
    let log = std::fs::read_to_string(log_path).expect("read MCP permission hook log");
    let inputs = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("parse hook input"))
        .collect::<Vec<_>>();
    #[allow(deprecated)]
    let turn_cwd = turn_context.cwd.clone();
    assert_eq!(
        inputs,
        vec![serde_json::json!({
            "session_id": session.session_id(),
            "turn_id": "turn_id",
            "cwd": turn_cwd,
            "transcript_path": null,
            "model": turn_context.model_info.slug,
            "permission_mode": "default",
            "tool_name": "mcp__memory__create_entities",
            "hook_event_name": "PermissionRequest",
            "tool_input": { "entities": [] }
        })]
    );
}

#[tokio::test]
async fn permission_request_hook_runs_after_remembered_mcp_approval() {
    let (mut session, turn_context) = make_session_and_context().await;
    let log_path = install_mcp_permission_request_hook(
        &mut session,
        &turn_context,
        "mcp__memory__.*",
        &serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": {
                    "behavior": "deny",
                    "message": "should be skipped"
                }
            }
        }),
    );
    let invocation = McpInvocation {
        server: "memory".to_string(),
        tool: "create_entities".to_string(),
        arguments: Some(serde_json::json!({ "entities": [] })),
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(
            Some(false),
            Some(true),
            /*open_world*/ None,
        )),
        connector_id: None,
        link_id: None,
        connector_name: None,
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: Some("Create entities".to_string()),
        tool_description: None,
        mcp_app_resource_uri: None,
    };
    let remembered_key =
        session_mcp_tool_approval_key(&invocation, Some(&metadata), AppToolApproval::Auto)
            .expect("memory MCP tool should support session approval");
    remember_mcp_tool_approval(&session, remembered_key).await;

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let decision = maybe_request_mcp_tool_approval(
        &session,
        &StepContext::for_test(Arc::clone(&turn_context)),
        "call-mcp-remembered",
        &invocation,
        &HookToolName::new("mcp__memory__create_entities"),
        Some(&metadata),
        AppToolApproval::Auto,
    )
    .await;

    assert_eq!(decision, Some(McpToolApprovalDecision::Accept));
    assert!(
        !log_path.exists(),
        "remembered approval should skip PermissionRequest hooks"
    );
}

#[tokio::test]
async fn guardian_mode_mcp_denial_returns_rationale_message() {
    let server = start_mock_server().await;
    let guardian_request_log = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-guardian"),
            ev_assistant_message(
                "msg-guardian",
                &serde_json::json!({
                    "risk_level": "high",
                    "user_authorization": "low",
                    "outcome": "deny",
                    "rationale": "The tool call would expose private calendar data without clear user authorization.",
                })
                .to_string(),
            ),
            ev_completed("resp-guardian"),
        ]),
    )
    .await;

    let (mut session, mut turn_context) = make_session_and_context().await;
    turn_context
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("test setup should allow updating approval policy");
    let mut config = (*turn_context.config).clone();
    config.model_provider.base_url = Some(format!("{}/v1", server.uri()));
    config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    let config = Arc::new(config);
    let models_manager = models_manager_with_provider(
        config.codex_home.to_path_buf(),
        Arc::clone(&session.services.auth_manager),
        config.model_provider.clone(),
    );
    session.services.models_manager = models_manager;
    turn_context.config = Arc::clone(&config);
    turn_context.provider = create_model_provider(
        config.model_provider.clone(),
        turn_context.auth_manager.clone(),
    );

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let invocation = McpInvocation {
        server: "custom_server".to_string(),
        tool: "dangerous_tool".to_string(),
        arguments: Some(serde_json::json!({ "calendar_id": "primary" })),
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(Some(false), Some(true), Some(true))),
        connector_id: None,
        link_id: None,
        connector_name: None,
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: Some("Dangerous Tool".to_string()),
        tool_description: Some("Reads calendar data.".to_string()),
        mcp_app_resource_uri: None,
    };

    let decision = maybe_request_mcp_tool_approval(
        &session,
        &StepContext::for_test(Arc::clone(&turn_context)),
        "call-guardian-deny",
        &invocation,
        &HookToolName::new("mcp__test__tool"),
        Some(&metadata),
        AppToolApproval::Auto,
    )
    .await;

    let Some(McpToolApprovalDecision::Decline {
        message: Some(message),
    }) = decision
    else {
        panic!("guardian-denied MCP approval should carry a rejection message");
    };
    assert!(message.contains("Reason: The tool call would expose private calendar data"));
    assert!(message.contains("policy circumvention"));
    assert_eq!(
        guardian_request_log.single_request().path(),
        "/v1/responses"
    );
}

#[tokio::test]
async fn prompt_mode_waits_for_approval_when_annotations_do_not_require_approval() {
    let (session, turn_context, _rx_event) = make_session_and_context_with_rx().await;
    {
        let mut active_turn = session.active_turn.lock().await;
        *active_turn = Some(ActiveTurn::default());
    }
    let invocation = McpInvocation {
        server: "custom_server".to_string(),
        tool: "read_only_tool".to_string(),
        arguments: None,
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(
            Some(true),
            /*destructive*/ None,
            /*open_world*/ None,
        )),
        connector_id: None,
        link_id: None,
        connector_name: None,
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: Some("Read Only Tool".to_string()),
        tool_description: None,
        mcp_app_resource_uri: None,
    };

    let mut approval_task = {
        let session = Arc::clone(&session);
        let turn_context = Arc::clone(&turn_context);
        tokio::spawn(async move {
            maybe_request_mcp_tool_approval(
                &session,
                &StepContext::for_test(Arc::clone(&turn_context)),
                "call-prompt",
                &invocation,
                &HookToolName::new("mcp__test__tool"),
                Some(&metadata),
                AppToolApproval::Prompt,
            )
            .await
        })
    };

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), &mut approval_task)
            .await
            .is_err(),
        "prompt mode should wait for approval instead of auto-allowing"
    );
    approval_task.abort();
}

#[tokio::test]
async fn full_access_mode_skips_mcp_tool_approval_for_all_approval_modes() {
    let (session, mut turn_context) = make_session_and_context().await;
    turn_context
        .approval_policy
        .set(AskForApproval::Never)
        .expect("test setup should allow updating approval policy");
    turn_context.permission_profile = PermissionProfile::Disabled;

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let invocation = McpInvocation {
        server: "playwright".to_string(),
        tool: "dangerous_tool".to_string(),
        arguments: Some(serde_json::json!({ "id": 1 })),
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(Some(false), Some(true), Some(true))),
        connector_id: Some("calendar".to_string()),
        link_id: None,
        connector_name: Some("Calendar".to_string()),
        connector_description: Some("Manage events".to_string()),
        connected_account_email: None,
        plugin_id: None,
        tool_title: Some("Dangerous Tool".to_string()),
        tool_description: Some("Performs a risky action.".to_string()),
        mcp_app_resource_uri: None,
    };

    for approval_mode in [
        AppToolApproval::Auto,
        AppToolApproval::Prompt,
        AppToolApproval::Approve,
    ] {
        let decision = maybe_request_mcp_tool_approval(
            &session,
            &StepContext::for_test(Arc::clone(&turn_context)),
            "call-2",
            &invocation,
            &HookToolName::new("mcp__test__tool"),
            Some(&metadata),
            approval_mode,
        )
        .await;

        assert_eq!(decision, None);
    }
}

#[tokio::test]
async fn approve_mode_skips_guardian_in_every_permission_mode() {
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let server = start_mock_server().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let invocation = McpInvocation {
        server: "playwright".to_string(),
        tool: "dangerous_tool".to_string(),
        arguments: Some(serde_json::json!({ "id": 1 })),
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(Some(false), Some(true), Some(true))),
        connector_id: Some("calendar".to_string()),
        link_id: None,
        connector_name: Some("Calendar".to_string()),
        connector_description: Some("Manage events".to_string()),
        connected_account_email: None,
        plugin_id: None,
        tool_title: Some("Dangerous Tool".to_string()),
        tool_description: Some("Performs a risky action.".to_string()),
        mcp_app_resource_uri: None,
    };

    for approval_policy in [
        AskForApproval::UnlessTrusted,
        AskForApproval::OnRequest,
        AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }),
        AskForApproval::Never,
    ] {
        let (mut session, mut turn_context) = make_session_and_context().await;
        turn_context.auth_manager = Some(crate::test_support::auth_manager_from_auth(
            codex_login::CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        ));
        turn_context
            .approval_policy
            .set(approval_policy)
            .expect("test setup should allow updating approval policy");
        let mut config = (*turn_context.config).clone();
        config.chatgpt_base_url = server.uri();
        config.model_provider.base_url = Some(format!("{}/v1", server.uri()));
        config.approvals_reviewer = ApprovalsReviewer::User;
        let config = Arc::new(config);
        let models_manager = models_manager_with_provider(
            config.codex_home.to_path_buf(),
            Arc::clone(&session.services.auth_manager),
            config.model_provider.clone(),
        );
        session.services.models_manager = models_manager;
        turn_context.config = Arc::clone(&config);
        turn_context.provider = create_model_provider(
            config.model_provider.clone(),
            turn_context.auth_manager.clone(),
        );

        let session = Arc::new(session);
        let turn_context = Arc::new(turn_context);
        let decision = maybe_request_mcp_tool_approval(
            &session,
            &StepContext::for_test(Arc::clone(&turn_context)),
            "call-3",
            &invocation,
            &HookToolName::new("mcp__test__tool"),
            Some(&metadata),
            AppToolApproval::Approve,
        )
        .await;

        assert_eq!(decision, None);
    }
}
