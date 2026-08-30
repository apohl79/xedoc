use std::time::Duration;

use anyhow::Result;
use anyhow::bail;
use app_test_support::ChatGptAuthFixture;
use app_test_support::DEFAULT_CLIENT_NAME;
use app_test_support::TestAppServer;
use app_test_support::start_analytics_events_server;
use app_test_support::to_response;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::PluginAuthPolicy;
use codex_app_server_protocol::PluginInstallParams;
use codex_app_server_protocol::PluginInstallResponse;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

// Plugin install tests wait on connector discovery after the install response path
// starts, which is noticeably slower on Windows CI.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::test]
async fn plugin_install_rejects_relative_marketplace_paths() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "plugin/install",
            Some(serde_json::json!({
                "marketplacePath": "relative-marketplace.json",
                "pluginName": "missing-plugin",
            })),
        )
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("Invalid request"));
    Ok(())
}

#[tokio::test]
async fn plugin_install_returns_invalid_request_for_missing_marketplace_file() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path: AbsolutePathBuf::try_from(
                codex_home.path().join("missing-marketplace.json"),
            )?,
            plugin_name: "missing-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("marketplace file"));
    assert!(err.error.message.contains("does not exist"));
    Ok(())
}

#[tokio::test]
async fn plugin_install_returns_invalid_request_for_not_available_plugin() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        Some("NOT_AVAILABLE"),
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin", &[])?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("not available for install"));
    Ok(())
}

#[tokio::test]
async fn plugin_install_returns_invalid_request_for_disallowed_product_plugin() -> Result<()> {
    let codex_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    std::fs::create_dir_all(repo_root.path().join(".agents/plugins"))?;
    std::fs::write(
        repo_root.path().join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "debug",
  "plugins": [
    {
      "name": "sample-plugin",
      "source": {
        "source": "local",
        "path": "./sample-plugin"
      },
      "policy": {
        "products": ["CHATGPT"]
      }
    }
  ]
}"#,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin", &[])?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_args(&["--session-source", "atlas"])
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;

    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert!(err.error.message.contains("not available for install"));
    Ok(())
}

#[tokio::test]
async fn plugin_install_tracks_analytics_event() -> Result<()> {
    let analytics_server = start_analytics_events_server().await?;
    let codex_home = TempDir::new()?;
    write_analytics_config(codex_home.path(), &analytics_server.uri())?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("account-123")
            .chatgpt_user_id("user-123")
            .chatgpt_account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;

    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin", &[])?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _response: PluginInstallResponse = to_response(response)?;

    let payload = wait_for_plugin_analytics_payload(&analytics_server).await?;
    assert_eq!(
        payload,
        json!({
            "events": [{
                "event_type": "codex_plugin_installed",
                "event_params": {
                    "plugin_id": "sample-plugin@debug",
                    "remote_plugin_id": null,
                    "plugin_name": "sample-plugin",
                    "marketplace_name": "debug",
                    "has_skills": false,
                    "mcp_server_count": 0,
                    "connector_ids": [],
                    "product_client_id": DEFAULT_CLIENT_NAME,
                }
            }]
        })
    );
    Ok(())
}

#[tokio::test]
async fn plugin_install_failure_tracks_analytics_event() -> Result<()> {
    let analytics_server = start_analytics_events_server().await?;
    let codex_home = TempDir::new()?;
    write_analytics_config(codex_home.path(), &analytics_server.uri())?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("account-123")
            .chatgpt_user_id("user-123")
            .chatgpt_account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;

    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./missing-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(err.error.code, -32600);

    let payload = wait_for_plugin_analytics_payload(&analytics_server).await?;
    let event_params = &payload["events"][0]["event_params"];
    assert_eq!(
        payload["events"][0]["event_type"],
        "codex_plugin_install_failed"
    );
    assert_eq!(event_params["plugin_id"], "sample-plugin@debug");
    assert_eq!(event_params["remote_plugin_id"], json!(null));
    assert_eq!(event_params["plugin_name"], "sample-plugin");
    assert_eq!(event_params["marketplace_name"], "debug");
    assert_eq!(event_params["has_skills"], json!(null));
    assert_eq!(event_params["mcp_server_count"], json!(null));
    assert_eq!(event_params["connector_ids"], json!(null));
    assert_eq!(event_params["product_client_id"], DEFAULT_CLIENT_NAME);
    assert_eq!(event_params["source"], "manual");
    assert_eq!(event_params["error_type"], "store_invalid");
    Ok(())
}

#[tokio::test]
async fn plugin_install_starts_mcp_oauth_through_protected_resource_metadata() -> Result<()> {
    let resource_server = MockServer::start().await;
    let authorization_server = MockServer::start().await;
    let resource_metadata_url = format!("{}/oauth-resource", resource_server.uri());
    let challenge = format!("Bearer resource_metadata=\"{resource_metadata_url}\"");
    Mock::given(method("GET"))
        .and(path("/mcp"))
        .respond_with(
            ResponseTemplate::new(401).insert_header("WWW-Authenticate", challenge.as_str()),
        )
        .mount(&resource_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/oauth-resource"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": resource_server.uri(),
            "authorization_servers": [authorization_server.uri()],
        })))
        .mount(&resource_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_endpoint": format!("{}/oauth/authorize", authorization_server.uri()),
            "token_endpoint": format!("{}/oauth/token", authorization_server.uri()),
            "registration_endpoint": format!("{}/oauth/register", authorization_server.uri()),
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["S256"],
        })))
        .mount(&authorization_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/register"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&authorization_server)
        .await;

    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "[features]\nplugins = true\n",
    )?;
    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin", &[])?;
    write_plugin_mcp_config(repo_root.path(), "sample-plugin", &resource_server.uri())?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: PluginInstallResponse = to_response(response)?;
    wait_for_request_count(
        &authorization_server,
        "POST",
        "/oauth/register",
        /*expected_count*/ 1,
    )
    .await?;

    let resource_metadata_requested = resource_server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .any(|request| request.url.path() == "/oauth-resource");
    assert!(resource_metadata_requested);
    Ok(())
}

#[tokio::test]
async fn plugin_install_starts_mcp_oauth_for_api_key_dual_surface_plugin() -> Result<()> {
    let oauth_server = MockServer::start().await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
mcp_oauth_credentials_store = "file"

[features]
plugins = true
"#,
    )?;

    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin", &["sample-mcp"])?;
    write_plugin_mcp_config(repo_root.path(), "sample-plugin", &oauth_server.uri())?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", Some("test-api-key"))])
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: PluginInstallResponse = to_response(response)?;

    assert_eq!(response.auth_policy, PluginAuthPolicy::OnInstall);
    assert!(oauth_discovery_request_count(&oauth_server).await > 0);
    Ok(())
}

#[tokio::test]
async fn plugin_install_makes_bundled_mcp_servers_available_to_followup_requests() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "[features]\nplugins = true\n",
    )?;
    let repo_root = TempDir::new()?;
    write_plugin_marketplace(
        repo_root.path(),
        "debug",
        "sample-plugin",
        "./sample-plugin",
        /*install_policy*/ None,
        /*auth_policy*/ None,
    )?;
    write_plugin_source(repo_root.path(), "sample-plugin", &[])?;
    std::fs::write(
        repo_root.path().join("sample-plugin/.mcp.json"),
        r#"{
  "mcpServers": {
    "sample-mcp": {
      "command": "echo"
    }
  }
}"#,
    )?;
    let marketplace_path =
        AbsolutePathBuf::try_from(repo_root.path().join(".agents/plugins/marketplace.json"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_plugin_install_request(PluginInstallParams {
            marketplace_path,
            plugin_name: "sample-plugin".to_string(),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _response: PluginInstallResponse = to_response(response)?;
    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(!config.contains("[mcp_servers.sample-mcp]"));
    assert!(!config.contains("command = \"echo\""));

    let request_id = mcp
        .send_raw_request(
            "mcpServer/oauth/login",
            Some(json!({
                "name": "sample-mcp",
            })),
        )
        .await?;
    let err = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(err.error.code, -32600);
    assert_eq!(
        err.error.message,
        "OAuth login is only supported for streamable HTTP servers."
    );
    Ok(())
}

fn write_analytics_config(codex_home: &std::path::Path, base_url: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!("chatgpt_base_url = \"{base_url}\"\n"),
    )
}

async fn wait_for_plugin_analytics_payload(server: &MockServer) -> Result<serde_json::Value> {
    timeout(DEFAULT_TIMEOUT, async {
        loop {
            let Some(requests) = server.received_requests().await else {
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            };
            if let Some(request) = requests.iter().find(|request| {
                request.method == "POST"
                    && request
                        .url
                        .path()
                        .ends_with("/codex/analytics-events/events")
            }) {
                return serde_json::from_slice(&request.body)
                    .map_err(|err| anyhow::anyhow!("invalid analytics payload: {err}"));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?
}

async fn oauth_discovery_request_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path().contains("oauth-authorization-server"))
        .count()
}

async fn wait_for_request_count(
    server: &MockServer,
    method_name: &str,
    path_suffix: &str,
    expected_count: usize,
) -> Result<()> {
    timeout(DEFAULT_TIMEOUT, async {
        loop {
            let Some(requests) = server.received_requests().await else {
                bail!("wiremock did not record requests");
            };
            let request_count = requests
                .iter()
                .filter(|request| {
                    request.method == method_name && request.url.path().ends_with(path_suffix)
                })
                .count();
            if request_count == expected_count {
                return Ok::<(), anyhow::Error>(());
            }
            if request_count > expected_count {
                bail!(
                    "expected exactly {expected_count} {method_name} {path_suffix} requests, got {request_count}"
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}

fn write_plugin_marketplace(
    repo_root: &std::path::Path,
    marketplace_name: &str,
    plugin_name: &str,
    source_path: &str,
    install_policy: Option<&str>,
    auth_policy: Option<&str>,
) -> std::io::Result<()> {
    let policy = if install_policy.is_some() || auth_policy.is_some() {
        let installation = install_policy
            .map(|installation| format!("\n        \"installation\": \"{installation}\""))
            .unwrap_or_default();
        let separator = if install_policy.is_some() && auth_policy.is_some() {
            ","
        } else {
            ""
        };
        let authentication = auth_policy
            .map(|authentication| {
                format!("{separator}\n        \"authentication\": \"{authentication}\"")
            })
            .unwrap_or_default();
        format!(",\n      \"policy\": {{{installation}{authentication}\n      }}")
    } else {
        String::new()
    };
    std::fs::create_dir_all(repo_root.join(".git"))?;
    std::fs::create_dir_all(repo_root.join(".agents/plugins"))?;
    std::fs::write(
        repo_root.join(".agents/plugins/marketplace.json"),
        format!(
            r#"{{
  "name": "{marketplace_name}",
  "plugins": [
    {{
      "name": "{plugin_name}",
      "source": {{
        "source": "local",
        "path": "{source_path}"
      }}{policy}
    }}
  ]
}}"#
        ),
    )
}

fn write_plugin_source(
    repo_root: &std::path::Path,
    plugin_name: &str,
    app_ids: &[&str],
) -> Result<()> {
    let plugin_root = repo_root.join(plugin_name);
    std::fs::create_dir_all(plugin_root.join(".codex-plugin"))?;
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"{plugin_name}"}}"#),
    )?;

    let apps = app_ids
        .iter()
        .map(|app_id| ((*app_id).to_string(), json!({ "id": app_id })))
        .collect::<serde_json::Map<_, _>>();
    std::fs::write(
        plugin_root.join(".app.json"),
        serde_json::to_vec_pretty(&json!({ "apps": apps }))?,
    )?;
    Ok(())
}

fn write_plugin_mcp_config(
    repo_root: &std::path::Path,
    plugin_name: &str,
    mcp_base_url: &str,
) -> Result<()> {
    std::fs::write(
        repo_root.join(plugin_name).join(".mcp.json"),
        format!(
            r#"{{
  "mcpServers": {{
    "sample-mcp": {{
      "type": "http",
      "url": "{mcp_base_url}/mcp"
    }}
  }}
}}"#
        ),
    )?;
    Ok(())
}
