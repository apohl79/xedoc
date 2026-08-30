//! Current-time tool execution behind a session host boundary.

use codex_protocol::models::ResponseInputItem;
use codex_tools::FunctionCallError;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use futures::future::BoxFuture;
use serde_json::json;
use std::collections::BTreeMap;

use crate::FunctionToolOutput;
use crate::ToolInvocation;
use crate::ToolOutput;
use crate::ToolPayload;
use crate::boxed_tool_output;

const NAMESPACE: &str = "clock";
const TOOL_NAME: &str = "curr_time";

/// Supplies the configured current time for a tool invocation.
pub trait CurrentTimeHost: Send + Sync {
    /// Returns the current time from the host's configured provider.
    fn current_time(&self) -> BoxFuture<'_, Result<String, String>>;
}

struct CurrentTimeOutput(String);

impl ToolOutput for CurrentTimeOutput {
    fn log_preview(&self) -> String {
        format!("It is {}.", self.0)
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        FunctionToolOutput::from_text(format!("It is {}.", self.0), Some(true))
            .to_response_item(call_id, payload)
    }
}

/// Handles the current-time tool through a host-provided clock.
pub struct CurrentTimeHandler;

impl<S, C> ToolExecutor<ToolInvocation<S, C>> for CurrentTimeHandler
where
    S: CurrentTimeHost + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(NAMESPACE, TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Namespace(ResponsesApiNamespace {
            name: NAMESPACE.to_string(),
            description: "Tools for reading and waiting on time.".to_string(),
            tools: vec![ResponsesApiNamespaceTool::Function(ResponsesApiTool {
                name: TOOL_NAME.to_string(),
                description: "Return the current time in UTC.".to_string(),
                strict: false,
                defer_loading: None,
                parameters: JsonSchema::object(
                    BTreeMap::new(),
                    /*required*/ None,
                    /*additional_properties*/ Some(false.into()),
                ),
                output_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "current_time": {
                            "type": "string",
                            "description": "Current UTC time formatted as YYYY-MM-DD HH:MM:SS UTC."
                        }
                    },
                    "required": ["current_time"],
                    "additionalProperties": false
                })),
            })],
        })
    }

    fn handle(&self, invocation: ToolInvocation<S, C>) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move {
            if !matches!(invocation.payload, ToolPayload::Function { .. }) {
                return Err(FunctionCallError::RespondToModel(format!(
                    "{TOOL_NAME} handler received unsupported payload"
                )));
            }

            let current_time = invocation.session.current_time().await.map_err(|err| {
                FunctionCallError::Fatal(format!("failed to read current time: {err}"))
            })?;
            Ok(boxed_tool_output(CurrentTimeOutput(current_time)))
        })
    }
}
