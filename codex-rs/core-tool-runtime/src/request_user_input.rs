//! Interactive user-input tool execution behind a session host boundary.

use codex_core_tool_specs::request_user_input_spec::REQUEST_USER_INPUT_TOOL_NAME;
use codex_core_tool_specs::request_user_input_spec::create_request_user_input_tool;
use codex_core_tool_specs::request_user_input_spec::normalize_request_user_input_args;
use codex_core_tool_specs::request_user_input_spec::request_user_input_tool_description;
use codex_core_tool_specs::request_user_input_spec::request_user_input_unavailable_message;
use codex_core_turn_context::TurnContext;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::request_user_input::RequestUserInputArgs;
use codex_protocol::request_user_input::RequestUserInputResponse;
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

/// Coordinates an interactive user-input request for the active session.
pub trait RequestUserInputHost: Send + Sync {
    /// Returns the active collaboration mode.
    fn collaboration_mode(&self) -> BoxFuture<'_, CollaborationMode>;

    /// Sends a user-input request and returns its response when available.
    fn request_user_input<'a>(
        &'a self,
        turn: &'a TurnContext,
        call_id: String,
        args: RequestUserInputArgs,
    ) -> BoxFuture<'a, Option<RequestUserInputResponse>>;
}

/// Handles interactive user-input requests.
pub struct RequestUserInputHandler {
    /// Collaboration modes where this tool is available.
    pub available_modes: Vec<ModeKind>,
}

impl<S, C> ToolExecutor<ToolInvocation<S, C>> for RequestUserInputHandler
where
    S: RequestUserInputHost + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    fn tool_name(&self) -> ToolName {
        ToolName::plain(REQUEST_USER_INPUT_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_request_user_input_tool(request_user_input_tool_description(&self.available_modes))
    }

    fn handle(&self, invocation: ToolInvocation<S, C>) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(handle_call(invocation, &self.available_modes))
    }
}

async fn handle_call<S, C>(
    invocation: ToolInvocation<S, C>,
    available_modes: &[ModeKind],
) -> Result<Box<dyn ToolOutput>, FunctionCallError>
where
    S: RequestUserInputHost + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    let ToolInvocation {
        session,
        turn,
        call_id,
        payload,
        ..
    } = invocation;

    let ToolPayload::Function { arguments } = payload else {
        return Err(FunctionCallError::RespondToModel(format!(
            "{REQUEST_USER_INPUT_TOOL_NAME} handler received unsupported payload"
        )));
    };

    if turn.session_source.is_non_root_agent() {
        return Err(FunctionCallError::RespondToModel(
            "request_user_input can only be used by the root thread".to_string(),
        ));
    }

    let mode = session.collaboration_mode().await.mode;
    if let Some(message) = request_user_input_unavailable_message(mode, available_modes) {
        return Err(FunctionCallError::RespondToModel(message));
    }

    let args = serde_json::from_str(&arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
    })?;
    let args =
        normalize_request_user_input_args(args).map_err(FunctionCallError::RespondToModel)?;
    let response = session
        .request_user_input(turn.as_ref(), call_id, args)
        .await
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(format!(
                "{REQUEST_USER_INPUT_TOOL_NAME} was cancelled before receiving a response"
            ))
        })?;

    let content = serde_json::to_string(&response).map_err(|err| {
        FunctionCallError::Fatal(format!(
            "failed to serialize {REQUEST_USER_INPUT_TOOL_NAME} response: {err}"
        ))
    })?;

    Ok(boxed_tool_output(FunctionToolOutput::from_text(
        content,
        Some(true),
    )))
}
