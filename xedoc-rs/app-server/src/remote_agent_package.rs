//! Installed remote-agent package boundary.

#[cfg(not(windows))]
#[path = "remote_agent_package_unix.rs"]
mod implementation;

#[cfg(not(windows))]
pub(crate) use implementation::RemoteAgentPackage;
#[cfg(not(windows))]
pub(crate) use implementation::installed_remote_agent_package;

#[cfg(windows)]
use std::ffi::OsString;
#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use serde_json::Value as JsonValue;

#[cfg(windows)]
pub(crate) struct RemoteAgentPackage {
    pub(crate) payload_version: String,
    pub(crate) payload_hash: String,
    pub(crate) tools_schema_version: u64,
    pub(crate) tools_hash: String,
    pub(crate) package_version: String,
    pub(crate) runtime_hash: String,
    pub(crate) tools: Arc<[RemoteAgentTool]>,
}

#[cfg(windows)]
#[derive(Clone)]
pub(crate) struct RemoteAgentTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: JsonValue,
}

#[cfg(windows)]
impl RemoteAgentPackage {
    pub(crate) fn command_argv(&self) -> Vec<OsString> {
        Vec::new()
    }
}

#[cfg(windows)]
pub(crate) fn installed_remote_agent_package() -> Result<Arc<RemoteAgentPackage>, String> {
    Err("built-in remote-agent packages are unsupported on Windows".to_string())
}
