use crate::config_manager::ConfigManager;
use std::io;
use std::sync::Arc;
use tracing::warn;
use xedoc_core::ThreadManager;
use xedoc_core::XedocThread;
use xedoc_protocol::ThreadId;
use xedoc_protocol::protocol::McpServerRefreshConfig;
use xedoc_protocol::protocol::Op;

pub(crate) async fn queue_strict_refresh(
    thread_manager: &Arc<ThreadManager>,
    config_manager: &ConfigManager,
) -> io::Result<()> {
    config_manager
        .load_latest_config(/*fallback_cwd*/ None)
        .await?;
    let mut refreshes = Vec::new();
    for thread_id in thread_manager.list_thread_ids().await {
        let thread = thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|err| io::Error::other(format!("failed to load thread {thread_id}: {err}")))?;
        let config = build_refresh_config(thread.as_ref(), config_manager).await?;
        refreshes.push((thread_id, thread, config));
    }
    for (thread_id, thread, config) in refreshes {
        queue_refresh(thread_id, thread, config).await?;
    }
    Ok(())
}

pub(crate) async fn queue_best_effort_refresh(
    thread_manager: &Arc<ThreadManager>,
    config_manager: &ConfigManager,
) {
    for thread_id in thread_manager.list_thread_ids().await {
        let thread = match thread_manager.get_thread(thread_id).await {
            Ok(thread) => thread,
            Err(err) => {
                warn!("failed to load thread {thread_id} for MCP refresh: {err}");
                continue;
            }
        };
        let config = match build_refresh_config(thread.as_ref(), config_manager).await {
            Ok(config) => config,
            Err(err) => {
                warn!("failed to build MCP refresh config for thread {thread_id}: {err}");
                continue;
            }
        };
        if let Err(err) = queue_refresh(thread_id, thread, config).await {
            warn!("{err}");
        }
    }
}

async fn build_refresh_config(
    thread: &XedocThread,
    config_manager: &ConfigManager,
) -> io::Result<McpServerRefreshConfig> {
    let thread_config = thread.config().await;
    let config = config_manager
        .load_latest_config_for_thread(thread_config.as_ref())
        .await?;
    let mcp_config = thread.runtime_mcp_config(&config).await;
    let mcp_servers = xedoc_mcp::configured_mcp_servers(&mcp_config);
    Ok(McpServerRefreshConfig {
        mcp_servers: serde_json::to_value(mcp_servers).map_err(io::Error::other)?,
        mcp_oauth_credentials_store_mode: serde_json::to_value(
            config.mcp_oauth_credentials_store_mode,
        )
        .map_err(io::Error::other)?,
        auth_keyring_backend_kind: serde_json::to_value(config.auth_keyring_backend_kind())
            .map_err(io::Error::other)?,
    })
}

async fn queue_refresh(
    thread_id: ThreadId,
    thread: Arc<XedocThread>,
    config: McpServerRefreshConfig,
) -> io::Result<()> {
    thread
        .submit(Op::RefreshMcpServers { config })
        .await
        .map(|_| ())
        .map_err(|err| {
            io::Error::other(format!(
                "failed to queue MCP refresh for thread {thread_id}: {err}"
            ))
        })
}
