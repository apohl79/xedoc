use std::fmt;
use std::sync::Arc;

use codex_mcp::McpConfig;
use codex_mcp::McpConnectionManager;
use codex_mcp::McpRuntimeContext;
use codex_protocol::capabilities::SelectedCapabilityRoot;

/// MCP config, plugin availability, exact environment bindings, and manager for one request.
pub struct McpRuntimeSnapshot {
    config: Arc<McpConfig>,
    plugins_available: bool,
    manager: Arc<McpConnectionManager>,
    runtime_context: McpRuntimeContext,
    ready_selected_capability_roots: Vec<SelectedCapabilityRoot>,
}

impl McpRuntimeSnapshot {
    /// Builds a snapshot from the resolved MCP configuration and live manager.
    pub fn new(
        config: Arc<McpConfig>,
        plugins_available: bool,
        manager: Arc<McpConnectionManager>,
        runtime_context: McpRuntimeContext,
        ready_selected_capability_roots: Vec<SelectedCapabilityRoot>,
    ) -> Self {
        Self {
            config,
            plugins_available,
            manager,
            runtime_context,
            ready_selected_capability_roots,
        }
    }

    pub fn config(&self) -> &McpConfig {
        self.config.as_ref()
    }

    /// Reports whether selected plugins contribute capabilities to this snapshot.
    pub fn plugins_available(&self) -> bool {
        self.plugins_available
    }

    pub fn manager(&self) -> &McpConnectionManager {
        self.manager.as_ref()
    }

    /// Clones the snapshot's manager handle.
    pub fn manager_arc(&self) -> Arc<McpConnectionManager> {
        Arc::clone(&self.manager)
    }

    pub fn runtime_context(&self) -> &McpRuntimeContext {
        &self.runtime_context
    }

    /// Returns selected capability roots ready for this snapshot.
    pub fn ready_selected_capability_roots(&self) -> &[SelectedCapabilityRoot] {
        &self.ready_selected_capability_roots
    }
}

impl fmt::Debug for McpRuntimeSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpRuntimeSnapshot")
            .finish_non_exhaustive()
    }
}
