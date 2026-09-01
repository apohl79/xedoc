//! Process transport primitives shared by unified-exec orchestration.

mod errors;
mod head_tail_buffer;
mod process;
mod process_state;

pub use errors::UnifiedExecError;
pub use head_tail_buffer::HeadTailBuffer;
pub use process::NoopSpawnLifecycle;
pub use process::OutputBuffer;
pub use process::OutputHandles;
pub use process::SpawnLifecycle;
pub use process::SpawnLifecycleHandle;
pub use process::UnifiedExecProcess;

/// Maximum number of bytes retained from a unified-exec process's output.
pub const UNIFIED_EXEC_OUTPUT_MAX_BYTES: usize = 1024 * 1024;
const UNIFIED_EXEC_OUTPUT_MAX_TOKENS: usize = UNIFIED_EXEC_OUTPUT_MAX_BYTES / 4;

/// Formats the marker inserted between retained output head and tail segments.
pub fn format_output_omission_marker(omitted_bytes: usize) -> String {
    format!("... {omitted_bytes} bytes omitted ...")
}
