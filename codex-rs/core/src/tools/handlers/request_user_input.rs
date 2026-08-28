use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::registry::CoreToolRuntime;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::request_user_input::RequestUserInputArgs;
use codex_protocol::request_user_input::RequestUserInputResponse;
use futures::future::BoxFuture;

pub use codex_core_tool_runtime::RequestUserInputHandler;
#[cfg(test)]
pub(crate) use codex_core_tool_specs::request_user_input_spec::REQUEST_USER_INPUT_TOOL_NAME;

impl codex_core_tool_runtime::RequestUserInputHost for Session {
    fn collaboration_mode(&self) -> BoxFuture<'_, CollaborationMode> {
        Box::pin(Session::collaboration_mode(self))
    }

    fn request_user_input<'a>(
        &'a self,
        turn: &'a TurnContext,
        call_id: String,
        args: RequestUserInputArgs,
    ) -> BoxFuture<'a, Option<RequestUserInputResponse>> {
        Box::pin(Session::request_user_input(self, turn, call_id, args))
    }
}

impl CoreToolRuntime for RequestUserInputHandler {}

#[cfg(test)]
#[path = "request_user_input_tests.rs"]
mod tests;
