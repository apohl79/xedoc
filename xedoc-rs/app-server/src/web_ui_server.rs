//! Capability-protected loopback transport for the native session renderer.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::extract::WebSocketUpgrade;
use axum::extract::ws::Message;
use axum::extract::ws::WebSocket;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use xedoc_app_server_protocol::JSONRPCErrorError;
use xedoc_app_server_protocol::ThreadWebUiOpenParams;
use xedoc_app_server_protocol::ThreadWebUiOpenResponse;
use xedoc_core::ThreadManager;
use xedoc_protocol::ThreadId;

use crate::error_code::internal_error;
use crate::error_code::invalid_request;

const FRAME_LIMIT: usize = 2 * 1024 * 1024;
const INPUT_LIMIT: usize = 16 * 1024 * 1024;
const SEND_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 5);

pub(crate) struct WebUiServer {
    threads: Arc<ThreadManager>,
    sessions: Mutex<HashMap<ThreadId, RunningSession>>,
    shutdown: CancellationToken,
}

struct RunningSession {
    address: SocketAddr,
    browser_token: String,
    producer_token: String,
    state: Arc<SessionState>,
    created_at: Instant,
}

struct SessionState {
    thread_id: String,
    host: String,
    origin: String,
    browser_token: String,
    producer_token: String,
    frame: watch::Sender<Option<String>>,
    inputs: mpsc::Sender<String>,
    input_receiver: Mutex<Option<mpsc::Receiver<String>>>,
    producer_connected: AtomicBool,
    connections: Arc<Semaphore>,
    shutdown: CancellationToken,
}

impl WebUiServer {
    pub(crate) fn new(threads: Arc<ThreadManager>) -> Self {
        Self {
            threads,
            sessions: Mutex::new(HashMap::new()),
            shutdown: CancellationToken::new(),
        }
    }

    pub(crate) async fn open(
        &self,
        params: ThreadWebUiOpenParams,
    ) -> Result<ThreadWebUiOpenResponse, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|_| invalid_request("invalid thread ID"))?;
        let thread = self
            .threads
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request("web UI requires a running thread"))?;
        thread.ensure_rollout_materialized().await;
        thread
            .flush_rollout()
            .await
            .map_err(|_| internal_error("failed to persist session for browser attachment"))?;
        if self.shutdown.is_cancelled() {
            return Err(internal_error("web UI server is shutting down"));
        }
        let mut sessions = self.sessions.lock().await;
        if !sessions.contains_key(&thread_id) {
            if sessions.len() >= 16 {
                sessions.retain(|_, session| {
                    let in_use = session.created_at.elapsed()
                        < Duration::from_secs(/*secs*/ 30)
                        || session.state.producer_connected.load(Ordering::SeqCst)
                        || session.state.connections.available_permits() < 8;
                    if !in_use {
                        session.state.shutdown.cancel();
                    }
                    in_use
                });
            }
            if sessions.len() >= 16 {
                return Err(invalid_request("too many browser sessions"));
            }
            let listener = std::net::TcpListener::bind("127.0.0.1:0")
                .map_err(|_| internal_error("failed to bind session web UI"))?;
            listener
                .set_nonblocking(true)
                .map_err(|_| internal_error("failed to configure session web UI"))?;
            let listener = TcpListener::from_std(listener)
                .map_err(|_| internal_error("failed to start session web UI"))?;
            let address = listener
                .local_addr()
                .map_err(|_| internal_error("failed to inspect session web UI"))?;
            let browser_token = Uuid::now_v7().to_string();
            let producer_token = Uuid::now_v7().to_string();
            let (frame, _) = watch::channel(None);
            let (inputs, input_receiver) = mpsc::channel(/*buffer*/ 4);
            let state = Arc::new(SessionState {
                thread_id: params.thread_id,
                host: address.to_string(),
                origin: format!("http://{address}"),
                browser_token: browser_token.clone(),
                producer_token: producer_token.clone(),
                frame,
                inputs,
                input_receiver: Mutex::new(Some(input_receiver)),
                producer_connected: AtomicBool::new(false),
                connections: Arc::new(Semaphore::new(/*permits*/ 8)),
                shutdown: self.shutdown.child_token(),
            });
            let router = Router::new()
                .route("/", get(page))
                .route("/favicon.svg", get(favicon))
                .route("/webui.js", get(script))
                .route("/webui.css", get(styles))
                .route("/session/{token}", get(browser_upgrade))
                .route("/bridge/{token}", get(producer_upgrade))
                .with_state(Arc::clone(&state));
            let shutdown = state.shutdown.clone();
            tokio::spawn(async move {
                if axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown.cancelled_owned())
                    .await
                    .is_err()
                {
                    tracing::warn!("session web UI server stopped");
                }
            });
            sessions.insert(
                thread_id,
                RunningSession {
                    address,
                    browser_token,
                    producer_token,
                    state,
                    created_at: Instant::now(),
                },
            );
        }
        let session = sessions
            .get(&thread_id)
            .ok_or_else(|| internal_error("session web UI was not started"))?;
        Ok(ThreadWebUiOpenResponse {
            url: format!(
                "http://{}/#capability={}",
                session.address, session.browser_token
            ),
            bridge_url: format!("ws://{}/bridge/{}", session.address, session.producer_token),
        })
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

async fn browser_upgrade(
    State(state): State<Arc<SessionState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Response {
    if token != state.browser_token
        || !authorized_headers(&state, &headers)
        || headers.get(header::ORIGIN).is_none()
    {
        return secured(StatusCode::FORBIDDEN.into_response());
    }
    let Ok(permit) = state.connections.clone().try_acquire_owned() else {
        return secured(StatusCode::TOO_MANY_REQUESTS.into_response());
    };
    secured(
        websocket
            .max_message_size(INPUT_LIMIT)
            .max_frame_size(INPUT_LIMIT)
            .on_upgrade(move |socket| async move {
                let _permit = permit;
                browser_socket(socket, state).await;
            }),
    )
}

async fn producer_upgrade(
    State(state): State<Arc<SessionState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Response {
    if token != state.producer_token
        || !authorized_headers(&state, &headers)
        || state
            .producer_connected
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    {
        return secured(StatusCode::FORBIDDEN.into_response());
    }
    let reset_on_failed_upgrade = Arc::clone(&state);
    secured(
        websocket
            .max_message_size(FRAME_LIMIT)
            .max_frame_size(FRAME_LIMIT)
            .on_failed_upgrade(move |_| {
                reset_on_failed_upgrade
                    .producer_connected
                    .store(false, Ordering::SeqCst);
            })
            .on_upgrade(move |socket| producer_socket(socket, state)),
    )
}

fn authorized_headers(state: &SessionState, headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .is_some_and(|host| host.as_bytes() == state.host.as_bytes())
        && headers
            .get(header::ORIGIN)
            .is_none_or(|origin| origin.as_bytes() == state.origin.as_bytes())
}

async fn producer_socket(mut socket: WebSocket, state: Arc<SessionState>) {
    let Some(mut inputs) = state.input_receiver.lock().await.take() else {
        state.producer_connected.store(false, Ordering::SeqCst);
        return;
    };
    while inputs.try_recv().is_ok() {}
    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => break,
            input = inputs.recv() => {
                let Some(input) = input else { break };
                if !send(&mut socket, input).await { break; }
            }
            frame = socket.recv() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        let valid = serde_json::from_str::<serde_json::Value>(&text)
                            .is_ok_and(|frame| frame["type"] == "frame" && frame["threadId"] == state.thread_id);
                        if !valid { break; }
                        state.frame.send_replace(Some(text.to_string()));
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_) | Message::Close(_))) | Some(Err(_)) | None => break,
                }
            }
        }
    }
    state.frame.send_replace(None);
    *state.input_receiver.lock().await = Some(inputs);
    state.producer_connected.store(false, Ordering::SeqCst);
}

async fn browser_socket(mut socket: WebSocket, state: Arc<SessionState>) {
    let mut frames = state.frame.subscribe();
    let initial = frames.borrow_and_update().clone();
    if !send(
        &mut socket,
        initial.unwrap_or_else(|| "{\"type\":\"disconnected\"}".to_string()),
    )
    .await
    {
        return;
    }
    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => break,
            changed = frames.changed() => {
                if changed.is_err() { break; }
                let frame = frames.borrow_and_update().clone();
                if !send(&mut socket, frame.unwrap_or_else(|| "{\"type\":\"disconnected\"}".to_string())).await { break; }
            }
            input = socket.recv() => {
                match input {
                    Some(Ok(Message::Text(text))) => {
                        let valid = serde_json::from_str::<serde_json::Value>(&text)
                            .is_ok_and(|input| matches!(input["type"].as_str(), Some("key" | "paste" | "image" | "file" | "history" | "draft" | "removeImage" | "settings" | "question" | "resize" | "recallScheduledPrompt" | "recallQueuedPrompt")));
                        if !valid || !state.producer_connected.load(Ordering::SeqCst) { break; }
                        if !matches!(timeout(SEND_TIMEOUT, state.inputs.send(text.to_string())).await, Ok(Ok(()))) { break; }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_) | Message::Close(_))) | Some(Err(_)) | None => break,
                }
            }
        }
    }
}

async fn send(socket: &mut WebSocket, text: String) -> bool {
    matches!(
        timeout(SEND_TIMEOUT, socket.send(Message::Text(text.into()))).await,
        Ok(Ok(()))
    )
}

async fn page(State(state): State<Arc<SessionState>>) -> Response {
    let mut response = asset(
        include_str!("web_ui_assets/index.html"),
        "text/html; charset=utf-8",
    );
    response.headers_mut().insert(
        "x-xedoc-producer-connected",
        HeaderValue::from_static(if state.producer_connected.load(Ordering::SeqCst) {
            "true"
        } else {
            "false"
        }),
    );
    response
}

async fn favicon() -> Response {
    asset(include_str!("web_ui_assets/favicon.svg"), "image/svg+xml")
}

async fn script() -> Response {
    asset(
        concat!(
            include_str!("web_ui_assets/question.js"),
            "\n",
            include_str!("web_ui_assets/webui.js")
        ),
        "text/javascript; charset=utf-8",
    )
}

async fn styles() -> Response {
    asset(
        include_str!("web_ui_assets/webui.css"),
        "text/css; charset=utf-8",
    )
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    secured(response)
}

fn secured(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'none'; base-uri 'none'; connect-src 'self'; img-src 'self' data:; object-src blob: data:; form-action 'none'; frame-ancestors 'none'; script-src 'self'; style-src 'self'; style-src-attr 'unsafe-inline'",
    ));
    response
}
