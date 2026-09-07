use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use crate::SkillsService;
use crate::agent::AgentControl;
use crate::agents_md_manager::AgentsMdManager;
use crate::client::ModelClient;
use crate::config::NetworkProxyAuditMetadata;
use crate::config::StartedNetworkProxy;
use crate::current_time::TimeProvider;
use crate::elicitation::ElicitationService;
use crate::environment_selection::ThreadEnvironments;
use crate::exec_policy::ExecPolicyManager;
use crate::mcp::McpManager;
use crate::session::McpRuntimeSnapshot;
use crate::tools::handlers::ToolSearchHandlerCache;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::sandboxing::ApprovalStore;
use crate::unified_exec::UnifiedExecProcessManager;
use anyhow::Result;
use arc_swap::ArcSwap;
use arc_swap::ArcSwapOption;
use std::path::PathBuf;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use xedoc_core_plugins::PluginsManager;
use xedoc_extension_api::ExtensionData;
use xedoc_extension_api::ExtensionDataInit;
use xedoc_extension_api::ExtensionRegistry;
use xedoc_hooks::Hooks;
use xedoc_login::AuthManager;
use xedoc_mcp::McpConfig;
use xedoc_mcp::McpConnectionManager;
use xedoc_mcp::McpRuntime;
use xedoc_mcp::McpRuntimeContext;
use xedoc_models_manager::manager::SharedModelsManager;
use xedoc_otel::SessionTelemetry;
use xedoc_protocol::capabilities::SelectedCapabilityRoot;
use xedoc_rollout::state_db::StateDbHandle;
use xedoc_thread_store::LiveThread;
use xedoc_thread_store::ThreadStore;
use xedoc_tool_output_reduce::ReductionRecord;
use xedoc_tool_output_reduce::ReductionSessionStats;
use xedoc_tool_output_reduce::ReductionSink;

pub(crate) struct SessionReductionSink {
    inner: Arc<dyn ReductionSink>,
    reductions: AtomicI64,
    tokens_saved: AtomicI64,
    cost_saved_bits: AtomicU64,
}

impl SessionReductionSink {
    pub(crate) fn new(inner: Arc<dyn ReductionSink>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            reductions: AtomicI64::new(0),
            tokens_saved: AtomicI64::new(0),
            cost_saved_bits: AtomicU64::new(0.0f64.to_bits()),
        })
    }
}

impl ReductionSink for SessionReductionSink {
    fn try_record(&self, record: ReductionRecord) {
        let tokens_saved = (record.est_tokens_in - record.est_tokens_out).max(0);
        self.reductions.fetch_add(1, Ordering::Relaxed);
        self.tokens_saved.fetch_add(tokens_saved, Ordering::Relaxed);
        if let Some(price) = record.input_price_per_1m
            && price.is_finite()
            && price >= 0.0
        {
            let cost = tokens_saved as f64 * price / 1_000_000.0;
            self.cost_saved_bits
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
                    Some((f64::from_bits(bits) + cost).to_bits())
                })
                .ok();
        }
        self.inner.try_record(record);
    }

    fn try_record_retrieval(&self, call_id: &str, spill_path: &str) {
        self.inner.try_record_retrieval(call_id, spill_path);
    }

    fn session_stats(&self) -> ReductionSessionStats {
        ReductionSessionStats {
            reductions: self.reductions.load(Ordering::Relaxed),
            tokens_saved: self.tokens_saved.load(Ordering::Relaxed),
            cost_saved_usd: f64::from_bits(self.cost_saved_bits.load(Ordering::Relaxed)),
        }
    }
}

pub(crate) struct SessionServices {
    /// Optional runtime-owned sink for non-blocking tool-output reduction records.
    ///
    /// Hosts that own a `StateRuntime` install its bounded sink here; core keeps
    /// the dependency trait-only so daemon and embedded sessions can opt in
    /// without introducing process-global metrics state.
    pub(crate) reduction_sink: Option<Arc<dyn ReductionSink>>,
    /// The single owner of live MCP connections for this thread.
    pub(crate) mcp_runtime: Arc<McpRuntime>,
    /// The latest atomically published MCP config and connection snapshot.
    pub(crate) mcp_runtime_snapshot: ArcSwapOption<McpRuntimeSnapshot>,
    /// Serializes environment-driven runtime rebuilds.
    pub(crate) mcp_projection_lock: Mutex<()>,
    pub(crate) mcp_startup_cancellation_token: Mutex<CancellationToken>,
    pub(crate) unified_exec_manager: UnifiedExecProcessManager,
    pub(crate) elicitations: ElicitationService,
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) shell_zsh_path: Option<PathBuf>,
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) main_execve_wrapper_exe: Option<PathBuf>,
    pub(crate) hooks: ArcSwap<Hooks>,
    pub(crate) user_shell: Arc<crate::shell::Shell>,
    pub(crate) show_raw_agent_reasoning: bool,
    pub(crate) exec_policy: Arc<ExecPolicyManager>,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) models_manager: SharedModelsManager,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) tool_approvals: Mutex<ApprovalStore>,
    pub(crate) skills_service: Arc<SkillsService>,
    pub(crate) agents_md_manager: Arc<AgentsMdManager>,
    pub(crate) plugins_manager: Arc<PluginsManager>,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) extensions: Arc<ExtensionRegistry<crate::config::Config>>,
    pub(crate) session_extension_data: ExtensionData,
    pub(crate) thread_extension_data: ExtensionData,
    pub(crate) supports_openai_form_elicitation: AtomicBool,
    /// Raw capability selections for this thread. Each model step resolves them against its
    /// current executor environments before using them.
    pub(crate) selected_capability_roots: Vec<SelectedCapabilityRoot>,
    pub(crate) mcp_thread_init: ExtensionDataInit,
    pub(crate) agent_control: AgentControl,
    pub(crate) network_proxy: ArcSwapOption<StartedNetworkProxy>,
    pub(crate) network_proxy_audit_metadata: NetworkProxyAuditMetadata,
    pub(crate) managed_network_requirements_configured: bool,
    pub(crate) network_approval: Arc<NetworkApprovalService>,
    pub(crate) state_db: Option<StateDbHandle>,
    pub(crate) live_thread: Option<LiveThread>,
    pub(crate) thread_store: Arc<dyn ThreadStore>,
    pub(crate) time_provider: Arc<dyn TimeProvider>,
    /// Session-scoped model client shared across turns.
    pub(crate) model_client: ArcSwap<ModelClient>,
    pub(crate) tool_search_handler_cache: ToolSearchHandlerCache,
    pub(crate) turn_environments: Arc<ThreadEnvironments>,
}

impl SessionServices {
    /// Publishes the initial connections before validating required servers so startup-time
    /// elicitation can resolve through the thread runtime while validation waits.
    pub(crate) async fn install_mcp_runtime(
        &self,
        config: Arc<McpConfig>,
        plugins_available: bool,
        runtime_context: McpRuntimeContext,
        ready_selected_capability_roots: Vec<SelectedCapabilityRoot>,
        connections: McpConnectionManager,
    ) -> Result<()> {
        let runtime = self.publish_mcp_runtime(
            config,
            plugins_available,
            runtime_context,
            ready_selected_capability_roots,
            connections,
        );
        runtime.manager().validate_required_servers().await
    }

    pub(crate) fn publish_mcp_runtime(
        &self,
        config: Arc<McpConfig>,
        plugins_available: bool,
        runtime_context: McpRuntimeContext,
        ready_selected_capability_roots: Vec<SelectedCapabilityRoot>,
        connections: McpConnectionManager,
    ) -> Arc<McpRuntimeSnapshot> {
        let connections = self.mcp_runtime.replace(connections);
        let runtime = Arc::new(McpRuntimeSnapshot::new(
            config,
            plugins_available,
            connections,
            runtime_context,
            ready_selected_capability_roots,
        ));
        self.mcp_runtime_snapshot.store(Some(Arc::clone(&runtime)));
        runtime
    }

    pub(crate) fn latest_mcp_runtime(&self) -> Arc<McpRuntimeSnapshot> {
        let Some(runtime) = self.mcp_runtime_snapshot.load_full() else {
            unreachable!("MCP runtime must be installed before handling requests");
        };
        runtime
    }
}
