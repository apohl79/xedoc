#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use codex_exec_server::EnvironmentManager;
#[cfg(test)]
use codex_features::Feature;
#[cfg(test)]
use codex_mcp::McpConfig;
#[cfg(test)]
use codex_mcp::McpConnectionManager;
#[cfg(test)]
use codex_mcp::McpRuntimeContext;
#[cfg(test)]
use codex_mcp::ResolvedMcpCatalog;
#[cfg(test)]
use rmcp::model::ElicitationCapability;

pub use codex_core_mcp_runtime::McpRuntimeSnapshot;

#[cfg(test)]
pub(crate) fn new_uninitialized_mcp_runtime_snapshot_for_test(
    config: &crate::config::Config,
) -> Arc<McpRuntimeSnapshot> {
    let mcp_config = McpConfig {
        codex_home: config.codex_home.to_path_buf(),
        mcp_oauth_credentials_store_mode: config.mcp_oauth_credentials_store_mode,
        auth_keyring_backend_kind: config.auth_keyring_backend_kind(),
        mcp_oauth_callback_port: config.mcp_oauth_callback_port,
        mcp_oauth_callback_url: config.mcp_oauth_callback_url.clone(),
        skill_mcp_dependency_install_enabled: config
            .features
            .enabled(Feature::SkillMcpDependencyInstall),
        approval_policy: config.permissions.approval_policy.clone(),
        codex_linux_sandbox_exe: config.codex_linux_sandbox_exe.clone(),
        use_legacy_landlock: config.features.use_legacy_landlock(),
        openai_developer_docs_enabled: config.features.enabled(Feature::OpenaiDeveloperDocs),
        prefix_mcp_tool_names: config.prefix_mcp_tool_names(),
        client_elicitation_capability: ElicitationCapability::default(),
        mcp_server_catalog: ResolvedMcpCatalog::default(),
    };
    let manager = McpConnectionManager::new_uninitialized_with_permission_profile(
        &config.permissions.approval_policy,
        config.permissions.permission_profile(),
        config.prefix_mcp_tool_names(),
    );
    let runtime_context = McpRuntimeContext::new(
        Arc::new(EnvironmentManager::default_for_tests()),
        config.cwd.to_path_buf(),
    );
    Arc::new(McpRuntimeSnapshot::new(
        Arc::new(mcp_config),
        /*plugins_available*/ false,
        Arc::new(manager),
        runtime_context,
        Vec::new(),
    ))
}
