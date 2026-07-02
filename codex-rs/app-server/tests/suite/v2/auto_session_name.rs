use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadNameUpdateSource;
use codex_app_server_protocol::ThreadNameUpdatedNotification;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadSetNameParams;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn auto_session_name_generates_title_until_manual_rename() -> Result<()> {
    let server = create_mock_responses_server_sequence(vec![
        assistant_response("resp-turn-1", "msg-turn-1", "First done"),
        assistant_response("resp-name-1", "msg-name-1", "Generated Session Name"),
        assistant_response("resp-turn-2", "msg-turn-2", "Second done"),
    ])
    .await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut app_server =
        TestAppServer::new_with_args(codex_home.path(), &["-c", "auto_session_name=true"]).await?;
    timeout(DEFAULT_READ_TIMEOUT, app_server.initialize()).await??;

    let thread = start_thread(&mut app_server).await?;
    let thread_id = thread.thread.id;
    start_turn(&mut app_server, &thread_id, "Discuss app-server titles").await?;

    let generated_notification = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_matching_notification(
            "generated thread/name/updated",
            |notification| {
                notification.method == "thread/name/updated"
                    && thread_name_update_source(notification)
                        == Some(ThreadNameUpdateSource::Generated)
            },
        ),
    )
    .await??;
    assert_thread_name_updated(
        generated_notification,
        &thread_id,
        Some("Generated Session Name"),
        ThreadNameUpdateSource::Generated,
    )?;
    assert_thread_name(&mut app_server, &thread_id, Some("Generated Session Name")).await?;

    let manual_name = "Manual Session Name";
    let rename_request = app_server
        .send_thread_set_name_request(ThreadSetNameParams {
            thread_id: thread_id.clone(),
            name: manual_name.to_string(),
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(rename_request)),
    )
    .await??;
    let manual_notification = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_matching_notification(
            "manual thread/name/updated",
            |notification| {
                notification.method == "thread/name/updated"
                    && thread_name_update_source(notification) == Some(ThreadNameUpdateSource::User)
            },
        ),
    )
    .await??;
    assert_thread_name_updated(
        manual_notification,
        &thread_id,
        Some(manual_name),
        ThreadNameUpdateSource::User,
    )?;

    start_turn(&mut app_server, &thread_id, "Continue after manual rename").await?;
    assert_thread_name(&mut app_server, &thread_id, Some(manual_name)).await?;

    Ok(())
}

#[tokio::test]
async fn auto_session_name_generates_title_mid_turn_from_streaming_response() -> Result<()> {
    let streamed_response =
        "streamed automatic session naming detail before user input ".to_string();
    let (finish_turn_tx, finish_turn_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![
                    responses::ev_response_created("resp-turn-1"),
                    responses::ev_message_item_added("msg-turn-1", ""),
                    responses::ev_output_text_delta(&streamed_response),
                ]),
            },
            StreamingSseChunk {
                gate: Some(finish_turn_rx),
                body: responses::sse(vec![
                    responses::ev_assistant_message("msg-turn-1", &streamed_response),
                    responses::ev_completed("resp-turn-1"),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: assistant_response("resp-name-1", "msg-name-1", "Mid Turn Session Name"),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: assistant_response("resp-name-2", "msg-name-2", "Final Session Name"),
        }],
    ])
    .await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), server.uri())?;

    let mut app_server =
        TestAppServer::new_with_args(codex_home.path(), &["-c", "auto_session_name=true"]).await?;
    timeout(DEFAULT_READ_TIMEOUT, app_server.initialize()).await??;

    let thread = start_thread(&mut app_server).await?;
    let thread_id = thread.thread.id;
    let request_id = app_server
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInput::Text {
                text: "Start the mid-turn naming work".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let generated_notification = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_matching_notification(
            "mid-turn generated thread/name/updated",
            |notification| {
                notification.method == "thread/name/updated"
                    && thread_name_update_source(notification)
                        == Some(ThreadNameUpdateSource::Generated)
            },
        ),
    )
    .await??;
    assert_thread_name_updated(
        generated_notification,
        &thread_id,
        Some("Mid Turn Session Name"),
        ThreadNameUpdateSource::Generated,
    )?;
    assert!(
        !app_server
            .pending_notification_methods()
            .iter()
            .any(|method| method == "turn/completed"),
        "generated session name should be emitted before turn/completed"
    );
    finish_turn_tx
        .send(())
        .expect("release main turn response stream");

    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let final_notification = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_matching_notification(
            "final generated thread/name/updated",
            |notification| {
                notification.method == "thread/name/updated"
                    && thread_name_update_source(notification)
                        == Some(ThreadNameUpdateSource::Generated)
            },
        ),
    )
    .await??;
    assert_thread_name_updated(
        final_notification,
        &thread_id,
        Some("Final Session Name"),
        ThreadNameUpdateSource::Generated,
    )?;
    assert_thread_name(&mut app_server, &thread_id, Some("Final Session Name")).await?;

    server.shutdown().await;

    Ok(())
}

async fn start_thread(app_server: &mut TestAppServer) -> Result<ThreadStartResponse> {
    let request_id = app_server
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(response)
}

async fn start_turn(app_server: &mut TestAppServer, thread_id: &str, text: &str) -> Result<()> {
    let request_id = app_server
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.to_string(),
            input: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    Ok(())
}

async fn assert_thread_name(
    app_server: &mut TestAppServer,
    thread_id: &str,
    expected_name: Option<&str>,
) -> Result<()> {
    let request_id = app_server
        .send_thread_read_request(ThreadReadParams {
            thread_id: thread_id.to_string(),
            include_turns: false,
        })
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: ThreadReadResponse = to_response(response)?;
    assert_eq!(response.thread.name.as_deref(), expected_name);
    Ok(())
}

fn assert_thread_name_updated(
    notification: JSONRPCNotification,
    thread_id: &str,
    expected_name: Option<&str>,
    expected_source: ThreadNameUpdateSource,
) -> Result<()> {
    let update: ThreadNameUpdatedNotification =
        serde_json::from_value(notification.params.context("thread/name/updated params")?)?;
    assert_eq!(update.thread_id, thread_id);
    assert_eq!(update.thread_name.as_deref(), expected_name);
    assert_eq!(update.source, expected_source);
    Ok(())
}

fn thread_name_update_source(notification: &JSONRPCNotification) -> Option<ThreadNameUpdateSource> {
    serde_json::from_value::<ThreadNameUpdatedNotification>(notification.params.clone()?)
        .map(|update| update.source)
        .ok()
}

fn assistant_response(response_id: &str, message_id: &str, text: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created(response_id),
        responses::ev_assistant_message(message_id, text),
        responses::ev_completed(response_id),
    ])
}

fn create_config_toml(codex_home: &Path, server_uri: &str) -> std::io::Result<()> {
    write_mock_responses_config_toml(
        codex_home,
        server_uri,
        &BTreeMap::<Feature, bool>::new(),
        i64::MAX,
        None,
        "mock_provider",
        "compact",
    )
}
