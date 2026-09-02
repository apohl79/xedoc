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
use xedoc_app_server_protocol::ModelManagerReadParams;
use xedoc_app_server_protocol::ModelManagerReadResponse;
use xedoc_app_server_protocol::ModelManagerUpdateParams;
use xedoc_app_server_protocol::ModelManagerUpdateResponse;
use xedoc_app_server_protocol::RequestId;

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
