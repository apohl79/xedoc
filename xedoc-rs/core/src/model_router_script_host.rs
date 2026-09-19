//! Host-side invocation and validation for scripted model routing.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use dirs::home_dir;
use futures::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use xedoc_async_utils::OrCancelExt;
use xedoc_features::Feature;
use xedoc_install_context::InstallContext;
use xedoc_protocol::models::BaseInstructions;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use xedoc_script_protocol::ClassifierRequest;
use xedoc_script_protocol::EligibleRoute;
use xedoc_script_protocol::Extension;
use xedoc_script_protocol::Interaction;
use xedoc_script_protocol::InteractionResponse;
use xedoc_script_protocol::InteractionSurface;
use xedoc_script_protocol::MAX_SCRIPT_SUMMARY_BYTES;
use xedoc_script_protocol::Method;
use xedoc_script_protocol::OpaqueId;
use xedoc_script_protocol::RequestId;
use xedoc_script_protocol::ResponseOutcome;
use xedoc_script_protocol::Route;
use xedoc_script_protocol::RouteDecision;
use xedoc_script_protocol::RouteDisposition;
use xedoc_script_protocol::RouteFeedback;
use xedoc_script_protocol::RouterState;
use xedoc_script_protocol::ScriptInvoker;
use xedoc_script_protocol::ScriptRequest;
use xedoc_script_protocol::ScriptResult;
use xedoc_script_protocol::SubprocessFailure;

use crate::Prompt;
use crate::client_common::ResponseEvent;
use crate::responses_metadata::XedocResponsesRequestKind;
use crate::responses_retry::ResponsesStreamRequest;
use crate::responses_retry::handle_retryable_response_stream_error;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use xedoc_core_session_name::append_message_text;

const MAX_SCRIPT_ERROR_MESSAGE_BYTES: usize = 1_024;
const MAX_SCRIPT_FEEDBACK_TEXT_BYTES: usize = 512;
const MAX_INTERACTION_IDENTIFIER_BYTES: usize = 128;
const MAX_INTERACTION_CONTINUATION_BYTES: usize = 8_192;
const MAX_INTERACTION_TEXT_BYTES: usize = 4_096;
const MAX_INTERACTION_TITLE_BYTES: usize = 256;
const MAX_INTERACTION_DESCRIPTION_BYTES: usize = 512;
const MAX_INTERACTION_ITEMS: usize = 64;
const MAX_INTERACTION_FIELDS: usize = 32;
const MAX_INTERACTION_OPTIONS: usize = 64;
const MAX_INTERACTION_DETAILS: usize = 32;
const MAX_INTERACTION_SECTIONS: usize = 16;
const MAX_INTERACTION_SECTION_ROWS: usize = 16;
const MAX_INTERACTION_SECTION_ROWS_TOTAL: usize = 16;
const MAX_INTERACTION_SECTION_ROW_BYTES: usize = 512;
const MAX_INTERACTION_SECTION_TEXT_BYTES: usize = 2_048;
const MAX_INTERACTION_INDENT: u8 = 8;
const MAX_INTERACTION_ACTIONS: usize = 16;
const MAX_INTERACTION_KEY_BINDINGS: usize = 4;
const MAX_INTERACTION_ACTION_VALUE_BYTES: usize = 4_096;
const MAX_INTERACTION_TEXT_FIELD_BYTES: u32 = 4_096;
const MAX_INTERACTION_JSON_DEPTH: usize = 8;
const MAX_MODEL_ROUTE_REASONING_EFFORTS: usize = 16;
const MAX_CLASSIFIER_CONTINUATION_BYTES: usize = 8_192;
const MAX_CLASSIFIER_INPUT_BYTES: usize = 8_192;
const MAX_CLASSIFIER_OUTPUT_BYTES: usize = 4_096;
const MAX_ROUTER_STATE_IDENTIFIER_BYTES: usize = 128;
const MAX_ROUTER_STATE_REVISION_BYTES: usize = 128;
const BUNDLED_ROUTER_SCRIPT_PATH: &str = "model-router/reference-router";
static SCRIPT_REPORTING_BASELINES: OnceLock<Mutex<HashMap<Vec<OsString>, Option<Route>>>> =
    OnceLock::new();

/// Configured host for one-shot model-router script invocations.
pub(crate) struct ModelRouterScriptHost {
    argv: Vec<OsString>,
    decision_timeout: Duration,
    interaction_timeout: Duration,
}

impl ModelRouterScriptHost {
    /// Creates a host only when a direct-exec router script is configured.
    #[must_use]
    pub(crate) fn from_config(config: &crate::config::Config) -> Option<Self> {
        if !config.features.enabled(Feature::ModelRouter) {
            return None;
        }
        let router_config = &config.model_router;
        let argv = router_config
            .script_argv()
            .map(direct_exec_argv)
            .or_else(bundled_router_argv)?;
        Some(Self {
            argv,
            decision_timeout: router_config.decision_timeout(),
            interaction_timeout: router_config.interaction_timeout(),
        })
    }

    /// Requests a route decision, retaining the caller's current route on failure.
    pub(crate) async fn decide(
        &self,
        session: &Session,
        turn_context: &TurnContext,
        classifier_config: &crate::config::Config,
        context: Value,
        params: Value,
        eligible_routes: &[EligibleRoute],
        route_mutable: bool,
        cancellation: CancellationToken,
    ) -> ModelRouterScriptDecisionOutcome {
        let response = self
            .invoke(
                Method::RoutingDecide,
                context.clone(),
                params,
                self.decision_timeout,
                cancellation.clone(),
            )
            .await;
        match response {
            Ok(ResponseOutcome::Result {
                result: ScriptResult::Route { decision },
            }) => {
                let outcome = validate_decision(decision, eligible_routes, route_mutable);
                self.remember_reporting_baseline(&outcome);
                outcome
            }
            Ok(ResponseOutcome::Result {
                result: ScriptResult::Interaction { interaction },
            }) => match validate_interaction(&interaction, eligible_routes) {
                Ok(()) => ModelRouterScriptDecisionOutcome::Interaction(interaction),
                Err(failure) => ModelRouterScriptDecisionOutcome::fallback(failure),
            },
            Ok(ResponseOutcome::Result {
                result: ScriptResult::ClassifierRequest { classifier },
            }) => {
                let classifier_started_at = Instant::now();
                let output = self
                    .run_classifier(
                        session,
                        turn_context,
                        classifier_config,
                        &classifier,
                        eligible_routes,
                        &cancellation,
                    )
                    .await;
                let classifier_elapsed_ms =
                    u64::try_from(classifier_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                let params = match classifier_continuation_params(
                    classifier.continuation.as_str(),
                    output,
                    classifier_elapsed_ms,
                    cancellation.is_cancelled(),
                ) {
                    Ok(params) => params,
                    Err(failure) => return ModelRouterScriptDecisionOutcome::fallback(failure),
                };
                match self
                    .invoke(
                        Method::RoutingClassifierRespond,
                        context,
                        params,
                        self.decision_timeout,
                        cancellation,
                    )
                    .await
                {
                    Ok(ResponseOutcome::Result {
                        result: ScriptResult::Route { decision },
                    }) => {
                        let outcome = validate_decision(decision, eligible_routes, route_mutable);
                        self.remember_reporting_baseline(&outcome);
                        outcome
                    }
                    Ok(ResponseOutcome::Result {
                        result: ScriptResult::Interaction { interaction },
                    }) => match validate_interaction(&interaction, eligible_routes) {
                        Ok(()) => ModelRouterScriptDecisionOutcome::Interaction(interaction),
                        Err(failure) => ModelRouterScriptDecisionOutcome::fallback(failure),
                    },
                    Ok(ResponseOutcome::Result {
                        result:
                            ScriptResult::ClassifierRequest { .. }
                            | ScriptResult::State { .. }
                            | ScriptResult::Complete { .. }
                            | ScriptResult::Message { .. },
                    }) => ModelRouterScriptDecisionOutcome::fallback(
                        ModelRouterScriptFailure::UnexpectedResult,
                    ),
                    Ok(ResponseOutcome::Error { error }) => {
                        ModelRouterScriptDecisionOutcome::fallback(script_error_failure(error))
                    }
                    Err(failure) => ModelRouterScriptDecisionOutcome::fallback(failure),
                }
            }
            Ok(ResponseOutcome::Result {
                result:
                    ScriptResult::State { .. }
                    | ScriptResult::Complete { .. }
                    | ScriptResult::Message { .. },
            }) => ModelRouterScriptDecisionOutcome::fallback(
                ModelRouterScriptFailure::UnexpectedResult,
            ),
            Ok(ResponseOutcome::Error { error }) => {
                ModelRouterScriptDecisionOutcome::fallback(script_error_failure(error))
            }
            Err(failure) => ModelRouterScriptDecisionOutcome::fallback(failure),
        }
    }

    async fn run_classifier(
        &self,
        session: &Session,
        turn_context: &TurnContext,
        classifier_config: &crate::config::Config,
        classifier: &ClassifierRequest,
        eligible_routes: &[EligibleRoute],
        cancellation: &CancellationToken,
    ) -> Result<String, ModelRouterScriptFailure> {
        if classifier.continuation.as_str().len() > MAX_CLASSIFIER_CONTINUATION_BYTES
            || classifier.input.is_empty()
            || classifier.input.len() > MAX_CLASSIFIER_INPUT_BYTES
            || classifier.route.provider_id.as_str() != classifier_config.model_provider_id
            || !route_is_eligible(&classifier.route, eligible_routes)
        {
            return Err(ModelRouterScriptFailure::InvalidClassifierRequest);
        }
        let reasoning_effort = classifier
            .route
            .reasoning_effort
            .as_str()
            .parse::<ReasoningEffortConfig>()
            .map_err(|_| ModelRouterScriptFailure::InvalidClassifierRequest)?;
        let classifier_turn = turn_context
            .with_configured_route(
                classifier_config.clone(),
                classifier.route.model.as_str().to_string(),
                reasoning_effort,
                &session.services.models_manager,
            )
            .await;
        let prompt = Prompt {
            input: vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: classifier.input.clone(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            }],
            base_instructions: BaseInstructions::default(),
            ..Default::default()
        };
        let window_id = session.current_window_id().await;
        let responses_metadata = classifier_turn.turn_metadata_state.to_responses_metadata(
            session.installation_id.clone(),
            window_id,
            XedocResponsesRequestKind::ModelRouterClassifier,
        );
        let mut client_session = session
            .model_client_for_turn(&classifier_turn)
            .await
            .new_session();
        let mut retries = 0;
        let max_retries = classifier_turn.provider.info().stream_max_retries();
        let mut stream = loop {
            let stream = client_session
                .stream(
                    &prompt,
                    &classifier_turn.model_info,
                    &classifier_turn.session_telemetry,
                    classifier_turn.reasoning_effort.clone(),
                    classifier_turn.reasoning_summary,
                    classifier_turn.config.service_tier.clone(),
                    &responses_metadata,
                )
                .or_cancel(cancellation)
                .await
                .map_err(|_| ModelRouterScriptFailure::ClassifierCancelled)?;
            match stream {
                Ok(stream) => break stream,
                Err(error) if error.is_retryable() => {
                    handle_retryable_response_stream_error(
                        &mut retries,
                        max_retries,
                        error,
                        &mut client_session,
                        session,
                        &classifier_turn,
                        ResponsesStreamRequest::Sampling,
                    )
                    .await
                    .map_err(|error| classifier_invocation_failure(error.to_string()))?;
                }
                Err(error) => return Err(classifier_invocation_failure(error.to_string())),
            }
        };
        let mut output = String::new();
        loop {
            let event = stream
                .next()
                .or_cancel(cancellation)
                .await
                .map_err(|_| ModelRouterScriptFailure::ClassifierCancelled)?;
            let Some(event) = event else {
                break;
            };
            let event = event.map_err(|error| classifier_invocation_failure(error.to_string()))?;
            append_classifier_event_text(&mut output, &event);
            if let ResponseEvent::Completed {
                response_id,
                token_usage,
                ..
            } = event
            {
                session
                    .record_auxiliary_token_usage(
                        &classifier_turn,
                        Some(response_id.as_str()),
                        token_usage.as_ref(),
                        "model_router_classifier",
                    )
                    .await;
                return Ok(output);
            }
        }
        Err(classifier_invocation_failure(
            "classifier stream ended before completion".to_string(),
        ))
    }

    /// Opens the script-owned model-router settings surface.
    pub(crate) async fn open_settings(
        &self,
        context: Value,
        eligible_routes: &[EligibleRoute],
        cancellation: CancellationToken,
    ) -> Result<ModelRouterScriptSettingsInteraction, ModelRouterScriptFailure> {
        let outcome = self
            .invoke(
                Method::SettingsOpen,
                context,
                Value::Object(Default::default()),
                self.interaction_timeout,
                cancellation,
            )
            .await?;
        match outcome {
            ResponseOutcome::Result {
                result: ScriptResult::Interaction { mut interaction },
            } => {
                validate_interaction(&interaction, eligible_routes)?;
                require_state_revision(&interaction)?;
                let session_update = interaction.session_update.take();
                Ok(ModelRouterScriptSettingsInteraction {
                    interaction,
                    session_update,
                })
            }
            ResponseOutcome::Result {
                result: ScriptResult::Route { .. },
            }
            | ResponseOutcome::Result {
                result: ScriptResult::Complete { .. },
            }
            | ResponseOutcome::Result {
                result: ScriptResult::ClassifierRequest { .. },
            }
            | ResponseOutcome::Result {
                result: ScriptResult::State { .. },
            }
            | ResponseOutcome::Result {
                result: ScriptResult::Message { .. },
            } => Err(ModelRouterScriptFailure::UnexpectedResult),
            ResponseOutcome::Error { error } => Err(script_error_failure(error)),
        }
    }

    /// Sends one settings response back to the script.
    pub(crate) async fn respond_settings(
        &self,
        context: Value,
        response: InteractionResponse,
        eligible_routes: &[EligibleRoute],
        cancellation: CancellationToken,
    ) -> Result<ModelRouterScriptSettingsInteraction, ModelRouterScriptFailure> {
        if response.state_revision.is_none() {
            return Err(ModelRouterScriptFailure::MissingStateRevision);
        }
        let params =
            serde_json::to_value(response).map_err(|_| ModelRouterScriptFailure::Encode)?;
        let outcome = self
            .invoke(
                Method::InteractionRespond,
                context,
                params,
                self.interaction_timeout,
                cancellation,
            )
            .await?;
        match outcome {
            ResponseOutcome::Result {
                result: ScriptResult::Interaction { mut interaction },
            } => {
                validate_interaction(&interaction, eligible_routes)?;
                require_state_revision(&interaction)?;
                let session_update = interaction.session_update.take();
                Ok(ModelRouterScriptSettingsInteraction {
                    interaction,
                    session_update,
                })
            }
            ResponseOutcome::Result {
                result: ScriptResult::Route { .. },
            }
            | ResponseOutcome::Result {
                result: ScriptResult::Complete { .. },
            }
            | ResponseOutcome::Result {
                result: ScriptResult::ClassifierRequest { .. },
            }
            | ResponseOutcome::Result {
                result: ScriptResult::State { .. },
            }
            | ResponseOutcome::Result {
                result: ScriptResult::Message { .. },
            } => Err(ModelRouterScriptFailure::UnexpectedResult),
            ResponseOutcome::Error { error } => Err(script_error_failure(error)),
        }
    }

    /// Sends one rendered interaction response back to the script.
    pub(crate) async fn respond(
        &self,
        context: Value,
        response: InteractionResponse,
        eligible_routes: &[EligibleRoute],
        route_mutable: bool,
        cancellation: CancellationToken,
    ) -> ModelRouterScriptInteractionOutcome {
        let params = serde_json::to_value(response).map_err(|_| ModelRouterScriptFailure::Encode);
        let Ok(params) = params else {
            return ModelRouterScriptInteractionOutcome::Failure(ModelRouterScriptFailure::Encode);
        };
        let response = self
            .invoke(
                Method::InteractionRespond,
                context,
                params,
                self.interaction_timeout,
                cancellation,
            )
            .await;
        match response {
            Ok(ResponseOutcome::Result {
                result: ScriptResult::Interaction { interaction },
            }) => match validate_interaction(&interaction, eligible_routes) {
                Ok(()) => ModelRouterScriptInteractionOutcome::Interaction(interaction),
                Err(failure) => ModelRouterScriptInteractionOutcome::Failure(failure),
            },
            Ok(ResponseOutcome::Result {
                result: ScriptResult::Route { decision },
            }) => {
                let outcome = validate_decision(decision, eligible_routes, route_mutable);
                self.remember_reporting_baseline(&outcome);
                match outcome {
                    ModelRouterScriptDecisionOutcome::Apply { decision, route } => {
                        ModelRouterScriptInteractionOutcome::Apply { decision, route }
                    }
                    ModelRouterScriptDecisionOutcome::KeepCurrent { decision, failure } => {
                        ModelRouterScriptInteractionOutcome::KeepCurrent { decision, failure }
                    }
                    ModelRouterScriptDecisionOutcome::Interaction(_) => {
                        ModelRouterScriptInteractionOutcome::Failure(
                            ModelRouterScriptFailure::UnexpectedResult,
                        )
                    }
                }
            }
            Ok(ResponseOutcome::Result {
                result:
                    ScriptResult::State { .. }
                    | ScriptResult::Complete { .. }
                    | ScriptResult::Message { .. },
            }) => ModelRouterScriptInteractionOutcome::Failure(
                ModelRouterScriptFailure::UnexpectedResult,
            ),
            Ok(ResponseOutcome::Result {
                result: ScriptResult::ClassifierRequest { .. },
            }) => ModelRouterScriptInteractionOutcome::Failure(
                ModelRouterScriptFailure::UnexpectedResult,
            ),
            Ok(ResponseOutcome::Error { error }) => {
                ModelRouterScriptInteractionOutcome::Failure(script_error_failure(error))
            }
            Err(failure) => ModelRouterScriptInteractionOutcome::Failure(failure),
        }
    }

    /// Returns a validated snapshot of the script-owned router policy.
    pub(crate) async fn state(
        &self,
        context: Value,
        cancellation: CancellationToken,
    ) -> Result<RouterState, ModelRouterScriptFailure> {
        let outcome = self
            .invoke(
                Method::RoutingState,
                context,
                Value::Object(Default::default()),
                self.decision_timeout,
                cancellation,
            )
            .await?;
        match outcome {
            ResponseOutcome::Result {
                result: ScriptResult::State { state },
            } => validate_router_state(state),
            ResponseOutcome::Result {
                result:
                    ScriptResult::Route { .. }
                    | ScriptResult::Interaction { .. }
                    | ScriptResult::ClassifierRequest { .. }
                    | ScriptResult::Complete { .. }
                    | ScriptResult::Message { .. },
            } => Err(ModelRouterScriptFailure::UnexpectedResult),
            ResponseOutcome::Error { error } => Err(script_error_failure(error)),
        }
    }

    /// Returns the latest validated reporting baseline supplied by this script.
    pub(crate) fn reporting_baseline(config: &crate::config::Config) -> Option<Route> {
        let host = Self::from_config(config)?;
        SCRIPT_REPORTING_BASELINES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&host.argv)
            .cloned()
            .flatten()
    }

    fn remember_reporting_baseline(&self, outcome: &ModelRouterScriptDecisionOutcome) {
        let decision = match outcome {
            ModelRouterScriptDecisionOutcome::Apply { decision, .. }
            | ModelRouterScriptDecisionOutcome::KeepCurrent {
                decision: Some(decision),
                ..
            } => decision,
            ModelRouterScriptDecisionOutcome::KeepCurrent { decision: None, .. }
            | ModelRouterScriptDecisionOutcome::Interaction(_) => return,
        };
        SCRIPT_REPORTING_BASELINES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(self.argv.clone(), decision.reporting_baseline.clone());
    }

    async fn invoke(
        &self,
        method: Method,
        context: Value,
        params: Value,
        timeout: Duration,
        cancellation: CancellationToken,
    ) -> Result<ResponseOutcome, ModelRouterScriptFailure> {
        let protocol_request = ScriptRequest {
            protocol: xedoc_script_protocol::ProtocolVersion::v1(),
            request_id: RequestId::new(OpaqueId::new(Uuid::now_v7().to_string())),
            extension: Extension::ModelRouter,
            method,
            context,
            params,
        };
        ScriptInvoker::new(self.argv.clone())
            .invoke(&protocol_request, timeout, cancellation)
            .await
            .map_err(ModelRouterScriptFailure::Invocation)
    }
}

fn classifier_continuation_params(
    continuation: &str,
    output: Result<String, ModelRouterScriptFailure>,
    elapsed_ms: u64,
    cancelled: bool,
) -> Result<Value, ModelRouterScriptFailure> {
    if cancelled {
        return Err(ModelRouterScriptFailure::ClassifierCancelled);
    }
    match output {
        Ok(output) => Ok(serde_json::json!({
            "continuation": continuation,
            "output": output,
            "elapsedMs": elapsed_ms,
        })),
        Err(ModelRouterScriptFailure::ClassifierCancelled) => {
            Err(ModelRouterScriptFailure::ClassifierCancelled)
        }
        Err(failure) => Ok(serde_json::json!({
            "continuation": continuation,
            "error": failure.diagnostic(),
            "elapsedMs": elapsed_ms,
        })),
    }
}

fn append_classifier_event_text(output: &mut String, event: &ResponseEvent) {
    match event {
        ResponseEvent::OutputTextDelta(delta)
            if output.len().saturating_add(delta.len()) <= MAX_CLASSIFIER_OUTPUT_BYTES =>
        {
            output.push_str(delta);
        }
        ResponseEvent::OutputItemDone(item) if output.is_empty() => {
            append_message_text(output, item);
            truncate_utf8(output, MAX_CLASSIFIER_OUTPUT_BYTES);
        }
        _ => {}
    }
}

fn bundled_router_argv() -> Option<Vec<OsString>> {
    InstallContext::current()
        .bundled_resource(BUNDLED_ROUTER_SCRIPT_PATH)
        .map(|path| {
            vec![
                OsString::from(bundled_router_python_command()),
                path.into_path_buf().into_os_string(),
            ]
        })
}

const fn bundled_router_python_command() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

pub(crate) fn interaction_request(
    interaction: Interaction,
) -> Result<
    (
        xedoc_protocol::protocol::ScriptedInteractionRequestEvent,
        String,
        String,
        String,
        Option<String>,
    ),
    ModelRouterScriptFailure,
> {
    let surface =
        serde_json::to_value(&interaction.surface).map_err(|_| ModelRouterScriptFailure::Encode)?;
    Ok((
        xedoc_protocol::protocol::ScriptedInteractionRequestEvent {
            request_id: Uuid::now_v7().to_string(),
            extension_id: "model-router".to_string(),
            interaction_id: interaction.id.as_str().to_string(),
            continuation: interaction.continuation.as_str().to_string(),
            state_revision: interaction
                .state_revision
                .as_ref()
                .map(|revision| revision.as_str().to_string()),
            expires_at: crate::turn_timing::now_unix_timestamp_ms() / 1_000 + 5 * 60,
            surface,
        },
        "model-router".to_string(),
        interaction.id.as_str().to_string(),
        interaction.continuation.as_str().to_string(),
        interaction
            .state_revision
            .as_ref()
            .map(|revision| revision.as_str().to_string()),
    ))
}

pub(crate) fn interaction_response(
    response: xedoc_protocol::protocol::ScriptedInteractionResponse,
) -> xedoc_script_protocol::InteractionResponse {
    xedoc_script_protocol::InteractionResponse {
        continuation: OpaqueId::new(response.continuation),
        interaction_id: OpaqueId::new(response.interaction_id),
        state_revision: response.state_revision.map(OpaqueId::new),
        outcome: match response.outcome {
            xedoc_protocol::protocol::ScriptedInteractionOutcome::Accepted => {
                xedoc_script_protocol::InteractionOutcome::Accepted
            }
            xedoc_protocol::protocol::ScriptedInteractionOutcome::Cancelled => {
                xedoc_script_protocol::InteractionOutcome::Cancelled
            }
            xedoc_protocol::protocol::ScriptedInteractionOutcome::Dismissed => {
                xedoc_script_protocol::InteractionOutcome::Dismissed
            }
        },
        action: response
            .action
            .map(|action| xedoc_script_protocol::SelectedAction {
                id: OpaqueId::new(action.id),
            }),
        values: response.values,
    }
}

/// Converts validated script metadata into the compact router feedback event.
pub(crate) fn decision_event(
    decision: ModelRouterScriptDecision,
    effective_route: &Route,
    thread_id: String,
    turn_id: String,
    scope: xedoc_protocol::protocol::ModelRouterScope,
    applied: bool,
    failure: Option<&ModelRouterScriptFailure>,
) -> xedoc_protocol::protocol::ModelRouterDecisionEvent {
    let proposed_route = decision.proposed_route.as_ref().unwrap_or(effective_route);
    let feedback_visible = decision.feedback.enabled;
    let mut classifications = BTreeMap::new();
    classifications.insert(
        "classification".to_string(),
        decision.feedback.classification,
    );
    classifications.insert(
        "routing calculation".to_string(),
        decision.feedback.routing_calculation,
    );
    xedoc_protocol::protocol::ModelRouterDecisionEvent {
        decision_id: decision.id.as_str().to_string(),
        thread_id,
        turn_id,
        scope,
        disposition: match (applied, decision.disposition, failure) {
            (true, _, _) => xedoc_protocol::protocol::ModelRouterDisposition::Applied,
            (false, ModelRouterScriptDecisionDisposition::Shadow, None) => {
                xedoc_protocol::protocol::ModelRouterDisposition::Shadow
            }
            (false, _, _) => xedoc_protocol::protocol::ModelRouterDisposition::Fallback,
        },
        reason: xedoc_protocol::protocol::ModelRouterDecisionReason::Classified,
        feedback_visible,
        diagnostic: failure.map(ModelRouterScriptFailure::diagnostic),
        summary: decision.summary,
        policy_revision: "script".to_string(),
        classifications,
        confidence_score: decision.feedback.confidence,
        confidence_margin: decision.feedback.confidence_margin,
        ranking_score: decision.feedback.ranking_score,
        ranking_minimum_class: decision.feedback.ranking_minimum_class,
        ranking_maximum_class: decision.feedback.ranking_maximum_class,
        ranking_minimum_rank: decision.feedback.ranking_minimum_rank,
        ranking_maximum_rank: decision.feedback.ranking_maximum_rank,
        ranking_target_rank: decision.feedback.ranking_target_rank,
        ranking_selected_rank: decision.feedback.ranking_selected_rank,
        proposed_provider_id: proposed_route.provider_id.as_str().to_string(),
        proposed_model_slug: proposed_route.model.as_str().to_string(),
        proposed_reasoning_effort: proposed_route.reasoning_effort.as_str().to_string(),
        effective_route: xedoc_protocol::protocol::ModelRouterEffectiveRoute::Available {
            provider_id: effective_route.provider_id.as_str().to_string(),
            model_slug: effective_route.model.as_str().to_string(),
            reasoning_effort: effective_route.reasoning_effort.as_str().to_string(),
        },
        prompt_sha256: String::new(),
        prompt_original_bytes: 0,
        prompt_truncated: false,
        created_at: crate::turn_timing::now_unix_timestamp_ms() / 1_000,
    }
}

/// Validated result of `routing.decide`.
pub(crate) enum ModelRouterScriptDecisionOutcome {
    /// The host may apply this route while the turn is mutable.
    Apply {
        /// Script decision identifier and summary.
        decision: ModelRouterScriptDecision,
        /// Host-eligible route proposed by the script.
        route: Route,
    },
    /// The caller must preserve its current route.
    KeepCurrent {
        /// Valid script decision when the script intentionally retained the route.
        decision: Option<ModelRouterScriptDecision>,
        /// Prompt-free failure that forced the host fallback.
        failure: Option<ModelRouterScriptFailure>,
    },
    /// The caller must render a validated blocking interaction.
    Interaction(Interaction),
}

/// Validated outcome of `interaction.respond`.
pub(crate) enum ModelRouterScriptInteractionOutcome {
    /// The host should replace the current surface.
    Interaction(Interaction),
    /// The host may apply a validated route.
    Apply {
        /// Script decision identifier and summary.
        decision: ModelRouterScriptDecision,
        /// Host-eligible route proposed by the script.
        route: Route,
    },
    /// The host must preserve the current route.
    KeepCurrent {
        /// Valid script decision when the script intentionally retained the route.
        decision: Option<ModelRouterScriptDecision>,
        /// Prompt-free failure that forced the host fallback.
        failure: Option<ModelRouterScriptFailure>,
    },
    /// The renderer should retain its last valid surface.
    Failure(ModelRouterScriptFailure),
}

/// A settings surface guaranteed to carry a script state revision.
pub(crate) struct ModelRouterScriptSettingsInteraction {
    /// Declarative script-owned settings surface.
    pub(crate) interaction: Interaction,
    /// Script-requested state that the host applies only to the current session.
    pub(crate) session_update: Option<xedoc_script_protocol::SessionUpdate>,
}

impl ModelRouterScriptDecisionOutcome {
    fn fallback(failure: ModelRouterScriptFailure) -> Self {
        Self::KeepCurrent {
            decision: None,
            failure: Some(failure),
        }
    }
}

/// Prompt-free data emitted with a validated script decision.
pub(crate) struct ModelRouterScriptDecision {
    /// Script-generated decision identifier.
    pub(crate) id: OpaqueId,
    /// Script-requested host disposition.
    pub(crate) disposition: ModelRouterScriptDecisionDisposition,
    /// Bounded prompt-free script summary.
    pub(crate) summary: Option<String>,
    /// Prompt-free script-provided feedback details.
    pub(crate) feedback: RouteFeedback,
    /// Script-owned route used to normalize cost reporting.
    pub(crate) reporting_baseline: Option<Route>,
    /// Route the script proposed, if any.
    pub(crate) proposed_route: Option<Route>,
}

/// Host-validated disposition attached to a scripted decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelRouterScriptDecisionDisposition {
    /// The host may apply the proposed route.
    Apply,
    /// The host retains the current route and reports the proposed route.
    Shadow,
    /// The script intentionally retained the current route without a proposal.
    KeepCurrent,
}

/// Prompt-free reason an interaction or routing invocation could not proceed.
#[derive(Debug)]
pub(crate) enum ModelRouterScriptFailure {
    /// The script could not complete its process invocation.
    Invocation(SubprocessFailure),
    /// The script returned an explicit structured error.
    ScriptError {
        /// Stable script-defined code.
        code: String,
        /// Bounded, control-safe message for the interaction error surface.
        message: String,
    },
    /// A settings interaction omitted its required state revision.
    MissingStateRevision,
    /// The script returned a result not allowed for the requested method.
    UnexpectedResult,
    /// The script proposed a route not in the supplied eligible catalog.
    IneligibleRoute,
    /// The script proposed an ineligible reporting baseline.
    IneligibleReportingBaseline,
    /// The script tried to change an immutable route.
    ImmutableRoute,
    /// The script returned an invalid constrained interaction.
    InvalidInteraction,
    /// The script returned invalid prompt-free routing feedback.
    InvalidFeedback,
    /// The script returned an invalid router policy snapshot.
    InvalidState,
    /// The script returned an invalid prompt-free decision summary.
    InvalidSummary,
    /// The script requested a malformed or unsupported classifier invocation.
    InvalidClassifierRequest,
    /// The configured model classifier could not return a response.
    ClassifierInvocation { detail: String },
    /// The active turn cancelled the classifier request.
    ClassifierCancelled,
    /// The host could not encode its interaction response.
    Encode,
}

impl ModelRouterScriptFailure {
    /// Returns a prompt-free causal diagnostic for host logs.
    #[must_use]
    pub(crate) fn diagnostic(&self) -> String {
        match self {
            Self::Invocation(failure) => failure.diagnostic.summary(&failure.kind),
            Self::ScriptError { code, .. } => format!("router script error: {code}"),
            Self::MissingStateRevision => "router interaction omitted state revision".to_string(),
            Self::UnexpectedResult => "router script returned an unexpected result".to_string(),
            Self::IneligibleRoute => "router script proposed an ineligible route".to_string(),
            Self::IneligibleReportingBaseline => {
                "router script proposed an ineligible reporting baseline".to_string()
            }
            Self::ImmutableRoute => {
                "router script attempted to mutate an immutable route".to_string()
            }
            Self::InvalidInteraction => "router script returned an invalid interaction".to_string(),
            Self::InvalidFeedback => "router script returned invalid routing feedback".to_string(),
            Self::InvalidState => "router script returned invalid router state".to_string(),
            Self::InvalidSummary => {
                "router script returned an invalid decision summary".to_string()
            }
            Self::InvalidClassifierRequest => {
                "router script returned an invalid classifier request".to_string()
            }
            Self::ClassifierInvocation { detail } => {
                format!("router classifier invocation failed: {detail}")
            }
            Self::ClassifierCancelled => "router classifier invocation cancelled".to_string(),
            Self::Encode => "router interaction could not be encoded".to_string(),
        }
    }

    /// Returns the safe script message suitable for an interaction error surface.
    #[must_use]
    pub(crate) fn interaction_message(&self) -> Option<&str> {
        match self {
            Self::ScriptError { message, .. } => Some(message),
            Self::Invocation(_)
            | Self::MissingStateRevision
            | Self::UnexpectedResult
            | Self::IneligibleRoute
            | Self::IneligibleReportingBaseline
            | Self::ImmutableRoute
            | Self::InvalidInteraction
            | Self::InvalidFeedback
            | Self::InvalidState
            | Self::InvalidSummary
            | Self::InvalidClassifierRequest
            | Self::ClassifierInvocation { .. }
            | Self::ClassifierCancelled
            | Self::Encode => None,
        }
    }
}

fn validate_decision(
    RouteDecision {
        id,
        disposition,
        route,
        reporting_baseline,
        summary,
        feedback,
    }: RouteDecision,
    eligible_routes: &[EligibleRoute],
    route_mutable: bool,
) -> ModelRouterScriptDecisionOutcome {
    let feedback = match validate_feedback(feedback) {
        Ok(feedback) => feedback,
        Err(failure) => return ModelRouterScriptDecisionOutcome::fallback(failure),
    };
    let summary = match validate_summary(summary) {
        Ok(summary) => summary,
        Err(failure) => return ModelRouterScriptDecisionOutcome::fallback(failure),
    };
    if reporting_baseline
        .as_ref()
        .is_some_and(|route| !route_is_eligible(route, eligible_routes))
    {
        return ModelRouterScriptDecisionOutcome::fallback(
            ModelRouterScriptFailure::IneligibleReportingBaseline,
        );
    }
    match (disposition, route) {
        (RouteDisposition::KeepCurrent, None) => ModelRouterScriptDecisionOutcome::KeepCurrent {
            decision: Some(ModelRouterScriptDecision {
                id,
                disposition: ModelRouterScriptDecisionDisposition::KeepCurrent,
                summary,
                feedback,
                reporting_baseline,
                proposed_route: None,
            }),
            failure: None,
        },
        (RouteDisposition::KeepCurrent, Some(_)) => {
            ModelRouterScriptDecisionOutcome::fallback(ModelRouterScriptFailure::UnexpectedResult)
        }
        (RouteDisposition::Shadow, None) => {
            ModelRouterScriptDecisionOutcome::fallback(ModelRouterScriptFailure::UnexpectedResult)
        }
        (RouteDisposition::Shadow, Some(route)) if !route_is_eligible(&route, eligible_routes) => {
            ModelRouterScriptDecisionOutcome::fallback(ModelRouterScriptFailure::IneligibleRoute)
        }
        (RouteDisposition::Shadow, Some(route)) => ModelRouterScriptDecisionOutcome::KeepCurrent {
            decision: Some(ModelRouterScriptDecision {
                id,
                disposition: ModelRouterScriptDecisionDisposition::Shadow,
                summary,
                feedback,
                reporting_baseline,
                proposed_route: Some(route),
            }),
            failure: None,
        },
        (RouteDisposition::Apply, None) => {
            ModelRouterScriptDecisionOutcome::fallback(ModelRouterScriptFailure::UnexpectedResult)
        }
        (RouteDisposition::Apply, Some(route)) if !route_mutable => {
            ModelRouterScriptDecisionOutcome::KeepCurrent {
                decision: Some(ModelRouterScriptDecision {
                    id,
                    disposition: ModelRouterScriptDecisionDisposition::Apply,
                    summary,
                    feedback,
                    reporting_baseline,
                    proposed_route: Some(route),
                }),
                failure: Some(ModelRouterScriptFailure::ImmutableRoute),
            }
        }
        (RouteDisposition::Apply, Some(route)) if !route_is_eligible(&route, eligible_routes) => {
            ModelRouterScriptDecisionOutcome::fallback(ModelRouterScriptFailure::IneligibleRoute)
        }
        (RouteDisposition::Apply, Some(route)) => {
            let decision = ModelRouterScriptDecision {
                id,
                disposition: ModelRouterScriptDecisionDisposition::Apply,
                summary,
                feedback,
                reporting_baseline,
                proposed_route: Some(route.clone()),
            };
            ModelRouterScriptDecisionOutcome::Apply { decision, route }
        }
    }
}

fn validate_summary(summary: Option<String>) -> Result<Option<String>, ModelRouterScriptFailure> {
    let Some(mut summary) = summary else {
        return Ok(None);
    };
    if summary
        .chars()
        .any(|character| character.is_control() && character != '\n')
    {
        return Err(ModelRouterScriptFailure::InvalidSummary);
    }
    truncate_utf8(&mut summary, MAX_SCRIPT_SUMMARY_BYTES);
    if summary.trim().is_empty() {
        return Err(ModelRouterScriptFailure::InvalidSummary);
    }
    Ok(Some(summary))
}

fn validate_feedback(
    mut feedback: RouteFeedback,
) -> Result<RouteFeedback, ModelRouterScriptFailure> {
    if !feedback.confidence.is_finite()
        || !feedback.confidence_margin.is_finite()
        || !(0.0..=1.0).contains(&feedback.confidence)
        || !(0.0..=1.0).contains(&feedback.confidence_margin)
    {
        return Err(ModelRouterScriptFailure::InvalidFeedback);
    }
    for value in [
        &mut feedback.classification,
        &mut feedback.routing_calculation,
    ] {
        if value.chars().any(char::is_control) || value.trim().is_empty() {
            return Err(ModelRouterScriptFailure::InvalidFeedback);
        }
        truncate_utf8(value, MAX_SCRIPT_FEEDBACK_TEXT_BYTES);
    }
    for class in [
        feedback.ranking_minimum_class.as_mut(),
        feedback.ranking_maximum_class.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        if class.chars().any(char::is_control) || class.trim().is_empty() {
            return Err(ModelRouterScriptFailure::InvalidFeedback);
        }
        truncate_utf8(class, MAX_SCRIPT_FEEDBACK_TEXT_BYTES);
    }
    Ok(feedback)
}

fn validate_router_state(state: RouterState) -> Result<RouterState, ModelRouterScriptFailure> {
    if !matches!(
        state.mode.as_str(),
        "off" | "shadow-subagents" | "shadow-full" | "subagents" | "full"
    ) || !matches!(
        state.approval.as_str(),
        "off" | "changes" | "all" | "not-confident"
    ) || !matches!(
        state.similarity_preset.as_str(),
        "strict" | "balanced" | "permissive"
    ) || !valid_router_state_text(&state.policy_revision, MAX_ROUTER_STATE_REVISION_BYTES)
        || !state
            .classifier_route
            .as_ref()
            .is_none_or(valid_router_state_route)
        || !state
            .reporting_baseline
            .as_ref()
            .is_none_or(valid_router_state_route)
    {
        return Err(ModelRouterScriptFailure::InvalidState);
    }
    Ok(state)
}

fn valid_router_state_route(route: &Route) -> bool {
    [
        route.provider_id.as_str(),
        route.model.as_str(),
        route.reasoning_effort.as_str(),
    ]
    .into_iter()
    .all(|value| valid_router_state_text(value, MAX_ROUTER_STATE_IDENTIFIER_BYTES))
}

fn valid_router_state_text(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= maximum_bytes && !value.chars().any(char::is_control)
}

fn direct_exec_argv(argv: Vec<String>) -> Vec<OsString> {
    argv.into_iter()
        .enumerate()
        .map(|(index, value)| {
            if index == 0 {
                expand_home_directory(value)
            } else {
                OsString::from(value)
            }
        })
        .collect()
}

fn expand_home_directory(program: String) -> OsString {
    let Some(home) = home_dir() else {
        return OsString::from(program);
    };
    if program == "~" {
        return home.into_os_string();
    }
    let Some(suffix) = program.strip_prefix("~/") else {
        return OsString::from(program);
    };
    home.join(suffix).into_os_string()
}

fn require_state_revision(interaction: &Interaction) -> Result<(), ModelRouterScriptFailure> {
    interaction
        .state_revision
        .as_ref()
        .map(|_| ())
        .ok_or(ModelRouterScriptFailure::MissingStateRevision)
}

fn script_error_failure(error: xedoc_script_protocol::ScriptError) -> ModelRouterScriptFailure {
    ModelRouterScriptFailure::ScriptError {
        code: error.code,
        message: safe_script_error_message(error.message),
    }
}

fn classifier_invocation_failure(detail: String) -> ModelRouterScriptFailure {
    ModelRouterScriptFailure::ClassifierInvocation {
        detail: safe_script_error_message(detail),
    }
}

fn safe_script_error_message(message: String) -> String {
    let mut safe = message
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>();
    truncate_utf8(&mut safe, MAX_SCRIPT_ERROR_MESSAGE_BYTES);
    if safe.trim().is_empty() {
        "Router script reported an error.".to_string()
    } else {
        safe
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

#[cfg(test)]
#[path = "model_router_script_host_tests.rs"]
mod tests;

fn validate_interaction(
    interaction: &Interaction,
    eligible_routes: &[EligibleRoute],
) -> Result<(), ModelRouterScriptFailure> {
    validate_opaque_identifier(&interaction.id)?;
    validate_opaque_continuation(&interaction.continuation)?;
    if let Some(state_revision) = interaction.state_revision.as_ref() {
        validate_opaque_identifier(state_revision)?;
    }
    if let Some(session_update) = interaction.session_update.as_ref()
        && let Some(mode) = session_update.router_mode.as_ref()
    {
        validate_opaque_identifier(mode)?;
    }
    match &interaction.surface {
        InteractionSurface::Menu(menu) => {
            validate_plain_text(&menu.title, MAX_INTERACTION_TITLE_BYTES)?;
            validate_optional_plain_text(
                menu.subtitle.as_deref(),
                MAX_INTERACTION_DESCRIPTION_BYTES,
            )?;
            validate_collection_len(menu.items.len(), MAX_INTERACTION_ITEMS)?;
            menu.items.iter().try_for_each(validate_menu_item)?;
            validate_unique_identifiers(menu.items.iter().map(|item| item.id.as_str()))?;
            validate_actions_do_not_open_nested_surfaces(
                menu.items.iter().map(|item| &item.action),
            )?;
            if menu.items.iter().any(|item| {
                item.disabled != Some(true) && action_has_renderable_binding(&item.action)
            }) {
                Ok(())
            } else {
                Err(ModelRouterScriptFailure::InvalidInteraction)
            }
        }
        InteractionSurface::Form(form) => {
            validate_form_structure(form)?;
            validate_actions_do_not_open_nested_surfaces(
                std::iter::once(&form.submit)
                    .chain(form.cancel.as_ref())
                    .chain(form.fields.iter().filter_map(form_field_action)),
            )?;
            validate_form_routes(&form.fields, eligible_routes)?;
            validate_form_reachability(form)
        }
        InteractionSurface::Confirmation(confirmation) => {
            validate_plain_text(&confirmation.title, MAX_INTERACTION_TITLE_BYTES)?;
            validate_plain_text(&confirmation.body, MAX_INTERACTION_TEXT_BYTES)?;
            validate_collection_len(confirmation.details.len(), MAX_INTERACTION_DETAILS)?;
            confirmation.details.iter().try_for_each(|detail| {
                validate_plain_text(&detail.label, MAX_INTERACTION_TITLE_BYTES)?;
                validate_plain_text(&detail.value, MAX_INTERACTION_TEXT_BYTES)
            })?;
            validate_collection_len(confirmation.sections.len(), MAX_INTERACTION_SECTIONS)?;
            confirmation.sections.iter().try_fold(
                (0usize, 0usize),
                |(section_text_bytes, section_rows), section| {
                    validate_optional_plain_text(
                        section.title.as_deref(),
                        MAX_INTERACTION_TITLE_BYTES,
                    )?;
                    validate_collection_len(section.rows.len(), MAX_INTERACTION_SECTION_ROWS)?;
                    section.rows.iter().try_fold(
                        (section_text_bytes, section_rows),
                        |(text_bytes, row_count), row| {
                            validate_plain_text(&row.text, MAX_INTERACTION_SECTION_ROW_BYTES)?;
                            if row.indent > MAX_INTERACTION_INDENT {
                                return Err(ModelRouterScriptFailure::InvalidInteraction);
                            }
                            let text_bytes = text_bytes.saturating_add(row.text.len());
                            let row_count = row_count.saturating_add(1);
                            if text_bytes > MAX_INTERACTION_SECTION_TEXT_BYTES
                                || row_count > MAX_INTERACTION_SECTION_ROWS_TOTAL
                            {
                                return Err(ModelRouterScriptFailure::InvalidInteraction);
                            }
                            Ok((text_bytes, row_count))
                        },
                    )
                },
            )?;
            validate_collection_len(confirmation.actions.len(), MAX_INTERACTION_ACTIONS)?;
            confirmation.actions.iter().try_for_each(validate_action)?;
            validate_unique_identifiers(
                confirmation.actions.iter().map(|action| action.id.as_str()),
            )?;
            if !confirmation.actions.iter().all(|action| {
                action.opens.as_ref().is_none_or(|target| {
                    confirmation
                        .override_form
                        .as_ref()
                        .is_some_and(|form| target == &form.id)
                })
            }) {
                return Err(ModelRouterScriptFailure::InvalidInteraction);
            }
            if let Some(form) = confirmation.override_form.as_ref() {
                validate_form_structure(form)?;
                validate_actions_do_not_open_nested_surfaces(
                    std::iter::once(&form.submit)
                        .chain(form.cancel.as_ref())
                        .chain(form.fields.iter().filter_map(form_field_action)),
                )?;
                validate_form_routes(&form.fields, eligible_routes)?;
                validate_form_reachability(form)?;
            }
            if confirmation
                .actions
                .iter()
                .any(action_has_renderable_binding)
            {
                Ok(())
            } else {
                Err(ModelRouterScriptFailure::InvalidInteraction)
            }
        }
        InteractionSurface::Notice(notice) => {
            validate_plain_text(&notice.title, MAX_INTERACTION_TITLE_BYTES)?;
            validate_plain_text(&notice.body, MAX_INTERACTION_TEXT_BYTES)
        }
    }
}

fn validate_menu_item(
    item: &xedoc_script_protocol::MenuItem,
) -> Result<(), ModelRouterScriptFailure> {
    validate_opaque_identifier(&item.id)?;
    validate_plain_text(&item.label, MAX_INTERACTION_TITLE_BYTES)?;
    validate_optional_plain_text(
        item.description.as_deref(),
        MAX_INTERACTION_DESCRIPTION_BYTES,
    )?;
    validate_action(&item.action)
}

fn validate_form_structure(
    form: &xedoc_script_protocol::FormSurface,
) -> Result<(), ModelRouterScriptFailure> {
    validate_opaque_identifier(&form.id)?;
    validate_plain_text(&form.title, MAX_INTERACTION_TITLE_BYTES)?;
    validate_optional_plain_text(form.subtitle.as_deref(), MAX_INTERACTION_DESCRIPTION_BYTES)?;
    validate_collection_len(form.fields.len(), MAX_INTERACTION_FIELDS)?;
    form.fields.iter().try_for_each(validate_form_field)?;
    validate_unique_identifiers(form.fields.iter().map(form_field_id))?;
    validate_action(&form.submit)?;
    form.cancel.as_ref().map_or(Ok(()), validate_action)
}

fn validate_form_field(
    field: &xedoc_script_protocol::FormField,
) -> Result<(), ModelRouterScriptFailure> {
    match field {
        xedoc_script_protocol::FormField::Select {
            id,
            label,
            description,
            value,
            current,
            options,
        } => {
            validate_opaque_identifier(id)?;
            validate_plain_text(label, MAX_INTERACTION_TITLE_BYTES)?;
            validate_optional_plain_text(
                description.as_deref(),
                MAX_INTERACTION_DESCRIPTION_BYTES,
            )?;
            value.as_ref().map_or(Ok(()), validate_opaque_identifier)?;
            current
                .as_ref()
                .map_or(Ok(()), validate_opaque_identifier)?;
            validate_collection_len(options.len(), MAX_INTERACTION_OPTIONS)?;
            options.iter().try_for_each(validate_select_option)?;
            validate_unique_identifiers(options.iter().map(|option| option.id.as_str()))?;
            if current
                .as_ref()
                .is_none_or(|current| options.iter().any(|option| option.id == *current))
            {
                Ok(())
            } else {
                Err(ModelRouterScriptFailure::InvalidInteraction)
            }
        }
        xedoc_script_protocol::FormField::Boolean {
            id,
            label,
            description,
            ..
        } => {
            validate_opaque_identifier(id)?;
            validate_plain_text(label, MAX_INTERACTION_TITLE_BYTES)?;
            validate_optional_plain_text(description.as_deref(), MAX_INTERACTION_DESCRIPTION_BYTES)
        }
        xedoc_script_protocol::FormField::ModelRoute {
            id,
            label,
            description,
            eligible_routes,
            ..
        } => {
            validate_opaque_identifier(id)?;
            validate_plain_text(label, MAX_INTERACTION_TITLE_BYTES)?;
            validate_optional_plain_text(
                description.as_deref(),
                MAX_INTERACTION_DESCRIPTION_BYTES,
            )?;
            validate_collection_len(eligible_routes.len(), MAX_INTERACTION_OPTIONS)?;
            if eligible_routes
                .iter()
                .any(|route| route.reasoning_efforts.len() > MAX_MODEL_ROUTE_REASONING_EFFORTS)
            {
                return Err(ModelRouterScriptFailure::InvalidInteraction);
            }
            Ok(())
        }
        xedoc_script_protocol::FormField::Text {
            id,
            label,
            description,
            value,
            sensitive,
            max_bytes,
        } => {
            validate_opaque_identifier(id)?;
            validate_plain_text(label, MAX_INTERACTION_TITLE_BYTES)?;
            validate_optional_plain_text(
                description.as_deref(),
                MAX_INTERACTION_DESCRIPTION_BYTES,
            )?;
            if *max_bytes == 0 || *max_bytes > MAX_INTERACTION_TEXT_FIELD_BYTES {
                return Err(ModelRouterScriptFailure::InvalidInteraction);
            }
            if *sensitive && !value.is_empty() {
                return Err(ModelRouterScriptFailure::InvalidInteraction);
            }
            validate_plain_text(value, usize::try_from(*max_bytes).unwrap_or(usize::MAX))
        }
        xedoc_script_protocol::FormField::Action {
            id,
            label,
            description,
            action,
        } => {
            validate_opaque_identifier(id)?;
            validate_plain_text(label, MAX_INTERACTION_TITLE_BYTES)?;
            validate_optional_plain_text(
                description.as_deref(),
                MAX_INTERACTION_DESCRIPTION_BYTES,
            )?;
            validate_action(action)
        }
    }
}

fn validate_select_option(
    option: &xedoc_script_protocol::SelectOption,
) -> Result<(), ModelRouterScriptFailure> {
    validate_opaque_identifier(&option.id)?;
    validate_plain_text(&option.label, MAX_INTERACTION_TITLE_BYTES)?;
    validate_optional_plain_text(
        option.description.as_deref(),
        MAX_INTERACTION_DESCRIPTION_BYTES,
    )
}

fn validate_action(action: &xedoc_script_protocol::Action) -> Result<(), ModelRouterScriptFailure> {
    validate_opaque_identifier(&action.id)?;
    action
        .opens
        .as_ref()
        .map_or(Ok(()), validate_opaque_identifier)?;
    validate_optional_plain_text(action.label.as_deref(), MAX_INTERACTION_TITLE_BYTES)?;
    validate_optional_plain_text(action.context.as_deref(), MAX_INTERACTION_DESCRIPTION_BYTES)?;
    validate_collection_len(action.key_bindings.len(), MAX_INTERACTION_KEY_BINDINGS)?;
    action
        .key_bindings
        .iter()
        .try_for_each(|binding| validate_key_binding(binding))?;
    if let Some(value) = action.value.as_ref() {
        validate_action_value(value)?;
    }
    Ok(())
}

fn validate_opaque_identifier(value: &OpaqueId) -> Result<(), ModelRouterScriptFailure> {
    let value = value.as_str();
    if value.is_empty()
        || value.len() > MAX_INTERACTION_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || b"._:-".contains(&character))
    {
        return Err(ModelRouterScriptFailure::InvalidInteraction);
    }
    Ok(())
}

fn validate_opaque_continuation(value: &OpaqueId) -> Result<(), ModelRouterScriptFailure> {
    let value = value.as_str();
    if value.is_empty()
        || value.len() > MAX_INTERACTION_CONTINUATION_BYTES
        || !value
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || b"._:-=".contains(&character))
    {
        return Err(ModelRouterScriptFailure::InvalidInteraction);
    }
    Ok(())
}

fn validate_optional_plain_text(
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), ModelRouterScriptFailure> {
    value.map_or(Ok(()), |value| validate_plain_text(value, max_bytes))
}

fn validate_plain_text(value: &str, max_bytes: usize) -> Result<(), ModelRouterScriptFailure> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(ModelRouterScriptFailure::InvalidInteraction);
    }
    Ok(())
}

fn validate_collection_len(len: usize, maximum: usize) -> Result<(), ModelRouterScriptFailure> {
    if len > maximum {
        Err(ModelRouterScriptFailure::InvalidInteraction)
    } else {
        Ok(())
    }
}

fn validate_unique_identifiers<'a>(
    mut identifiers: impl Iterator<Item = &'a str>,
) -> Result<(), ModelRouterScriptFailure> {
    let mut seen = std::collections::HashSet::new();
    if identifiers.all(|identifier| seen.insert(identifier)) {
        Ok(())
    } else {
        Err(ModelRouterScriptFailure::InvalidInteraction)
    }
}

fn form_field_id(field: &xedoc_script_protocol::FormField) -> &str {
    match field {
        xedoc_script_protocol::FormField::Select { id, .. }
        | xedoc_script_protocol::FormField::Boolean { id, .. }
        | xedoc_script_protocol::FormField::Text { id, .. }
        | xedoc_script_protocol::FormField::ModelRoute { id, .. }
        | xedoc_script_protocol::FormField::Action { id, .. } => id.as_str(),
    }
}

fn validate_key_binding(binding: &str) -> Result<(), ModelRouterScriptFailure> {
    if binding.is_empty() || binding.len() > 32 || binding.chars().any(char::is_control) {
        return Err(ModelRouterScriptFailure::InvalidInteraction);
    }
    let mut parts = binding.split('-');
    let mut key = loop {
        let Some(part) = parts.next() else {
            return Err(ModelRouterScriptFailure::InvalidInteraction);
        };
        if matches!(part, "ctrl" | "control" | "alt" | "option" | "shift") {
        } else {
            break part.to_string();
        }
    };
    for part in parts {
        key.push('-');
        key.push_str(part);
    }
    let valid = matches!(
        key.as_str(),
        "enter"
            | "return"
            | "tab"
            | "backspace"
            | "esc"
            | "escape"
            | "delete"
            | "del"
            | "up"
            | "down"
            | "left"
            | "right"
            | "home"
            | "end"
            | "page-up"
            | "pageup"
            | "pgup"
            | "page-down"
            | "pagedown"
            | "pgdn"
            | "space"
            | "spacebar"
            | "minus"
    ) || (key.len() == 1 && key.is_ascii())
        || key
            .strip_prefix('f')
            .and_then(|number| number.parse::<u8>().ok())
            .is_some_and(|number| (1..=24).contains(&number));
    if valid {
        Ok(())
    } else {
        Err(ModelRouterScriptFailure::InvalidInteraction)
    }
}

fn validate_action_value(value: &Value) -> Result<(), ModelRouterScriptFailure> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| ModelRouterScriptFailure::InvalidInteraction)?;
    if bytes.len() > MAX_INTERACTION_ACTION_VALUE_BYTES {
        return Err(ModelRouterScriptFailure::InvalidInteraction);
    }
    validate_json_text(value, /*depth*/ 0)
}

fn validate_json_text(value: &Value, depth: usize) -> Result<(), ModelRouterScriptFailure> {
    if depth > MAX_INTERACTION_JSON_DEPTH {
        return Err(ModelRouterScriptFailure::InvalidInteraction);
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) => validate_plain_text(value, MAX_INTERACTION_TEXT_BYTES),
        Value::Array(values) => {
            validate_collection_len(values.len(), MAX_INTERACTION_OPTIONS)?;
            values
                .iter()
                .try_for_each(|value| validate_json_text(value, depth + 1))
        }
        Value::Object(values) => {
            validate_collection_len(values.len(), MAX_INTERACTION_OPTIONS)?;
            values.iter().try_for_each(|(key, value)| {
                validate_plain_text(key, MAX_INTERACTION_IDENTIFIER_BYTES)?;
                validate_json_text(value, depth + 1)
            })
        }
    }
}

fn validate_actions_do_not_open_nested_surfaces<'a>(
    mut actions: impl Iterator<Item = &'a xedoc_script_protocol::Action>,
) -> Result<(), ModelRouterScriptFailure> {
    if actions.all(|action| action.opens.is_none()) {
        Ok(())
    } else {
        Err(ModelRouterScriptFailure::InvalidInteraction)
    }
}

fn validate_form_reachability(
    form: &xedoc_script_protocol::FormSurface,
) -> Result<(), ModelRouterScriptFailure> {
    if action_has_renderable_binding(&form.submit)
        || form
            .cancel
            .as_ref()
            .is_some_and(action_has_renderable_binding)
        || form
            .fields
            .iter()
            .filter_map(form_field_action)
            .any(action_has_renderable_binding)
    {
        Ok(())
    } else {
        Err(ModelRouterScriptFailure::InvalidInteraction)
    }
}

fn action_has_renderable_binding(action: &xedoc_script_protocol::Action) -> bool {
    // `validate_action` has already admitted only bindings handled by
    // `ScriptedInteractionView::key_binding_matches`.
    !action.key_bindings.is_empty()
}

fn validate_form_routes(
    fields: &[xedoc_script_protocol::FormField],
    eligible_routes: &[EligibleRoute],
) -> Result<(), ModelRouterScriptFailure> {
    fields
        .iter()
        .filter_map(|field| match field {
            xedoc_script_protocol::FormField::ModelRoute {
                value,
                eligible_routes: form_eligible_routes,
                ..
            } => Some((value, form_eligible_routes)),
            xedoc_script_protocol::FormField::Select { .. }
            | xedoc_script_protocol::FormField::Boolean { .. }
            | xedoc_script_protocol::FormField::Text { .. }
            | xedoc_script_protocol::FormField::Action { .. } => None,
        })
        .try_for_each(|(value, form_eligible_routes)| {
            if form_eligible_routes
                .iter()
                .all(|route| route_is_eligible_route(route, eligible_routes))
                && value
                    .as_ref()
                    .is_none_or(|route| route_is_eligible(route, eligible_routes))
            {
                Ok(())
            } else {
                Err(ModelRouterScriptFailure::InvalidInteraction)
            }
        })
}

fn form_field_action(
    field: &xedoc_script_protocol::FormField,
) -> Option<&xedoc_script_protocol::Action> {
    match field {
        xedoc_script_protocol::FormField::Action { action, .. } => Some(action),
        xedoc_script_protocol::FormField::Select { .. }
        | xedoc_script_protocol::FormField::Boolean { .. }
        | xedoc_script_protocol::FormField::Text { .. }
        | xedoc_script_protocol::FormField::ModelRoute { .. } => None,
    }
}

fn route_is_eligible(route: &Route, eligible_routes: &[EligibleRoute]) -> bool {
    eligible_routes.iter().any(|eligible| {
        eligible.provider_id == route.provider_id
            && eligible.model == route.model
            && eligible.reasoning_efforts.contains(&route.reasoning_effort)
    })
}

fn route_is_eligible_route(route: &EligibleRoute, eligible_routes: &[EligibleRoute]) -> bool {
    eligible_routes.iter().any(|host_route| {
        host_route.provider_id == route.provider_id
            && host_route.model == route.model
            && route
                .reasoning_efforts
                .iter()
                .all(|effort| host_route.reasoning_efforts.contains(effort))
    })
}
