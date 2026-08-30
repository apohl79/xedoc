//! Context-window budget reporting behind a session host boundary.

use codex_core_tool_specs::get_context_remaining_spec::GET_CONTEXT_REMAINING_TOOL_NAME;
use codex_core_tool_specs::get_context_remaining_spec::create_get_context_remaining_tool;
use codex_core_turn_context::TurnContext;
use codex_protocol::models::ResponseInputItem;
use codex_tools::FunctionCallError;
use codex_tools::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use futures::future::BoxFuture;

use crate::FunctionToolOutput;
use crate::ToolInvocation;
use crate::ToolOutput;
use crate::ToolPayload;
use crate::boxed_tool_output;

/// Supplies the remaining context-window budget for a turn.
pub trait ContextWindowHost: Send + Sync {
    /// Returns the base-window token count remaining for the active turn.
    fn context_window_tokens<'a>(&'a self, turn: &'a TurnContext) -> BoxFuture<'a, Option<i64>>;
}

#[derive(Debug, Clone)]
struct GetContextRemainingOutput {
    tokens_left: Option<i64>,
}

impl GetContextRemainingOutput {
    fn new(tokens_left: Option<i64>) -> Self {
        Self { tokens_left }
    }

    fn fragment(&self) -> String {
        match self.tokens_left {
            Some(tokens_left) => {
                format!("You have {tokens_left} tokens left in this context window.")
            }
            None => "You have unknown tokens left in this context window.".to_string(),
        }
    }
}

impl ToolOutput for GetContextRemainingOutput {
    fn log_preview(&self) -> String {
        self.fragment()
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        FunctionToolOutput::from_text(self.fragment(), Some(true))
            .to_response_item(call_id, payload)
    }
}

/// Handles context-window budget queries through the session host.
pub struct GetContextRemainingHandler;

impl<S, C> ToolExecutor<ToolInvocation<S, C>> for GetContextRemainingHandler
where
    S: ContextWindowHost + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    fn tool_name(&self) -> ToolName {
        ToolName::plain(GET_CONTEXT_REMAINING_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_get_context_remaining_tool()
    }

    fn handle(&self, invocation: ToolInvocation<S, C>) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move {
            if !matches!(invocation.payload, ToolPayload::Function { .. }) {
                return Err(FunctionCallError::RespondToModel(
                    "get_context_remaining handler received unsupported payload".to_string(),
                ));
            }

            let tokens_left = invocation
                .session
                .context_window_tokens(invocation.turn.as_ref())
                .await;
            Ok(boxed_tool_output(GetContextRemainingOutput::new(
                tokens_left,
            )))
        })
    }
}
