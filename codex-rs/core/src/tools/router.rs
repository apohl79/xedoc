use std::ops::Deref;
use std::sync::Arc;

use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::tools::handlers::ToolSearchHandlerCache;
use crate::tools::registry::AnyToolResult;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolRegistry;
use crate::tools::spec_plan::build_tool_router;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::models::ResponseItem;
use codex_tools::DiscoverableTool;
use codex_tools::ToolCall as ExtensionToolCall;
use codex_tools::ToolExecutor;
use codex_tools::ToolSpec;

pub use codex_core_tool_runtime::ToolCall;
pub(crate) use codex_core_tool_runtime::ToolCallSource;
pub(crate) use codex_core_tool_runtime::ToolDispatcher;

type CoreToolRouter = codex_core_tool_runtime::ToolRouter<Session, StepContext, ToolRegistry>;

/// Core's typed session adapter for the generic tool router.
pub(crate) struct ToolRouter(CoreToolRouter);

pub(crate) struct ToolRouterParams<'a> {
    pub(crate) tool_runtimes: Vec<Arc<dyn CoreToolRuntime>>,
    pub(crate) tool_suggest_candidates: Option<ToolSuggestCandidates>,
    pub(crate) extension_tool_executors: Vec<Arc<dyn ToolExecutor<ExtensionToolCall>>>,
    pub(crate) dynamic_tools: &'a [DynamicToolSpec],
}

pub(crate) use codex_core_tool_specs::ToolSuggestPresentation;

#[derive(Clone, Debug)]
pub(crate) struct ToolSuggestCandidates {
    pub(crate) tools: Vec<DiscoverableTool>,
    pub(crate) presentation: ToolSuggestPresentation,
}

impl ToolRouter {
    pub(crate) fn from_context(
        step_context: &StepContext,
        params: ToolRouterParams<'_>,
        tool_search_handler_cache: &ToolSearchHandlerCache,
    ) -> Self {
        build_tool_router(step_context, params, tool_search_handler_cache)
    }

    pub(crate) fn from_parts(registry: ToolRegistry, model_visible_specs: Vec<ToolSpec>) -> Self {
        Self(CoreToolRouter::from_parts(registry, model_visible_specs))
    }

    #[tracing::instrument(level = "trace", skip_all, err)]
    pub(crate) fn build_tool_call(
        item: ResponseItem,
    ) -> Result<Option<ToolCall>, FunctionCallError> {
        CoreToolRouter::build_tool_call(item)
    }
}

impl Deref for ToolRouter {
    type Target = CoreToolRouter;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[tracing::instrument(level = "trace", skip_all)]
pub(crate) fn extension_tool_executors(
    session: &Session,
) -> Vec<Arc<dyn ToolExecutor<ExtensionToolCall>>> {
    session
        .services
        .extensions
        .tool_contributors()
        .iter()
        .flat_map(|contributor| {
            contributor.tools(
                &session.services.session_extension_data,
                &session.services.thread_extension_data,
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
