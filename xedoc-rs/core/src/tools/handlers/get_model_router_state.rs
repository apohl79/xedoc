//! Read-only model-router state inspection tool.

use super::*;
use crate::model_router::current_script_route;
use crate::model_router_script_host::ModelRouterScriptHost;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::registry::CoreToolRuntime;
use serde::Serialize;
use serde_json::json;
use xedoc_script_protocol::Route;
use xedoc_script_protocol::RouterState;
use xedoc_tools::JsonSchema;
use xedoc_tools::ResponsesApiTool;
use xedoc_tools::ToolExecutor;
use xedoc_tools::ToolName;
use xedoc_tools::ToolSpec;

const TOOL_NAME: &str = "get_model_router_state";

/// Reads the configured and session-scoped model-router state.
pub struct Handler;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Result {
    scope: &'static str,
    current_route: Option<Route>,
    session_mode: Option<String>,
    policy: RouterState,
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: TOOL_NAME.to_string(),
            description: "Read the current session's model-router state. This tool is read-only."
                .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                Default::default(),
                Some(Vec::new()),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle(&self, invocation: ToolInvocation) -> xedoc_tools::ToolExecutorFuture<'_> {
        Box::pin(async move {
            let ToolInvocation {
                session,
                turn,
                cancellation_token,
                ..
            } = invocation;
            let session_mode = session.model_router_session_mode().await;
            let current_route = current_script_route(&turn.config);
            let scope = if turn.session_source.is_non_root_agent() {
                "subagent"
            } else {
                "root"
            };
            let context = json!({
                "turn": {"scope": scope},
                "currentRoute": &current_route,
                "session": {"routerMode": &session_mode},
            });
            let host = ModelRouterScriptHost::from_config(&turn.config).ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "model-router state is unavailable because no router script is configured"
                        .to_string(),
                )
            })?;
            let policy = host
                .state(context, cancellation_token.child_token())
                .await
                .map_err(|error| {
                    FunctionCallError::RespondToModel(format!(
                        "model-router state is unavailable: {}",
                        error.diagnostic()
                    ))
                })?;
            let result = Result {
                scope,
                current_route,
                session_mode,
                policy,
            };
            serde_json::to_string(&result)
                .map(|result| boxed_tool_output(FunctionToolOutput::from_text(result, Some(true))))
                .map_err(|error| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to serialize {TOOL_NAME} result: {error}"
                    ))
                })
        })
    }
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}
