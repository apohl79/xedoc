//! New-context-window tool execution behind a session host boundary.

use futures::future::BoxFuture;
use xedoc_core_tool_specs::new_context_window_spec::NEW_CONTEXT_WINDOW_TOOL_NAME;
use xedoc_core_tool_specs::new_context_window_spec::create_new_context_window_tool;
use xedoc_tools::FunctionCallError;
use xedoc_tools::ToolExecutor;
use xedoc_tools::ToolName;
use xedoc_tools::ToolSpec;

use crate::FunctionToolOutput;
use crate::ToolInvocation;
use crate::ToolPayload;
use crate::boxed_tool_output;

/// Requests an immediate new context window from the session host.
pub trait NewContextWindowHost: Send + Sync {
    /// Records that the active session should start a new context window.
    fn request_new_context_window(&self) -> BoxFuture<'_, ()>;
}

/// Model-visible confirmation for an explicit context-window reset.
pub const NEW_CONTEXT_WINDOW_MESSAGE: &str =
    "A new context window will start without summarizing conversation history.";

/// Handles explicit new-context-window requests.
pub struct NewContextWindowHandler;

impl<S, C> ToolExecutor<ToolInvocation<S, C>> for NewContextWindowHandler
where
    S: NewContextWindowHost + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    fn tool_name(&self) -> ToolName {
        ToolName::plain(NEW_CONTEXT_WINDOW_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_new_context_window_tool()
    }

    fn handle(&self, invocation: ToolInvocation<S, C>) -> xedoc_tools::ToolExecutorFuture<'_> {
        Box::pin(async move {
            if !matches!(invocation.payload, ToolPayload::Function { .. }) {
                return Err(FunctionCallError::RespondToModel(
                    "new_context handler received unsupported payload".to_string(),
                ));
            }

            invocation.session.request_new_context_window().await;

            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                NEW_CONTEXT_WINDOW_MESSAGE.to_string(),
                Some(true),
            )))
        })
    }
}
