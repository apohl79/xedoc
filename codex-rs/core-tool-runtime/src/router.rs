//! Generic model-tool routing independent of the session implementation.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use codex_protocol::models::ResponseItem;
use codex_protocol::models::SearchToolCallParams;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use tokio_util::sync::CancellationToken;

use crate::AnyToolResult;
use crate::SharedTurnDiffTracker;
use crate::ToolArgumentDiffConsumer;
use crate::ToolCallSource;
use crate::ToolDispatcher;
use crate::ToolInvocation;
use crate::ToolPayload;
use crate::ToolStepContext;

/// A model-visible tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// Fully-qualified tool name.
    pub tool_name: ToolName,
    /// Provider-visible call identifier.
    pub call_id: String,
    /// Model-provided tool input.
    pub payload: ToolPayload,
}

/// Routes model-visible tool calls through one host registry.
pub struct ToolRouter<S, C, D> {
    registry: D,
    model_visible_specs: Vec<ToolSpec>,
    session_marker: std::marker::PhantomData<fn(S, C)>,
}

impl<S, C, D> ToolRouter<S, C, D> {
    /// Builds a router from a registry and model-visible tool specifications.
    pub fn from_parts(registry: D, model_visible_specs: Vec<ToolSpec>) -> Self {
        Self {
            registry,
            model_visible_specs,
            session_marker: std::marker::PhantomData,
        }
    }

    /// Returns a copy of the tool specifications shown to the model.
    pub fn model_visible_specs(&self) -> Vec<ToolSpec> {
        self.model_visible_specs.clone()
    }

    /// Parses one response item into a local tool call, if applicable.
    #[tracing::instrument(level = "trace", skip_all, err)]
    pub fn build_tool_call(
        item: ResponseItem,
    ) -> Result<Option<ToolCall>, codex_tools::FunctionCallError> {
        match item {
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => Ok(Some(ToolCall {
                tool_name: ToolName::new(namespace, name),
                call_id,
                payload: ToolPayload::Function { arguments },
            })),
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id),
                execution,
                arguments,
                ..
            } if execution == "client" => {
                let arguments: SearchToolCallParams =
                    serde_json::from_value(arguments).map_err(|err| {
                        codex_tools::FunctionCallError::RespondToModel(format!(
                            "failed to parse tool_search arguments: {err}"
                        ))
                    })?;
                Ok(Some(ToolCall {
                    tool_name: ToolName::plain("tool_search"),
                    call_id,
                    payload: ToolPayload::ToolSearch { arguments },
                }))
            }
            ResponseItem::ToolSearchCall { .. } => Ok(None),
            ResponseItem::CustomToolCall {
                name,
                namespace,
                input,
                call_id,
                ..
            } => Ok(Some(ToolCall {
                tool_name: ToolName::new(namespace, name),
                call_id,
                payload: ToolPayload::Custom { input },
            })),
            _ => Ok(None),
        }
    }
}

impl<S, C, D> ToolRouter<S, C, D>
where
    C: ToolStepContext,
    D: ToolDispatcher<S, C>,
{
    /// Returns whether one tool supports parallel execution.
    pub fn tool_supports_parallel(&self, call: &ToolCall) -> bool {
        self.registry
            .supports_parallel_tool_calls(&call.tool_name)
            .unwrap_or(false)
    }

    /// Returns whether cancellation waits for this tool's runtime teardown.
    pub fn tool_waits_for_runtime_cancellation(&self, call: &ToolCall) -> bool {
        self.registry
            .waits_for_runtime_cancellation(&call.tool_name)
            .unwrap_or(false)
    }

    /// Returns a streamed-argument diff consumer for a tool, when supported.
    pub fn create_diff_consumer(
        &self,
        tool_name: &ToolName,
    ) -> Option<Box<dyn ToolArgumentDiffConsumer>> {
        self.registry.create_diff_consumer(tool_name)
    }

    /// Returns registered tool names for test-only inspection.
    pub fn registered_tool_names_for_test(&self) -> Vec<ToolName> {
        self.registry.tool_names_for_test()
    }

    /// Returns a tool's visibility for test-only inspection.
    pub fn tool_exposure_for_test(&self, tool_name: &ToolName) -> Option<ToolExposure> {
        self.registry.tool_exposure_for_test(tool_name)
    }

    /// Dispatches a tool call without terminal-outcome coordination.
    #[tracing::instrument(level = "trace", skip_all, err)]
    pub async fn dispatch_tool_call_with_code_mode_result(
        &self,
        session: Arc<S>,
        step_context: Arc<C>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
        source: ToolCallSource,
    ) -> Result<AnyToolResult<D::PostToolUsePayload>, codex_tools::FunctionCallError> {
        self.dispatch_tool_call_with_code_mode_result_inner(
            session,
            step_context,
            cancellation_token,
            tracker,
            call,
            source,
            None,
        )
        .await
    }

    /// Dispatches a tool call with shared terminal-outcome coordination.
    #[tracing::instrument(level = "trace", skip_all, err)]
    #[expect(
        clippy::too_many_arguments,
        reason = "tool dispatch preserves the existing cancellation contract"
    )]
    pub async fn dispatch_tool_call_with_terminal_outcome(
        &self,
        session: Arc<S>,
        step_context: Arc<C>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
        source: ToolCallSource,
        terminal_outcome_reached: Arc<AtomicBool>,
    ) -> Result<AnyToolResult<D::PostToolUsePayload>, codex_tools::FunctionCallError> {
        self.dispatch_tool_call_with_code_mode_result_inner(
            session,
            step_context,
            cancellation_token,
            tracker,
            call,
            source,
            Some(terminal_outcome_reached),
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "tool dispatch preserves the existing cancellation contract"
    )]
    async fn dispatch_tool_call_with_code_mode_result_inner(
        &self,
        session: Arc<S>,
        step_context: Arc<C>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
        source: ToolCallSource,
        terminal_outcome_reached: Option<Arc<AtomicBool>>,
    ) -> Result<AnyToolResult<D::PostToolUsePayload>, codex_tools::FunctionCallError> {
        let ToolCall {
            tool_name,
            call_id,
            payload,
        } = call;
        let turn = step_context.turn_context();
        let invocation = ToolInvocation {
            session,
            turn,
            step_context,
            cancellation_token,
            tracker,
            call_id,
            tool_name,
            source,
            payload,
        };
        self.registry
            .dispatch_any_with_terminal_outcome(invocation, terminal_outcome_reached)
            .await
    }
}
