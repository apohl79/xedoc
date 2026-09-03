use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use xedoc_app_server_protocol::JSONRPCResponse;
use xedoc_app_server_protocol::ManagedModelSettings;
use xedoc_app_server_protocol::ManagedProviderSettings;
use xedoc_app_server_protocol::ModelManagerReadParams;
use xedoc_app_server_protocol::ModelManagerReadResponse;
use xedoc_app_server_protocol::ModelManagerUpdateParams;
use xedoc_app_server_protocol::ModelManagerUpdateResponse;
use xedoc_app_server_protocol::ModelProviderOauthDeleteParams;
use xedoc_app_server_protocol::ModelProviderOauthDeleteResponse;
use xedoc_app_server_protocol::RequestId;
use xedoc_provider_anthropic::AnthropicOAuthCredential;
use xedoc_provider_anthropic::store_anthropic_oauth_credential;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

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
    let credential = AnthropicOAuthCredential {
        access_token: "access-token".to_string(),
        refresh_token: "refresh-token".to_string(),
        email: "user@example.com".to_string(),
        expires_at: "2030-01-01T00:00:00.000Z".to_string(),
        account_id: "account-id".to_string(),
        last_refresh_at: None,
    };
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
