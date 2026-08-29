use crate::session::session::Session;
use crate::session::step_context::StepContext;

pub(crate) type ToolInvocation = codex_core_tool_runtime::ToolInvocation<Session, StepContext>;

impl codex_core_tool_runtime::ToolStepContext for StepContext {
    fn turn_context(&self) -> std::sync::Arc<codex_core_turn_context::TurnContext> {
        std::sync::Arc::clone(&self.turn)
    }
}

pub use codex_core_tool_runtime::AbortedToolOutput;
pub use codex_core_tool_runtime::ApplyPatchToolOutput;
pub use codex_core_tool_runtime::ExecCommandToolOutput;
pub use codex_core_tool_runtime::FunctionToolOutput;
pub use codex_core_tool_runtime::McpToolOutput;
pub(crate) use codex_core_tool_runtime::SharedTurnDiffTracker;
pub use codex_core_tool_runtime::ToolCallSource;
pub use codex_core_tool_runtime::ToolOutput;
pub use codex_core_tool_runtime::ToolPayload;
pub use codex_core_tool_runtime::boxed_tool_output;
