use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::to_response;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::ORIGIN;
use xedoc_app_server_protocol::RequestId;
use xedoc_app_server_protocol::ThreadStartParams;
use xedoc_app_server_protocol::ThreadStartResponse;
use xedoc_app_server_protocol::ThreadWebUiOpenResponse;

const READ_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 10);

#[tokio::test]
async fn routing_table_inputs_reach_the_native_controller() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let home = TempDir::new()?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"
model = "mock-model"
model_provider = "mock_provider"
[model_providers.mock_provider]
name = "Mock provider"
base_url = "{}/v1"
wire_api = "responses"
"#,
            server.uri()
        ),
    )?;
    let mut app = TestAppServer::builder()
        .with_xedoc_home(home.path())
        .build()
        .await?;
    app.initialize().await?;
    let id = app
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let thread: ThreadStartResponse = to_response(
        timeout(
            READ_TIMEOUT,
            app.read_stream_until_response_message(RequestId::Integer(id)),
        )
        .await??,
    )?;
    let id = app
        .send_raw_request(
            "thread/webUi/open",
            Some(json!({"threadId": thread.thread.id})),
        )
        .await?;
    let opened: ThreadWebUiOpenResponse = to_response(
        timeout(
            READ_TIMEOUT,
            app.read_stream_until_response_message(RequestId::Integer(id)),
        )
        .await??,
    )?;
    let (mut producer, _) = connect_async(opened.bridge_url).await?;
    let page = url::Url::parse(&opened.url)?;
    let host = page.host_str().context("missing web UI host")?;
    let port = page.port().context("missing web UI port")?;
    let token = page
        .fragment()
        .and_then(|fragment| fragment.strip_prefix("capability="))
        .context("missing browser capability")?;
    let mut request = format!("ws://{host}:{port}/session/{token}").into_client_request()?;
    request.headers_mut().insert(
        ORIGIN,
        HeaderValue::from_str(&format!("http://{host}:{port}"))?,
    );
    let (mut browser, _) = connect_async(request).await?;
    for input in [
        json!({"type": "routingTable"}),
        json!({"type": "routingTable", "table": "openai-balanced"}),
    ] {
        let message = Message::Text(input.to_string().into());
        browser.send(message.clone()).await?;
        let forwarded = timeout(READ_TIMEOUT, producer.next())
            .await?
            .context("native controller disconnected")??;
        assert_eq!(forwarded, message);
    }
    Ok(())
}
