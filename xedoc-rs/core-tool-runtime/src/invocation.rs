//! Shared tool invocation state independent of the session implementation.

use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use xedoc_core_turn_context::TurnContext;
use xedoc_core_turn_diff::TurnDiffTracker;
use xedoc_protocol::models::FunctionCallOutputBody;
use xedoc_protocol::models::ResponseInputItem;
use xedoc_protocol::protocol::EventMsg;
use xedoc_tool_output_reduce::ReductionConfig;
use xedoc_tool_output_reduce::ReductionInput;
use xedoc_tool_output_reduce::ReductionSink;
use xedoc_tool_output_reduce::reduce;
use xedoc_tools::ToolExposure;
use xedoc_tools::ToolName;

pub use xedoc_core_tool_output::AbortedToolOutput;
pub use xedoc_core_tool_output::ApplyPatchToolOutput;
pub use xedoc_core_tool_output::ExecCommandToolOutput;
pub use xedoc_core_tool_output::FunctionToolOutput;
pub use xedoc_core_tool_output::McpToolOutput;
pub use xedoc_core_tool_output::ToolCallSource;
pub use xedoc_core_tool_output::ToolOutput;
pub use xedoc_core_tool_output::ToolPayload;
pub use xedoc_core_tool_output::ToolSearchOutput;
pub use xedoc_core_tool_output::boxed_tool_output;

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
    fn finish(&mut self) -> Result<Option<EventMsg>, xedoc_tools::FunctionCallError> {
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
    /// Runtime-owned sink for reduction records, when the host provides one.
    pub reduction_sink: Option<Arc<dyn ReductionSink>>,
    /// Tool name used to tag the reduction record.
    pub tool_name: String,
    /// Thread identifier associated with this tool call.
    pub thread_id: Option<String>,
    /// Turn identifier associated with this tool call.
    pub turn_id: Option<String>,
    /// Effective reducer configuration captured at tool-ingestion time.
    pub reduction_config: Option<ReductionConfig>,
    /// Stable hash of command arguments, never the raw command.
    pub command_hash: Option<String>,
}

impl<P> AnyToolResult<P> {
    /// Converts this result into a model input item.
    pub fn into_response(self) -> ResponseInputItem {
        let Self {
            call_id,
            payload,
            result,
            reduction_sink,
            tool_name,
            thread_id,
            turn_id,
            reduction_config,
            command_hash,
            ..
        } = self;
        let mut response = result.to_response_item(&call_id, &payload);
        if let (Some(sink), Some(config)) = (reduction_sink, reduction_config.as_ref()) {
            let body = match &mut response {
                ResponseInputItem::FunctionCallOutput { output, .. }
                | ResponseInputItem::CustomToolCallOutput { output, .. } => &mut output.body,
                _ => return response,
            };
            match body {
                FunctionCallOutputBody::Text(text) => {
                    let original = text.clone();
                    let reduced = reduce(
                        ReductionInput {
                            tool_name: &tool_name,
                            call_id: &call_id,
                            text: &original,
                            command_hash: command_hash.as_deref(),
                        },
                        config,
                    );
                    *text = reduced.text;
                    let mut record = reduced.record;
                    record.thread_id = thread_id;
                    record.turn_id = turn_id;
                    if record.bytes_in > 0 && !original.trim().is_empty() {
                        sink.try_record(record);
                    }
                }
                FunctionCallOutputBody::ContentItems(items) => {
                    // Content items are structured model output.  Leave them
                    // byte-identical until a content-item-aware reducer exists.
                    let _ = items;
                }
            }
        }
        response
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
        Result<AnyToolResult<Self::PostToolUsePayload>, xedoc_tools::FunctionCallError>,
    >;

    /// Returns registered tool names for test-only inspection.
    fn tool_names_for_test(&self) -> Vec<ToolName>;

    /// Returns a tool's configured visibility for test-only inspection.
    fn tool_exposure_for_test(&self, tool_name: &ToolName) -> Option<ToolExposure>;
}
