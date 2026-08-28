//! MCP configuration projection and runtime snapshots for Codex threads.

mod mcp_manager;
mod runtime_snapshot;
pub mod tool_call_telemetry;
pub mod tool_item_metadata;
pub mod tool_metadata;
pub mod tool_request_metadata;

pub use mcp_manager::McpManager;
pub use mcp_manager::McpRuntimeProjection;
pub use runtime_snapshot::McpRuntimeSnapshot;
