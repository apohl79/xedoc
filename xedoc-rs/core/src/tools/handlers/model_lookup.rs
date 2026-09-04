use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use xedoc_models_manager::manager::RefreshStrategy;
use xedoc_protocol::openai_models::ModelPreset;
use xedoc_tools::JsonSchema;
use xedoc_tools::ResponsesApiTool;
use xedoc_tools::ToolName;
use xedoc_tools::ToolSpec;

const TOOL_NAME: &str = "model_lookup";

pub struct ModelLookupHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelLookupArgs {
    provider: Option<String>,
    query: Option<String>,
}

#[derive(Debug, Serialize)]
struct ModelLookupEntry<'a> {
    model: &'a str,
    provider: &'a str,
    reasoning_efforts: Vec<&'a str>,
}

fn create_model_lookup_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "provider".to_string(),
            JsonSchema::string(Some(
                "Optional provider ID or name to filter models.".to_string(),
            )),
        ),
        (
            "query".to_string(),
            JsonSchema::string(Some(
                "Optional case-insensitive substring to match against model IDs.".to_string(),
            )),
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: TOOL_NAME.to_string(),
        description: "Look up the live picker-visible model catalog. Use this before choosing a model for spawn_agent when the desired provider or model is not listed in the spawn_agent guidance.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, /*required*/ None, Some(false.into())),
        output_schema: None,
    })
}

impl ToolExecutor<ToolInvocation> for ModelLookupHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_model_lookup_tool()
    }

    fn handle(&self, invocation: ToolInvocation) -> xedoc_tools::ToolExecutorFuture<'_> {
        Box::pin(async move {
            let ToolInvocation {
                session,
                turn,
                payload,
                ..
            } = invocation;
            let ToolPayload::Function { arguments } = payload else {
                return Err(FunctionCallError::RespondToModel(format!(
                    "{TOOL_NAME} handler received unsupported payload"
                )));
            };
            let args: ModelLookupArgs = parse_arguments(&arguments)?;
            let models = session
                .services
                .models_manager
                .list_models(
                    RefreshStrategy::OnlineIfUncached,
                    turn.config.http_client_factory(),
                )
                .await;
            let provider_filter = args.provider.as_deref().map(str::to_ascii_lowercase);
            let query_filter = args.query.as_deref().map(str::to_ascii_lowercase);
            let entries = models
                .iter()
                .filter(|model| model.show_in_picker)
                .filter(|model| {
                    provider_filter.as_deref().is_none_or(|provider| {
                        model.provider_id.to_ascii_lowercase().contains(provider)
                    })
                })
                .filter(|model| {
                    query_filter
                        .as_deref()
                        .is_none_or(|query| model.model.to_ascii_lowercase().contains(query))
                })
                .map(model_lookup_entry)
                .collect::<Vec<_>>();
            let output = serde_json::to_string(&entries).map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to serialize {TOOL_NAME} result: {err}"
                ))
            })?;
            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                output,
                /*success*/ Some(true),
            )))
        })
    }
}

fn model_lookup_entry(model: &ModelPreset) -> ModelLookupEntry<'_> {
    ModelLookupEntry {
        model: &model.model,
        provider: &model.provider_id,
        reasoning_efforts: model
            .supported_reasoning_efforts
            .iter()
            .map(|effort| effort.effort.as_str())
            .collect(),
    }
}

impl CoreToolRuntime for ModelLookupHandler {}
