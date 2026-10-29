use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::RwLock;

use futures::FutureExt;
use xedoc_protocol::capabilities::CapabilityRootLocation;
use xedoc_protocol::capabilities::SelectedCapabilityRoot;

use crate::CapabilityRootsDiscoverParams;
use crate::CapabilityRootsDiscoverResponse;
use crate::ExecServerError;
use crate::ExecServerRuntimePaths;
use crate::ExecutorFileSystem;
use crate::HttpClient;
use crate::NoiseChannelIdentity;
use crate::NoiseRendezvousConnectProvider;
use crate::client::LazyRemoteExecServerClient;
use crate::client::http_client::ReqwestHttpClient;
use crate::client_api::DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT;
use crate::client_api::ExecServerTransportParams;
use crate::environment_provider::DefaultEnvironmentProvider;
use crate::environment_provider::EnvironmentDefault;
use crate::environment_provider::EnvironmentProvider;
use crate::environment_provider::EnvironmentProviderSnapshot;
use crate::environment_provider::normalize_exec_server_url;
use crate::environment_toml::environment_provider_from_xedoc_home;
use crate::local_file_system::LocalFileSystem;
use crate::local_process::LocalProcess;
use crate::process::ExecBackend;
use crate::protocol::EnvironmentInfo;
use crate::remote::NoiseRendezvousEnvironmentConfig;
use crate::remote_file_system::RemoteFileSystem;
use crate::remote_process::RemoteProcess;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio_util::task::AbortOnDropHandle;

pub const XEDOC_EXEC_SERVER_URL_ENV_VAR: &str = "XEDOC_EXEC_SERVER_URL";
pub const XEDOC_EXEC_SERVER_NOISE_REGISTRY_URL_ENV_VAR: &str =
    "XEDOC_EXEC_SERVER_NOISE_REGISTRY_URL";
pub const XEDOC_EXEC_SERVER_NOISE_ENVIRONMENT_ID_ENV_VAR: &str =
    "XEDOC_EXEC_SERVER_NOISE_ENVIRONMENT_ID";
pub const XEDOC_EXEC_SERVER_NOISE_AUTH_TOKEN_ENV_VAR: &str = "XEDOC_EXEC_SERVER_NOISE_AUTH_TOKEN";
pub const XEDOC_EXEC_SERVER_NOISE_CHATGPT_ACCOUNT_ID_ENV_VAR: &str =
    "XEDOC_EXEC_SERVER_NOISE_CHATGPT_ACCOUNT_ID";

/// The current connection state for one concrete environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentConnectionState {
    /// An initialized exec-server connection is available.
    Connected,
    /// No initialized exec-server connection is currently available.
    Disconnected,
}

/// Owns the execution/filesystem environments available to the Xedoc runtime.
///
/// `EnvironmentManager` is a shared registry for concrete environments. Its
/// default constructor preserves the legacy `XEDOC_EXEC_SERVER_URL` behavior
/// while configured construction accepts a provider-supplied snapshot.
///
/// Setting `XEDOC_EXEC_SERVER_URL=none` disables environment access by leaving
/// the default environment unset and omitting the local environment. Callers
/// use `default_environment().is_some()` as the signal for model-facing
/// shell/filesystem tool availability.
///
/// Remote environments begin connecting when added to the manager. Their
/// filesystem and execution backends share that startup result and reconnect
/// after later disconnects as needed.
#[derive(Debug)]
pub struct EnvironmentManager {
    default_environment: Option<String>,
    pub(super) environments: RwLock<HashMap<String, Arc<Environment>>>,
    local_environment: Option<Arc<Environment>>,
    local_runtime_paths: Option<ExecServerRuntimePaths>,
}

/// Information supplied by the environment owner when a deferred environment is ready.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvironmentReadyInfo {
    /// Ordered capability roots selected for this environment.
    pub selected_capability_roots: Vec<SelectedCapabilityRoot>,
}

/// The one-shot capability to complete a deferred environment registration.
#[must_use = "the deferred environment cannot connect until registration is completed"]
pub struct DeferredEnvironmentRegistration {
    completion: oneshot::Sender<Result<(), String>>,
    environment_id: String,
    ready_info: Arc<OnceLock<EnvironmentReadyInfo>>,
}

/// Maximum capability roots accepted from deferred environment ready information.
pub const MAX_SELECTED_CAPABILITY_ROOTS: usize = 256;

pub const LOCAL_ENVIRONMENT_ID: &str = "local";
pub const REMOTE_ENVIRONMENT_ID: &str = "remote";

/// Non-mutating connection status observed by an environment owner.
///
/// Computing this status never starts, waits for, or reconnects an exec-server
/// transport. Already-ready remote environments may receive a fail-fast probe
/// over their existing connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentObservedStatus {
    /// A local environment, or a remote environment whose existing connection answered a probe.
    Ready,
    /// The configured environment has no ready connection and no observed connection failure.
    ///
    /// This includes lazy transports that have never been started and initial startup that has
    /// not finished. Computing status does not start the environment or wait for startup.
    Pending,
    /// A connection attempt, prior connection, or fail-fast status probe observed a failure.
    ///
    /// This does not promise that the failure is terminal: later normal environment use may
    /// recover the connection. Computing status itself does not trigger recovery.
    Disconnected {
        /// Human-readable reason recorded by the failed connection attempt or probe.
        error: String,
    },
}

impl EnvironmentManager {
    /// Builds a test-only manager without configured sandbox helper paths.
    pub fn default_for_tests() -> Self {
        Self {
            default_environment: Some(LOCAL_ENVIRONMENT_ID.to_string()),
            environments: RwLock::new(HashMap::from([(
                LOCAL_ENVIRONMENT_ID.to_string(),
                Arc::new(Environment::default_for_tests()),
            )])),
            local_environment: Some(Arc::new(Environment::default_for_tests())),
            local_runtime_paths: None,
        }
    }

    /// Builds a manager with no configured execution environments.
    pub fn without_environments() -> Self {
        Self {
            default_environment: None,
            environments: RwLock::new(HashMap::new()),
            local_environment: None,
            local_runtime_paths: None,
        }
    }

    /// Builds a test-only manager from a raw exec-server URL value.
    pub async fn create_for_tests(
        exec_server_url: Option<String>,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Self {
        Self::from_default_provider_url(exec_server_url, local_runtime_paths).await
    }

    /// Builds a manager from `XEDOC_HOME` and local runtime paths used when
    /// creating local filesystem helpers.
    ///
    /// If `XEDOC_HOME/environments.toml` is present, it defines the configured
    /// environments. Otherwise this preserves the legacy
    /// `XEDOC_EXEC_SERVER_URL` behavior.
    pub async fn from_xedoc_home(
        xedoc_home: impl AsRef<std::path::Path>,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        if let Some(config) = noise_environment_config_from_env()? {
            return Self::from_noise_environment_config(config, local_runtime_paths);
        }
        let provider = environment_provider_from_xedoc_home(xedoc_home.as_ref())?;
        Self::from_snapshot(provider.snapshot().await?, local_runtime_paths)
    }

    /// Builds a manager from the legacy environment-variable provider without
    /// reading user config files from `XEDOC_HOME`.
    pub async fn from_env(
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        if let Some(config) = noise_environment_config_from_env()? {
            return Self::from_noise_environment_config(config, local_runtime_paths);
        }
        let provider = DefaultEnvironmentProvider::from_env();
        Self::from_snapshot(provider.snapshot().await?, local_runtime_paths)
    }

    async fn from_default_provider_url(
        exec_server_url: Option<String>,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Self {
        let provider = DefaultEnvironmentProvider::new(exec_server_url);
        match Self::from_snapshot(provider.snapshot_inner(), local_runtime_paths) {
            Ok(manager) => manager,
            Err(err) => panic!("default provider should create valid environments: {err}"),
        }
    }

    fn from_noise_environment_config(
        config: NoiseRendezvousEnvironmentConfig,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        let manager = Self {
            default_environment: Some(REMOTE_ENVIRONMENT_ID.to_string()),
            environments: RwLock::new(HashMap::new()),
            local_environment: None,
            local_runtime_paths,
        };
        manager.upsert_noise_environment(
            REMOTE_ENVIRONMENT_ID.to_string(),
            config.connect_provider(),
        )?;
        Ok(manager)
    }

    /// Builds a test-only manager that keeps the provider default while also
    /// allowing tests to select the local environment explicitly.
    pub async fn create_for_tests_with_local(
        exec_server_url: Option<String>,
        local_runtime_paths: ExecServerRuntimePaths,
    ) -> Self {
        let mut snapshot = DefaultEnvironmentProvider::new(exec_server_url).snapshot_inner();
        snapshot.include_local = true;
        match Self::from_snapshot(snapshot, Some(local_runtime_paths)) {
            Ok(manager) => manager,
            Err(err) => panic!("test provider with local should create valid environments: {err}"),
        }
    }

    fn from_snapshot(
        snapshot: EnvironmentProviderSnapshot,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        let EnvironmentProviderSnapshot {
            environments,
            default,
            include_local,
        } = snapshot;
        let mut environment_map =
            HashMap::with_capacity(environments.len() + usize::from(include_local));
        let local_environment = if include_local {
            let local_runtime_paths = local_runtime_paths.clone().ok_or_else(|| {
                ExecServerError::Protocol(
                    "local environment requires configured runtime paths".to_string(),
                )
            })?;
            let local_environment = Arc::new(Environment::local(local_runtime_paths));
            environment_map.insert(
                LOCAL_ENVIRONMENT_ID.to_string(),
                Arc::clone(&local_environment),
            );
            Some(local_environment)
        } else {
            None
        };
        for (id, environment) in environments {
            if id.is_empty() {
                return Err(ExecServerError::Protocol(
                    "environment id cannot be empty".to_string(),
                ));
            }
            if id == LOCAL_ENVIRONMENT_ID {
                return Err(ExecServerError::Protocol(format!(
                    "environment id `{LOCAL_ENVIRONMENT_ID}` is reserved for EnvironmentManager"
                )));
            }
            if environment_map
                .insert(id.clone(), Arc::new(environment))
                .is_some()
            {
                return Err(ExecServerError::Protocol(format!(
                    "environment id `{id}` is duplicated"
                )));
            }
        }
        let default_environment = match default {
            EnvironmentDefault::Disabled => None,
            EnvironmentDefault::EnvironmentId(environment_id) => {
                if !environment_map.contains_key(&environment_id) {
                    return Err(ExecServerError::Protocol(format!(
                        "default environment `{environment_id}` is not configured"
                    )));
                }
                Some(environment_id)
            }
        };
        // The snapshot is valid; start connecting its remote environments in the background.
        for environment in environment_map.values() {
            environment.start_connecting();
        }
        Ok(Self {
            default_environment,
            environments: RwLock::new(environment_map),
            local_environment,
            local_runtime_paths,
        })
    }

    /// Returns the default environment instance.
    pub fn default_environment(&self) -> Option<Arc<Environment>> {
        self.default_environment
            .as_deref()
            .and_then(|environment_id| self.get_environment(environment_id))
    }

    /// Returns the id of the default environment.
    pub fn default_environment_id(&self) -> Option<&str> {
        self.default_environment.as_deref()
    }

    /// Returns the ordered environment ids used for new thread startup.
    pub fn default_environment_ids(&self) -> Vec<String> {
        let Some(default_environment_id) = self.default_environment.as_ref() else {
            return Vec::new();
        };
        let environments = self
            .environments
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut environment_ids = Vec::with_capacity(environments.len());
        environment_ids.push(default_environment_id.clone());
        environment_ids.extend(
            environments
                .keys()
                .filter(|environment_id| *environment_id != default_environment_id)
                .cloned(),
        );
        environment_ids
    }

    /// Returns the local environment instance when one is configured.
    pub fn try_local_environment(&self) -> Option<Arc<Environment>> {
        self.local_environment.as_ref().map(Arc::clone)
    }

    /// Returns a named environment instance.
    pub fn get_environment(&self, environment_id: &str) -> Option<Arc<Environment>> {
        self.environments
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(environment_id)
            .cloned()
    }

    /// Returns the current status of one named environment when it is configured.
    pub async fn get_environment_status(
        &self,
        environment_id: &str,
    ) -> Option<EnvironmentObservedStatus> {
        let environment = self.get_environment(environment_id)?;
        Some(environment.status().await)
    }

    /// Adds or replaces a named remote environment without changing the
    /// manager's default environment selection. Uses the default WebSocket
    /// connection timeout when none is provided.
    pub fn upsert_environment(
        &self,
        environment_id: String,
        exec_server_url: String,
        connect_timeout: Option<std::time::Duration>,
    ) -> Result<(), ExecServerError> {
        validate_environment_id(&environment_id)?;
        let exec_server_url = validate_remote_exec_server_url(exec_server_url)?;
        let environment = Arc::new(Environment::remote_with_transport(
            ExecServerTransportParams::websocket_url(
                exec_server_url,
                connect_timeout.unwrap_or(DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT),
            ),
            self.local_runtime_paths.clone(),
        ));
        self.insert_environment(environment_id, environment);
        Ok(())
    }

    /// Adds or replaces a Noise rendezvous environment that will become ready later.
    pub fn register_deferred_noise_environment(
        &self,
        environment_id: String,
        provider: Arc<dyn NoiseRendezvousConnectProvider>,
    ) -> Result<DeferredEnvironmentRegistration, ExecServerError> {
        validate_environment_id(&environment_id)?;
        let identity = noise_channel_identity()?;
        let (completion, readiness) = oneshot::channel();
        let ready_info = Arc::new(OnceLock::new());
        let mut environment = Environment::remote_with_transport(
            ExecServerTransportParams::Deferred(Box::new(crate::client_api::Deferred {
                readiness: readiness.shared(),
                transport: ExecServerTransportParams::NoiseRendezvous { provider, identity },
            })),
            self.local_runtime_paths.clone(),
        );
        environment.ready_info = Some(Arc::clone(&ready_info));
        let environment = Arc::new(environment);
        self.insert_environment(environment_id.clone(), environment);
        Ok(DeferredEnvironmentRegistration {
            completion,
            environment_id,
            ready_info,
        })
    }

    /// Adds or replaces a named remote environment that connects through an
    /// authenticated, end-to-end encrypted rendezvous stream.
    ///
    /// The provider is retained so every reconnect obtains fresh authorization.
    /// This transport never falls back to the URL-only remote environment path.
    pub fn upsert_noise_environment(
        &self,
        environment_id: String,
        provider: Arc<dyn NoiseRendezvousConnectProvider>,
    ) -> Result<(), ExecServerError> {
        validate_environment_id(&environment_id)?;
        let identity = noise_channel_identity()?;
        let environment = Arc::new(Environment::remote_with_transport(
            ExecServerTransportParams::NoiseRendezvous { provider, identity },
            self.local_runtime_paths.clone(),
        ));
        self.insert_environment(environment_id, environment);
        Ok(())
    }

    fn insert_environment(&self, environment_id: String, environment: Arc<Environment>) {
        self.environments
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(environment_id, Arc::clone(&environment));
        environment.start_connecting();
    }
}

impl DeferredEnvironmentRegistration {
    /// Completes provisioning with ready information or a terminal error message.
    pub fn complete(
        self,
        result: Result<EnvironmentReadyInfo, String>,
    ) -> Result<(), ExecServerError> {
        let result = match result {
            Ok(ready_info) => {
                if ready_info.selected_capability_roots.len() > MAX_SELECTED_CAPABILITY_ROOTS {
                    let error = ExecServerError::Protocol(format!(
                        "environment ready info contains more than {MAX_SELECTED_CAPABILITY_ROOTS} selected capability roots"
                    ));
                    let _ = self.completion.send(Err(error.to_string()));
                    return Err(error);
                }
                let mut root_ids =
                    HashSet::with_capacity(ready_info.selected_capability_roots.len());
                for root in &ready_info.selected_capability_roots {
                    let CapabilityRootLocation::Environment { environment_id, .. } = &root.location;
                    if root.id.trim().is_empty()
                        || environment_id != &self.environment_id
                        || !root_ids.insert(root.id.as_str())
                    {
                        let error = ExecServerError::Protocol(format!(
                            "selected capability roots must have unique non-empty IDs and belong to environment `{}`",
                            self.environment_id
                        ));
                        let _ = self.completion.send(Err(error.to_string()));
                        return Err(error);
                    }
                }
                if self.ready_info.set(ready_info).is_err() {
                    let error = ExecServerError::Protocol(
                        "deferred environment ready info was already set".to_string(),
                    );
                    let _ = self.completion.send(Err(error.to_string()));
                    return Err(error);
                }
                Ok(())
            }
            Err(message) => Err(message),
        };
        self.completion.send(result).map_err(|_| {
            ExecServerError::Disconnected("deferred environment registration is inactive".into())
        })
    }
}

fn noise_channel_identity() -> Result<NoiseChannelIdentity, ExecServerError> {
    NoiseChannelIdentity::generate().map_err(|error| {
        ExecServerError::Protocol(format!(
            "failed to generate Noise harness identity: {error}"
        ))
    })
}

fn validate_environment_id(environment_id: &str) -> Result<(), ExecServerError> {
    if environment_id.is_empty() {
        return Err(ExecServerError::Protocol(
            "environment id cannot be empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_remote_exec_server_url(exec_server_url: String) -> Result<String, ExecServerError> {
    let (exec_server_url, disabled) = normalize_exec_server_url(Some(exec_server_url));
    if disabled {
        return Err(ExecServerError::Protocol(
            "remote environment cannot use disabled exec-server url".to_string(),
        ));
    }
    exec_server_url.ok_or_else(|| {
        ExecServerError::Protocol("remote environment requires an exec-server url".to_string())
    })
}

fn noise_environment_config_from_env()
-> Result<Option<NoiseRendezvousEnvironmentConfig>, ExecServerError> {
    noise_environment_config_from_values(
        optional_environment_value(XEDOC_EXEC_SERVER_NOISE_REGISTRY_URL_ENV_VAR),
        optional_environment_value(XEDOC_EXEC_SERVER_NOISE_ENVIRONMENT_ID_ENV_VAR),
        optional_environment_value(XEDOC_EXEC_SERVER_NOISE_AUTH_TOKEN_ENV_VAR),
        optional_environment_value(XEDOC_EXEC_SERVER_NOISE_CHATGPT_ACCOUNT_ID_ENV_VAR),
    )
}

fn noise_environment_config_from_values(
    registry_url: Option<String>,
    environment_id: Option<String>,
    auth_token: Option<String>,
    chatgpt_account_id: Option<String>,
) -> Result<Option<NoiseRendezvousEnvironmentConfig>, ExecServerError> {
    let (registry_url, environment_id, auth_token) =
        match (registry_url, environment_id, auth_token) {
            (None, None, None) => return Ok(None),
            (Some(registry_url), Some(environment_id), Some(auth_token)) => {
                (registry_url, environment_id, auth_token)
            }
            _ => {
                return Err(ExecServerError::EnvironmentRegistryConfig(format!(
                    "Noise environment requires {XEDOC_EXEC_SERVER_NOISE_REGISTRY_URL_ENV_VAR}, \
{XEDOC_EXEC_SERVER_NOISE_ENVIRONMENT_ID_ENV_VAR}, and \
{XEDOC_EXEC_SERVER_NOISE_AUTH_TOKEN_ENV_VAR}"
                )));
            }
        };

    let config = NoiseRendezvousEnvironmentConfig::new(
        registry_url,
        environment_id,
        auth_token,
        chatgpt_account_id,
    )?;
    Ok(Some(config))
}

fn optional_environment_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Concrete execution/filesystem environment selected for a session.
///
/// This bundles the selected backend metadata together with the local runtime
/// paths used by filesystem helpers.
#[derive(Clone)]
pub struct Environment {
    remote_client: Option<LazyRemoteExecServerClient>,
    ready_info: Option<Arc<OnceLock<EnvironmentReadyInfo>>>,
    // Dropping the environment stops unfinished background startup work.
    startup_task: Arc<Mutex<Option<AbortOnDropHandle<()>>>>,
    exec_backend: Arc<dyn ExecBackend>,
    filesystem: Arc<dyn ExecutorFileSystem>,
    http_client: Arc<dyn HttpClient>,
    local_runtime_paths: Option<ExecServerRuntimePaths>,
}

impl Environment {
    /// Builds a test-only local environment without configured sandbox helper paths.
    pub fn default_for_tests() -> Self {
        Self {
            remote_client: None,
            ready_info: None,
            startup_task: Arc::new(Mutex::new(None)),
            exec_backend: Arc::new(LocalProcess::default()),
            filesystem: Arc::new(LocalFileSystem::unsandboxed()),
            http_client: Arc::new(ReqwestHttpClient),
            local_runtime_paths: None,
        }
    }
}

impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Environment").finish_non_exhaustive()
    }
}

impl Environment {
    /// Builds an environment from the raw `XEDOC_EXEC_SERVER_URL` value.
    pub fn create(
        exec_server_url: Option<String>,
        local_runtime_paths: ExecServerRuntimePaths,
    ) -> Result<Self, ExecServerError> {
        Self::create_inner(exec_server_url, Some(local_runtime_paths))
    }

    /// Builds a test-only environment without configured sandbox helper paths.
    pub fn create_for_tests(exec_server_url: Option<String>) -> Result<Self, ExecServerError> {
        Self::create_inner(exec_server_url, /*local_runtime_paths*/ None)
    }

    /// Builds an environment from the raw `XEDOC_EXEC_SERVER_URL` value and
    /// local runtime paths used when creating local filesystem helpers.
    fn create_inner(
        exec_server_url: Option<String>,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        let (exec_server_url, disabled) = normalize_exec_server_url(exec_server_url);
        if disabled {
            return Err(ExecServerError::Protocol(
                "disabled mode does not create an Environment".to_string(),
            ));
        }

        Ok(match exec_server_url {
            Some(exec_server_url) => Self::remote_inner(exec_server_url, local_runtime_paths),
            None => match local_runtime_paths {
                Some(local_runtime_paths) => Self::local(local_runtime_paths),
                None => Self::default_for_tests(),
            },
        })
    }

    pub(crate) fn local(local_runtime_paths: ExecServerRuntimePaths) -> Self {
        Self {
            remote_client: None,
            ready_info: None,
            startup_task: Arc::new(Mutex::new(None)),
            exec_backend: Arc::new(LocalProcess::with_local_runtime_paths(
                local_runtime_paths.clone(),
            )),
            filesystem: Arc::new(LocalFileSystem::with_runtime_paths(
                local_runtime_paths.clone(),
            )),
            http_client: Arc::new(ReqwestHttpClient),
            local_runtime_paths: Some(local_runtime_paths),
        }
    }

    pub(crate) fn remote_inner(
        exec_server_url: String,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Self {
        Self::remote_with_transport(
            ExecServerTransportParams::websocket_url(
                exec_server_url,
                DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT,
            ),
            local_runtime_paths,
        )
    }

    pub(crate) fn remote_with_transport(
        remote_transport: ExecServerTransportParams,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Self {
        let client = LazyRemoteExecServerClient::new(remote_transport);
        let exec_backend: Arc<dyn ExecBackend> = Arc::new(RemoteProcess::new(client.clone()));
        let filesystem: Arc<dyn ExecutorFileSystem> =
            Arc::new(RemoteFileSystem::new(client.clone()));

        Self {
            remote_client: Some(client.clone()),
            ready_info: None,
            startup_task: Arc::new(Mutex::new(None)),
            exec_backend,
            filesystem,
            http_client: Arc::new(client),
            local_runtime_paths,
        }
    }

    pub fn is_remote(&self) -> bool {
        self.remote_client.is_some()
    }

    /// Returns capability roots supplied with the deferred environment's ready signal.
    pub fn selected_capability_roots(&self) -> &[SelectedCapabilityRoot] {
        self.ready_info
            .as_ref()
            .and_then(|ready_info| ready_info.get())
            .map_or(&[], |ready_info| {
                ready_info.selected_capability_roots.as_slice()
            })
    }

    /// Subscribes to the current connection state for this remote environment.
    pub fn subscribe_connection_state(
        &self,
    ) -> Option<watch::Receiver<EnvironmentConnectionState>> {
        self.remote_client
            .as_ref()
            .map(LazyRemoteExecServerClient::subscribe_connection_state)
    }

    pub fn local_runtime_paths(&self) -> Option<&ExecServerRuntimePaths> {
        self.local_runtime_paths.as_ref()
    }

    /// Returns environment information from the selected execution/filesystem environment.
    pub async fn info(&self) -> Result<EnvironmentInfo, ExecServerError> {
        match &self.remote_client {
            Some(client) => client.environment_info().await,
            None => Ok(EnvironmentInfo::local()),
        }
    }

    /// Discovers plugin and skill manifests through the environment's high-level discovery API.
    pub async fn discover_capability_roots(
        &self,
        params: CapabilityRootsDiscoverParams,
    ) -> Result<CapabilityRootsDiscoverResponse, ExecServerError> {
        match &self.remote_client {
            Some(client) => client.get().await?.discover_capability_roots(params).await,
            None => crate::discover_capability_roots(self.filesystem.as_ref(), params)
                .await
                .map_err(|error| ExecServerError::Protocol(error.to_string())),
        }
    }

    /// Starts connecting a remote environment without waiting for it.
    /// Requires an active Tokio runtime when background startup is supported.
    pub fn start_connecting(&self) {
        let Some(client) = &self.remote_client else {
            return;
        };
        let mut startup_task = self
            .startup_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if startup_task.is_none() {
            *startup_task = client.start_connecting();
        }
    }

    /// Starts the initial connection after an environment is actually selected for use.
    pub(crate) fn start_connecting_for_use(environment: &Arc<Self>) {
        if environment.remote_client.is_none() {
            return;
        }
        let mut startup_task = environment
            .startup_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if startup_task.is_none() {
            let environment = Arc::clone(environment);
            *startup_task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
                if let Err(error) = environment.wait_until_ready().await {
                    tracing::debug!(%error, "exec-server environment startup failed");
                }
            })));
        }
    }

    /// Returns whether initial startup has either succeeded or permanently failed.
    pub fn startup_finished(&self) -> bool {
        self.remote_client
            .as_ref()
            .is_none_or(LazyRemoteExecServerClient::startup_finished)
    }

    /// Waits for initial startup. A failed startup is never attempted again.
    pub async fn wait_until_ready(&self) -> Result<(), ExecServerError> {
        match &self.remote_client {
            Some(client) => client.wait_until_ready().await,
            None => Ok(()),
        }
    }

    /// Returns whether the environment can serve a request without waiting or reconnecting.
    pub(crate) fn readiness_result(&self) -> Option<Result<(), ExecServerError>> {
        match &self.remote_client {
            Some(client) => client.readiness_result(),
            None => Some(Ok(())),
        }
    }

    /// Returns the environment's status without starting or recovering it.
    ///
    /// Local environments are always ready. Remote environments with an
    /// already-ready cached connection receive a fail-fast `environment/status`
    /// probe; other remote states are returned from cached connection state
    /// without waiting for startup or recovery.
    pub async fn status(&self) -> EnvironmentObservedStatus {
        match &self.remote_client {
            Some(client) => client.status().await,
            None => EnvironmentObservedStatus::Ready,
        }
    }

    pub fn get_exec_backend(&self) -> Arc<dyn ExecBackend> {
        Arc::clone(&self.exec_backend)
    }

    pub fn get_http_client(&self) -> Arc<dyn HttpClient> {
        Arc::clone(&self.http_client)
    }

    pub fn get_filesystem(&self) -> Arc<dyn ExecutorFileSystem> {
        Arc::clone(&self.filesystem)
    }

    /// Returns a filesystem view that fails instead of starting or waiting for a connection.
    pub fn get_filesystem_without_reconnect(&self) -> Arc<dyn ExecutorFileSystem> {
        match &self.remote_client {
            Some(client) => Arc::new(RemoteFileSystem::new(client.fail_fast())),
            None => Arc::clone(&self.filesystem),
        }
    }
}
