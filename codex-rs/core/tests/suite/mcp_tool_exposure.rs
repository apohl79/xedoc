use anyhow::Result;
use codex_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID;
use codex_config::types::McpServerConfig;
use codex_config::types::McpServerTransportConfig;
use codex_core::config::Config;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadStartInput;
use codex_mcp::McpResourceClient;
use codex_protocol::protocol::McpServerRefreshConfig;
use codex_protocol::protocol::Op;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

struct McpResourceClientCapture {
    client: Arc<Mutex<Option<McpResourceClient>>>,
}

impl ThreadLifecycleContributor<Config> for McpResourceClientCapture {
    fn on_thread_start<'a>(
        &'a self,
        input: ThreadStartInput<'a, Config>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let client = input
                .session_store
                .get::<McpResourceClient>()
                .expect("session store should contain an MCP resource client");
            *self
                .client
                .lock()
                .expect("capture lock should not be poisoned") = Some(client.as_ref().clone());
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_resource_client_follows_published_mcp_runtime() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    let captured_client = Arc::new(Mutex::new(None));
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.thread_lifecycle_contributor(Arc::new(McpResourceClientCapture {
        client: Arc::clone(&captured_client),
    }));
    let test = core_test_support::test_codex::test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .build(&server)
        .await?;
    let resource_client = captured_client
        .lock()
        .expect("capture lock should not be poisoned")
        .clone()
        .expect("thread start should capture the MCP resource client");
    assert!(!resource_client.has_server("refreshed").await);

    let refreshed_server = McpServerConfig {
        transport: McpServerTransportConfig::StreamableHttp {
            url: format!("{}/mcp", server.uri()),
            bearer_token_env_var: None,
            http_headers: None,
            env_http_headers: None,
        },
        auth: Default::default(),
        environment_id: DEFAULT_MCP_SERVER_ENVIRONMENT_ID.to_string(),
        enabled: true,
        required: false,
        supports_parallel_tool_calls: false,
        disabled_reason: None,
        startup_timeout_sec: Some(Duration::from_millis(100)),
        tool_timeout_sec: None,
        default_tools_approval_mode: None,
        enabled_tools: None,
        disabled_tools: None,
        scopes: None,
        oauth: None,
        oauth_resource: None,
        tools: HashMap::new(),
    };
    test.codex
        .submit(Op::RefreshMcpServers {
            config: McpServerRefreshConfig {
                mcp_servers: serde_json::to_value(HashMap::from([(
                    "refreshed".to_string(),
                    refreshed_server,
                )]))?,
                mcp_oauth_credentials_store_mode: serde_json::to_value(
                    test.config.mcp_oauth_credentials_store_mode,
                )?,
                auth_keyring_backend_kind: serde_json::to_value(
                    test.config.auth_keyring_backend_kind(),
                )?,
            },
        })
        .await?;
    test.submit_turn("observe the refreshed MCP runtime")
        .await?;

    assert!(resource_client.has_server("refreshed").await);
    response.single_request();
    Ok(())
}
