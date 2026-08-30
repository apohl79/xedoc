//! Plan-update tool execution behind a session host boundary.

use futures::future::BoxFuture;
use xedoc_core_tool_specs::plan_spec::create_update_plan_tool;
use xedoc_core_turn_context::TurnContext;
use xedoc_protocol::config_types::ModeKind;
use xedoc_protocol::models::FunctionCallOutputPayload;
use xedoc_protocol::models::ResponseInputItem;
use xedoc_protocol::plan_tool::UpdatePlanArgs;
use xedoc_tools::FunctionCallError;
use xedoc_tools::ToolExecutor;
use xedoc_tools::ToolName;
use xedoc_tools::ToolSpec;

use crate::ToolInvocation;
use crate::ToolOutput;
use crate::ToolPayload;
use crate::boxed_tool_output;

/// Emits a plan update for the active session and turn.
pub trait PlanHost: Send + Sync {
    /// Publishes one plan update to the host event stream.
    fn send_plan_update<'a>(
        &'a self,
        turn: &'a TurnContext,
        args: UpdatePlanArgs,
    ) -> BoxFuture<'a, ()>;
}

/// Handles update-plan requests.
pub struct PlanHandler;

/// A successful update-plan tool response.
struct PlanToolOutput;

const PLAN_UPDATED_MESSAGE: &str = "Plan updated";

impl ToolOutput for PlanToolOutput {
    fn log_preview(&self) -> String {
        PLAN_UPDATED_MESSAGE.to_string()
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        let mut output = FunctionCallOutputPayload::from_text(PLAN_UPDATED_MESSAGE.to_string());
        output.success = Some(true);

        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output,
        }
    }
}

impl<S, C> ToolExecutor<ToolInvocation<S, C>> for PlanHandler
where
    S: PlanHost + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    fn tool_name(&self) -> ToolName {
        ToolName::plain("update_plan")
    }

    fn spec(&self) -> ToolSpec {
        create_update_plan_tool()
    }

    fn handle(&self, invocation: ToolInvocation<S, C>) -> xedoc_tools::ToolExecutorFuture<'_> {
        Box::pin(handle_call(invocation))
    }
}

async fn handle_call<S, C>(
    invocation: ToolInvocation<S, C>,
) -> Result<Box<dyn ToolOutput>, FunctionCallError>
where
    S: PlanHost + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    let ToolInvocation {
        session,
        turn,
        payload,
        ..
    } = invocation;

    let ToolPayload::Function { arguments } = payload else {
        return Err(FunctionCallError::RespondToModel(
            "update_plan handler received unsupported payload".to_string(),
        ));
    };

    if turn.mode == ModeKind::Plan {
        return Err(FunctionCallError::RespondToModel(
            "update_plan is a TODO/checklist tool and is not allowed in Plan mode".to_string(),
        ));
    }

    let args = parse_update_plan_arguments(&arguments)?;
    session.send_plan_update(turn.as_ref(), args).await;

    Ok(boxed_tool_output(PlanToolOutput))
}

fn parse_update_plan_arguments(arguments: &str) -> Result<UpdatePlanArgs, FunctionCallError> {
    serde_json::from_str(arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
    })
}
