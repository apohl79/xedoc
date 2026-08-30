use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::registry::CoreToolRuntime;
use futures::future::BoxFuture;
use xedoc_protocol::config_types::CollaborationMode;
use xedoc_protocol::request_user_input::RequestUserInputArgs;
use xedoc_protocol::request_user_input::RequestUserInputResponse;

pub use xedoc_core_tool_runtime::RequestUserInputHandler;
#[cfg(test)]
pub(crate) use xedoc_core_tool_specs::request_user_input_spec::REQUEST_USER_INPUT_TOOL_NAME;

impl xedoc_core_tool_runtime::RequestUserInputHost for Session {
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
