//! Shared tool invocation state independent of the session implementation.

use std::sync::Arc;

use codex_core_turn_context::TurnContext;
use codex_core_turn_diff::TurnDiffTracker;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::EventMsg;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use futures::future::BoxFuture;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub use codex_core_tool_output::AbortedToolOutput;
pub use codex_core_tool_output::ApplyPatchToolOutput;
pub use codex_core_tool_output::ExecCommandToolOutput;
pub use codex_core_tool_output::FunctionToolOutput;
pub use codex_core_tool_output::McpToolOutput;
pub use codex_core_tool_output::ToolCallSource;
pub use codex_core_tool_output::ToolOutput;
pub use codex_core_tool_output::ToolPayload;
pub use codex_core_tool_output::ToolSearchOutput;
pub use codex_core_tool_output::boxed_tool_output;

/// Shared, mutable diff state for all tool calls in a turn.
pub type SharedTurnDiffTracker = Arc<Mutex<TurnDiffTracker>>;

/// The complete state needed to dispatch one local tool call.
///
/// `S` is the session host and `C` is request-scoped step state. Keeping both
/// generic keeps tool implementations independent from the root core crate.
pub struct ToolInvocation<S, C> {
    /// Session-scoped host state.
    pub session: Arc<S>,
    /// Compatibility turn state for handlers that have not migrated to `step_context`.
    pub turn: Arc<TurnContext>,
    /// Request-scoped state that may change between model sampling requests.
    pub step_context: Arc<C>,
    /// Cancellation propagated from the active turn.
    pub cancellation_token: CancellationToken,
    /// Shared file-diff state for this turn.
    pub tracker: SharedTurnDiffTracker,
    /// Provider-visible tool call identifier.
    pub call_id: String,
    /// Fully-qualified local tool name.
    pub tool_name: ToolName,
    /// The caller that initiated this tool invocation.
    pub source: ToolCallSource,
    /// Payload supplied by the model or code runtime.
    pub payload: ToolPayload,
}

impl<S, C> Clone for ToolInvocation<S, C> {
    fn clone(&self) -> Self {
        Self {
            session: self.session.clone(),
            turn: self.turn.clone(),
            step_context: self.step_context.clone(),
            cancellation_token: self.cancellation_token.clone(),
            tracker: self.tracker.clone(),
            call_id: self.call_id.clone(),
            tool_name: self.tool_name.clone(),
            source: self.source.clone(),
            payload: self.payload.clone(),
        }
    }
}

/// Supplies the turn context embedded in request-scoped tool state.
pub trait ToolStepContext: Send + Sync {
    /// Clones the turn context associated with this request.
    fn turn_context(&self) -> Arc<TurnContext>;
}

/// Consumes streamed argument diffs and emits protocol events.
pub trait ToolArgumentDiffConsumer: Send {
    /// Consumes the next diff for one tool call.
    fn consume_diff(&mut self, turn: &TurnContext, call_id: String, diff: &str)
    -> Option<EventMsg>;

    /// Finishes consuming diffs before the tool call completes.
    fn finish(&mut self) -> Result<Option<EventMsg>, codex_tools::FunctionCallError> {
        Ok(None)
    }
}

/// Result of tool dispatch, retaining hook payload state owned by the host.
pub struct AnyToolResult<P> {
    /// Provider-visible call identifier.
    pub call_id: String,
    /// Original tool input.
    pub payload: ToolPayload,
    /// Tool response.
    pub result: Box<dyn ToolOutput>,
    /// Optional post-tool hook payload owned by the host runtime.
    pub post_tool_use_payload: Option<P>,
}

impl<P> AnyToolResult<P> {
    /// Converts this result into a model input item.
    pub fn into_response(self) -> ResponseInputItem {
        let Self {
            call_id,
            payload,
            result,
            ..
        } = self;
        result.to_response_item(&call_id, &payload)
    }

    /// Converts this result into a Code Mode value.
    pub fn code_mode_result(self) -> Value {
        let Self {
            payload, result, ..
        } = self;
        result.code_mode_result(&payload)
    }
}

/// Host-side registry used by the tool router.
pub trait ToolDispatcher<S, C>: Send + Sync {
    /// Host-owned payload used after a successful tool invocation.
    type PostToolUsePayload;

    /// Returns a diff consumer for a registered tool, when supported.
    fn create_diff_consumer(
        &self,
        tool_name: &ToolName,
    ) -> Option<Box<dyn ToolArgumentDiffConsumer>>;

    /// Returns whether a registered tool supports parallel execution.
    fn supports_parallel_tool_calls(&self, tool_name: &ToolName) -> Option<bool>;

    /// Returns whether cancellation waits for runtime teardown.
    fn waits_for_runtime_cancellation(&self, tool_name: &ToolName) -> Option<bool>;

    /// Dispatches one tool invocation through the host registry.
    fn dispatch_any_with_terminal_outcome<'a>(
        &'a self,
        invocation: ToolInvocation<S, C>,
        terminal_outcome_reached: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> BoxFuture<
        'a,
        Result<AnyToolResult<Self::PostToolUsePayload>, codex_tools::FunctionCallError>,
    >;

    /// Returns registered tool names for test-only inspection.
    fn tool_names_for_test(&self) -> Vec<ToolName>;

    /// Returns a tool's configured visibility for test-only inspection.
    fn tool_exposure_for_test(&self, tool_name: &ToolName) -> Option<ToolExposure>;
}
