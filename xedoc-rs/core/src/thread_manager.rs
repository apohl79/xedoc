use crate::SkillsService;
use crate::agent::AgentControl;
use crate::config::Config;
use crate::config::ThreadStoreConfig;
use crate::config::model_catalogs_for_config;
use crate::current_time::TimeProvider;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::environment_selection::default_thread_environment_selections;
use crate::mcp::McpManager;
use crate::rollout::truncation;
use crate::session::INITIAL_SUBMIT_ID;
use crate::session::SessionIo;
use crate::session::SessionSpawnArgs;
use crate::session::resolve_multi_agent_version;
use crate::session::session::Session;
use crate::tasks::InterruptedTurnHistoryMarker;
use crate::tasks::interrupted_turn_history_marker;
use crate::xedoc_thread::XedocThread;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::sync::broadcast;
use tracing::info;
use tracing::instrument;
use tracing::warn;
use xedoc_agent_graph_store::AgentGraphStore;
use xedoc_agent_graph_store::LocalAgentGraphStore;
use xedoc_app_server_protocol::ThreadHistoryBuilder;
use xedoc_app_server_protocol::TurnStatus;
use xedoc_core_plugins::PluginsManager;
use xedoc_exec_server::EnvironmentManager;
use xedoc_extension_api::ExtensionDataInit;
use xedoc_extension_api::ExtensionRegistry;
use xedoc_extension_api::LoadedUserInstructions;
use xedoc_extension_api::UserInstructionsProvider;
use xedoc_extension_api::empty_extension_registry;
use xedoc_features::Feature;
use xedoc_login::AuthManager;
use xedoc_login::XedocAuth;
use xedoc_login::default_client::XEDOC_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR;
use xedoc_login::default_client::originator;
use xedoc_model_provider::configured_provider_has_credentials;
use xedoc_model_provider::create_model_provider;
use xedoc_model_provider::create_model_provider_for_configured_id;
use xedoc_model_provider_info::LMSTUDIO_OSS_PROVIDER_ID;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_model_provider_info::OLLAMA_OSS_PROVIDER_ID;
use xedoc_model_provider_info::OPENAI_PROVIDER_ID;
use xedoc_models_manager::availability::AvailabilityGatedModelsManager;
use xedoc_models_manager::manager::RefreshStrategy;
use xedoc_models_manager::manager::SharedModelsManager;
use xedoc_models_manager::registry_manager::RegistryModelsManager;
use xedoc_protocol::ThreadId;
use xedoc_protocol::config_types::CollaborationModeMask;
use xedoc_protocol::error::Result as XedocResult;
use xedoc_protocol::error::XedocErr;
use xedoc_protocol::openai_models::ModelPreset;
use xedoc_protocol::protocol::Event;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::InitialHistory;
use xedoc_protocol::protocol::MultiAgentVersion;
use xedoc_protocol::protocol::Op;
use xedoc_protocol::protocol::ResumedHistory;
use xedoc_protocol::protocol::RolloutItem;
use xedoc_protocol::protocol::SessionConfiguredEvent;
use xedoc_protocol::protocol::SessionSource;
use xedoc_protocol::protocol::SubAgentSource;
use xedoc_protocol::protocol::ThreadHistoryMode;
use xedoc_protocol::protocol::ThreadSource;
use xedoc_protocol::protocol::TurnAbortReason;
use xedoc_protocol::protocol::TurnAbortedEvent;
use xedoc_protocol::protocol::TurnEnvironmentSelection;
use xedoc_protocol::protocol::W3cTraceContext;
use xedoc_rollout::state_db::StateDbHandle;
use xedoc_thread_store::InMemoryThreadStore;
use xedoc_thread_store::LoadThreadHistoryParams;
use xedoc_thread_store::LocalThreadStore;
use xedoc_thread_store::LocalThreadStoreConfig;
use xedoc_thread_store::ReadThreadByRolloutPathParams;
use xedoc_thread_store::ReadThreadParams;
use xedoc_thread_store::StoredModelContext;
use xedoc_thread_store::StoredThread;
use xedoc_thread_store::ThreadMetadataPatch;
use xedoc_thread_store::ThreadStore;
use xedoc_thread_store::ThreadStoreError;
use xedoc_thread_store::UpdateThreadMetadataParams;
use xedoc_utils_absolute_path::AbsolutePathBuf;

const THREAD_CREATED_CHANNEL_CAPACITY: usize = 1024;

/// Test-only override for enabling thread-manager behaviors used by integration
/// tests.
///
/// In production builds this value should remain at its default (`false`) and
/// must not be toggled.
static FORCE_TEST_THREAD_MANAGER_BEHAVIOR: AtomicBool = AtomicBool::new(false);

type CapturedOps = Vec<(ThreadId, Op)>;
type SharedCapturedOps = Arc<std::sync::Mutex<CapturedOps>>;

pub(crate) fn set_thread_manager_test_mode_for_tests(enabled: bool) {
    FORCE_TEST_THREAD_MANAGER_BEHAVIOR.store(enabled, Ordering::Relaxed);
}

fn should_use_test_thread_manager_behavior() -> bool {
    FORCE_TEST_THREAD_MANAGER_BEHAVIOR.load(Ordering::Relaxed)
}

struct TempXedocHomeGuard {
    path: PathBuf,
}

impl Drop for TempXedocHomeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Represents a newly created Xedoc thread (formerly called a conversation), including the first event
/// (which is [`EventMsg::SessionConfigured`]).
pub struct NewThread {
    pub thread_id: ThreadId,
    pub thread: Arc<XedocThread>,
    pub session_configured: SessionConfiguredEvent,
}

// TODO(ccunningham): Add an explicit non-interrupting live-turn snapshot once
// core can represent sampling boundaries directly instead of relying on
// whichever items happened to be persisted mid-turn.
//
// Two likely future variants:
// - `TruncateToLastSamplingBoundary` for callers that want a coherent fork from
//   the last stable model boundary without synthesizing an interrupt.
// - `WaitUntilNextSamplingBoundary` (or similar) for callers that prefer to
//   fork after the next sampling boundary rather than interrupting immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkSnapshot {
    /// Fork a committed prefix ending strictly before the nth user message.
    ///
    /// When `n` is within range, this cuts before that 0-based user-message
    /// boundary. When `n` is out of range and the source thread is currently
    /// mid-turn, this instead cuts before the active turn's opening boundary
    /// so the fork drops the unfinished turn suffix. When `n` is out of range
    /// and the source thread is already at a turn boundary, this returns the
    /// full committed history unchanged.
    TruncateBeforeNthUserMessage(usize),

    /// Fork the current persisted history as if the source thread had been
    /// interrupted now.
    ///
    /// If the persisted snapshot ends mid-turn, this appends the same
    /// `<turn_aborted>` marker produced by a real interrupt. If the snapshot is
    /// already at a turn boundary, this returns the current persisted history
    /// unchanged.
    Interrupted,
}

/// Preserve legacy `fork_thread(usize, ...)` callsites by mapping them to the
/// existing truncate-before-nth-user-message snapshot mode.
impl From<usize> for ForkSnapshot {
    fn from(value: usize) -> Self {
        Self::TruncateBeforeNthUserMessage(value)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ThreadShutdownReport {
    pub completed: Vec<ThreadId>,
    pub submit_failed: Vec<ThreadId>,
    pub timed_out: Vec<ThreadId>,
}

enum ShutdownOutcome {
    Complete,
    SubmitFailed,
    TimedOut,
}

/// [`ThreadManager`] is responsible for creating threads and maintaining
/// them in memory.
pub struct ThreadManager {
    state: Arc<ThreadManagerState>,
    _test_xedoc_home_guard: Option<TempXedocHomeGuard>,
}

pub struct StartThreadOptions {
    pub config: Config,
    pub allow_provider_model_fallback: bool,
    pub initial_history: InitialHistory,
    pub history_mode: Option<ThreadHistoryMode>,
    pub session_source: Option<SessionSource>,
    pub thread_source: Option<ThreadSource>,
    pub dynamic_tools: Vec<xedoc_protocol::dynamic_tools::DynamicToolSpec>,
    pub metrics_service_name: Option<String>,
    pub parent_trace: Option<W3cTraceContext>,
    pub environments: Vec<TurnEnvironmentSelection>,
    pub thread_extension_init: ExtensionDataInit,
    pub supports_openai_form_elicitation: bool,
}

fn originator_from_service_name(service_name: Option<&str>) -> Option<String> {
    let service_name = service_name?.trim();
    for originator in [
        "xedoc_work_desktop",
        "xedoc_work_web",
        "xedoc_work_mobile",
        "xedoc_work_cca",
        "chatgpt_cca",
    ] {
        if service_name.eq_ignore_ascii_case(originator) {
            return Some(originator.to_string());
        }
    }
    None
}

fn effective_originator_value(
    metrics_service_name: Option<&str>,
    env_originator: Option<String>,
    persisted_originator: Option<String>,
    inherited_originator: Option<String>,
    default_originator: String,
) -> String {
    originator_from_service_name(metrics_service_name)
        .or(persisted_originator)
        .or(inherited_originator)
        .or(env_originator)
        .unwrap_or(default_originator)
}

pub(crate) struct ResumeThreadWithHistoryOptions {
    pub(crate) config: Config,
    pub(crate) initial_history: InitialHistory,
    pub(crate) agent_control: AgentControl,
    pub(crate) session_source: SessionSource,
    pub(crate) parent_thread_id: Option<ThreadId>,
    pub(crate) inherited_environments: Option<TurnEnvironmentSnapshot>,
    pub(crate) inherited_exec_policy: Option<Arc<crate::exec_policy::ExecPolicyManager>>,
}

/// Shared, `Arc`-owned state for [`ThreadManager`]. This `Arc` is required to have a single
/// `Arc` reference that can be downgraded to by `AgentControl` while preventing every single
/// function to require an `Arc<&Self>`.
pub(crate) struct ThreadManagerState {
    threads: Arc<RwLock<HashMap<ThreadId, Arc<XedocThread>>>>,
    thread_created_tx: broadcast::Sender<ThreadId>,
    auth_manager: Arc<AuthManager>,
    models_manager: SharedModelsManager,
    environment_manager: Arc<EnvironmentManager>,
    skills_service: Arc<SkillsService>,
    plugins_manager: Arc<PluginsManager>,
    mcp_manager: Arc<McpManager>,
    extensions: Arc<ExtensionRegistry<Config>>,
    user_instructions_provider: Arc<dyn UserInstructionsProvider>,
    thread_store: Arc<dyn ThreadStore>,
    agent_graph_store: Option<Arc<dyn AgentGraphStore>>,
    external_time_provider: Option<Arc<dyn TimeProvider>>,
    session_source: SessionSource,
    installation_id: String,
    // Captures submitted ops for testing purpose when test mode is enabled.
    ops_log: Option<SharedCapturedOps>,
}

pub fn build_models_manager(
    config: &Config,
    auth_manager: Arc<AuthManager>,
) -> SharedModelsManager {
    let mut managers: Vec<(String, SharedModelsManager)> = Vec::new();
    let model_catalogs = model_catalogs_for_config(config);
    let configured_provider_ids = config
        .config_layer_stack
        .effective_user_config()
        .map(|user_config| {
            user_config
                .get("model_providers")
                .and_then(toml::Value::as_table)
                .map(|providers| providers.keys().cloned().collect::<HashSet<_>>())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let mut provider_infos = config.model_providers.iter().collect::<Vec<_>>();
    provider_infos.sort_by(|(left_id, _), (right_id, _)| {
        (*left_id != &config.model_provider_id)
            .cmp(&(*right_id != &config.model_provider_id))
            .then_with(|| left_id.cmp(right_id))
    });
    info!(
        active_provider_id = %config.model_provider_id,
        configured_provider_ids = ?configured_provider_ids,
        available_provider_ids = ?provider_infos.iter().map(|(provider_id, _)| provider_id).collect::<Vec<_>>(),
        "model provider catalog configuration"
    );
    for (provider_id, provider_info) in provider_infos {
        let xedoc_home = config
            .xedoc_home
            .join("models_cache")
            .join(provider_id)
            .to_path_buf();
        let provider = create_model_provider_for_configured_id(
            provider_id.clone(),
            provider_info.clone(),
            Some(auth_manager.clone()),
        );
        let model_catalog = model_catalogs.get(provider_id).cloned().or_else(|| {
            (provider_id == &config.model_provider_id)
                .then(|| config.model_catalog.clone())
                .flatten()
        });
        let manager = provider.models_manager(xedoc_home, model_catalog);
        let manager: SharedModelsManager = Arc::new(RegistryModelsManager::new(
            provider_id.clone(),
            config.model_registry.clone(),
            manager,
        ));
        let always_enabled = model_provider_is_always_enabled(
            provider_id,
            &config.model_provider_id,
            &configured_provider_ids,
        );
        let manager = if always_enabled {
            manager
        } else {
            let auth_manager = Arc::clone(&auth_manager);
            let provider_id = provider_id.clone();
            let provider_info = provider_info.clone();
            Arc::new(AvailabilityGatedModelsManager::new(manager, move || {
                let available = configured_provider_has_credentials(
                    &provider_id,
                    &provider_info,
                    &auth_manager,
                )
                .unwrap_or_else(|error| {
                    warn!(
                        provider_id,
                        "failed to inspect model-provider credentials: {error}"
                    );
                    false
                });
                info!(
                    provider_id,
                    always_enabled, available, "model provider availability gate evaluated"
                );
                available
            }))
        };
        managers.push((provider_id.clone(), manager));
    }
    if managers.is_empty() {
        // Fallback to single-provider with the active provider
        let provider = create_model_provider_for_configured_id(
            config.model_provider_id.clone(),
            config.model_provider.clone(),
            Some(auth_manager),
        );
        let model_catalog = model_catalogs
            .get(&config.model_provider_id)
            .cloned()
            .or(config.model_catalog.clone());
        let manager = provider.models_manager(
            config
                .xedoc_home
                .join("models_cache")
                .join(&config.model_provider_id)
                .to_path_buf(),
            model_catalog,
        );
        return Arc::new(RegistryModelsManager::new(
            config.model_provider_id.clone(),
            config.model_registry.clone(),
            manager,
        ));
    }
    Arc::new(xedoc_models_manager::manager::MultiProviderModelsManager::new(managers))
}

fn model_provider_is_always_enabled(
    provider_id: &str,
    active_provider_id: &str,
    configured_provider_ids: &HashSet<String>,
) -> bool {
    provider_id == active_provider_id
        || provider_id == OPENAI_PROVIDER_ID
        || provider_id == OLLAMA_OSS_PROVIDER_ID
        || provider_id == LMSTUDIO_OSS_PROVIDER_ID
        || configured_provider_ids.contains(provider_id)
}

pub fn thread_store_from_config(
    config: &Config,
    state_db: Option<StateDbHandle>,
) -> Arc<dyn ThreadStore> {
    match &config.experimental_thread_store {
        ThreadStoreConfig::Local => {
            if config
                .features
                .enabled(Feature::LocalThreadStoreCompression)
            {
                xedoc_rollout::spawn_rollout_compression_worker(config.xedoc_home.to_path_buf());
            }
            Arc::new(LocalThreadStore::new(
                LocalThreadStoreConfig::from_config(config),
                state_db,
            ))
        }
        ThreadStoreConfig::InMemory { id } => InMemoryThreadStore::for_id(id),
    }
}

/// Construct the default SQLite-backed agent graph store when local state is available.
pub fn local_agent_graph_store_from_state_db(
    state_db: Option<&StateDbHandle>,
) -> Option<Arc<dyn AgentGraphStore>> {
    state_db.map(|state_db| {
        Arc::new(LocalAgentGraphStore::new(Arc::clone(state_db))) as Arc<dyn AgentGraphStore>
    })
}

impl ThreadManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: &Config,
        auth_manager: Arc<AuthManager>,
        models_manager: SharedModelsManager,
        session_source: SessionSource,
        environment_manager: Arc<EnvironmentManager>,
        extensions: Arc<ExtensionRegistry<Config>>,
        user_instructions_provider: Arc<dyn UserInstructionsProvider>,
        thread_store: Arc<dyn ThreadStore>,
        agent_graph_store: Option<Arc<dyn AgentGraphStore>>,
        installation_id: String,
        external_time_provider: Option<Arc<dyn TimeProvider>>,
    ) -> Self {
        let xedoc_home = config.xedoc_home.clone();
        let restriction_product = session_source.restriction_product();
        let (thread_created_tx, _) = broadcast::channel(THREAD_CREATED_CHANNEL_CAPACITY);
        let plugins_manager = Arc::new(PluginsManager::new_with_options(
            xedoc_home.to_path_buf(),
            restriction_product,
            auth_manager.get_api_auth_mode(),
        ));
        let mcp_manager = Arc::new(McpManager::new_with_extensions(
            Arc::clone(&plugins_manager),
            Arc::clone(&extensions),
        ));
        let skills_service = Arc::new(SkillsService::new_with_restriction_product(
            xedoc_home,
            config.bundled_skills_enabled(),
            restriction_product,
        ));
        Self {
            state: Arc::new(ThreadManagerState {
                threads: Arc::new(RwLock::new(HashMap::new())),
                thread_created_tx,
                models_manager,
                environment_manager,
                skills_service,
                plugins_manager,
                mcp_manager,
                extensions,
                user_instructions_provider,
                thread_store,
                agent_graph_store,
                external_time_provider,
                auth_manager,
                session_source,
                installation_id,
                ops_log: should_use_test_thread_manager_behavior()
                    .then(|| Arc::new(std::sync::Mutex::new(Vec::new()))),
            }),
            _test_xedoc_home_guard: None,
        }
    }

    /// Construct with a dummy AuthManager containing the provided XedocAuth.
    /// Used for integration tests: should not be used by ordinary business logic.
    pub(crate) fn with_models_provider_for_tests(
        auth: XedocAuth,
        provider: ModelProviderInfo,
    ) -> Self {
        set_thread_manager_test_mode_for_tests(/*enabled*/ true);
        let xedoc_home = std::env::temp_dir().join(format!(
            "xedoc-thread-manager-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&xedoc_home)
            .unwrap_or_else(|err| panic!("temp xedoc home dir create failed: {err}"));
        let mut manager = Self::with_models_provider_and_home_for_tests(
            auth,
            provider,
            xedoc_home.clone(),
            Arc::new(EnvironmentManager::default_for_tests()),
        );
        manager._test_xedoc_home_guard = Some(TempXedocHomeGuard { path: xedoc_home });
        manager
    }

    /// Construct with a dummy AuthManager containing the provided XedocAuth and xedoc home.
    /// Used for integration tests: should not be used by ordinary business logic.
    pub(crate) fn with_models_provider_and_home_for_tests(
        auth: XedocAuth,
        provider: ModelProviderInfo,
        xedoc_home: PathBuf,
        environment_manager: Arc<EnvironmentManager>,
    ) -> Self {
        Self::with_models_provider_home_and_state_for_tests(
            auth,
            provider,
            xedoc_home,
            environment_manager,
            /*state_db*/ None,
        )
    }

    pub(crate) fn with_models_provider_home_and_state_for_tests(
        auth: XedocAuth,
        provider: ModelProviderInfo,
        xedoc_home: PathBuf,
        environment_manager: Arc<EnvironmentManager>,
        state_db: Option<StateDbHandle>,
    ) -> Self {
        set_thread_manager_test_mode_for_tests(/*enabled*/ true);
        let auth_manager = AuthManager::from_auth_for_testing(auth);
        let installation_id = uuid::Uuid::new_v4().to_string();
        let skills_xedoc_home = match AbsolutePathBuf::from_absolute_path_checked(&xedoc_home) {
            Ok(xedoc_home) => xedoc_home,
            Err(err) => panic!("test xedoc_home should be absolute: {err}"),
        };
        let (thread_created_tx, _) = broadcast::channel(THREAD_CREATED_CHANNEL_CAPACITY);
        let restriction_product = SessionSource::Exec.restriction_product();
        let plugins_manager = Arc::new(PluginsManager::new_with_options(
            xedoc_home.clone(),
            restriction_product,
            auth_manager.get_api_auth_mode(),
        ));
        let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
        let skills_service = Arc::new(SkillsService::new_with_restriction_product(
            skills_xedoc_home,
            /*bundled_skills_enabled*/ true,
            restriction_product,
        ));
        // This test constructor has no Config input. Tests that need a non-local
        // process store should construct ThreadManager::new with an explicit store.
        let thread_store: Arc<dyn ThreadStore> = Arc::new(LocalThreadStore::new(
            LocalThreadStoreConfig {
                xedoc_home: xedoc_home.clone(),
                sqlite_home: xedoc_home.clone(),
                default_model_provider_id: OPENAI_PROVIDER_ID.to_string(),
            },
            state_db.clone(),
        ));
        let agent_graph_store = local_agent_graph_store_from_state_db(state_db.as_ref());
        Self {
            state: Arc::new(ThreadManagerState {
                threads: Arc::new(RwLock::new(HashMap::new())),
                thread_created_tx,
                models_manager: create_model_provider(provider, Some(auth_manager.clone()))
                    .models_manager(xedoc_home, /*config_model_catalog*/ None),
                environment_manager,
                skills_service,
                plugins_manager,
                mcp_manager,
                extensions: empty_extension_registry(),
                user_instructions_provider: Arc::new(
                    crate::test_support::EmptyUserInstructionsProvider,
                ),
                thread_store,
                agent_graph_store,
                external_time_provider: None,
                auth_manager,
                session_source: SessionSource::Exec,
                installation_id,
                ops_log: should_use_test_thread_manager_behavior()
                    .then(|| Arc::new(std::sync::Mutex::new(Vec::new()))),
            }),
            _test_xedoc_home_guard: None,
        }
    }

    pub fn session_source(&self) -> SessionSource {
        self.state.session_source.clone()
    }

    pub fn auth_manager(&self) -> Arc<AuthManager> {
        self.state.auth_manager.clone()
    }

    pub fn skills_service(&self) -> Arc<SkillsService> {
        self.state.skills_service.clone()
    }

    pub fn plugins_manager(&self) -> Arc<PluginsManager> {
        self.state.plugins_manager.clone()
    }

    pub fn mcp_manager(&self) -> Arc<McpManager> {
        self.state.mcp_manager.clone()
    }

    pub fn environment_manager(&self) -> Arc<EnvironmentManager> {
        self.state.environment_manager.clone()
    }

    pub fn default_environment_selections(
        &self,
        cwd: &AbsolutePathBuf,
        workspace_roots: &[AbsolutePathBuf],
    ) -> Vec<TurnEnvironmentSelection> {
        default_thread_environment_selections(
            self.state.environment_manager.as_ref(),
            cwd,
            workspace_roots,
        )
    }

    pub fn validate_environment_selections(
        &self,
        environments: &[TurnEnvironmentSelection],
    ) -> XedocResult<()> {
        let mut environment_ids = HashSet::with_capacity(environments.len());
        for environment in environments {
            if !environment_ids.insert(environment.environment_id.as_str()) {
                return Err(XedocErr::InvalidRequest(format!(
                    "duplicate turn environment id `{}`",
                    environment.environment_id
                )));
            }
            self.state
                .environment_manager
                .get_environment(&environment.environment_id)
                .ok_or_else(|| {
                    XedocErr::InvalidRequest(format!(
                        "unknown turn environment id `{}`",
                        environment.environment_id
                    ))
                })?;
        }
        Ok(())
    }

    pub fn get_models_manager(&self) -> SharedModelsManager {
        self.state.models_manager.clone()
    }

    pub async fn list_models(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: xedoc_http_client::HttpClientFactory,
    ) -> Vec<ModelPreset> {
        self.state
            .models_manager
            .list_models(refresh_strategy, http_client_factory)
            .await
    }

    pub fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        self.state.models_manager.list_collaboration_modes()
    }

    pub async fn list_thread_ids(&self) -> Vec<ThreadId> {
        self.state.list_thread_ids().await
    }

    pub fn subscribe_thread_created(&self) -> broadcast::Receiver<ThreadId> {
        self.state.thread_created_tx.subscribe()
    }

    pub async fn get_thread(&self, thread_id: ThreadId) -> XedocResult<Arc<XedocThread>> {
        self.state.get_thread(thread_id).await
    }

    /// Updates metadata for loaded and cold threads through one entrypoint.
    ///
    /// Loaded threads route through `XedocThread`/`LiveThread`, so metadata changes stay ordered
    /// with live rollout writes. Cold threads go directly to the store, which owns unloaded JSONL
    /// compatibility and SQLite metadata updates.
    pub async fn update_thread_metadata(
        &self,
        thread_id: ThreadId,
        patch: ThreadMetadataPatch,
        include_archived: bool,
    ) -> XedocResult<StoredThread> {
        if let Ok(thread) = self.get_thread(thread_id).await {
            if thread.config_snapshot().await.ephemeral {
                return Err(XedocErr::InvalidRequest(format!(
                    "ephemeral thread does not support metadata updates: {thread_id}"
                )));
            }
            return thread
                .update_thread_metadata(patch, include_archived)
                .await
                .map_err(|err| thread_store_metadata_update_error(thread_id, err));
        }
        self.state
            .thread_store
            .update_thread_metadata(UpdateThreadMetadataParams {
                thread_id,
                patch,
                include_archived,
            })
            .await
            .map_err(|err| match err {
                ThreadStoreError::ThreadNotFound { thread_id } => {
                    XedocErr::ThreadNotFound(thread_id)
                }
                err => thread_store_metadata_update_error(thread_id, err),
            })
    }

    /// List `thread_id` plus all known descendants in its spawn subtree.
    pub async fn list_agent_subtree_thread_ids(
        &self,
        thread_id: ThreadId,
    ) -> XedocResult<Vec<ThreadId>> {
        let mut subtree_thread_ids = Vec::new();
        let mut seen_thread_ids = HashSet::new();
        subtree_thread_ids.push(thread_id);
        seen_thread_ids.insert(thread_id);

        if let Some(agent_graph_store) = self.state.agent_graph_store() {
            for descendant_id in agent_graph_store
                .list_thread_spawn_descendants(thread_id, /*status_filter*/ None)
                .await
                .map_err(|err| {
                    XedocErr::Fatal(format!("failed to load thread-spawn descendants: {err}"))
                })?
            {
                if seen_thread_ids.insert(descendant_id) {
                    subtree_thread_ids.push(descendant_id);
                }
            }
        }

        for descendant_id in self
            .agent_control()
            .list_live_agent_subtree_thread_ids(thread_id)
            .await?
        {
            if seen_thread_ids.insert(descendant_id) {
                subtree_thread_ids.push(descendant_id);
            }
        }

        Ok(subtree_thread_ids)
    }

    pub async fn start_thread(&self, config: Config) -> XedocResult<NewThread> {
        // Box delegated thread-spawn futures so these convenience wrappers do
        // not inline the full spawn path into every caller's async state.
        Box::pin(self.start_thread_with_tools(config, Vec::new())).await
    }

    pub async fn start_thread_with_tools(
        &self,
        config: Config,
        dynamic_tools: Vec<xedoc_protocol::dynamic_tools::DynamicToolSpec>,
    ) -> XedocResult<NewThread> {
        let environments = default_thread_environment_selections(
            self.state.environment_manager.as_ref(),
            &config.cwd,
            &config.workspace_roots,
        );
        Box::pin(self.start_thread_with_options(StartThreadOptions {
            config,
            allow_provider_model_fallback: false,
            initial_history: InitialHistory::New,
            history_mode: None,
            session_source: None,
            thread_source: None,
            dynamic_tools,
            metrics_service_name: None,
            parent_trace: None,
            environments,
            thread_extension_init: ExtensionDataInit::default(),
            supports_openai_form_elicitation: false,
        }))
        .await
    }

    pub async fn start_thread_with_options(
        &self,
        options: StartThreadOptions,
    ) -> XedocResult<NewThread> {
        self.start_thread_with_options_and_fork_source(options, /*forked_from_thread_id*/ None)
            .await
    }

    async fn start_thread_with_options_and_fork_source(
        &self,
        options: StartThreadOptions,
        forked_from_thread_id: Option<ThreadId>,
    ) -> XedocResult<NewThread> {
        let agent_control = self.agent_control_for_config(&options.config);
        let (resumed_session_source, resumed_thread_source) = options
            .initial_history
            .get_resumed_session_sources()
            .unwrap_or_else(|| (self.state.session_source.clone(), None));
        let session_source = options.session_source.unwrap_or(resumed_session_source);
        let thread_source = options.thread_source.or(resumed_thread_source);
        Box::pin(self.state.spawn_thread_with_source(
            options.config,
            options.initial_history,
            options.history_mode,
            options.allow_provider_model_fallback,
            Arc::clone(&self.state.auth_manager),
            agent_control,
            session_source,
            /*parent_thread_id*/ None,
            forked_from_thread_id,
            thread_source,
            options.dynamic_tools,
            options.metrics_service_name,
            /*inherited_environments*/ None,
            /*inherited_exec_policy*/ None,
            options.parent_trace,
            options.environments,
            options.thread_extension_init,
            options.supports_openai_form_elicitation,
            /*user_shell_override*/ None,
        ))
        .await
    }

    // TODO(jif) merge with fork_agent
    /// Spawn a subagent by forking persisted history from `forked_from_thread_id`.
    pub async fn spawn_subagent(
        &self,
        forked_from_thread_id: ThreadId,
        mut options: StartThreadOptions,
    ) -> XedocResult<NewThread> {
        let fork_source = self.get_thread(forked_from_thread_id).await?;
        // Persist queued rollout updates before reading the fork snapshot.
        fork_source.ensure_rollout_materialized().await;
        fork_source.flush_rollout().await?;
        let stored_thread = fork_source
            .read_thread(
                /*include_archived*/ true, /*include_history*/ true,
            )
            .await
            .map_err(|err| {
                XedocErr::Fatal(format!(
                    "failed to read subagent fork source {forked_from_thread_id}: {err}"
                ))
            })?;
        let history = stored_thread_to_initial_history(stored_thread, fork_source.rollout_path())?;
        let inherited_multi_agent_version = fork_source
            .multi_agent_version()
            .unwrap_or(MultiAgentVersion::V1);
        options.initial_history = fork_history_from_snapshot(
            ForkSnapshot::Interrupted,
            history,
            InterruptedTurnHistoryMarker::from_config_and_version(
                &options.config,
                inherited_multi_agent_version,
            ),
        );
        self.start_thread_with_options_and_fork_source(options, Some(forked_from_thread_id))
            .await
    }

    pub async fn resume_thread_from_rollout(
        &self,
        config: Config,
        rollout_path: PathBuf,
        auth_manager: Arc<AuthManager>,
        parent_trace: Option<W3cTraceContext>,
        supports_openai_form_elicitation: bool,
    ) -> XedocResult<NewThread> {
        let initial_history = self.initial_history_from_rollout_path(rollout_path).await?;
        Box::pin(self.resume_thread_with_history(
            config,
            initial_history,
            auth_manager,
            parent_trace,
            supports_openai_form_elicitation,
        ))
        .await
    }

    #[instrument(level = "trace", skip_all)]
    pub async fn resume_thread_with_history(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
        parent_trace: Option<W3cTraceContext>,
        supports_openai_form_elicitation: bool,
    ) -> XedocResult<NewThread> {
        let agent_control = self.agent_control_for_config(&config);
        let environments = default_thread_environment_selections(
            self.state.environment_manager.as_ref(),
            &config.cwd,
            &config.workspace_roots,
        );
        let (session_source, thread_source) = initial_history
            .get_resumed_session_sources()
            .unwrap_or_else(|| (self.state.session_source.clone(), None));
        if let InitialHistory::Resumed(resumed) = &initial_history
            && initial_history.get_multi_agent_version() == Some(MultiAgentVersion::V2)
            && !session_source.is_non_root_agent()
        {
            agent_control
                .restore_v2_agent_metadata(&config, resumed.conversation_id)
                .await;
        }
        Box::pin(self.state.spawn_thread_with_source(
            config,
            initial_history,
            /*history_mode*/ None,
            /*allow_provider_model_fallback*/ false,
            auth_manager,
            agent_control,
            session_source,
            /*parent_thread_id*/ None,
            /*forked_from_thread_id*/ None,
            thread_source,
            Vec::new(),
            /*metrics_service_name*/ None,
            /*inherited_environments*/ None,
            /*inherited_exec_policy*/ None,
            parent_trace,
            environments,
            /*thread_extension_init*/ ExtensionDataInit::default(),
            supports_openai_form_elicitation,
            /*user_shell_override*/ None,
        ))
        .await
    }

    pub(crate) async fn start_thread_with_user_shell_override_for_tests(
        &self,
        config: Config,
        user_shell_override: crate::shell::Shell,
        supports_openai_form_elicitation: bool,
    ) -> XedocResult<NewThread> {
        let agent_control = self.agent_control_for_config(&config);
        let environments = default_thread_environment_selections(
            self.state.environment_manager.as_ref(),
            &config.cwd,
            &config.workspace_roots,
        );
        Box::pin(self.state.spawn_thread(
            config,
            InitialHistory::New,
            Arc::clone(&self.state.auth_manager),
            agent_control,
            /*parent_thread_id*/ None,
            /*forked_from_thread_id*/ None,
            /*thread_source*/ None,
            Vec::new(),
            /*metrics_service_name*/ None,
            /*parent_trace*/ None,
            environments,
            /*thread_extension_init*/ ExtensionDataInit::default(),
            supports_openai_form_elicitation,
            /*user_shell_override*/ Some(user_shell_override),
        ))
        .await
    }

    pub(crate) async fn resume_thread_from_rollout_with_user_shell_override_for_tests(
        &self,
        config: Config,
        rollout_path: PathBuf,
        auth_manager: Arc<AuthManager>,
        user_shell_override: crate::shell::Shell,
        supports_openai_form_elicitation: bool,
    ) -> XedocResult<NewThread> {
        let agent_control = self.agent_control_for_config(&config);
        let initial_history = self.initial_history_from_rollout_path(rollout_path).await?;
        let environments = default_thread_environment_selections(
            self.state.environment_manager.as_ref(),
            &config.cwd,
            &config.workspace_roots,
        );
        let (session_source, thread_source) = initial_history
            .get_resumed_session_sources()
            .unwrap_or_else(|| (self.state.session_source.clone(), None));
        Box::pin(self.state.spawn_thread_with_source(
            config,
            initial_history,
            /*history_mode*/ None,
            /*allow_provider_model_fallback*/ false,
            auth_manager,
            agent_control,
            session_source,
            /*parent_thread_id*/ None,
            /*forked_from_thread_id*/ None,
            thread_source,
            Vec::new(),
            /*metrics_service_name*/ None,
            /*inherited_environments*/ None,
            /*inherited_exec_policy*/ None,
            /*parent_trace*/ None,
            environments,
            /*thread_extension_init*/ ExtensionDataInit::default(),
            supports_openai_form_elicitation,
            /*user_shell_override*/ Some(user_shell_override),
        ))
        .await
    }

    /// Removes the thread from the manager's internal map, though the thread is stored
    /// as `Arc<XedocThread>`, it is possible that other references to it exist elsewhere.
    /// Returns the thread if the thread was found and removed.
    pub async fn remove_thread(&self, thread_id: &ThreadId) -> Option<Arc<XedocThread>> {
        self.state.threads.write().await.remove(thread_id)
    }

    /// Tries to shut down all tracked threads concurrently within the provided timeout.
    /// Threads that complete shutdown are removed from the manager; incomplete shutdowns
    /// remain tracked so callers can retry or inspect them later.
    pub async fn shutdown_all_threads_bounded(&self, timeout: Duration) -> ThreadShutdownReport {
        let threads = {
            let threads = self.state.threads.read().await;
            threads
                .iter()
                .map(|(thread_id, thread)| (*thread_id, Arc::clone(thread)))
                .collect::<Vec<_>>()
        };

        let mut shutdowns = threads
            .into_iter()
            .map(|(thread_id, thread)| async move {
                let outcome = match tokio::time::timeout(timeout, thread.shutdown_and_wait()).await
                {
                    Ok(Ok(())) => ShutdownOutcome::Complete,
                    Ok(Err(_)) => ShutdownOutcome::SubmitFailed,
                    Err(_) => ShutdownOutcome::TimedOut,
                };
                (thread_id, outcome)
            })
            .collect::<FuturesUnordered<_>>();
        let mut report = ThreadShutdownReport::default();

        while let Some((thread_id, outcome)) = shutdowns.next().await {
            match outcome {
                ShutdownOutcome::Complete => report.completed.push(thread_id),
                ShutdownOutcome::SubmitFailed => report.submit_failed.push(thread_id),
                ShutdownOutcome::TimedOut => report.timed_out.push(thread_id),
            }
        }

        let mut tracked_threads = self.state.threads.write().await;
        for thread_id in &report.completed {
            tracked_threads.remove(thread_id);
        }

        report
            .completed
            .sort_by_key(std::string::ToString::to_string);
        report
            .submit_failed
            .sort_by_key(std::string::ToString::to_string);
        report
            .timed_out
            .sort_by_key(std::string::ToString::to_string);
        report
    }

    /// Fork an existing thread by snapshotting rollout history according to
    /// `snapshot` and starting a new thread with identical configuration
    /// (unless overridden by the caller's `config`). The new thread will have
    /// a fresh id.
    pub async fn fork_thread<S>(
        &self,
        snapshot: S,
        config: Config,
        path: PathBuf,
        thread_source: Option<ThreadSource>,
        parent_trace: Option<W3cTraceContext>,
    ) -> XedocResult<NewThread>
    where
        S: Into<ForkSnapshot>,
    {
        let snapshot = snapshot.into();
        let history = self.initial_history_from_rollout_path(path).await?;
        self.fork_thread_from_history(
            snapshot,
            config,
            history,
            thread_source,
            parent_trace,
            /*supports_openai_form_elicitation*/ false,
        )
        .await
    }

    async fn initial_history_from_rollout_path(
        &self,
        rollout_path: PathBuf,
    ) -> XedocResult<InitialHistory> {
        let requested_rollout_path = rollout_path.clone();
        let stored_thread = self
            .state
            .thread_store
            .read_thread_by_rollout_path(ReadThreadByRolloutPathParams {
                rollout_path,
                include_archived: true,
                include_history: true,
            })
            .await
            .map_err(thread_store_rollout_read_error)?;
        stored_thread_to_initial_history(stored_thread, Some(requested_rollout_path))
    }

    /// Fork an existing thread from already-loaded store history.
    pub async fn fork_thread_from_history<S>(
        &self,
        snapshot: S,
        config: Config,
        history: InitialHistory,
        thread_source: Option<ThreadSource>,
        parent_trace: Option<W3cTraceContext>,
        supports_openai_form_elicitation: bool,
    ) -> XedocResult<NewThread>
    where
        S: Into<ForkSnapshot>,
    {
        self.fork_thread_with_initial_history(
            snapshot.into(),
            config,
            history,
            thread_source,
            parent_trace,
            supports_openai_form_elicitation,
        )
        .await
    }

    async fn fork_thread_with_initial_history(
        &self,
        snapshot: ForkSnapshot,
        config: Config,
        history: InitialHistory,
        thread_source: Option<ThreadSource>,
        parent_trace: Option<W3cTraceContext>,
        supports_openai_form_elicitation: bool,
    ) -> XedocResult<NewThread> {
        // `forked_from_id()` describes this history's existing lineage. When
        // forking a resumed thread, the child copies the resumed thread itself.
        let source_thread_id = match &history {
            InitialHistory::Resumed(resumed) => Some(resumed.conversation_id),
            InitialHistory::Forked(_) => history.forked_from_id(),
            InitialHistory::New | InitialHistory::Cleared => None,
        };
        let multi_agent_version = self
            .state
            .effective_multi_agent_version_for_spawn(
                &history,
                /*session_source*/ None,
                /*parent_thread_id*/ None,
                source_thread_id,
                &config,
            )
            .await;
        let interrupted_marker =
            InterruptedTurnHistoryMarker::from_config_and_version(&config, multi_agent_version);
        let history = fork_history_from_snapshot(snapshot, history, interrupted_marker);
        let environments = default_thread_environment_selections(
            self.state.environment_manager.as_ref(),
            &config.cwd,
            &config.workspace_roots,
        );
        let agent_control = self.agent_control_for_config(&config);
        Box::pin(self.state.spawn_thread(
            config,
            history,
            Arc::clone(&self.state.auth_manager),
            agent_control,
            /*parent_thread_id*/ None,
            source_thread_id,
            thread_source,
            Vec::new(),
            /*metrics_service_name*/ None,
            parent_trace,
            environments,
            /*thread_extension_init*/ ExtensionDataInit::default(),
            supports_openai_form_elicitation,
            /*user_shell_override*/ None,
        ))
        .await
    }

    pub(crate) fn agent_control(&self) -> AgentControl {
        AgentControl::new(Arc::downgrade(&self.state), /*rollout_budget*/ None)
    }

    fn agent_control_for_config(&self, config: &Config) -> AgentControl {
        AgentControl::new(Arc::downgrade(&self.state), config.rollout_budget.clone())
    }

    #[cfg(test)]
    pub(crate) fn captured_ops(&self) -> Vec<(ThreadId, Op)> {
        self.state
            .ops_log
            .as_ref()
            .and_then(|ops_log| ops_log.lock().ok().map(|log| log.clone()))
            .unwrap_or_default()
    }
}

impl ThreadManagerState {
    pub(crate) fn agent_graph_store(&self) -> Option<Arc<dyn AgentGraphStore>> {
        self.agent_graph_store.clone()
    }

    pub(crate) async fn list_thread_ids(&self) -> Vec<ThreadId> {
        self.threads
            .read()
            .await
            .iter()
            .filter_map(|(thread_id, thread)| {
                (!thread.session_source.is_internal()).then_some(*thread_id)
            })
            .collect()
    }

    /// List parent-child edges for currently loaded thread-spawn agents.
    pub(crate) async fn list_live_thread_spawn_edges(&self) -> Vec<(ThreadId, ThreadId)> {
        self.threads
            .read()
            .await
            .iter()
            .filter_map(|(thread_id, thread)| {
                if thread.session_source.is_internal() {
                    return None;
                }
                match &thread.session_source {
                    SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                        parent_thread_id,
                        ..
                    }) => Some((*parent_thread_id, *thread_id)),
                    _ => None,
                }
            })
            .collect()
    }

    /// Fetch a thread by ID or return ThreadNotFound.
    pub(crate) async fn get_thread(&self, thread_id: ThreadId) -> XedocResult<Arc<XedocThread>> {
        let threads = self.threads.read().await;
        match threads.get(&thread_id) {
            Some(thread) if !thread.session_source.is_internal() => Ok(thread.clone()),
            Some(_) | None => Err(XedocErr::ThreadNotFound(thread_id)),
        }
    }

    pub(crate) async fn read_stored_thread(
        &self,
        params: ReadThreadParams,
    ) -> XedocResult<StoredThread> {
        let thread_id = params.thread_id;
        self.thread_store
            .read_thread(params)
            .await
            .map_err(|err| match err {
                ThreadStoreError::ThreadNotFound { thread_id } => {
                    XedocErr::ThreadNotFound(thread_id)
                }
                ThreadStoreError::InvalidRequest { message } => {
                    if message.starts_with("no rollout found for thread id ") {
                        XedocErr::ThreadNotFound(thread_id)
                    } else {
                        XedocErr::Fatal(format!(
                            "failed to read stored thread {thread_id}: invalid thread-store request: {message}"
                        ))
                    }
                }
                err => XedocErr::Fatal(format!("failed to read stored thread {thread_id}: {err}")),
            })
    }

    pub(crate) async fn load_latest_model_context(
        &self,
        params: LoadThreadHistoryParams,
    ) -> XedocResult<StoredModelContext> {
        let thread_id = params.thread_id;
        self.thread_store
            .load_latest_model_context(params)
            .await
            .map_err(|err| match err {
                ThreadStoreError::ThreadNotFound { thread_id } => {
                    XedocErr::ThreadNotFound(thread_id)
                }
                err => XedocErr::Fatal(format!(
                    "failed to load model context for thread {thread_id}: {err}"
                )),
            })
    }

    /// Send an operation to a thread by ID.
    pub(crate) async fn send_op(&self, thread_id: ThreadId, op: Op) -> XedocResult<String> {
        let thread = self.get_thread(thread_id).await?;
        if let Some(ops_log) = &self.ops_log
            && let Ok(mut log) = ops_log.lock()
        {
            log.push((thread_id, op.clone()));
        }
        thread.submit(op).await
    }

    /// Remove a thread from the manager by ID, returning it when present.
    pub(crate) async fn remove_thread(&self, thread_id: &ThreadId) -> Option<Arc<XedocThread>> {
        self.threads.write().await.remove(thread_id)
    }

    pub(crate) async fn effective_multi_agent_version_for_spawn(
        &self,
        initial_history: &InitialHistory,
        session_source: Option<&SessionSource>,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
        config: &Config,
    ) -> MultiAgentVersion {
        if let Some(multi_agent_version) = config.multi_agent_version_override() {
            return multi_agent_version;
        }
        self.initial_multi_agent_version_for_spawn(
            initial_history,
            session_source,
            parent_thread_id,
            forked_from_thread_id,
        )
        .await
        .unwrap_or_else(|| config.multi_agent_version_from_features())
    }

    async fn initial_multi_agent_version_for_spawn(
        &self,
        initial_history: &InitialHistory,
        session_source: Option<&SessionSource>,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
    ) -> Option<MultiAgentVersion> {
        let inherited_thread_id = match session_source {
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            })) => Some(*parent_thread_id),
            _ => match initial_history {
                InitialHistory::Resumed(resumed) => Some(resumed.conversation_id),
                InitialHistory::Forked(_) => forked_from_thread_id.or(parent_thread_id),
                InitialHistory::New | InitialHistory::Cleared => parent_thread_id,
            },
        };
        let inherited_multi_agent_version = match inherited_thread_id {
            Some(thread_id) => self
                .get_thread(thread_id)
                .await
                .ok()
                .and_then(|thread| thread.multi_agent_version()),
            None => None,
        };
        resolve_multi_agent_version(initial_history, inherited_multi_agent_version)
    }

    /// Resolves the provider snapshot for a newly spawned runtime.
    ///
    /// Loads a fresh provider snapshot for:
    /// - fresh root threads;
    /// - cold resumes;
    /// - root forks.
    ///
    /// Uses an existing snapshot for:
    /// - subagents, which inherit from their parent without invoking the
    ///   provider;
    /// - running resumes and compaction paths, which retain the live session.
    ///
    /// Provider warnings only apply to fresh loads. If a parent runtime is no
    /// longer available, its child starts without provider instructions rather
    /// than loading independently.
    async fn user_instructions_for_spawn(
        &self,
        session_source: &SessionSource,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
    ) -> LoadedUserInstructions {
        let is_root_agent = !session_source.is_non_root_agent();
        if is_root_agent {
            return self
                .user_instructions_provider
                .load_user_instructions()
                .await;
        }

        let inherited_thread_id = match session_source {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            }) => Some(*parent_thread_id),
            _ => parent_thread_id.or(forked_from_thread_id),
        };
        let instructions = match inherited_thread_id {
            // The spawn path retains only thread IDs, so look up the live
            // runtime again here to inherit its user instructions.
            Some(thread_id) => match self.get_thread(thread_id).await {
                Ok(thread) => thread.session.user_instructions().await,
                Err(_) => None,
            },
            None => None,
        };
        LoadedUserInstructions {
            instructions,
            warnings: Vec::new(),
        }
    }

    async fn inherited_originator_for_parent_thread(
        &self,
        session_source: &SessionSource,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
    ) -> Option<String> {
        let inherited_thread_id = match session_source {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            }) => Some(*parent_thread_id),
            _ => parent_thread_id.or(forked_from_thread_id),
        };
        let thread = self.get_thread(inherited_thread_id?).await.ok()?;
        let originator = thread.config_snapshot().await.originator;
        (!originator.is_empty()).then_some(originator)
    }

    async fn effective_originator(
        &self,
        initial_history: &InitialHistory,
        metrics_service_name: Option<&str>,
        session_source: &SessionSource,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
    ) -> String {
        let persisted_originator = initial_history.get_session_originator();
        let inherited_originator = match initial_history {
            InitialHistory::New | InitialHistory::Cleared => {
                self.inherited_originator_for_parent_thread(
                    session_source,
                    parent_thread_id,
                    forked_from_thread_id,
                )
                .await
            }
            InitialHistory::Forked(_) if persisted_originator.is_none() => {
                self.inherited_originator_for_parent_thread(
                    session_source,
                    parent_thread_id,
                    forked_from_thread_id,
                )
                .await
            }
            InitialHistory::Resumed(_) | InitialHistory::Forked(_) => None,
        };

        let env_originator = std::env::var(XEDOC_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR)
            .is_ok()
            .then(|| originator().value);
        effective_originator_value(
            metrics_service_name,
            env_originator,
            persisted_originator,
            inherited_originator,
            originator().value,
        )
    }

    /// Spawn a new thread with no history using a provided config.
    pub(crate) async fn spawn_new_thread(
        &self,
        config: Config,
        agent_control: AgentControl,
    ) -> XedocResult<NewThread> {
        Box::pin(self.spawn_new_thread_with_source(
            config,
            agent_control,
            self.session_source.clone(),
            /*history_mode*/ None,
            /*parent_thread_id*/ None,
            /*forked_from_thread_id*/ None,
            /*thread_source*/ None,
            /*metrics_service_name*/ None,
            /*inherited_environments*/ None,
            /*inherited_exec_policy*/ None,
            /*environments*/ None,
        ))
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn spawn_new_thread_with_source(
        &self,
        config: Config,
        agent_control: AgentControl,
        session_source: SessionSource,
        history_mode: Option<ThreadHistoryMode>,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
        thread_source: Option<ThreadSource>,
        metrics_service_name: Option<String>,
        inherited_environments: Option<TurnEnvironmentSnapshot>,
        inherited_exec_policy: Option<Arc<crate::exec_policy::ExecPolicyManager>>,
        environments: Option<Vec<TurnEnvironmentSelection>>,
    ) -> XedocResult<NewThread> {
        let environments = environments.unwrap_or_else(|| {
            default_thread_environment_selections(
                self.environment_manager.as_ref(),
                &config.cwd,
                &config.workspace_roots,
            )
        });
        Box::pin(self.spawn_thread_with_source(
            config,
            InitialHistory::New,
            history_mode,
            /*allow_provider_model_fallback*/ false,
            Arc::clone(&self.auth_manager),
            agent_control,
            session_source,
            parent_thread_id,
            forked_from_thread_id,
            thread_source,
            Vec::new(),
            metrics_service_name,
            inherited_environments,
            inherited_exec_policy,
            /*parent_trace*/ None,
            environments,
            /*thread_extension_init*/ ExtensionDataInit::default(),
            /*supports_openai_form_elicitation*/ false,
            /*user_shell_override*/ None,
        ))
        .await
    }

    pub(crate) async fn resume_thread_with_history_with_source(
        &self,
        options: ResumeThreadWithHistoryOptions,
    ) -> XedocResult<NewThread> {
        let ResumeThreadWithHistoryOptions {
            config,
            initial_history,
            agent_control,
            session_source,
            parent_thread_id,
            inherited_environments,
            inherited_exec_policy,
        } = options;
        let environments = default_thread_environment_selections(
            self.environment_manager.as_ref(),
            &config.cwd,
            &config.workspace_roots,
        );
        let thread_source = initial_history.get_resumed_thread_source();
        Box::pin(self.spawn_thread_with_source(
            config,
            initial_history,
            /*history_mode*/ None,
            /*allow_provider_model_fallback*/ false,
            Arc::clone(&self.auth_manager),
            agent_control,
            session_source,
            parent_thread_id,
            /*forked_from_thread_id*/ None,
            thread_source,
            Vec::new(),
            /*metrics_service_name*/ None,
            inherited_environments,
            inherited_exec_policy,
            /*parent_trace*/ None,
            environments,
            /*thread_extension_init*/ ExtensionDataInit::default(),
            /*supports_openai_form_elicitation*/ false,
            /*user_shell_override*/ None,
        ))
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fork_thread_with_source(
        &self,
        config: Config,
        initial_history: InitialHistory,
        history_mode: Option<ThreadHistoryMode>,
        agent_control: AgentControl,
        session_source: SessionSource,
        thread_source: Option<ThreadSource>,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
        inherited_environments: Option<TurnEnvironmentSnapshot>,
        inherited_exec_policy: Option<Arc<crate::exec_policy::ExecPolicyManager>>,
        environments: Option<Vec<TurnEnvironmentSelection>>,
        thread_extension_init: ExtensionDataInit,
    ) -> XedocResult<NewThread> {
        let environments = environments.unwrap_or_else(|| {
            default_thread_environment_selections(
                self.environment_manager.as_ref(),
                &config.cwd,
                &config.workspace_roots,
            )
        });
        Box::pin(self.spawn_thread_with_source(
            config,
            initial_history,
            history_mode,
            /*allow_provider_model_fallback*/ false,
            Arc::clone(&self.auth_manager),
            agent_control,
            session_source,
            parent_thread_id,
            forked_from_thread_id,
            thread_source,
            Vec::new(),
            /*metrics_service_name*/ None,
            inherited_environments,
            inherited_exec_policy,
            /*parent_trace*/ None,
            environments,
            thread_extension_init,
            /*supports_openai_form_elicitation*/ false,
            /*user_shell_override*/ None,
        ))
        .await
    }

    /// Spawn a new thread with optional history and register it with the manager.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn spawn_thread(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
        agent_control: AgentControl,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
        thread_source: Option<ThreadSource>,
        dynamic_tools: Vec<xedoc_protocol::dynamic_tools::DynamicToolSpec>,
        metrics_service_name: Option<String>,
        parent_trace: Option<W3cTraceContext>,
        environments: Vec<TurnEnvironmentSelection>,
        thread_extension_init: ExtensionDataInit,
        supports_openai_form_elicitation: bool,
        user_shell_override: Option<crate::shell::Shell>,
    ) -> XedocResult<NewThread> {
        Box::pin(self.spawn_thread_with_source(
            config,
            initial_history,
            /*history_mode*/ None,
            /*allow_provider_model_fallback*/ false,
            auth_manager,
            agent_control,
            self.session_source.clone(),
            parent_thread_id,
            forked_from_thread_id,
            thread_source,
            dynamic_tools,
            metrics_service_name,
            /*inherited_environments*/ None,
            /*inherited_exec_policy*/ None,
            parent_trace,
            environments,
            thread_extension_init,
            supports_openai_form_elicitation,
            user_shell_override,
        ))
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn spawn_thread_with_source(
        &self,
        config: Config,
        initial_history: InitialHistory,
        history_mode: Option<ThreadHistoryMode>,
        allow_provider_model_fallback: bool,
        auth_manager: Arc<AuthManager>,
        agent_control: AgentControl,
        session_source: SessionSource,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
        thread_source: Option<ThreadSource>,
        dynamic_tools: Vec<xedoc_protocol::dynamic_tools::DynamicToolSpec>,
        metrics_service_name: Option<String>,
        inherited_environments: Option<TurnEnvironmentSnapshot>,
        inherited_exec_policy: Option<Arc<crate::exec_policy::ExecPolicyManager>>,
        parent_trace: Option<W3cTraceContext>,
        environments: Vec<TurnEnvironmentSelection>,
        thread_extension_init: ExtensionDataInit,
        supports_openai_form_elicitation: bool,
        user_shell_override: Option<crate::shell::Shell>,
    ) -> XedocResult<NewThread> {
        let is_resumed_thread = matches!(&initial_history, InitialHistory::Resumed(_));
        if let InitialHistory::Resumed(resumed) = &initial_history {
            let mut threads = self.threads.write().await;
            if let Some(thread) = threads.get(&resumed.conversation_id).cloned() {
                if thread.is_running() {
                    if let Some(requested_rollout_path) = resumed.rollout_path.as_deref()
                        && thread.rollout_path().as_deref() != Some(requested_rollout_path)
                    {
                        return Err(XedocErr::InvalidRequest(format!(
                            "thread {} is already running with a different rollout path",
                            resumed.conversation_id
                        )));
                    }
                    return Ok(NewThread {
                        thread_id: resumed.conversation_id,
                        session_configured: thread.session_configured(),
                        thread,
                    });
                }
                threads.remove(&resumed.conversation_id);
            }
        }
        let user_instructions = self
            .user_instructions_for_spawn(&session_source, parent_thread_id, forked_from_thread_id)
            .await;
        let user_instructions_provider = (!session_source.is_non_root_agent())
            .then(|| Arc::clone(&self.user_instructions_provider));
        let tracked_session_source = session_source.clone();
        let multi_agent_version = self
            .initial_multi_agent_version_for_spawn(
                &initial_history,
                Some(&session_source),
                parent_thread_id,
                forked_from_thread_id,
            )
            .await;
        let originator = self
            .effective_originator(
                &initial_history,
                metrics_service_name.as_deref(),
                &session_source,
                parent_thread_id,
                forked_from_thread_id,
            )
            .await;
        let (session, io) = Box::pin(Session::spawn(SessionSpawnArgs {
            config,
            allow_provider_model_fallback,
            user_instructions,
            user_instructions_provider,
            installation_id: self.installation_id.clone(),
            auth_manager,
            models_manager: Arc::clone(&self.models_manager),
            environment_manager: Arc::clone(&self.environment_manager),
            skills_service: Arc::clone(&self.skills_service),
            plugins_manager: Arc::clone(&self.plugins_manager),
            mcp_manager: Arc::clone(&self.mcp_manager),
            extensions: Arc::clone(&self.extensions),
            conversation_history: initial_history,
            requested_history_mode: history_mode,
            session_source,
            forked_from_thread_id,
            parent_thread_id,
            thread_source,
            originator,
            agent_control,
            dynamic_tools,
            metrics_service_name,
            inherited_environments,
            inherited_exec_policy,
            user_shell_override,
            parent_trace,
            environment_selections: environments,
            thread_extension_init,
            supports_openai_form_elicitation,
            thread_store: Arc::clone(&self.thread_store),
            external_time_provider: self.external_time_provider.clone(),
            inherited_multi_agent_version: multi_agent_version,
        }))
        .await?;
        let new_thread = self
            .finalize_thread_spawn(session, io, tracked_session_source)
            .await?;
        if is_resumed_thread {
            new_thread.thread.emit_thread_resume_lifecycle().await;
        }
        Ok(new_thread)
    }

    async fn finalize_thread_spawn(
        &self,
        session: Arc<Session>,
        io: SessionIo,
        session_source: SessionSource,
    ) -> XedocResult<NewThread> {
        let thread_id = session.thread_id();
        let event = io.next_event().await?;
        let session_configured = match event {
            Event {
                id,
                msg: EventMsg::SessionConfigured(session_configured),
            } if id == INITIAL_SUBMIT_ID => session_configured,
            _ => {
                return Err(XedocErr::SessionConfiguredNotFirstEvent);
            }
        };

        {
            let mut threads = self.threads.write().await;
            if let std::collections::hash_map::Entry::Vacant(e) = threads.entry(thread_id) {
                let thread = Arc::new(XedocThread::new(
                    session,
                    io,
                    session_configured.clone(),
                    session_configured.rollout_path.clone(),
                    session_source,
                ));
                e.insert(thread.clone());
                return Ok(NewThread {
                    thread_id,
                    thread,
                    session_configured,
                });
            }
        }

        if let Err(err) = io.shutdown_and_wait().await {
            warn!("failed to shut down duplicate thread {thread_id}: {err}");
        }
        Err(XedocErr::InvalidRequest(format!(
            "thread {thread_id} is already running"
        )))
    }

    pub(crate) fn notify_thread_created(&self, thread_id: ThreadId) {
        let _ = self.thread_created_tx.send(thread_id);
    }
}

fn stored_thread_to_initial_history(
    stored_thread: StoredThread,
    rollout_path: Option<PathBuf>,
) -> XedocResult<InitialHistory> {
    let thread_id = stored_thread.thread_id;
    let history = stored_thread.history.ok_or_else(|| {
        XedocErr::Fatal(format!(
            "thread {thread_id} did not include persisted history"
        ))
    })?;
    Ok(InitialHistory::Resumed(ResumedHistory {
        conversation_id: thread_id,
        history: Arc::new(history.items),
        rollout_path: rollout_path.or(stored_thread.rollout_path),
    }))
}

fn thread_store_rollout_read_error(err: ThreadStoreError) -> XedocErr {
    match err {
        ThreadStoreError::ThreadNotFound { thread_id } => XedocErr::ThreadNotFound(thread_id),
        ThreadStoreError::InvalidRequest { message } => XedocErr::InvalidRequest(message),
        err => XedocErr::Fatal(format!("failed to read thread by rollout path: {err}")),
    }
}

fn thread_store_metadata_update_error(thread_id: ThreadId, err: ThreadStoreError) -> XedocErr {
    match err {
        ThreadStoreError::ThreadNotFound { thread_id } => XedocErr::ThreadNotFound(thread_id),
        ThreadStoreError::InvalidRequest { message } => XedocErr::InvalidRequest(message),
        ThreadStoreError::Unsupported { operation } => XedocErr::UnsupportedOperation(format!(
            "thread metadata update is not supported by this store: {operation}"
        )),
        err => XedocErr::Fatal(format!(
            "failed to update thread metadata {thread_id}: {err}"
        )),
    }
}

/// Return a fork snapshot cut strictly before the nth user message (0-based).
///
/// Out-of-range values keep the full committed history at a turn boundary, but
/// when the source thread is currently mid-turn they fall back to cutting
/// before the active turn's opening boundary so the fork omits the unfinished
/// suffix entirely.
fn truncate_before_nth_user_message(
    history: InitialHistory,
    n: usize,
    snapshot_state: &SnapshotTurnState,
) -> InitialHistory {
    let items = history.get_rollout_items().to_vec();
    let user_positions = truncation::user_message_positions_in_rollout(&items);
    let rolled = if snapshot_state.ends_mid_turn && n >= user_positions.len() {
        if let Some(cut_idx) = snapshot_state
            .active_turn_start_index
            .or_else(|| user_positions.last().copied())
        {
            items[..cut_idx].to_vec()
        } else {
            items
        }
    } else {
        truncation::truncate_rollout_before_nth_user_message_from_start(&items, n)
    };

    if rolled.is_empty() {
        InitialHistory::New
    } else {
        InitialHistory::Forked(rolled)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct SnapshotTurnState {
    ends_mid_turn: bool,
    active_turn_id: Option<String>,
    active_turn_started_at: Option<i64>,
    active_turn_start_index: Option<usize>,
}

fn snapshot_turn_state(history: &InitialHistory) -> SnapshotTurnState {
    let rollout_items = history.get_rollout_items();
    let mut builder = ThreadHistoryBuilder::new();
    for item in rollout_items {
        builder.handle_rollout_item(item);
    }
    let active_turn_id = builder.active_turn_id_if_explicit();
    if builder.has_active_turn() && active_turn_id.is_some() {
        let active_turn_snapshot = builder.active_turn_snapshot();
        if active_turn_snapshot
            .as_ref()
            .is_some_and(|turn| turn.status != TurnStatus::InProgress)
        {
            return SnapshotTurnState {
                ends_mid_turn: false,
                active_turn_id: None,
                active_turn_started_at: None,
                active_turn_start_index: None,
            };
        }

        return SnapshotTurnState {
            ends_mid_turn: true,
            active_turn_id,
            active_turn_started_at: active_turn_snapshot.and_then(|turn| turn.started_at),
            active_turn_start_index: builder.active_turn_start_index(),
        };
    }

    let Some(last_user_position) = truncation::user_message_positions_in_rollout(rollout_items)
        .last()
        .copied()
    else {
        return SnapshotTurnState {
            ends_mid_turn: false,
            active_turn_id: None,
            active_turn_started_at: None,
            active_turn_start_index: None,
        };
    };

    // Synthetic fork/resume histories can contain user/assistant response items
    // without explicit turn lifecycle events. If the persisted snapshot has no
    // terminating boundary after its last user message, treat it as mid-turn.
    SnapshotTurnState {
        ends_mid_turn: !rollout_items[last_user_position + 1..].iter().any(|item| {
            matches!(
                item,
                RolloutItem::EventMsg(EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_))
            )
        }),
        active_turn_id: None,
        active_turn_started_at: None,
        active_turn_start_index: None,
    }
}

fn fork_history_from_snapshot(
    snapshot: ForkSnapshot,
    history: InitialHistory,
    interrupted_marker: InterruptedTurnHistoryMarker,
) -> InitialHistory {
    let snapshot_state = snapshot_turn_state(&history);
    match snapshot {
        ForkSnapshot::TruncateBeforeNthUserMessage(nth_user_message) => {
            truncate_before_nth_user_message(history, nth_user_message, &snapshot_state)
        }
        ForkSnapshot::Interrupted => {
            let history = match history {
                InitialHistory::New => InitialHistory::New,
                InitialHistory::Cleared => InitialHistory::Cleared,
                InitialHistory::Forked(history) => InitialHistory::Forked(history),
                InitialHistory::Resumed(resumed) => {
                    InitialHistory::Forked(Arc::unwrap_or_clone(resumed.history))
                }
            };
            if snapshot_state.ends_mid_turn {
                append_interrupted_boundary(
                    history,
                    snapshot_state.active_turn_id,
                    snapshot_state.active_turn_started_at,
                    interrupted_marker,
                )
            } else {
                history
            }
        }
    }
}

/// Append the same persisted interrupt boundary used by the live interrupt path
/// to an existing fork snapshot after the source thread has been confirmed to
/// be mid-turn.
fn append_interrupted_boundary(
    history: InitialHistory,
    turn_id: Option<String>,
    started_at: Option<i64>,
    interrupted_marker: InterruptedTurnHistoryMarker,
) -> InitialHistory {
    let aborted_event = RolloutItem::EventMsg(EventMsg::TurnAborted(TurnAbortedEvent {
        turn_id,
        reason: TurnAbortReason::Interrupted,
        started_at,
        completed_at: None,
        duration_ms: None,
    }));

    match history {
        InitialHistory::New | InitialHistory::Cleared => {
            let mut history = Vec::new();
            if let Some(marker) = interrupted_turn_history_marker(interrupted_marker) {
                history.push(RolloutItem::ResponseItem(marker));
            }
            history.push(aborted_event);
            InitialHistory::Forked(history)
        }
        InitialHistory::Forked(mut history) => {
            if let Some(marker) = interrupted_turn_history_marker(interrupted_marker) {
                history.push(RolloutItem::ResponseItem(marker));
            }
            history.push(aborted_event);
            InitialHistory::Forked(history)
        }
        InitialHistory::Resumed(resumed) => {
            let mut history = Arc::unwrap_or_clone(resumed.history);
            if let Some(marker) = interrupted_turn_history_marker(interrupted_marker) {
                history.push(RolloutItem::ResponseItem(marker));
            }
            history.push(aborted_event);
            InitialHistory::Forked(history)
        }
    }
}

#[cfg(test)]
#[path = "thread_manager_tests.rs"]
mod tests;
