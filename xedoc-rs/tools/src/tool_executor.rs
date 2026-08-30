use crate::FunctionCallError;
use crate::ToolName;
use crate::ToolOutput;
use crate::ToolSearchInfo;
use crate::ToolSpec;
use std::future::Future;
use std::pin::Pin;

/// The boxed future returned by [`ToolExecutor::handle`].
pub type ToolExecutorFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn ToolOutput>, FunctionCallError>> + Send + 'a>>;

/// Controls where a tool is exposed to the model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolExposure {
    /// Include this tool in the initial model-visible tool list.
    Direct,

    /// Register this tool for later discovery, but omit it from the initial
    /// model-visible tool list. Deferred tools must provide search metadata via
    /// [`ToolExecutor::search_info`]. The default implementation derives
    /// metadata from function and namespace specs.
    Deferred,

    /// Keep this tool registered for dispatch without exposing it to the model.
    Hidden,
}

impl ToolExposure {
    pub fn is_direct(self) -> bool {
        matches!(self, Self::Direct)
    }
}

/// Shared runtime contract for model-visible tools.
///
/// Implementations keep the model-visible spec tied to the executable runtime.
/// Host crates can layer routing, hooks, telemetry, or other orchestration on
/// top without reopening the spec/runtime split.
pub trait ToolExecutor<Invocation>: Send + Sync {
    /// The concrete tool name handled by this runtime instance.
    fn tool_name(&self) -> ToolName;

    fn spec(&self) -> ToolSpec;

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }

    fn search_info(&self) -> Option<ToolSearchInfo> {
        let spec = self.spec();
        ToolSearchInfo::from_tool_spec(spec, /*source_info*/ None)
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        false
    }

    fn handle(&self, invocation: Invocation) -> ToolExecutorFuture<'_>;
}
