use crate::ToolInvocation;
use crate::ToolPayload;
use crate::ToolSearchOutput;
use crate::boxed_tool_output;
use bm25::Document;
use bm25::Language;
use bm25::SearchEngine;
use bm25::SearchEngineBuilder;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::instrument;
use xedoc_core_tool_specs::tool_search_spec::create_tool_search_tool;
use xedoc_tools::FunctionCallError;
use xedoc_tools::LoadableToolSpec;
use xedoc_tools::TOOL_SEARCH_DEFAULT_LIMIT;
use xedoc_tools::TOOL_SEARCH_TOOL_NAME;
use xedoc_tools::ToolExecutor;
use xedoc_tools::ToolName;
use xedoc_tools::ToolSearchEntry;
use xedoc_tools::ToolSearchInfo;
use xedoc_tools::ToolSpec;
use xedoc_tools::coalesce_loadable_tool_specs;

pub struct ToolSearchHandler {
    search_infos: Vec<ToolSearchInfo>,
    spec: ToolSpec,
    search_engine: SearchEngine<usize>,
}

#[derive(Default)]
pub struct ToolSearchHandlerCache {
    cached: Mutex<Option<Arc<ToolSearchHandler>>>,
}

impl ToolSearchHandlerCache {
    #[instrument(level = "trace", skip_all, fields(search_info_count = search_infos.len()))]
    pub fn get_or_build(&self, search_infos: Vec<ToolSearchInfo>) -> Arc<ToolSearchHandler> {
        {
            let cached = self.cached();
            if let Some(cached) = cached.as_ref()
                && cached.search_infos == search_infos
            {
                return Arc::clone(cached);
            }
        }

        let handler = Arc::new(ToolSearchHandler::new(search_infos));
        let mut cached = self.cached();
        if let Some(cached) = cached.as_ref()
            && cached.search_infos == handler.search_infos
        {
            return Arc::clone(cached);
        }

        *cached = Some(Arc::clone(&handler));
        handler
    }

    fn cached(&self) -> std::sync::MutexGuard<'_, Option<Arc<ToolSearchHandler>>> {
        match self.cached.lock() {
            Ok(cached) => cached,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl ToolSearchHandler {
    #[instrument(
        level = "trace",
        skip_all,
        fields(search_info_count = search_infos.len())
    )]
    pub fn new(search_infos: Vec<ToolSearchInfo>) -> Self {
        let search_source_infos = search_infos
            .iter()
            .filter_map(|search_info| search_info.source_info.clone())
            .collect::<Vec<_>>();
        let spec = create_tool_search_tool(&search_source_infos, TOOL_SEARCH_DEFAULT_LIMIT);
        let documents: Vec<Document<usize>> = search_infos
            .iter()
            .map(|search_info| search_info.entry.search_text.clone())
            .enumerate()
            .map(|(idx, search_text)| Document::new(idx, search_text))
            .collect();
        let search_engine =
            SearchEngineBuilder::<usize>::with_documents(Language::English, documents).build();

        Self {
            search_infos,
            spec,
            search_engine,
        }
    }
}

impl<S: Send + Sync + 'static, C: Send + Sync + 'static> ToolExecutor<ToolInvocation<S, C>>
    for ToolSearchHandler
{
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_SEARCH_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn handle(&self, invocation: ToolInvocation<S, C>) -> xedoc_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl ToolSearchHandler {
    async fn handle_call<S, C>(
        &self,
        invocation: ToolInvocation<S, C>,
    ) -> Result<Box<dyn crate::ToolOutput>, FunctionCallError> {
        let ToolInvocation { payload, .. } = invocation;

        let args = match payload {
            ToolPayload::ToolSearch { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::Fatal(format!(
                    "{TOOL_SEARCH_TOOL_NAME} handler received unsupported payload"
                )));
            }
        };

        let query = args.query.trim();
        if query.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "query must not be empty".to_string(),
            ));
        }
        let limit = args.limit.unwrap_or(TOOL_SEARCH_DEFAULT_LIMIT);

        if limit == 0 {
            return Err(FunctionCallError::RespondToModel(
                "limit must be greater than zero".to_string(),
            ));
        }

        if self.search_infos.is_empty() {
            return Ok(boxed_tool_output(ToolSearchOutput { tools: Vec::new() }));
        }

        let tools = self.search(query, limit)?;

        Ok(boxed_tool_output(ToolSearchOutput { tools }))
    }
}

impl ToolSearchHandler {
    fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<LoadableToolSpec>, FunctionCallError> {
        let results = self
            .search_engine
            .search(query, limit)
            .into_iter()
            .map(|result| result.document.id)
            .filter_map(|id| self.search_infos.get(id))
            .map(|search_info| &search_info.entry);
        self.search_output_tools(results)
    }

    fn search_output_tools<'a>(
        &self,
        results: impl IntoIterator<Item = &'a ToolSearchEntry>,
    ) -> Result<Vec<LoadableToolSpec>, FunctionCallError> {
        Ok(coalesce_loadable_tool_specs(
            results.into_iter().map(|entry| entry.output.clone()),
        ))
    }
}
