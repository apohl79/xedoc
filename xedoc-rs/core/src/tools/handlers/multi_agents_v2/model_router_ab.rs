use super::*;
use crate::session::ab_pairs::AbPairPreference;
use crate::tools::context::FunctionToolOutput;
use xedoc_tools::JsonSchema;
use xedoc_tools::ResponsesApiTool;
use xedoc_tools::ToolSpec;

const TOOL_NAME: &str = "model_router_ab";

pub(crate) struct Handler;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Args {
    action: Action,
    pair_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Next,
    Off,
    Routed,
    Orchestrator,
    Tie,
    Unusable,
}

#[derive(Serialize)]
struct Result {
    status: &'static str,
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        let properties = std::collections::BTreeMap::from([
            (
                "action".to_string(),
                JsonSchema::string(Some(
                    "Use next to arm one root-turn A/B pair, off to disable it, or record routed, orchestrator, tie, or unusable after a pair.".to_string(),
                )),
            ),
            (
                "pair_id".to_string(),
                JsonSchema::string(Some(
                    "Required when recording a pair outcome; use the pair_id returned by spawn_agent."
                        .to_string(),
                )),
            ),
        ]);
        ToolSpec::Function(ResponsesApiTool {
            name: TOOL_NAME.to_string(),
            description:
                "Control a bounded one-turn model-router A/B experiment. Root thread only."
                    .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                properties,
                Some(vec!["action".to_string()]),
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
                payload,
                ..
            } = invocation;
            if turn.session_source.is_non_root_agent() {
                return Err(FunctionCallError::RespondToModel(
                    "model_router_ab is available only to the root thread".to_string(),
                ));
            }
            let arguments = function_arguments(payload)?;
            let args: Args = parse_arguments(&arguments)?;
            let status = match args.action {
                Action::Next => {
                    session.arm_model_router_ab_next().await;
                    "armed"
                }
                Action::Off => {
                    session.disable_model_router_ab().await;
                    "disabled"
                }
                action => {
                    let pair_id = args.pair_id.as_deref().ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "pair_id is required when recording an A/B outcome".to_string(),
                        )
                    })?;
                    let preference = match action {
                        Action::Routed => AbPairPreference::Routed,
                        Action::Orchestrator => AbPairPreference::Orchestrator,
                        Action::Tie => AbPairPreference::Tie,
                        Action::Unusable => AbPairPreference::Unusable,
                        Action::Next | Action::Off => unreachable!(),
                    };
                    if !session
                        .record_model_router_ab_preference(pair_id, preference)
                        .await
                    {
                        return Err(FunctionCallError::RespondToModel(
                            "A/B pair is unavailable or already has an outcome".to_string(),
                        ));
                    }
                    "recorded"
                }
            };
            serde_json::to_string(&Result { status })
                .map(|result| boxed_tool_output(FunctionToolOutput::from_text(result, Some(true))))
                .map_err(|error| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to serialize model_router_ab result: {error}"
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
