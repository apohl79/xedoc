use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::registry::CoreToolRuntime;
use futures::future::BoxFuture;

pub use codex_core_tool_runtime::GetContextRemainingHandler;

impl codex_core_tool_runtime::ContextWindowHost for Session {
    fn context_window_tokens<'a>(&'a self, turn: &'a TurnContext) -> BoxFuture<'a, Option<i64>> {
        Box::pin(async move {
            crate::session::context_window::context_window_token_status(self, turn)
                .await
                .base_window_tokens_remaining
        })
    }
}

impl CoreToolRuntime for GetContextRemainingHandler {}
