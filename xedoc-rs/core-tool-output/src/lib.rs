//! Model-facing tool output serialization and truncation.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod mcp_result;
mod output;

#[doc(hidden)]
pub use mcp_result::sanitize_mcp_tool_result_for_model;
#[doc(hidden)]
pub use mcp_result::truncate_mcp_tool_result_for_event;
#[doc(hidden)]
pub use output::AbortedToolOutput;
#[doc(hidden)]
pub use output::ApplyPatchToolOutput;
#[doc(hidden)]
pub use output::ExecCommandToolOutput;
#[doc(hidden)]
pub use output::FunctionToolOutput;
#[doc(hidden)]
pub use output::McpToolOutput;
#[doc(hidden)]
pub use output::ToolCallSource;
pub use output::ToolOutput;
pub use output::ToolPayload;
#[doc(hidden)]
pub use output::ToolSearchOutput;
#[doc(hidden)]
pub use output::boxed_tool_output;
