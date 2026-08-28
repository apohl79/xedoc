//! Shared tool-runtime contracts independent of the session implementation.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod current_time;
mod get_context_remaining;
mod invocation;
mod list_available_plugins_to_install;
mod new_context_window;
mod plan;
mod request_user_input;
mod router;
mod test_sync;
mod tool_search;

pub use current_time::CurrentTimeHandler;
pub use current_time::CurrentTimeHost;
pub use get_context_remaining::ContextWindowHost;
pub use get_context_remaining::GetContextRemainingHandler;
pub use invocation::AbortedToolOutput;
pub use invocation::AnyToolResult;
pub use invocation::ApplyPatchToolOutput;
pub use invocation::ExecCommandToolOutput;
pub use invocation::FunctionToolOutput;
pub use invocation::McpToolOutput;
pub use invocation::SharedTurnDiffTracker;
pub use invocation::ToolArgumentDiffConsumer;
pub use invocation::ToolCallSource;
pub use invocation::ToolDispatcher;
pub use invocation::ToolInvocation;
pub use invocation::ToolOutput;
pub use invocation::ToolPayload;
pub use invocation::ToolSearchOutput;
pub use invocation::ToolStepContext;
pub use invocation::boxed_tool_output;
pub use list_available_plugins_to_install::ListAvailablePluginsToInstallHandler;
pub use new_context_window::NEW_CONTEXT_WINDOW_MESSAGE;
pub use new_context_window::NewContextWindowHandler;
pub use new_context_window::NewContextWindowHost;
pub use plan::PlanHandler;
pub use plan::PlanHost;
pub use request_user_input::RequestUserInputHandler;
pub use request_user_input::RequestUserInputHost;
pub use router::ToolCall;
pub use router::ToolRouter;
pub use test_sync::TestSyncHandler;
pub use tool_search::ToolSearchHandler;
pub use tool_search::ToolSearchHandlerCache;
