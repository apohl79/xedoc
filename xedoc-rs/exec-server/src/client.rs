use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use arc_swap::ArcSwap;
use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::OnceCell;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio_util::task::AbortOnDropHandle;
use xedoc_exec_server_protocol::JSONRPCNotification;

use tokio::time::timeout;
use tracing::Instrument;
use tracing::debug;
use tracing::instrument::WithSubscriber;

use crate::ProcessId;
use crate::client::http_client::response_body_stream::MAX_QUEUED_HTTP_BODY_BYTES;
use crate::client::http_client::response_body_stream::QueuedHttpBodyDelta;
use crate::client_api::ExecServerClientConnectOptions;
use crate::client_api::ExecServerTransportParams;
use crate::client_api::HttpClient;
use crate::client_api::RemoteExecServerConnectArgs;
use crate::client_api::StdioExecServerConnectArgs;
use crate::client_transport::ExecServerReconnectStrategy;
use crate::connection::JsonRpcConnection;
use crate::environment::EnvironmentConnectionState;
use crate::process::ExecProcessEvent;
use crate::process::ExecProcessEventLog;
use crate::process::ExecProcessEventReceiver;
use crate::protocol::CAPABILITY_ROOTS_DISCOVER_METHOD;
use crate::protocol::CapabilityRootsDiscoverParams;
use crate::protocol::CapabilityRootsDiscoverResponse;
use crate::protocol::ENVIRONMENT_INFO_METHOD;
use crate::protocol::ENVIRONMENT_STATUS_METHOD;
use crate::protocol::EXEC_CLOSED_METHOD;
use crate::protocol::EXEC_EXITED_METHOD;
use crate::protocol::EXEC_METHOD;
use crate::protocol::EXEC_OUTPUT_DELTA_METHOD;
use crate::protocol::EXEC_READ_METHOD;
use crate::protocol::EXEC_SIGNAL_METHOD;
use crate::protocol::EXEC_TERMINATE_METHOD;
use crate::protocol::EXEC_WRITE_METHOD;
use crate::protocol::EnvironmentInfo;
use crate::protocol::EnvironmentStatus;
use crate::protocol::ExecClosedNotification;
use crate::protocol::ExecExitedNotification;
use crate::protocol::ExecOutputDeltaNotification;
use crate::protocol::ExecParams;
use crate::protocol::ExecResponse;
use crate::protocol::FS_CANONICALIZE_METHOD;
use crate::protocol::FS_CLOSE_METHOD;
use crate::protocol::FS_COPY_METHOD;
use crate::protocol::FS_CREATE_DIRECTORY_METHOD;
use crate::protocol::FS_GET_METADATA_METHOD;
use crate::protocol::FS_OPEN_METHOD;
use crate::protocol::FS_READ_BLOCK_METHOD;
use crate::protocol::FS_READ_DIRECTORY_METHOD;
use crate::protocol::FS_READ_FILE_METHOD;
use crate::protocol::FS_REMOVE_METHOD;
use crate::protocol::FS_WALK_METHOD;
use crate::protocol::FS_WRITE_FILE_METHOD;
use crate::protocol::FsCanonicalizeParams;
use crate::protocol::FsCanonicalizeResponse;
use crate::protocol::FsCloseParams;
use crate::protocol::FsCloseResponse;
use crate::protocol::FsCopyParams;
use crate::protocol::FsCopyResponse;
use crate::protocol::FsCreateDirectoryParams;
use crate::protocol::FsCreateDirectoryResponse;
use crate::protocol::FsGetMetadataParams;
use crate::protocol::FsGetMetadataResponse;
use crate::protocol::FsOpenParams;
use crate::protocol::FsOpenResponse;
use crate::protocol::FsReadBlockParams;
use crate::protocol::FsReadBlockResponse;
use crate::protocol::FsReadDirectoryParams;
use crate::protocol::FsReadDirectoryResponse;
use crate::protocol::FsReadFileParams;
use crate::protocol::FsReadFileResponse;
use crate::protocol::FsRemoveParams;
use crate::protocol::FsRemoveResponse;
use crate::protocol::FsWalkParams;
use crate::protocol::FsWalkResponse;
use crate::protocol::FsWriteFileParams;
use crate::protocol::FsWriteFileResponse;
use crate::protocol::HTTP_REQUEST_BODY_DELTA_METHOD;
use crate::protocol::INITIALIZE_METHOD;
use crate::protocol::INITIALIZED_METHOD;
use crate::protocol::InitializeParams;
use crate::protocol::InitializeResponse;
use crate::protocol::ProcessOutputChunk;
use crate::protocol::ProcessSignal;
use crate::protocol::ReadParams;
use crate::protocol::ReadResponse;
use crate::protocol::SignalParams;
use crate::protocol::SignalResponse;
use crate::protocol::TerminateParams;
use crate::protocol::TerminateResponse;
use crate::protocol::WriteParams;
use crate::protocol::WriteResponse;
use crate::rpc::RpcCallError;
use crate::rpc::RpcClient;

pub(crate) mod http_client;
#[path = "client_recovery.rs"]
mod recovery;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);
const ENVIRONMENT_INFO_TIMEOUT: Duration = Duration::from_secs(30);
const ENVIRONMENT_STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_EVENT_CHANNEL_CAPACITY: usize = 256;
const PROCESS_EVENT_RETAINED_BYTES: usize = 1024 * 1024;
const MAX_PENDING_PROCESS_EVENTS: usize = 256;
const MAX_PENDING_PROCESS_EVENT_BYTES: usize = 1024 * 1024;

impl Default for ExecServerClientConnectOptions {
    fn default() -> Self {
        Self {
            client_name: "xedoc-core".to_string(),
            initialize_timeout: INITIALIZE_TIMEOUT,
            resume_session_id: None,
        }
    }
}

impl From<RemoteExecServerConnectArgs> for ExecServerClientConnectOptions {
    fn from(value: RemoteExecServerConnectArgs) -> Self {
        Self {
            client_name: value.client_name,
            initialize_timeout: value.initialize_timeout,
            resume_session_id: value.resume_session_id,
        }
    }
}

impl From<StdioExecServerConnectArgs> for ExecServerClientConnectOptions {
    fn from(value: StdioExecServerConnectArgs) -> Self {
        Self {
            client_name: value.client_name,
            initialize_timeout: value.initialize_timeout,
            resume_session_id: value.resume_session_id,
        }
    }
}

impl RemoteExecServerConnectArgs {
    pub fn new(websocket_url: String, client_name: String) -> Self {
        Self {
            websocket_url,
            client_name,
            connect_timeout: CONNECT_TIMEOUT,
            initialize_timeout: INITIALIZE_TIMEOUT,
            resume_session_id: None,
        }
    }
}

pub(crate) struct SessionState {
    wake_tx: watch::Sender<u64>,
    events: ExecProcessEventLog,
    ordered_events: StdMutex<OrderedSessionEvents>,
    recoverable: AtomicBool,
    next_write_id: AtomicU64,
}

#[derive(Default)]
struct OrderedSessionEvents {
    last_published_seq: u64,
    exit_published: bool,
    closed_published: bool,
    // Server-side output, exit, and closed notifications are emitted by
    // different tasks and can reach the client out of order. Keep future events
    // here until all lower sequence numbers have been published.
    pending: BTreeMap<u64, ExecProcessEvent>,
    pending_bytes: usize,
    failure: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Session {
    client: ExecServerClient,
    process_id: ProcessId,
    state: Arc<SessionState>,
}

struct Inner {
    connection: StdMutex<ConnectionState>,
    connection_changed: watch::Sender<()>,
    // The remote transport delivers one shared notification stream for every
    // process on the connection. Keep a local process_id -> session registry so
    // we can turn those connection-global notifications into process wakeups
    // without making notifications the source of truth for output delivery.
    sessions: ArcSwap<HashMap<ProcessId, Arc<SessionState>>>,
    // ArcSwap makes reads cheap on the hot notification path, but writes still
    // need serialization so concurrent register/remove operations do not
    // overwrite each other's copy-on-write updates.
    sessions_write_lock: StdMutex<()>,
    // Streaming HTTP responses are keyed by a client-generated request id
    // because they share the same connection-global notification channel as
    // process output. Keep the routing table local to the client so higher
    // layers can consume body chunks like a normal byte stream.
    http_body_streams: ArcSwap<HashMap<String, mpsc::Sender<QueuedHttpBodyDelta>>>,
    http_body_stream_failures: ArcSwap<HashMap<String, String>>,
    http_body_streams_write_lock: Mutex<()>,
    http_body_stream_byte_budget: Arc<Semaphore>,
    http_body_stream_next_id: AtomicU64,
    session_id: OnceLock<String>,
    reconnect_strategy: Option<ExecServerReconnectStrategy>,
}

struct ConnectionState {
    status: ConnectionStatus,
    active_process_starts: usize,
    environment_connection_state_tx: watch::Sender<EnvironmentConnectionState>,
}

enum ConnectionStatus {
    Connected(Arc<RpcClient>),
    Recovering,
    Failed(String),
}

impl ConnectionState {
    fn set_status(&mut self, status: ConnectionStatus) {
        self.status = status;
        self.publish_environment_connection_state();
    }

    fn publish_environment_connection_state(&self) {
        let state = match &self.status {
            ConnectionStatus::Connected(rpc_client) if !rpc_client.is_disconnected() => {
                EnvironmentConnectionState::Connected
            }
            ConnectionStatus::Connected(_)
            | ConnectionStatus::Recovering
            | ConnectionStatus::Failed(_) => EnvironmentConnectionState::Disconnected,
        };
        let _ = self
            .environment_connection_state_tx
            .send_if_modified(|current| {
                if *current == state {
                    false
                } else {
                    *current = state;
                    true
                }
            });
    }
}

#[derive(Clone, Copy)]
enum RecoveryPolicy {
    Wait,
    FailFast,
}

#[derive(Clone)]
pub struct ExecServerClient {
    inner: Arc<Inner>,
    recovery_policy: RecoveryPolicy,
}

struct ActiveProcessStart {
    inner: Arc<Inner>,
}

impl Drop for ActiveProcessStart {
    fn drop(&mut self) {
        self.inner.finish_process_start();
    }
}

type ConnectionResult = Result<ExecServerClient, Arc<ExecServerError>>;
type ConnectionAttempt = OnceCell<ConnectionResult>;

#[derive(Clone)]
pub(crate) struct LazyRemoteExecServerClient {
    transport_params: ExecServerTransportParams,
    recovery_policy: RecoveryPolicy,
    // Saves the first startup result so callers share it and failures remain final.
    startup: Arc<ConnectionAttempt>,
    // The latest successful client, replaced whenever reconnecting succeeds.
    current_client: Arc<StdMutex<Option<ExecServerClient>>>,
    reconnect: Arc<StdMutex<Option<Arc<ConnectionAttempt>>>>,
    environment_connection_state_tx: watch::Sender<EnvironmentConnectionState>,
}

impl LazyRemoteExecServerClient {
    pub(crate) fn new(transport_params: ExecServerTransportParams) -> Self {
        Self {
            transport_params,
            recovery_policy: RecoveryPolicy::Wait,
            startup: Arc::new(ConnectionAttempt::new()),
            current_client: Arc::new(StdMutex::new(None)),
            reconnect: Arc::new(StdMutex::new(None)),
            environment_connection_state_tx: watch::channel(
                EnvironmentConnectionState::Disconnected,
            )
            .0,
        }
    }

    pub(crate) fn subscribe_connection_state(&self) -> watch::Receiver<EnvironmentConnectionState> {
        self.environment_connection_state_tx.subscribe()
    }

    pub(crate) fn start_connecting(&self) -> Option<AbortOnDropHandle<()>> {
        // Stdio starts a process, so keep it lazy until the environment is used.
        if matches!(
            self.transport_params,
            ExecServerTransportParams::StdioCommand { .. }
        ) {
            return None;
        }
        let client = self.clone();
        Some(AbortOnDropHandle::new(tokio::spawn(async move {
            if let Err(error) = client.wait_until_ready().await {
                debug!(%error, "exec-server environment startup failed");
            }
        })))
    }

    pub(crate) fn startup_finished(&self) -> bool {
        self.startup.get().is_some()
    }

    pub(crate) fn readiness_result(&self) -> Option<Result<(), ExecServerError>> {
        if let Some(client) = self.cached_client() {
            return client.readiness_result();
        }
        self.startup.get().and_then(|result| match result {
            Ok(client) => client.readiness_result(),
            Err(error) => Some(Err(ExecServerError::ConnectionAttempt(Arc::clone(error)))),
        })
    }

    pub(crate) async fn status(&self) -> crate::EnvironmentObservedStatus {
        // Fail-fast lookup preserves the non-mutating contract: never start or recover a client.
        let client = match self.fail_fast().get().await {
            Ok(client) => client,
            Err(error) => {
                // Without a completed startup attempt, there is no exec-server connection to probe.
                if self.cached_client().is_none() && self.startup.get().is_none() {
                    return crate::EnvironmentObservedStatus::Pending;
                }
                // A known connection failure is reported without retrying it as part of status.
                return crate::EnvironmentObservedStatus::Disconnected {
                    error: error.to_string(),
                };
            }
        };
        // Every callable client is probed so callers never receive a cached health result.
        match client.environment_status().await {
            Ok(_) => crate::EnvironmentObservedStatus::Ready,
            Err(error) => crate::EnvironmentObservedStatus::Disconnected {
                error: error.to_string(),
            },
        }
    }

    pub(crate) fn fail_fast(&self) -> Self {
        Self {
            recovery_policy: RecoveryPolicy::FailFast,
            ..self.clone()
        }
    }

    pub(crate) async fn wait_until_ready(&self) -> Result<(), ExecServerError> {
        self.initial_client().await.map(drop)
    }

    pub(crate) async fn get(&self) -> Result<ExecServerClient, ExecServerError> {
        if matches!(self.recovery_policy, RecoveryPolicy::FailFast) {
            let client = match self.cached_client() {
                Some(client) => client,
                None => match self.startup.get() {
                    Some(Ok(client)) => client.clone(),
                    Some(Err(error)) => {
                        return Err(ExecServerError::ConnectionAttempt(Arc::clone(error)));
                    }
                    None => {
                        return Err(ExecServerError::Disconnected(
                            "exec-server environment is not ready".to_string(),
                        ));
                    }
                },
            };
            return client.fail_fast();
        }
        if let Some(client) = self.connected_client() {
            return Ok(client);
        }

        let Some(cached_client) = self.cached_client() else {
            let client = self.initial_client().await?;
            if !client.is_disconnected() || !self.can_reconnect() {
                return Ok(client);
            }
            return self.reconnect().await;
        };

        if !self.can_reconnect() {
            return Ok(cached_client);
        }

        self.reconnect().await
    }

    async fn initial_client(&self) -> Result<ExecServerClient, ExecServerError> {
        // The first caller starts the work; every other caller waits for that same result.
        let result = self.startup.get_or_init(|| self.connect_once()).await;
        match result {
            Ok(client) => {
                let mut current_client = self
                    .current_client
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if current_client.is_none() {
                    *current_client = Some(client.clone());
                }
                Ok(client.clone())
            }
            Err(error) => Err(ExecServerError::ConnectionAttempt(Arc::clone(error))),
        }
    }

    async fn reconnect(&self) -> Result<ExecServerClient, ExecServerError> {
        // Callers handling the same outage share one reconnect attempt.
        let attempt = {
            let mut reconnect = self
                .reconnect
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(client) = self.connected_client() {
                return Ok(client);
            }
            reconnect
                .get_or_insert_with(|| Arc::new(ConnectionAttempt::new()))
                .clone()
        };
        let result = attempt
            .get_or_init(|| async {
                let result = self.connect_once().await;
                if let Ok(client) = &result {
                    *self
                        .current_client
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(client.clone());
                }
                result
            })
            .await;
        let mut reconnect = self
            .reconnect
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Forget only this completed attempt so a later operation can retry after failure.
        if reconnect
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &attempt))
        {
            *reconnect = None;
        }
        result.clone().map_err(ExecServerError::ConnectionAttempt)
    }

    fn connected_client(&self) -> Option<ExecServerClient> {
        self.cached_client()
            .filter(|client| !client.is_disconnected())
    }

    fn cached_client(&self) -> Option<ExecServerClient> {
        self.current_client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn can_reconnect(&self) -> bool {
        matches!(
            self.transport_params,
            ExecServerTransportParams::Deferred(_)
                | ExecServerTransportParams::WebSocketUrl { .. }
                | ExecServerTransportParams::NoiseRendezvous { .. }
        )
    }

    async fn connect_once(&self) -> ConnectionResult {
        let result = ExecServerClient::connect_for_transport(self.transport_params.clone())
            .await
            .map_err(Arc::new);
        if let Ok(client) = &result {
            client
                .attach_environment_connection_state(self.environment_connection_state_tx.clone());
        }
        result
    }
}

impl HttpClient for LazyRemoteExecServerClient {
    fn http_request(
        &self,
        params: crate::HttpRequestParams,
    ) -> BoxFuture<'_, Result<crate::HttpRequestResponse, ExecServerError>> {
        async move { self.get().await?.http_request(params).await }.boxed()
    }

    fn http_request_stream(
        &self,
        params: crate::HttpRequestParams,
    ) -> BoxFuture<
        '_,
        Result<(crate::HttpRequestResponse, crate::HttpResponseBodyStream), ExecServerError>,
    > {
        async move { self.get().await?.http_request_stream(params).await }.boxed()
    }
}

impl LazyRemoteExecServerClient {
    pub(crate) async fn environment_info(&self) -> Result<EnvironmentInfo, ExecServerError> {
        self.get().await?.environment_info().await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecServerError {
    #[error("failed to spawn exec-server: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("timed out connecting to exec-server websocket `{url}` after {timeout:?}")]
    WebSocketConnectTimeout { url: String, timeout: Duration },
    #[error("failed to connect to exec-server websocket `{url}`: {source}")]
    WebSocketConnect {
        url: String,
        #[source]
        source: tokio_tungstenite::tungstenite::Error,
    },
    #[error("timed out waiting for exec-server initialize handshake after {timeout:?}")]
    InitializeTimedOut { timeout: Duration },
    #[error("exec-server transport closed")]
    Closed,
    #[error("{0}")]
    Disconnected(String),
    #[error("failed to serialize or deserialize exec-server JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("HTTP request failed: {0}")]
    HttpRequest(String),
    #[error("exec-server protocol error: {0}")]
    Protocol(String),
    #[error("exec-server rejected request ({code}): {message}")]
    Server { code: i64, message: String },
    #[error("environment registry request failed ({status}{code_suffix}): {message}", code_suffix = .code.as_ref().map(|code| format!(", {code}")).unwrap_or_default())]
    EnvironmentRegistryHttp {
        status: reqwest::StatusCode,
        code: Option<String>,
        message: String,
    },
    #[error("environment registry configuration error: {0}")]
    EnvironmentRegistryConfig(String),
    #[error("environment registry authentication error: {0}")]
    EnvironmentRegistryAuth(String),
    #[error("environment registry request failed: {0}")]
    EnvironmentRegistryRequest(#[from] reqwest::Error),
    #[error("exec-server connection attempt failed: {0}")]
    ConnectionAttempt(#[source] Arc<ExecServerError>),
}

impl ExecServerClient {
    fn attach_environment_connection_state(
        &self,
        state_tx: watch::Sender<EnvironmentConnectionState>,
    ) {
        let mut connection = self
            .inner
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        connection.environment_connection_state_tx = state_tx;
        connection.publish_environment_connection_state();
    }

    fn fail_fast(&self) -> Result<Self, ExecServerError> {
        self.rpc_client_without_recovery()?;
        Ok(Self {
            inner: Arc::clone(&self.inner),
            recovery_policy: RecoveryPolicy::FailFast,
        })
    }

    fn rpc_client_without_recovery(&self) -> Result<Arc<RpcClient>, ExecServerError> {
        let connection = self
            .inner
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &connection.status {
            ConnectionStatus::Connected(rpc_client) if !rpc_client.is_disconnected() => {
                Ok(Arc::clone(rpc_client))
            }
            ConnectionStatus::Connected(_) | ConnectionStatus::Recovering => Err(
                ExecServerError::Disconnected("exec-server environment is recovering".to_string()),
            ),
            ConnectionStatus::Failed(message) => {
                Err(ExecServerError::Disconnected(message.clone()))
            }
        }
    }

    async fn rpc_client(&self) -> Result<Arc<RpcClient>, ExecServerError> {
        match self.recovery_policy {
            RecoveryPolicy::Wait => self.inner.rpc_client().await,
            RecoveryPolicy::FailFast => self.rpc_client_without_recovery(),
        }
    }

    async fn initialize_rpc(
        &self,
        rpc_client: &RpcClient,
        options: ExecServerClientConnectOptions,
    ) -> Result<InitializeResponse, ExecServerError> {
        let ExecServerClientConnectOptions {
            client_name,
            initialize_timeout,
            resume_session_id,
        } = options;

        timeout(initialize_timeout, async {
            let response: InitializeResponse = rpc_client
                .call(
                    INITIALIZE_METHOD,
                    &InitializeParams {
                        client_name,
                        resume_session_id,
                    },
                )
                .await?;
            let session_id = self
                .inner
                .session_id
                .get_or_init(|| response.session_id.clone());
            if session_id != &response.session_id {
                return Err(ExecServerError::Protocol(format!(
                    "exec-server initialized an unexpected session {}",
                    response.session_id
                )));
            }
            rpc_client
                .notify(INITIALIZED_METHOD, &serde_json::json!({}))
                .await?;
            Ok(response)
        })
        .await
        .map_err(|_| ExecServerError::InitializeTimedOut {
            timeout: initialize_timeout,
        })?
    }

    pub async fn exec(&self, params: ExecParams) -> Result<ExecResponse, ExecServerError> {
        self.call(EXEC_METHOD, &params).await
    }

    pub async fn environment_info(&self) -> Result<EnvironmentInfo, ExecServerError> {
        let rpc_client = self.rpc_client().await?;
        map_rpc_call_result(
            rpc_client
                .call_with_timeout(ENVIRONMENT_INFO_METHOD, &(), ENVIRONMENT_INFO_TIMEOUT)
                .await,
        )
    }

    pub async fn environment_status(&self) -> Result<EnvironmentStatus, ExecServerError> {
        // Health checks only reuse an existing RPC connection and never initiate recovery.
        let rpc_client = self.rpc_client_without_recovery()?;
        map_rpc_call_result(
            rpc_client
                .call_with_timeout(ENVIRONMENT_STATUS_METHOD, &(), ENVIRONMENT_STATUS_TIMEOUT)
                .await,
        )
    }

    pub async fn discover_capability_roots(
        &self,
        params: CapabilityRootsDiscoverParams,
    ) -> Result<CapabilityRootsDiscoverResponse, ExecServerError> {
        self.call(CAPABILITY_ROOTS_DISCOVER_METHOD, &params).await
    }

    pub async fn read(&self, params: ReadParams) -> Result<ReadResponse, ExecServerError> {
        self.call(EXEC_READ_METHOD, &params).await
    }

    pub async fn write(
        &self,
        process_id: &ProcessId,
        chunk: Vec<u8>,
        write_id: String,
    ) -> Result<WriteResponse, ExecServerError> {
        self.call(
            EXEC_WRITE_METHOD,
            &WriteParams {
                process_id: process_id.clone(),
                chunk: chunk.into(),
                write_id,
            },
        )
        .await
    }

    pub async fn signal(
        &self,
        process_id: &ProcessId,
        signal: ProcessSignal,
    ) -> Result<(), ExecServerError> {
        let _response: SignalResponse = self
            .call(
                EXEC_SIGNAL_METHOD,
                &SignalParams {
                    process_id: process_id.clone(),
                    signal,
                },
            )
            .await?;
        Ok(())
    }

    pub async fn terminate(
        &self,
        process_id: &ProcessId,
    ) -> Result<TerminateResponse, ExecServerError> {
        self.call_for_cleanup(
            EXEC_TERMINATE_METHOD,
            &TerminateParams {
                process_id: process_id.clone(),
            },
        )
        .await
    }

    pub async fn fs_read_file(
        &self,
        params: FsReadFileParams,
    ) -> Result<FsReadFileResponse, ExecServerError> {
        self.call(FS_READ_FILE_METHOD, &params).await
    }

    pub async fn fs_open(&self, params: FsOpenParams) -> Result<FsOpenResponse, ExecServerError> {
        self.call(FS_OPEN_METHOD, &params).await
    }

    pub async fn fs_read_block(
        &self,
        params: FsReadBlockParams,
    ) -> Result<FsReadBlockResponse, ExecServerError> {
        self.call(FS_READ_BLOCK_METHOD, &params).await
    }

    pub async fn fs_close(
        &self,
        params: FsCloseParams,
    ) -> Result<FsCloseResponse, ExecServerError> {
        self.call_for_cleanup(FS_CLOSE_METHOD, &params).await
    }

    pub async fn fs_write_file(
        &self,
        params: FsWriteFileParams,
    ) -> Result<FsWriteFileResponse, ExecServerError> {
        self.call(FS_WRITE_FILE_METHOD, &params).await
    }

    pub async fn fs_create_directory(
        &self,
        params: FsCreateDirectoryParams,
    ) -> Result<FsCreateDirectoryResponse, ExecServerError> {
        self.call(FS_CREATE_DIRECTORY_METHOD, &params).await
    }

    pub async fn fs_get_metadata(
        &self,
        params: FsGetMetadataParams,
    ) -> Result<FsGetMetadataResponse, ExecServerError> {
        self.call(FS_GET_METADATA_METHOD, &params).await
    }

    pub async fn fs_canonicalize(
        &self,
        params: FsCanonicalizeParams,
    ) -> Result<FsCanonicalizeResponse, ExecServerError> {
        self.call(FS_CANONICALIZE_METHOD, &params).await
    }

    pub async fn fs_read_directory(
        &self,
        params: FsReadDirectoryParams,
    ) -> Result<FsReadDirectoryResponse, ExecServerError> {
        self.call(FS_READ_DIRECTORY_METHOD, &params).await
    }

    pub async fn fs_walk(&self, params: FsWalkParams) -> Result<FsWalkResponse, ExecServerError> {
        self.call(FS_WALK_METHOD, &params).await
    }

    pub async fn fs_remove(
        &self,
        params: FsRemoveParams,
    ) -> Result<FsRemoveResponse, ExecServerError> {
        self.call(FS_REMOVE_METHOD, &params).await
    }

    pub async fn fs_copy(&self, params: FsCopyParams) -> Result<FsCopyResponse, ExecServerError> {
        self.call(FS_COPY_METHOD, &params).await
    }

    pub(crate) async fn start_process(
        &self,
        params: ExecParams,
    ) -> Result<Session, ExecServerError> {
        loop {
            let rpc_client = self.rpc_client().await?;
            if !self.inner.begin_process_start(&rpc_client) {
                continue;
            }

            let process_id = params.process_id.clone();
            let state = Arc::new(SessionState::new(/*recoverable*/ false));
            if let Err(error) = self.inner.insert_session(&process_id, Arc::clone(&state)) {
                self.inner.finish_process_start();
                return Err(error);
            }
            let active_start = ActiveProcessStart {
                inner: Arc::clone(&self.inner),
            };
            let client = self.clone();
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            let process_start_task = async move {
                let _active_start = active_start;
                match client
                    .call_rpc::<_, ExecResponse>(&rpc_client, EXEC_METHOD, &params)
                    .await
                {
                    Ok(_) => {
                        state.recoverable.store(true, Ordering::Release);
                        let session = Session {
                            client: client.clone(),
                            process_id: process_id.clone(),
                            state: Arc::clone(&state),
                        };
                        if result_tx.send(Ok(session)).is_err() {
                            state.recoverable.store(false, Ordering::Release);
                            tokio::spawn(async move {
                                cleanup_process_start(&client, &process_id, &state).await;
                            });
                        }
                    }
                    Err(error) => {
                        if is_transport_closed_error(&error) {
                            tokio::spawn(async move {
                                cleanup_process_start(&client, &process_id, &state).await;
                            });
                        } else {
                            client.inner.remove_session_if(&process_id, &state);
                        }
                        let _ = result_tx.send(Err(error));
                    }
                }
            };
            tokio::spawn(
                process_start_task
                    .in_current_span()
                    .with_current_subscriber(),
            );
            return result_rx.await.map_err(|_| {
                ExecServerError::Protocol("process start task stopped unexpectedly".to_string())
            })?;
        }
    }

    #[cfg(test)]
    pub(crate) async fn register_session(
        &self,
        process_id: &ProcessId,
    ) -> Result<Session, ExecServerError> {
        let state = Arc::new(SessionState::new(/*recoverable*/ true));
        self.inner.insert_session(process_id, Arc::clone(&state))?;
        Ok(Session {
            client: self.clone(),
            process_id: process_id.clone(),
            state,
        })
    }

    pub fn session_id(&self) -> Option<String> {
        self.inner.session_id.get().cloned()
    }

    fn is_disconnected(&self) -> bool {
        self.inner.is_failed()
    }

    fn readiness_result(&self) -> Option<Result<(), ExecServerError>> {
        let connection = self
            .inner
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &connection.status {
            ConnectionStatus::Connected(rpc_client) if !rpc_client.is_disconnected() => {
                Some(Ok(()))
            }
            ConnectionStatus::Connected(_) | ConnectionStatus::Recovering => None,
            ConnectionStatus::Failed(message) => {
                Some(Err(ExecServerError::Disconnected(message.clone())))
            }
        }
    }

    pub(crate) async fn connect(
        connection: JsonRpcConnection,
        options: ExecServerClientConnectOptions,
    ) -> Result<Self, ExecServerError> {
        Self::connect_with_recovery(connection, options, /*reconnect_strategy*/ None).await
    }

    pub(crate) async fn connect_with_recovery(
        connection: JsonRpcConnection,
        options: ExecServerClientConnectOptions,
        reconnect_strategy: Option<ExecServerReconnectStrategy>,
    ) -> Result<Self, ExecServerError> {
        let (rpc_client, events_rx) = RpcClient::new(connection);
        let rpc_client = Arc::new(rpc_client);
        let session_id = OnceLock::new();
        let (connection_changed, _connection_changed_rx) = watch::channel(());
        let inner = Arc::new(Inner {
            connection: StdMutex::new(ConnectionState {
                status: ConnectionStatus::Connected(Arc::clone(&rpc_client)),
                active_process_starts: 0,
                environment_connection_state_tx: watch::channel(
                    EnvironmentConnectionState::Connected,
                )
                .0,
            }),
            connection_changed,
            sessions: ArcSwap::from_pointee(HashMap::new()),
            sessions_write_lock: StdMutex::new(()),
            http_body_streams: ArcSwap::from_pointee(HashMap::new()),
            http_body_stream_failures: ArcSwap::from_pointee(HashMap::new()),
            http_body_streams_write_lock: Mutex::new(()),
            http_body_stream_byte_budget: Arc::new(Semaphore::new(MAX_QUEUED_HTTP_BODY_BYTES)),
            http_body_stream_next_id: AtomicU64::new(1),
            session_id,
            reconnect_strategy,
        });
        let client = Self {
            inner,
            recovery_policy: RecoveryPolicy::Wait,
        };
        // An explicit resume can redirect notifications from running processes
        // before initialize returns. Drain them immediately so a burst cannot
        // fill the bounded event channel and block the initialize response.
        client.spawn_rpc_reader(&rpc_client, events_rx);
        client.initialize_rpc(&rpc_client, options).await?;
        Ok(client)
    }

    async fn call<P, T>(&self, method: &str, params: &P) -> Result<T, ExecServerError>
    where
        P: serde::Serialize,
        T: serde::de::DeserializeOwned,
    {
        let rpc_client = self.rpc_client().await?;
        self.call_rpc(&rpc_client, method, params).await
    }

    async fn call_rpc<P, T>(
        &self,
        rpc_client: &Arc<RpcClient>,
        method: &str,
        params: &P,
    ) -> Result<T, ExecServerError>
    where
        P: serde::Serialize,
        T: serde::de::DeserializeOwned,
    {
        map_rpc_call_result(rpc_client.call(method, params).await)
    }

    async fn call_for_cleanup<P, T>(&self, method: &str, params: &P) -> Result<T, ExecServerError>
    where
        P: serde::Serialize,
        T: serde::de::DeserializeOwned,
    {
        let rpc_client = self.inner.rpc_client().await?;
        map_rpc_call_result(rpc_client.call_for_cleanup(method, params).await)
    }
}

fn map_rpc_call_result<T>(result: Result<T, RpcCallError>) -> Result<T, ExecServerError> {
    result.map_err(|error| {
        let error = ExecServerError::from(error);
        if is_transport_closed_error(&error) {
            ExecServerError::Disconnected(disconnected_message(/*reason*/ None))
        } else {
            error
        }
    })
}

async fn cleanup_process_start(
    client: &ExecServerClient,
    process_id: &ProcessId,
    state: &Arc<SessionState>,
) {
    loop {
        match client.terminate(process_id).await {
            Ok(_) => break,
            Err(error) if is_transport_closed_error(&error) && !client.inner.is_failed() => {
                continue;
            }
            Err(_) => break,
        }
    }
    client.inner.remove_session_if(process_id, state);
}

impl From<RpcCallError> for ExecServerError {
    fn from(value: RpcCallError) -> Self {
        match value {
            RpcCallError::Closed => Self::Closed,
            RpcCallError::Json(err) => Self::Json(err),
            RpcCallError::Server(error) => Self::Server {
                code: error.code,
                message: error.message,
            },
            RpcCallError::TimedOut { method, timeout } => Self::Protocol(format!(
                "timed out waiting for exec-server `{method}` response after {timeout:?}"
            )),
            RpcCallError::PendingRequestLimitExceeded { limit } => Self::Protocol(format!(
                "exec-server has reached its limit of {limit} pending requests"
            )),
        }
    }
}

impl SessionState {
    fn new(recoverable: bool) -> Self {
        let (wake_tx, _wake_rx) = watch::channel(0);
        Self {
            wake_tx,
            events: ExecProcessEventLog::new(
                PROCESS_EVENT_CHANNEL_CAPACITY,
                PROCESS_EVENT_RETAINED_BYTES,
            ),
            ordered_events: StdMutex::new(OrderedSessionEvents::default()),
            recoverable: AtomicBool::new(recoverable),
            next_write_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.wake_tx.subscribe()
    }

    pub(crate) fn subscribe_events(&self) -> ExecProcessEventReceiver {
        self.events.subscribe()
    }

    fn note_change(&self, seq: u64) {
        self.wake_tx
            .send_modify(|current| *current = (*current).max(seq));
    }

    /// Publishes a process event only when all earlier sequenced events have
    /// already been published.
    ///
    /// Returns `true` only when this call actually publishes the ordered
    /// `Closed` event. The caller uses that signal to remove the session route
    /// after the terminal event is visible to subscribers, rather than when a
    /// possibly-early closed notification first arrives.
    fn publish_ordered_event(&self, event: ExecProcessEvent) -> Result<bool, String> {
        let Some(seq) = event.seq() else {
            self.events.publish(event);
            return Ok(false);
        };

        let mut ordered_events = self
            .ordered_events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // We have already delivered this sequence number or moved past it,
        // so accepting it again would duplicate output or lifecycle events.
        if ordered_events.failure.is_some()
            || ordered_events.closed_published
            || seq <= ordered_events.last_published_seq
        {
            return Ok(false);
        }

        ordered_events.insert_pending(event)?;
        Ok(self.publish_ready(&mut ordered_events))
    }

    fn publish_ready(&self, ordered_events: &mut OrderedSessionEvents) -> bool {
        let mut published_closed = false;
        loop {
            let next_seq = ordered_events.last_published_seq.saturating_add(1);
            let Some(event) = ordered_events.pending.remove(&next_seq) else {
                break;
            };
            ordered_events.pending_bytes = ordered_events
                .pending_bytes
                .saturating_sub(pending_process_event_bytes(&event));
            ordered_events.last_published_seq = next_seq;
            ordered_events.exit_published |= matches!(&event, ExecProcessEvent::Exited { .. });
            let is_closed = matches!(&event, ExecProcessEvent::Closed { .. });
            ordered_events.closed_published |= is_closed;
            published_closed |= is_closed;
            self.events.publish(event);
        }
        published_closed
    }

    fn set_failure(&self, message: String) {
        let mut ordered_events = self
            .ordered_events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ordered_events.failure.is_some() || ordered_events.closed_published {
            return;
        }
        ordered_events.failure = Some(message.clone());
        ordered_events.pending.clear();
        ordered_events.pending_bytes = 0;
        self.events.publish(ExecProcessEvent::Failed(message));
        drop(ordered_events);
        self.wake_tx
            .send_modify(|current| *current = current.saturating_add(1));
    }

    fn failed_response(&self) -> Option<ReadResponse> {
        self.ordered_events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure
            .clone()
            .map(|message| self.synthesized_failure(message))
    }

    fn synthesized_failure(&self, message: String) -> ReadResponse {
        let next_seq = (*self.wake_tx.borrow()).saturating_add(1);
        ReadResponse {
            chunks: Vec::new(),
            next_seq,
            exited: true,
            exit_code: None,
            closed: true,
            failure: Some(message),
            sandbox_denied: false,
        }
    }

    fn next_write_id(&self) -> String {
        self.next_write_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string()
    }
}

impl OrderedSessionEvents {
    fn insert_pending(&mut self, event: ExecProcessEvent) -> Result<(), String> {
        let Some(seq) = event.seq() else {
            return Err("cannot reorder an unsequenced process event".to_string());
        };
        if self.pending.contains_key(&seq) {
            return Ok(());
        }

        let next_seq = self.last_published_seq.saturating_add(1);
        // The next expected event is synchronously published by every caller,
        // so it can drain a full buffer without becoming retained state.
        let closes_gap = seq == next_seq;
        if !closes_gap && self.pending.len() >= MAX_PENDING_PROCESS_EVENTS {
            return Err(format!(
                "process event reorder buffer exceeds {MAX_PENDING_PROCESS_EVENTS} entries"
            ));
        }

        let event_bytes = pending_process_event_bytes(&event);
        if event_bytes > MAX_PENDING_PROCESS_EVENT_BYTES {
            return Err(format!(
                "process event exceeds {MAX_PENDING_PROCESS_EVENT_BYTES} bytes"
            ));
        }
        let pending_bytes = self.pending_bytes.saturating_add(event_bytes);
        if !closes_gap && pending_bytes > MAX_PENDING_PROCESS_EVENT_BYTES {
            return Err(format!(
                "process event reorder buffer exceeds {MAX_PENDING_PROCESS_EVENT_BYTES} bytes"
            ));
        }

        self.pending.insert(seq, event);
        self.pending_bytes = pending_bytes;
        Ok(())
    }
}

fn pending_process_event_bytes(event: &ExecProcessEvent) -> usize {
    match event {
        ExecProcessEvent::Output(chunk) => chunk.chunk.0.len(),
        ExecProcessEvent::Failed(message) => message.len(),
        ExecProcessEvent::Exited { .. } | ExecProcessEvent::Closed { .. } => 0,
    }
}

fn finish_process_event(
    inner: &Inner,
    process_id: &ProcessId,
    session: &Arc<SessionState>,
    result: Result<bool, String>,
) {
    match result {
        Ok(true) => inner.remove_session_if(process_id, session),
        Ok(false) => {}
        Err(message) => {
            session.set_failure(message);
            inner.remove_session_if(process_id, session);
        }
    }
}

impl Session {
    pub(crate) fn process_id(&self) -> &ProcessId {
        &self.process_id
    }

    pub(crate) fn subscribe_wake(&self) -> watch::Receiver<u64> {
        self.state.subscribe()
    }

    pub(crate) fn subscribe_events(&self) -> ExecProcessEventReceiver {
        self.state.subscribe_events()
    }

    pub(crate) async fn read(
        &self,
        after_seq: Option<u64>,
        max_bytes: Option<usize>,
        wait_ms: Option<u64>,
    ) -> Result<ReadResponse, ExecServerError> {
        loop {
            if let Some(response) = self.state.failed_response() {
                return Ok(response);
            }

            match self
                .client
                .read(ReadParams {
                    process_id: self.process_id.clone(),
                    after_seq,
                    max_bytes,
                    wait_ms,
                })
                .await
            {
                Ok(response) => return Ok(response),
                Err(error)
                    if is_transport_closed_error(&error) && !self.client.inner.is_failed() =>
                {
                    continue;
                }
                Err(error) if is_transport_closed_error(&error) => {
                    if let Some(response) = self.state.failed_response() {
                        return Ok(response);
                    }
                    let message = error.to_string();
                    self.state.set_failure(message.clone());
                    return Ok(self.state.synthesized_failure(message));
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) async fn write(&self, chunk: Vec<u8>) -> Result<WriteResponse, ExecServerError> {
        let write_id = self.state.next_write_id();
        loop {
            match self
                .client
                .write(&self.process_id, chunk.clone(), write_id.clone())
                .await
            {
                Ok(response) => return Ok(response),
                Err(error)
                    if is_transport_closed_error(&error) && !self.client.inner.is_failed() =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) async fn signal(&self, signal: ProcessSignal) -> Result<(), ExecServerError> {
        self.client.signal(&self.process_id, signal).await
    }

    pub(crate) async fn terminate(&self) -> Result<(), ExecServerError> {
        self.client.terminate(&self.process_id).await?;
        Ok(())
    }

    pub(crate) async fn unregister(&self) {
        self.client
            .inner
            .remove_session_if(&self.process_id, &self.state);
    }
}

impl Inner {
    fn get_session(&self, process_id: &ProcessId) -> Option<Arc<SessionState>> {
        self.sessions.load().get(process_id).cloned()
    }

    fn insert_session(
        &self,
        process_id: &ProcessId,
        session: Arc<SessionState>,
    ) -> Result<(), ExecServerError> {
        let _sessions_write_guard = self
            .sessions_write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Do not register a process session that can never receive environment
        // notifications. Without this check, remote MCP startup could create a
        // dead session and wait for process output that will never arrive.
        if let Some(message) = self.failure_message() {
            return Err(ExecServerError::Disconnected(message));
        }
        let sessions = self.sessions.load();
        if sessions.contains_key(process_id) {
            return Err(ExecServerError::Protocol(format!(
                "session already registered for process {process_id}"
            )));
        }
        let mut next_sessions = sessions.as_ref().clone();
        next_sessions.insert(process_id.clone(), session);
        self.sessions.store(Arc::new(next_sessions));
        Ok(())
    }

    fn remove_session_if(&self, process_id: &ProcessId, expected: &Arc<SessionState>) {
        let _sessions_write_guard = self
            .sessions_write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sessions = self.sessions.load();
        if !sessions
            .get(process_id)
            .is_some_and(|session| Arc::ptr_eq(session, expected))
        {
            return;
        }
        let mut next_sessions = sessions.as_ref().clone();
        next_sessions.remove(process_id);
        self.sessions.store(Arc::new(next_sessions));
    }

    fn take_all_sessions(&self) -> HashMap<ProcessId, Arc<SessionState>> {
        let _sessions_write_guard = self
            .sessions_write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sessions = self.sessions.load();
        let drained_sessions = sessions.as_ref().clone();
        self.sessions.store(Arc::new(HashMap::new()));
        drained_sessions
    }
}

fn disconnected_message(reason: Option<&str>) -> String {
    match reason {
        Some(reason) => format!("exec-server transport disconnected: {reason}"),
        None => "exec-server transport disconnected".to_string(),
    }
}

fn is_transport_closed_error(error: &ExecServerError) -> bool {
    matches!(
        error,
        ExecServerError::Closed | ExecServerError::Disconnected(_)
    ) || matches!(
        error,
        ExecServerError::Server {
            code: -32000,
            message,
        } if message == "JSON-RPC transport closed"
    )
}

fn fail_all_sessions(inner: &Arc<Inner>, message: String) {
    let sessions = inner.take_all_sessions();

    for (_, session) in sessions {
        // Sessions synthesize a closed read response and emit a pushed Failed
        // event. That covers both polling consumers and streaming consumers
        // such as environment-backed MCP stdio.
        session.set_failure(message.clone());
    }
}

/// Fails all in-flight work that depends on the shared JSON-RPC transport.
async fn fail_all_in_flight_work(inner: &Arc<Inner>, message: String) {
    fail_all_sessions(inner, message.clone());
    inner.fail_all_http_body_streams(message).await;
}

async fn handle_server_notification(
    inner: &Arc<Inner>,
    notification: JSONRPCNotification,
) -> Result<(), ExecServerError> {
    match notification.method.as_str() {
        EXEC_OUTPUT_DELTA_METHOD => {
            let params: ExecOutputDeltaNotification =
                serde_json::from_value(notification.params.unwrap_or(Value::Null))?;
            if let Some(session) = inner.get_session(&params.process_id) {
                let result =
                    session.publish_ordered_event(ExecProcessEvent::Output(ProcessOutputChunk {
                        seq: params.seq,
                        stream: params.stream,
                        chunk: params.chunk,
                    }));
                if result.is_ok() {
                    session.note_change(params.seq);
                }
                finish_process_event(inner, &params.process_id, &session, result);
            }
        }
        EXEC_EXITED_METHOD => {
            let params: ExecExitedNotification =
                serde_json::from_value(notification.params.unwrap_or(Value::Null))?;
            if let Some(session) = inner.get_session(&params.process_id) {
                let result = session.publish_ordered_event(ExecProcessEvent::Exited {
                    seq: params.seq,
                    exit_code: params.exit_code,
                    sandbox_denied: params.sandbox_denied,
                });
                if result.is_ok() {
                    session.note_change(params.seq);
                }
                finish_process_event(inner, &params.process_id, &session, result);
            }
        }
        EXEC_CLOSED_METHOD => {
            let params: ExecClosedNotification =
                serde_json::from_value(notification.params.unwrap_or(Value::Null))?;
            if let Some(session) = inner.get_session(&params.process_id) {
                // Closed is terminal, but it can arrive before tail output or
                // exited. Keep routing this process until the ordered publisher
                // says Closed has actually been delivered.
                let result =
                    session.publish_ordered_event(ExecProcessEvent::Closed { seq: params.seq });
                if result.is_ok() {
                    session.note_change(params.seq);
                }
                finish_process_event(inner, &params.process_id, &session, result);
            }
        }
        HTTP_REQUEST_BODY_DELTA_METHOD => {
            inner
                .handle_http_body_delta_notification(notification.params)
                .await?;
        }
        other => {
            debug!("ignoring unknown exec-server notification: {other}");
        }
    }
    Ok(())
}
