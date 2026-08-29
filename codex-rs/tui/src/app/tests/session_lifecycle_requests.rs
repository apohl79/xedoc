use super::*;
use app_test_support::create_fake_parented_rollout_with_source;
use app_test_support::create_fake_rollout;
use app_test_support::rollout_path;
use codex_app_server_client::AppServerEvent;
use codex_app_server_client::InProcessAppServerClient;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_app_server_protocol::ClientNotification;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::AgentPath;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::TurnStartedEvent;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use std::sync::Mutex;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

/// Returns and resets `(thread/loaded/list, thread/read)` request counts.
fn take_backfill_counts(requests: &Arc<Mutex<Vec<String>>>) -> (usize, usize) {
    let requests = std::mem::take(&mut *requests.lock().expect("request recorder lock"));
    (
        requests
            .iter()
            .filter(|method| *method == "thread/loaded/list")
            .count(),
        requests
            .iter()
            .filter(|method| *method == "thread/read")
            .count(),
    )
}

/// Starts an embedded app server behind a loopback WebSocket proxy that records JSON-RPC methods.
async fn start_recording_app_server(
    config: &Config,
) -> Result<(
    AppServerSession,
    Arc<Mutex<Vec<String>>>,
    JoinHandle<Result<()>>,
)> {
    let state_db =
        crate::init_state_db_for_app_server_target(config, &crate::AppServerTarget::Embedded)
            .await?;
    let embedded = crate::start_embedded_app_server(
        codex_arg0::Arg0DispatchPaths::default(),
        config.clone(),
        Vec::new(),
        codex_config::LoaderOverrides::default(),
        /*strict_config*/ false,
        codex_config::CloudConfigBundleLoader::default(),
        /*log_db*/ None,
        state_db,
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
    )
    .await?;
    let codex_home = config.codex_home.display().to_string();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let request_sink = Arc::clone(&requests);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", listener.local_addr()?);
    let proxy = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut websocket = accept_async(stream).await?;
        while let Some(frame) = websocket.next().await {
            let Message::Text(text) = frame? else {
                continue;
            };
            let message = serde_json::from_str::<JSONRPCMessage>(&text)?;
            match message {
                JSONRPCMessage::Request(request) if request.method == "initialize" => {
                    websocket
                        .send(Message::Text(
                            serde_json::to_string(&JSONRPCMessage::Response(JSONRPCResponse {
                                id: request.id,
                                result: serde_json::json!({
                                    "userAgent": "codex-tui-test",
                                    "codexHome": codex_home,
                                }),
                            }))?
                            .into(),
                        ))
                        .await?;
                }
                JSONRPCMessage::Request(request) => {
                    request_sink
                        .lock()
                        .expect("request recorder lock")
                        .push(request.method.clone());
                    let request_id = request.id.clone();
                    let request =
                        serde_json::from_value::<ClientRequest>(serde_json::to_value(request)?)?;
                    let response = match embedded.request(request).await? {
                        Ok(result) => JSONRPCMessage::Response(JSONRPCResponse {
                            id: request_id,
                            result,
                        }),
                        Err(error) => JSONRPCMessage::Error(JSONRPCError {
                            id: request_id,
                            error,
                        }),
                    };
                    websocket
                        .send(Message::Text(serde_json::to_string(&response)?.into()))
                        .await?;
                }
                JSONRPCMessage::Notification(notification)
                    if notification.method == "initialized" => {}
                JSONRPCMessage::Notification(notification) => {
                    embedded
                        .notify(serde_json::from_value::<ClientNotification>(
                            serde_json::to_value(notification)?,
                        )?)
                        .await?;
                }
                JSONRPCMessage::Response(_) | JSONRPCMessage::Error(_) => {}
            }
        }
        embedded.shutdown().await?;
        Ok(())
    });
    let app_server = crate::connect_remote_app_server(crate::RemoteAppServerEndpoint::WebSocket {
        websocket_url,
        auth_token: None,
    })
    .await?;

    Ok((
        AppServerSession::new(
            app_server,
            crate::app_server_session::ThreadParamsMode::Embedded,
        ),
        requests,
        proxy,
    ))
}

async fn forward_remote_app_server_message(
    websocket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    embedded: &InProcessAppServerClient,
    request_sink: &Mutex<Vec<String>>,
    codex_home: &str,
    frame: std::result::Result<Message, tokio_tungstenite::tungstenite::Error>,
) -> Result<()> {
    let Message::Text(text) = frame? else {
        return Ok(());
    };
    let message = serde_json::from_str::<JSONRPCMessage>(&text)?;
    match message {
        JSONRPCMessage::Request(request) if request.method == "initialize" => {
            websocket
                .send(Message::Text(
                    serde_json::to_string(&JSONRPCMessage::Response(JSONRPCResponse {
                        id: request.id,
                        result: serde_json::json!({
                            "userAgent": "codex-tui-test",
                            "codexHome": codex_home,
                        }),
                    }))?
                    .into(),
                ))
                .await?;
        }
        JSONRPCMessage::Request(request) => {
            request_sink
                .lock()
                .expect("request recorder lock")
                .push(request.method.clone());
            let request_id = request.id.clone();
            let request = serde_json::from_value::<ClientRequest>(serde_json::to_value(request)?)?;
            let response = match embedded.request(request).await? {
                Ok(result) => JSONRPCMessage::Response(JSONRPCResponse {
                    id: request_id,
                    result,
                }),
                Err(error) => JSONRPCMessage::Error(JSONRPCError {
                    id: request_id,
                    error,
                }),
            };
            websocket
                .send(Message::Text(serde_json::to_string(&response)?.into()))
                .await?;
        }
        JSONRPCMessage::Notification(notification) if notification.method == "initialized" => {}
        JSONRPCMessage::Notification(notification) => {
            embedded
                .notify(serde_json::from_value::<ClientNotification>(
                    serde_json::to_value(notification)?,
                )?)
                .await?;
        }
        JSONRPCMessage::Response(_) | JSONRPCMessage::Error(_) => {}
    }
    Ok(())
}

/// Starts a proxy that drops the first WebSocket client without a closing handshake, then drops
/// the first replacement while restoring a thread before accepting the next replacement.
async fn start_reconnectable_app_server(
    config: &Config,
) -> Result<(
    AppServerSession,
    RemoteAppServerEndpoint,
    oneshot::Sender<()>,
    Arc<Mutex<Vec<String>>>,
    JoinHandle<Result<()>>,
)> {
    let state_db =
        crate::init_state_db_for_app_server_target(config, &crate::AppServerTarget::Embedded)
            .await?;
    let embedded = crate::start_embedded_app_server(
        codex_arg0::Arg0DispatchPaths::default(),
        config.clone(),
        Vec::new(),
        codex_config::LoaderOverrides::default(),
        /*strict_config*/ false,
        codex_config::CloudConfigBundleLoader::default(),
        /*log_db*/ None,
        state_db,
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
    )
    .await?;
    let codex_home = config.codex_home.display().to_string();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let request_sink = Arc::clone(&requests);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = RemoteAppServerEndpoint::WebSocket {
        websocket_url: format!("ws://{}", listener.local_addr()?),
        auth_token: None,
    };
    let (disconnect_tx, mut disconnect_rx) = oneshot::channel();
    let proxy = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await?;
        let mut first_websocket = accept_async(first_stream).await?;
        loop {
            tokio::select! {
                _ = &mut disconnect_rx => break,
                frame = first_websocket.next() => {
                    let Some(frame) = frame else {
                        break;
                    };
                    forward_remote_app_server_message(
                        &mut first_websocket,
                        &embedded,
                        &request_sink,
                        &codex_home,
                        frame,
                    )
                    .await?;
                }
            }
        }
        drop(first_websocket);

        let (second_stream, _) = listener.accept().await?;
        let mut second_websocket = accept_async(second_stream).await?;
        let mut force_restore_disconnect = true;
        loop {
            let Some(frame) = second_websocket.next().await else {
                break;
            };
            let is_thread_resume = if let Ok(Message::Text(text)) = &frame {
                serde_json::from_str::<JSONRPCMessage>(text)
                    .ok()
                    .is_some_and(|message| {
                        matches!(
                            message,
                            JSONRPCMessage::Request(request)
                                if request.method == "thread/resume"
                        )
                    })
            } else {
                false
            };
            if force_restore_disconnect && is_thread_resume {
                force_restore_disconnect = false;
                drop(second_websocket);
                let (third_stream, _) = listener.accept().await?;
                second_websocket = accept_async(third_stream).await?;
                continue;
            }
            forward_remote_app_server_message(
                &mut second_websocket,
                &embedded,
                &request_sink,
                &codex_home,
                frame,
            )
            .await?;
        }
        embedded.shutdown().await?;
        Ok(())
    });
    let app_server = crate::connect_remote_app_server(endpoint.clone()).await?;

    Ok((
        AppServerSession::new(
            app_server,
            crate::app_server_session::ThreadParamsMode::Embedded,
        ),
        endpoint,
        disconnect_tx,
        requests,
        proxy,
    ))
}

#[test]
fn session_lifecycle_avoids_redundant_subagent_metadata_reads() -> Result<()> {
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    std::thread::Builder::new()
        .name("tui-session-lifecycle-requests".to_string())
        .stack_size(TEST_STACK_SIZE_BYTES)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let mut app = make_test_app().await;
                let codex_home = tempdir()?;
                app.config.codex_home = codex_home.path().to_path_buf().abs();
                app.config.sqlite_home = codex_home.path().to_path_buf();
                std::fs::write(
                    codex_home.path().join("config.toml"),
                    r#"
[model_providers.anthropic]
name = "Anthropic"
base_url = "http://127.0.0.1:8317/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
"#,
                )?;
                app.config
                    .model_providers
                    .insert("anthropic".to_string(), app.config.model_provider.clone());
                let root_timestamp = "2026-01-01T00-00-00";
                let root_thread_id = ThreadId::from_string(
                    &create_fake_rollout(
                        codex_home.path(),
                        root_timestamp,
                        "2026-01-01T00:00:00Z",
                        "Saved user message",
                        Some(app.config.model_provider_id.as_str()),
                        /*git_info*/ None,
                    )
                    .expect("create root rollout"),
                )?;
                let child_thread_id = ThreadId::from_string(
                    &create_fake_parented_rollout_with_source(
                        codex_home.path(),
                        "2026-01-01T00-00-01",
                        "2026-01-01T00:00:01Z",
                        "Saved child message",
                        Some(app.config.model_provider_id.as_str()),
                        /*git_info*/ None,
                        RolloutSessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                            parent_thread_id: root_thread_id,
                            depth: 1,
                            agent_path: Some(
                                AgentPath::try_from("/root/worker").expect("valid agent path"),
                            ),
                            agent_nickname: Some("worker".to_string()),
                            agent_role: Some("worker".to_string()),
                        }),
                        root_thread_id.into(),
                        root_thread_id,
                    )
                    .expect("create child rollout"),
                )?;
                let root_rollout_path = rollout_path(
                    codex_home.path(),
                    root_timestamp,
                    &root_thread_id.to_string(),
                );
                let (mut app_server, requests, proxy) =
                    start_recording_app_server(&app.config).await?;
                let root = app_server
                    .resume_thread(
                        app.config.clone(),
                        root_thread_id,
                        app.resume_model_settings(),
                    )
                    .await?;
                app.enqueue_primary_thread_session(root.session, root.turns)
                    .await?;
                app.chat_widget.set_model_provider("anthropic");
                app.chat_widget.set_model("claude-opus-5");
                app_server
                    .resume_thread(
                        app.config.clone(),
                        child_thread_id,
                        app.resume_model_settings(),
                    )
                    .await?;
                let mut tui = crate::tui::test_support::make_test_tui()?;
                take_backfill_counts(&requests);

                let control = Box::pin(app.handle_event(
                    &mut tui,
                    &mut app_server,
                    AppEvent::ForkCurrentSession,
                ))
                .await?;

                assert!(matches!(control, AppRunControl::Continue));
                assert_ne!(app.chat_widget.thread_id(), Some(root_thread_id));
                assert_eq!(app.chat_widget.config_ref().model_provider_id, "anthropic");
                // Forking may read the source metadata once when the response includes its parent
                // id. It must not scan or backfill loaded threads for the newly created fork.
                assert!(matches!(take_backfill_counts(&requests), (0, 0) | (0, 1)));

                app.start_fresh_session_with_summary_hint(
                    &mut tui,
                    &mut app_server,
                    /*session_start_source*/ None,
                    /*initial_user_message*/ None,
                )
                .await;

                assert_ne!(app.chat_widget.thread_id(), Some(root_thread_id));
                assert_eq!(take_backfill_counts(&requests), (0, 0));

                let loaded_threads = app_server
                    .thread_loaded_list(ThreadLoadedListParams {
                        cursor: None,
                        limit: None,
                    })
                    .await?
                    .data;
                let expected_reads = loaded_threads
                    .iter()
                    .filter(|thread_id| *thread_id != &root_thread_id.to_string())
                    .count();
                assert!(loaded_threads.contains(&child_thread_id.to_string()));
                take_backfill_counts(&requests);
                app.harness_overrides.cwd = Some(app.config.cwd.to_path_buf());

                let control = app
                    .resume_target_session(
                        &mut tui,
                        &mut app_server,
                        crate::resume_picker::SessionTarget {
                            path: Some(root_rollout_path),
                            thread_id: root_thread_id,
                        },
                    )
                    .await?;

                assert!(matches!(control, AppRunControl::Continue));
                assert_eq!(app.chat_widget.thread_id(), Some(root_thread_id));
                assert_eq!(take_backfill_counts(&requests), (1, expected_reads));
                assert_eq!(
                    app.agent_navigation.get(&child_thread_id),
                    Some(&AgentPickerThreadEntry {
                        agent_nickname: Some("worker".to_string()),
                        agent_role: Some("worker".to_string()),
                        agent_path: Some("/root/worker".to_string()),
                        model_provider_id: None,
                        model: None,
                        reasoning_effort: None,
                        total_tokens: None,
                        is_running: false,
                        is_closed: false,
                        current_activity: None,
                    })
                );

                Box::pin(app.open_agent_picker(&mut app_server)).await;

                // The picker refreshes the primary thread once. Discovered children were already
                // refreshed by the picker's initial backfill and must not be read a second time.
                assert_eq!(take_backfill_counts(&requests), (1, expected_reads + 1));
                app_server.shutdown().await?;
                proxy.await??;
                Ok(())
            })
        })?
        .join()
        .expect("session lifecycle request test thread")
}

#[test]
fn local_daemon_reconnect_resumes_live_threads() -> Result<()> {
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    std::thread::Builder::new()
        .name("tui-local-daemon-reconnect".to_string())
        .stack_size(TEST_STACK_SIZE_BYTES)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let mut app = make_test_app().await;
                let codex_home = tempdir()?;
                app.config.codex_home = codex_home.path().to_path_buf().abs();
                app.config.sqlite_home = codex_home.path().to_path_buf();
                let stale_turn_id = "turn-interrupted-by-restart";
                let thread_id = ThreadId::from_string(
                    &create_fake_rollout(
                        codex_home.path(),
                        "2026-01-01T00-00-00",
                        "2026-01-01T00:00:00Z",
                        "Saved user message",
                        Some(app.config.model_provider_id.as_str()),
                        /*git_info*/ None,
                    )
                    .expect("create root rollout"),
                )?;
                codex_rollout::append_rollout_item_to_path(
                    &rollout_path(
                        codex_home.path(),
                        "2026-01-01T00-00-00",
                        &thread_id.to_string(),
                    ),
                    &RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
                        turn_id: stale_turn_id.to_string(),
                        trace_id: None,
                        started_at: None,
                        model_context_window: None,
                        collaboration_mode_kind: Default::default(),
                    })),
                )
                .await?;
                let (mut app_server, endpoint, disconnect_tx, requests, proxy) =
                    start_reconnectable_app_server(&app.config).await?;
                app.app_server_target = crate::AppServerTarget::LocalDaemon { endpoint };

                let started = app_server
                    .resume_thread(app.config.clone(), thread_id, app.resume_model_settings())
                    .await?;
                let mut stale_turn = started
                    .turns
                    .iter()
                    .find(|turn| turn.id == stale_turn_id)
                    .cloned()
                    .expect("resumed thread should include stale turn");
                stale_turn.status = TurnStatus::InProgress;
                app.enqueue_primary_thread_session(started.session, started.turns)
                    .await?;
                app.chat_widget.handle_server_notification(
                    ServerNotification::TurnStarted(TurnStartedNotification {
                        thread_id: thread_id.to_string(),
                        turn: stale_turn,
                    }),
                    /*replay_kind*/ None,
                );
                let channel = app
                    .thread_event_channels
                    .get(&thread_id)
                    .expect("primary thread event channel");
                channel.store.lock().await.active_turn_id = Some(stale_turn_id.to_string());
                assert!(app.chat_widget.is_task_running_for_test());

                disconnect_tx
                    .send(())
                    .expect("first connection should still be active");
                let event = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    app_server.next_event(),
                )
                .await?
                .ok_or_else(|| color_eyre::eyre::eyre!("remote event stream closed"))?;
                assert!(matches!(event, AppServerEvent::Disconnected { .. }));

                let AppServerEvent::Disconnected { message } = event else {
                    panic!("expected app-server disconnect event");
                };
                app.handle_app_server_disconnected(&mut app_server, message)
                    .await;
                while let Some(event) = app
                    .active_thread_rx
                    .as_mut()
                    .and_then(|receiver| receiver.try_recv().ok())
                {
                    app.handle_thread_event_now(event);
                }

                let resume_count = requests
                    .lock()
                    .expect("request recorder lock")
                    .iter()
                    .filter(|method| method.as_str() == "thread/resume")
                    .count();
                assert_eq!(resume_count, 2);
                assert!(!app.chat_widget.is_task_running_for_test());
                app_server.shutdown().await?;
                proxy.await??;
                Ok(())
            })
        })?
        .join()
        .expect("local daemon reconnect test thread")
}
