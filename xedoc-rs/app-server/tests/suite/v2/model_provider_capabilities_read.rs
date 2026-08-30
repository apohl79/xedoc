use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use xedoc_app_server_protocol::JSONRPCResponse;
use xedoc_app_server_protocol::ModelProviderCapabilitiesReadParams;
use xedoc_app_server_protocol::ModelProviderCapabilitiesReadResponse;
use xedoc_app_server_protocol::RequestId;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn read_default_provider_capabilities() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_xedoc_home(xedoc_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_model_provider_capabilities_read_request(ModelProviderCapabilitiesReadParams {})
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let received: ModelProviderCapabilitiesReadResponse = to_response(response)?;

    let expected = ModelProviderCapabilitiesReadResponse {
        namespace_tools: true,
        image_generation: true,
        web_search: true,
    };
    assert_eq!(received, expected);
    Ok(())
}

#[tokio::test]
async fn read_amazon_bedrock_provider_capabilities() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    std::fs::write(
        xedoc_home.path().join("config.toml"),
        r#"model_provider = "amazon-bedrock"
"#,
    )?;
    let mut mcp = TestAppServer::builder()
        .with_xedoc_home(xedoc_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_model_provider_capabilities_read_request(ModelProviderCapabilitiesReadParams {})
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let received: ModelProviderCapabilitiesReadResponse = to_response(response)?;

    let expected = ModelProviderCapabilitiesReadResponse {
        namespace_tools: true,
        image_generation: false,
        web_search: false,
    };
    assert_eq!(received, expected);
    Ok(())
}
