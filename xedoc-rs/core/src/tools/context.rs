use crate::session::session::Session;
use crate::session::step_context::StepContext;

pub(crate) type ToolInvocation = xedoc_core_tool_runtime::ToolInvocation<Session, StepContext>;

impl xedoc_core_tool_runtime::ToolStepContext for StepContext {
    fn turn_context(&self) -> std::sync::Arc<xedoc_core_turn_context::TurnContext> {
        std::sync::Arc::clone(&self.turn)
    }
}

pub use xedoc_core_tool_runtime::AbortedToolOutput;
pub use xedoc_core_tool_runtime::ApplyPatchToolOutput;
pub use xedoc_core_tool_runtime::ExecCommandToolOutput;
pub use xedoc_core_tool_runtime::FunctionToolOutput;
pub use xedoc_core_tool_runtime::McpToolOutput;
pub(crate) use xedoc_core_tool_runtime::SharedTurnDiffTracker;
pub use xedoc_core_tool_runtime::ToolCallSource;
pub use xedoc_core_tool_runtime::ToolOutput;
pub use xedoc_core_tool_runtime::ToolPayload;
pub use xedoc_core_tool_runtime::boxed_tool_output;
