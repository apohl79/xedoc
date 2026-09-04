use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::query_param;
use xedoc_app_server::in_process;
use xedoc_app_server::in_process::InProcessClientHandle;
use xedoc_app_server::in_process::InProcessServerEvent;
use xedoc_app_server::in_process::InProcessStartArgs;
use xedoc_app_server_protocol::ClientInfo;
use xedoc_app_server_protocol::ClientRequest;
use xedoc_app_server_protocol::InitializeParams;
use xedoc_app_server_protocol::JSONRPCResponse;
use xedoc_app_server_protocol::ManagedModelSettings;
use xedoc_app_server_protocol::ManagedProviderSettings;
use xedoc_app_server_protocol::ModelListParams;
use xedoc_app_server_protocol::ModelListResponse;
use xedoc_app_server_protocol::ModelManagerReadParams;
use xedoc_app_server_protocol::ModelManagerReadResponse;
use xedoc_app_server_protocol::ModelManagerUpdateParams;
use xedoc_app_server_protocol::ModelManagerUpdateResponse;
use xedoc_app_server_protocol::ModelProviderApiKeyDeleteParams;
use xedoc_app_server_protocol::ModelProviderApiKeyDeleteResponse;
use xedoc_app_server_protocol::ModelProviderApiKeySetParams;
use xedoc_app_server_protocol::ModelProviderApiKeySetResponse;
use xedoc_app_server_protocol::ModelProviderOauthDeleteParams;
use xedoc_app_server_protocol::ModelProviderOauthDeleteResponse;
use xedoc_app_server_protocol::RequestId;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ThreadStartParams;
use xedoc_app_server_protocol::ThreadStartResponse;
use xedoc_app_server_protocol::TurnCompletedNotification;
use xedoc_app_server_protocol::TurnStartParams;
use xedoc_app_server_protocol::TurnStatus;
use xedoc_app_server_protocol::UserInput;
use xedoc_arg0::Arg0DispatchPaths;
use xedoc_config::CloudConfigBundleLoader;
use xedoc_config::LoaderOverrides;
use xedoc_core::config::ConfigBuilder;
use xedoc_exec_server::EnvironmentManager;
use xedoc_keyring_store::tests::MockKeyringStore;
use xedoc_login::AuthManager;
use xedoc_login::ProviderCredentialStore;
use xedoc_login::XedocAuth;
use xedoc_model_provider_info::ANTHROPIC_PROVIDER_ID;
use xedoc_model_provider_info::GEMINI_PROVIDER_ID;
use xedoc_protocol::protocol::SessionSource;
use xedoc_provider_anthropic::AnthropicOAuthCredential;
use xedoc_provider_anthropic::store_anthropic_oauth_credential;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const TEST_ANTHROPIC_API_KEY: &str = "test-anthropic-api-key";
const TEST_GEMINI_API_KEY: &str = "test-gemini-api-key";

#[derive(Clone, Copy)]
enum ActiveTestProvider {
    OpenAi,
    Anthropic,
}

#[tokio::test]
async fn setting_anthropic_api_key_authenticates_an_existing_thread() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", TEST_ANTHROPIC_API_KEY))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_sse_body()),
        )
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;

    let xedoc_home = TempDir::new()?;
    let mut client =
        start_in_process_client(&xedoc_home, &server, ActiveTestProvider::Anthropic).await?;
    let thread_id = start_ephemeral_thread(&client, /*request_id*/ 1).await?;

    let set_key_response = request(
        &client,
        ClientRequest::ModelProviderApiKeySet {
            request_id: RequestId::Integer(2),
            params: ModelProviderApiKeySetParams {
                provider_id: "anthropic".to_string(),
                api_key: TEST_ANTHROPIC_API_KEY.to_string(),
            },
        },
    )
    .await?;
    assert_eq!(
        serde_json::from_value::<ModelProviderApiKeySetResponse>(set_key_response)?,
        ModelProviderApiKeySetResponse {}
    );

    let completed = run_turn(&mut client, /*request_id*/ 3, &thread_id).await?;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    client.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn existing_thread_follows_anthropic_oauth_and_api_key_removal() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer access-token"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_sse_body()),
        )
        .expect(/*requests*/ 2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", TEST_ANTHROPIC_API_KEY))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_sse_body()),
        )
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;

    let xedoc_home = TempDir::new()?;
    let accounts_directory = xedoc_home
        .path()
        .join("providers")
        .join("anthropic")
        .join("accounts");
    store_anthropic_oauth_credential(&accounts_directory, &anthropic_credential())?;
    let mut client =
        start_in_process_client(&xedoc_home, &server, ActiveTestProvider::Anthropic).await?;
    let thread_id = start_ephemeral_thread(&client, /*request_id*/ 30).await?;
    assert_eq!(
        run_turn(&mut client, /*request_id*/ 31, &thread_id)
            .await?
            .turn
            .status,
        TurnStatus::Completed
    );

    request(
        &client,
        ClientRequest::ModelProviderOauthDelete {
            request_id: RequestId::Integer(32),
            params: ModelProviderOauthDeleteParams {
                provider_id: ANTHROPIC_PROVIDER_ID.to_string(),
            },
        },
    )
    .await?;
    assert_eq!(
        run_turn(&mut client, /*request_id*/ 33, &thread_id)
            .await?
            .turn
            .status,
        TurnStatus::Failed
    );

    store_anthropic_oauth_credential(&accounts_directory, &anthropic_credential())?;
    assert_eq!(
        run_turn(&mut client, /*request_id*/ 34, &thread_id)
            .await?
            .turn
            .status,
        TurnStatus::Completed
    );
    request(
        &client,
        ClientRequest::ModelProviderApiKeySet {
            request_id: RequestId::Integer(35),
            params: ModelProviderApiKeySetParams {
                provider_id: ANTHROPIC_PROVIDER_ID.to_string(),
                api_key: TEST_ANTHROPIC_API_KEY.to_string(),
            },
        },
    )
    .await?;
    assert_eq!(
        run_turn(&mut client, /*request_id*/ 36, &thread_id)
            .await?
            .turn
            .status,
        TurnStatus::Completed
    );
    request(
        &client,
        ClientRequest::ModelProviderApiKeyDelete {
            request_id: RequestId::Integer(37),
            params: ModelProviderApiKeyDeleteParams {
                provider_id: ANTHROPIC_PROVIDER_ID.to_string(),
            },
        },
    )
    .await?;
    assert_eq!(
        run_turn(&mut client, /*request_id*/ 38, &thread_id)
            .await?
            .turn
            .status,
        TurnStatus::Failed
    );
    client.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn provider_api_key_changes_model_visibility_without_restarting_app_server() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(query_param("pageSize", "1000"))
        .and(header("x-goog-api-key", TEST_GEMINI_API_KEY))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200).set_body_json(serde_json::json!({
                "models": [{
                    "name": "models/gemini-3.6-flash",
                    "displayName": "Gemini 3.6 Flash",
                    "description": "Fast coding model",
                    "inputTokenLimit": 1_048_576,
                    "supportedGenerationMethods": ["generateContent"]
                }]
            })),
        )
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let xedoc_home = TempDir::new()?;
    let client = start_in_process_client(&xedoc_home, &server, ActiveTestProvider::OpenAi).await?;

    let initial = list_models(&client, /*request_id*/ 10).await?;
    assert!(
        initial
            .data
            .iter()
            .all(|model| model.provider_id != GEMINI_PROVIDER_ID)
    );

    request(
        &client,
        ClientRequest::ModelProviderApiKeySet {
            request_id: RequestId::Integer(11),
            params: ModelProviderApiKeySetParams {
                provider_id: GEMINI_PROVIDER_ID.to_string(),
                api_key: TEST_GEMINI_API_KEY.to_string(),
            },
        },
    )
    .await?;
    let configured = list_models(&client, /*request_id*/ 12).await?;
    assert!(
        configured
            .data
            .iter()
            .any(|model| model.provider_id == GEMINI_PROVIDER_ID)
    );

    let delete_response = request(
        &client,
        ClientRequest::ModelProviderApiKeyDelete {
            request_id: RequestId::Integer(13),
            params: ModelProviderApiKeyDeleteParams {
                provider_id: GEMINI_PROVIDER_ID.to_string(),
            },
        },
    )
    .await?;
    assert_eq!(
        serde_json::from_value::<ModelProviderApiKeyDeleteResponse>(delete_response)?,
        ModelProviderApiKeyDeleteResponse { deleted: true }
    );
    let deleted = list_models(&client, /*request_id*/ 14).await?;
    assert!(
        deleted
            .data
            .iter()
            .all(|model| model.provider_id != GEMINI_PROVIDER_ID)
    );

    request(
        &client,
        ClientRequest::ModelProviderApiKeySet {
            request_id: RequestId::Integer(15),
            params: ModelProviderApiKeySetParams {
                provider_id: GEMINI_PROVIDER_ID.to_string(),
                api_key: TEST_GEMINI_API_KEY.to_string(),
            },
        },
    )
    .await?;
    let restored = list_models(&client, /*request_id*/ 16).await?;
    assert!(
        restored
            .data
            .iter()
            .any(|model| model.provider_id == GEMINI_PROVIDER_ID)
    );

    std::fs::remove_file(xedoc_home.path().join("secrets").join("local.age"))?;
    let externally_deleted = list_models(&client, /*request_id*/ 17).await?;
    assert!(
        externally_deleted
            .data
            .iter()
            .all(|model| model.provider_id != GEMINI_PROVIDER_ID)
    );
    client.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn anthropic_oauth_changes_model_visibility_without_restarting_app_server() -> Result<()> {
    let server = MockServer::start().await;
    let xedoc_home = TempDir::new()?;
    let client = start_in_process_client(&xedoc_home, &server, ActiveTestProvider::OpenAi).await?;

    let initial = list_models(&client, /*request_id*/ 20).await?;
    assert!(
        initial
            .data
            .iter()
            .all(|model| model.provider_id != ANTHROPIC_PROVIDER_ID)
    );

    let accounts_directory = xedoc_home
        .path()
        .join("providers")
        .join("anthropic")
        .join("accounts");
    store_anthropic_oauth_credential(&accounts_directory, &anthropic_credential())?;
    let configured = list_models(&client, /*request_id*/ 21).await?;
    assert!(
        configured
            .data
            .iter()
            .any(|model| model.provider_id == ANTHROPIC_PROVIDER_ID)
    );

    let delete_response = request(
        &client,
        ClientRequest::ModelProviderOauthDelete {
            request_id: RequestId::Integer(22),
            params: ModelProviderOauthDeleteParams {
                provider_id: ANTHROPIC_PROVIDER_ID.to_string(),
            },
        },
    )
    .await?;
    assert_eq!(
        serde_json::from_value::<ModelProviderOauthDeleteResponse>(delete_response)?,
        ModelProviderOauthDeleteResponse { deleted: true }
    );
    let deleted = list_models(&client, /*request_id*/ 23).await?;
    assert!(
        deleted
            .data
            .iter()
            .all(|model| model.provider_id != ANTHROPIC_PROVIDER_ID)
    );
    client.shutdown().await?;
    Ok(())
}

async fn start_in_process_client(
    xedoc_home: &TempDir,
    server: &MockServer,
    active_provider: ActiveTestProvider,
) -> Result<InProcessClientHandle> {
    let config_toml = match active_provider {
        ActiveTestProvider::OpenAi => r#"
model = "gpt-5.6-terra"
model_provider = "openai"
approval_policy = "never"
sandbox_mode = "read-only"
"#
        .to_string(),
        ActiveTestProvider::Anthropic => format!(
            r#"
model = "claude-opus-5"
model_provider = "anthropic"
approval_policy = "never"
sandbox_mode = "read-only"

[model_providers.anthropic]
name = "Anthropic"
base_url = "{}/v1"
env_key = "ANTHROPIC_API_KEY"
wire_api = "anthropic"
request_max_retries = 0
stream_max_retries = 0
stream_idle_timeout_ms = 5000
supports_websockets = false
http_headers = {{ "anthropic-version" = "2023-06-01" }}
"#,
            server.uri()
        ),
    };
    std::fs::write(xedoc_home.path().join("config.toml"), config_toml)?;
    let loader_overrides = LoaderOverrides::without_managed_config_for_tests();
    let mut config = ConfigBuilder::default()
        .xedoc_home(xedoc_home.path().to_path_buf())
        .fallback_cwd(Some(xedoc_home.path().to_path_buf()))
        .loader_overrides(loader_overrides.clone())
        .build()
        .await?;
    config
        .model_providers
        .get_mut(GEMINI_PROVIDER_ID)
        .context("built-in Gemini provider missing")?
        .base_url = Some(server.uri());
    let credentials = ProviderCredentialStore::new_with_keyring_store(
        xedoc_home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    let auth_manager = AuthManager::from_auth_for_testing_with_provider_credentials(
        XedocAuth::from_api_key("unused-openai-api-key"),
        xedoc_home.path().to_path_buf(),
        credentials,
    );
    Ok(in_process::start_with_auth_manager(
        InProcessStartArgs {
            arg0_paths: Arg0DispatchPaths::default(),
            config: Arc::new(config),
            cli_overrides: Vec::new(),
            loader_overrides,
            strict_config: false,
            cloud_config_bundle: CloudConfigBundleLoader::default(),
            thread_config_loader: Arc::new(xedoc_config::NoopThreadConfigLoader),
            log_db: None,
            state_db: None,
            environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
            config_warnings: Vec::new(),
            session_source: SessionSource::Cli,
            enable_xedoc_api_key_env: false,
            initialize: InitializeParams {
                client_info: ClientInfo {
                    name: "xedoc-app-server-tests".to_string(),
                    title: None,
                    version: "0.1.0".to_string(),
                },
                capabilities: None,
            },
            channel_capacity: in_process::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
        },
        auth_manager,
    )
    .await?)
}

async fn request(
    client: &InProcessClientHandle,
    request: ClientRequest,
) -> Result<serde_json::Value> {
    client
        .request(request)
        .await?
        .map_err(|error| anyhow::anyhow!("app-server request failed: {}", error.message))
}

async fn list_models(client: &InProcessClientHandle, request_id: i64) -> Result<ModelListResponse> {
    let response = request(
        client,
        ClientRequest::ModelList {
            request_id: RequestId::Integer(request_id),
            params: ModelListParams::default(),
        },
    )
    .await?;
    Ok(serde_json::from_value(response)?)
}

async fn start_ephemeral_thread(client: &InProcessClientHandle, request_id: i64) -> Result<String> {
    let response = request(
        client,
        ClientRequest::ThreadStart {
            request_id: RequestId::Integer(request_id),
            params: ThreadStartParams {
                ephemeral: Some(true),
                ..ThreadStartParams::default()
            },
        },
    )
    .await?;
    let ThreadStartResponse { thread, .. } = serde_json::from_value(response)?;
    Ok(thread.id)
}

async fn run_turn(
    client: &mut InProcessClientHandle,
    request_id: i64,
    thread_id: &str,
) -> Result<TurnCompletedNotification> {
    request(
        client,
        ClientRequest::TurnStart {
            request_id: RequestId::Integer(request_id),
            params: TurnStartParams {
                thread_id: thread_id.to_string(),
                input: vec![UserInput::Text {
                    text: "who are you?".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    wait_for_turn_completed(client).await
}

async fn wait_for_turn_completed(
    client: &mut InProcessClientHandle,
) -> Result<TurnCompletedNotification> {
    loop {
        let event = timeout(DEFAULT_TIMEOUT, client.next_event())
            .await?
            .context("in-process app-server stopped before turn/completed")?;
        if let InProcessServerEvent::ServerNotification(ServerNotification::TurnCompleted(
            completed,
        )) = event
        {
            return Ok(completed);
        }
    }
}

fn anthropic_sse_body() -> String {
    [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"authenticated\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ]
    .concat()
}

fn anthropic_credential() -> AnthropicOAuthCredential {
    AnthropicOAuthCredential {
        access_token: "access-token".to_string(),
        refresh_token: "refresh-token".to_string(),
        email: "user@example.com".to_string(),
        expires_at: "2030-01-01T00:00:00.000Z".to_string(),
        account_id: "account-id".to_string(),
        last_refresh_at: None,
    }
}

#[tokio::test]
async fn model_settings_update_is_persisted_and_returned() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    let mut app_server = TestAppServer::builder()
        .with_xedoc_home(xedoc_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, app_server.initialize()).await??;

    let initial = read_model_manager(&mut app_server).await?;
    let openai = initial
        .providers
        .iter()
        .find(|provider| provider.id == "openai")
        .context("OpenAI provider missing")?;
    let model = openai
        .models
        .iter()
        .find(|model| model.id == "gpt-5.6-sol")
        .context("gpt-5.6-sol missing")?;
    let compact_limit = model.auto_compact_token_limit - 1;

    let request_id = app_server
        .send_model_manager_update_request(ModelManagerUpdateParams::ModelSettings {
            provider_id: openai.id.clone(),
            model_id: model.id.clone(),
            context_window: model.context_window,
            max_context_window: model.max_context_window,
            auto_compact_token_limit: compact_limit,
            base_instructions: model.base_instructions.clone(),
        })
        .await?;
    let response = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<ModelManagerUpdateResponse>(response)?,
        ModelManagerUpdateResponse {}
    );

    let updated = read_model_manager(&mut app_server).await?;
    let actual = updated
        .providers
        .iter()
        .find(|provider| provider.id == "openai")
        .and_then(|provider| {
            provider
                .models
                .iter()
                .find(|model| model.id == "gpt-5.6-sol")
        })
        .context("updated gpt-5.6-sol missing")?;
    let mut expected: ManagedModelSettings = model.clone();
    expected.auto_compact_token_limit = compact_limit;
    assert_eq!(actual, &expected);
    assert!(xedoc_home.path().join("models.json").is_file());
    Ok(())
}

#[tokio::test]
async fn anthropic_oauth_login_can_be_removed() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    let accounts_directory = xedoc_home
        .path()
        .join("providers")
        .join("anthropic")
        .join("accounts");
    let credential = anthropic_credential();
    store_anthropic_oauth_credential(&accounts_directory, &credential)?;
    let mut app_server = TestAppServer::builder()
        .with_xedoc_home(xedoc_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, app_server.initialize()).await??;

    let original = managed_provider(&read_model_manager(&mut app_server).await?, "anthropic")?;
    assert!(original.oauth_configured);
    let request_id = app_server
        .send_raw_request(
            "modelProvider/oauth/delete",
            Some(serde_json::to_value(ModelProviderOauthDeleteParams {
                provider_id: "anthropic".to_string(),
            })?),
        )
        .await?;
    let response = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<ModelProviderOauthDeleteResponse>(response)?,
        ModelProviderOauthDeleteResponse { deleted: true }
    );
    assert_eq!(
        managed_provider(&read_model_manager(&mut app_server).await?, "anthropic")?,
        ManagedProviderSettings {
            oauth_configured: false,
            ..original
        }
    );
    Ok(())
}

async fn read_model_manager(app_server: &mut TestAppServer) -> Result<ModelManagerReadResponse> {
    let request_id = app_server
        .send_model_manager_read_request(ModelManagerReadParams {})
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(response)
}

fn managed_provider(
    response: &ModelManagerReadResponse,
    provider_id: &str,
) -> Result<ManagedProviderSettings> {
    response
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .cloned()
        .with_context(|| format!("{provider_id} provider missing"))
}
