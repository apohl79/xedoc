use super::MANAGED_AUTH_MAX_ATTEMPTS;
use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::responses_metadata::XedocResponsesMetadata;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use std::fmt;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use xedoc_api::Provider;
use xedoc_api::ResponseEvent;
use xedoc_api::SharedAuthProvider;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;
use xedoc_login::AuthManager;
use xedoc_login::XedocAuth;
use xedoc_login::auth::AgentIdentityAuthPolicy;
use xedoc_model_provider::ModelProvider;
use xedoc_model_provider::ModelProviderFuture;
use xedoc_model_provider::ProviderAccountResult;
use xedoc_model_provider::ProviderAccountState;
use xedoc_model_provider::SharedModelProvider;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_model_provider_info::WireApi;
use xedoc_models_manager::manager::SharedModelsManager;
use xedoc_models_manager::manager::StaticModelsManager;
use xedoc_otel::SessionTelemetry;
use xedoc_protocol::ThreadId;
use xedoc_protocol::config_types::ReasoningSummary;
use xedoc_protocol::error::Result;
use xedoc_protocol::error::XedocErr;
use xedoc_protocol::models::BaseInstructions;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ModelsResponse;
use xedoc_protocol::protocol::SessionSource;
use xedoc_provider_anthropic::AnthropicAccountPool;
use xedoc_provider_anthropic::AnthropicOAuthAccount;
use xedoc_provider_anthropic::AnthropicOAuthAuthProvider;
use xedoc_provider_anthropic::AnthropicOAuthCredential;
use xedoc_provider_anthropic::persist_anthropic_oauth_credential;

struct TestAnthropicProvider {
    info: ModelProviderInfo,
    accounts: Arc<AnthropicAccountPool>,
    refresh_endpoint: String,
}

impl fmt::Debug for TestAnthropicProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestAnthropicProvider")
            .field("info", &self.info)
            .field("accounts", &self.accounts)
            .field("refresh_endpoint", &"<redacted>")
            .finish()
    }
}

impl ModelProvider for TestAnthropicProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        None
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<XedocAuth>> {
        Box::pin(async { None })
    }

    fn account_state(&self) -> ProviderAccountResult {
        Ok(ProviderAccountState {
            account: None,
            requires_openai_auth: false,
        })
    }

    fn api_provider(&self) -> ModelProviderFuture<'_, Result<Provider>> {
        Box::pin(async {
            self.info.to_api_provider(/*auth_mode*/ None)
        })
    }

    fn api_auth(&self) -> ModelProviderFuture<'_, Result<SharedAuthProvider>> {
        Box::pin(async move {
            let account = self.accounts.select().map_err(|_| {
                XedocErr::Io(io::Error::other("no Anthropic test account is available"))
            })?;
            Ok(Arc::new(AnthropicOAuthAuthProvider::new(
                account,
                Arc::clone(&self.accounts),
                self.refresh_endpoint.clone(),
            )) as SharedAuthProvider)
        })
    }

    fn models_manager(
        &self,
        _xedoc_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        Arc::new(StaticModelsManager::new(
            /*auth_manager*/ None,
            config_model_catalog.unwrap_or_default(),
        ))
    }
}

fn provider(server: &MockServer, accounts: Arc<AnthropicAccountPool>) -> SharedModelProvider {
    Arc::new(TestAnthropicProvider {
        info: ModelProviderInfo {
            name: "Anthropic".to_string(),
            base_url: Some(format!("{}/v1", server.uri())),
            wire_api: WireApi::Anthropic,
            http_headers: Some(std::collections::HashMap::from([(
                "anthropic-version".to_string(),
                "2023-06-01".to_string(),
            )])),
            request_max_retries: Some(5),
            stream_idle_timeout_ms: Some(5_000),
            namespace_tools: false,
            ..ModelProviderInfo::default()
        },
        accounts,
        refresh_endpoint: format!("{}/v1/oauth/token", server.uri()),
    })
}

fn client(provider: SharedModelProvider) -> ModelClient {
    ModelClient::from_model_provider(
        provider,
        "anthropic".to_string(),
        AgentIdentityAuthPolicy::JwtOnly,
        SessionSource::Cli,
        "test_originator".to_string(),
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*item_ids_enabled*/ false,
        /*concurrent_reasoning_summaries_enabled*/ false,
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    )
}

fn account(directory: &Path, name: &str, access_token: &str) -> AnthropicOAuthAccount {
    let source_path = directory.join(format!("anthropic-{name}.json"));
    let credential = AnthropicOAuthCredential {
        access_token: access_token.to_string(),
        refresh_token: format!("{access_token}-refresh"),
        email: format!("{name}@example.com"),
        expires_at: "2030-01-01T00:00:00.000Z".to_string(),
        account_id: format!("{name}-id"),
        last_refresh_at: None,
    };
    persist_anthropic_oauth_credential(&source_path, &credential).expect("persist credential");
    AnthropicOAuthAccount {
        credential,
        source_path,
    }
}

fn account_pool(directory: &Path, tokens: &[&str]) -> Arc<AnthropicAccountPool> {
    let accounts = tokens
        .iter()
        .enumerate()
        .map(|(index, token)| account(directory, &format!("user-{index}"), token))
        .collect();
    Arc::new(AnthropicAccountPool::new(accounts).expect("account pool"))
}

fn successful_stream() -> ResponseTemplate {
    let body = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"native\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ]
    .concat();
    ResponseTemplate::new(/*status*/ 200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn refreshed_token_response() -> ResponseTemplate {
    ResponseTemplate::new(/*status*/ 200).set_body_json(serde_json::json!({
        "access_token": "first-refreshed",
        "refresh_token": "first-refreshed-refresh",
        "expires_in": 3600,
        "account": {
            "email_address": "user-0@example.com",
            "uuid": "user-0-id"
        }
    }))
}

async fn open_stream(client: &ModelClient) -> Result<crate::client_common::ResponseStream> {
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "reply natively".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        base_instructions: BaseInstructions {
            text: "Follow the test contract.".to_string(),
        },
        ..Prompt::default()
    };
    let model_info: ModelInfo = serde_json::from_value(serde_json::json!({
        "slug": "claude-test",
        "display_name": "claude-test",
        "description": "test",
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "base_instructions": "base",
        "support_verbosity": false,
        "truncation_policy": {"mode": "bytes", "limit": 10_000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 200_000,
        "experimental_supported_tools": []
    }))
    .expect("model info");
    let thread_id = ThreadId::new();
    let metadata = XedocResponsesMetadata::new(
        "installation".to_string(),
        thread_id.to_string(),
        thread_id.to_string(),
        format!("{thread_id}:0"),
    );
    let mut session = client.new_session();
    session
        .stream(
            &prompt,
            &model_info,
            &SessionTelemetry::new(
                thread_id,
                "claude-test",
                "claude-test",
                /*account_id*/ None,
                /*account_email*/ None,
                /*auth_mode*/ None,
                "test-originator".to_string(),
                /*log_user_prompts*/ false,
                "test-terminal".to_string(),
                SessionSource::Cli,
            ),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            &metadata,
        )
        .await
}

async fn complete_stream(client: &ModelClient) -> Result<()> {
    let mut stream = open_stream(client).await?;
    let mut completed = false;
    while let Some(event) = stream.next().await {
        if matches!(event?, ResponseEvent::Completed { .. }) {
            completed = true;
        }
    }
    assert!(completed);
    Ok(())
}

async fn authorization_headers(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .expect("recorded requests")
        .into_iter()
        .filter(|request| request.url.path() == "/v1/messages")
        .filter_map(|request| {
            request
                .headers
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect()
}

#[tokio::test]
async fn unauthorized_refreshes_once_and_retries_the_same_account() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = Arc::clone(&attempts);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_request: &wiremock::Request| {
            if response_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(/*status*/ 401)
            } else {
                successful_stream()
            }
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(refreshed_token_response())
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir()?;
    let accounts = account_pool(directory.path(), &["first"]);
    let client = client(provider(&server, accounts));

    complete_stream(&client).await?;

    assert_eq!(
        authorization_headers(&server).await,
        vec!["Bearer first", "Bearer first-refreshed"]
    );
    Ok(())
}

#[tokio::test]
async fn a_second_unauthorized_rotates_without_refreshing_twice() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = Arc::clone(&attempts);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_request: &wiremock::Request| {
            if response_attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                ResponseTemplate::new(/*status*/ 401)
            } else {
                successful_stream()
            }
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(refreshed_token_response())
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir()?;
    let accounts = account_pool(directory.path(), &["first", "second"]);
    let client = client(provider(&server, accounts));

    complete_stream(&client).await?;

    assert_eq!(
        authorization_headers(&server).await,
        vec!["Bearer first", "Bearer first-refreshed", "Bearer second"]
    );
    Ok(())
}

#[tokio::test]
async fn managed_failover_stops_after_three_model_attempts() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(/*status*/ 429))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir()?;
    let accounts = account_pool(directory.path(), &["first", "second", "third", "fourth"]);
    let client = client(provider(&server, Arc::clone(&accounts)));

    let error = match open_stream(&client).await {
        Ok(_) => panic!("rate limit should exhaust the managed retry budget"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("429"));
    assert_eq!(
        authorization_headers(&server).await.len(),
        MANAGED_AUTH_MAX_ATTEMPTS
    );
    assert_eq!(
        accounts
            .select()
            .expect("next request account")
            .credential
            .access_token,
        "fourth"
    );
    Ok(())
}

#[tokio::test]
async fn non_retryable_client_error_does_not_rotate_accounts() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(/*status*/ 400))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir()?;
    let accounts = account_pool(directory.path(), &["first", "second"]);
    let client = client(provider(&server, accounts));

    let error = match open_stream(&client).await {
        Ok(_) => panic!("client error should propagate"),
        Err(error) => error,
    };

    assert!(matches!(error, XedocErr::InvalidRequest(message) if message.is_empty()));
    assert_eq!(authorization_headers(&server).await, vec!["Bearer first"]);
    Ok(())
}
