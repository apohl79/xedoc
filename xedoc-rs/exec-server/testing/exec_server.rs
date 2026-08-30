//! Minimal exec-server fixture for Bazel-only integration tests.
//!
//! Linking only exec-server avoids depending on the full Xedoc CLI binary
//! when a test only needs a WebSocket executor endpoint.

use xedoc_exec_server::ExecServerRuntimePaths;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let current_exe = std::env::current_exe()?;
    let runtime_paths =
        ExecServerRuntimePaths::new(current_exe, /*xedoc_linux_sandbox_exe*/ None)?;
    xedoc_exec_server::run_main("ws://127.0.0.1:0", runtime_paths).await
}
