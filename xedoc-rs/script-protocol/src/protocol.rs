//! JSON request and response shapes for extension scripts.

use crate::Interaction;
use crate::ReportDocument;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

/// The only supported script protocol version.
pub const SCRIPT_PROTOCOL_V1: &str = "xedoc.script/v1";

/// Protocol version carried by every request and response.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolVersion(String);

impl ProtocolVersion {
    /// Creates the supported protocol version.
    #[must_use]
    pub fn v1() -> Self {
        Self(SCRIPT_PROTOCOL_V1.to_string())
    }
}

/// Script-defined identifier that the host must not interpret.
#[derive(Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpaqueId(String);

impl OpaqueId {
    /// Wraps an opaque identifier supplied by the host or script.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrows the opaque identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifier assigned to one host request.
#[derive(Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(OpaqueId);

impl RequestId {
    /// Wraps a host-generated opaque request identifier.
    #[must_use]
    pub fn new(value: OpaqueId) -> Self {
        Self(value)
    }

    /// Borrows the opaque request identifier.
    #[must_use]
    pub fn as_opaque_id(&self) -> &OpaqueId {
        &self.0
    }
}

/// Supported extension capability.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Extension {
    /// Scripted model routing.
    ModelRouter,
    /// A plugin-provided session extension.
    SessionExtension,
}

/// Supported extension method.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Requests a model route for a pending turn.
    #[serde(rename = "routing.decide")]
    RoutingDecide,
    /// Requests the configured router policy state.
    #[serde(rename = "routing.state")]
    RoutingState,
    /// Requests the initial settings interaction.
    #[serde(rename = "settings.open")]
    SettingsOpen,
    /// Submits a response to an interaction.
    #[serde(rename = "interaction.respond")]
    InteractionRespond,
    /// Submits the host-executed classifier result to the router.
    #[serde(rename = "routing.classifier.respond")]
    RoutingClassifierRespond,
    /// Requests a session extension's first-run or reconfiguration interaction.
    #[serde(rename = "extension.setup.open")]
    ExtensionSetupOpen,
    /// Invokes an approved session-extension slash command.
    #[serde(rename = "extension.command.invoke")]
    ExtensionCommandInvoke,
    /// Requests a script-authored report document.
    #[serde(rename = "report.render")]
    ReportRender,
}

/// One JSON document sent from the host to a script.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptRequest {
    /// Protocol version expected by the script.
    pub protocol: ProtocolVersion,
    /// Host-generated identifier echoed by the response.
    pub request_id: RequestId,
    /// Capability addressed by this request.
    pub extension: Extension,
    /// Operation to perform.
    pub method: Method,
    /// Bounded host-owned context for the operation.
    pub context: Value,
    /// Method-specific input.
    pub params: Value,
}

/// One JSON document returned by a script.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptResponse {
    /// Protocol version used by the script.
    pub protocol: ProtocolVersion,
    /// Host request identifier being answered.
    pub request_id: RequestId,
    /// Either a successful result or a structured script error.
    #[serde(flatten)]
    pub outcome: ResponseOutcome,
}

/// Mutually exclusive response payload.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ResponseOutcome {
    /// A successful script result.
    Result {
        /// Result returned by the script.
        result: ScriptResult,
    },
    /// An explicit script error.
    Error {
        /// Error returned by the script.
        error: ScriptError,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireScriptResponse {
    protocol: ProtocolVersion,
    request_id: RequestId,
    result: Option<ScriptResult>,
    error: Option<ScriptError>,
}

impl<'de> Deserialize<'de> for ScriptResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let WireScriptResponse {
            protocol,
            request_id,
            result,
            error,
        } = WireScriptResponse::deserialize(deserializer)?;
        let outcome = match (result, error) {
            (Some(result), None) => ResponseOutcome::Result { result },
            (None, Some(error)) => ResponseOutcome::Error { error },
            (Some(_), Some(_)) => {
                return Err(serde::de::Error::custom(
                    "script response must contain exactly one of result or error",
                ));
            }
            (None, None) => {
                return Err(serde::de::Error::custom(
                    "script response must contain a result or error",
                ));
            }
        };
        Ok(Self {
            protocol,
            request_id,
            outcome,
        })
    }
}

/// Successful script result.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScriptResult {
    /// A routing decision the host must validate before applying.
    Route {
        /// Proposed route decision.
        decision: RouteDecision,
    },
    /// A constrained interaction the host may render.
    Interaction {
        /// Interaction to render.
        interaction: Interaction,
    },
    /// A bounded classifier request the host must execute through Xedoc.
    ClassifierRequest {
        /// Requested classifier invocation.
        classifier: ClassifierRequest,
    },
    /// A bounded router policy snapshot.
    State {
        /// Current script-owned policy state.
        state: RouterState,
    },
    /// A terminal bounded success result with no follow-up interaction.
    Complete {
        /// Safe, possibly multiline summary suitable for the host to show to the user.
        summary: Option<String>,
    },
    /// A bounded user-visible message emitted by a session extension.
    Message {
        /// Presentation level selected by the extension.
        level: ScriptMessageLevel,
        /// Safe message text for the user.
        message: String,
    },
    /// A constrained report document the host may render.
    Report {
        /// Declarative report to render.
        report: ReportDocument,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScriptMessageLevel {
    Info,
    Warning,
    Error,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireScriptResult {
    kind: ScriptResultKind,
    decision: Option<RouteDecision>,
    interaction: Option<Interaction>,
    classifier: Option<ClassifierRequest>,
    state: Option<RouterState>,
    summary: Option<String>,
    level: Option<ScriptMessageLevel>,
    message: Option<String>,
    report: Option<ReportDocument>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum ScriptResultKind {
    Route,
    Interaction,
    ClassifierRequest,
    State,
    Complete,
    Message,
    Report,
}

impl<'de> Deserialize<'de> for ScriptResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let WireScriptResult {
            kind,
            decision,
            interaction,
            classifier,
            state,
            summary,
            level,
            message,
            report,
        } = WireScriptResult::deserialize(deserializer)?;
        match (
            kind,
            decision,
            interaction,
            classifier,
            state,
            summary,
            level,
            message,
            report,
        ) {
            (ScriptResultKind::Route, Some(decision), None, None, None, None, None, None, None) => {
                Ok(Self::Route { decision })
            }
            (
                ScriptResultKind::Interaction,
                None,
                Some(interaction),
                None,
                None,
                None,
                None,
                None,
                None,
            ) => Ok(Self::Interaction { interaction }),
            (
                ScriptResultKind::ClassifierRequest,
                None,
                None,
                Some(classifier),
                None,
                None,
                None,
                None,
                None,
            ) => Ok(Self::ClassifierRequest { classifier }),
            (ScriptResultKind::State, None, None, None, Some(state), None, None, None, None) => {
                Ok(Self::State { state })
            }
            (ScriptResultKind::Complete, None, None, None, None, summary, None, None, None) => {
                Ok(Self::Complete { summary })
            }
            (
                ScriptResultKind::Message,
                None,
                None,
                None,
                None,
                None,
                Some(level),
                Some(message),
                None,
            ) => Ok(Self::Message { level, message }),
            (ScriptResultKind::Report, None, None, None, None, None, None, None, Some(report)) => {
                Ok(Self::Report { report })
            }
            (ScriptResultKind::Route, _, _, _, _, _, _, _, _) => Err(serde::de::Error::custom(
                "route result must contain only a decision",
            )),
            (ScriptResultKind::Interaction, _, _, _, _, _, _, _, _) => Err(
                serde::de::Error::custom("interaction result must contain only an interaction"),
            ),
            (ScriptResultKind::ClassifierRequest, _, _, _, _, _, _, _, _) => Err(
                serde::de::Error::custom("classifier request must contain only a classifier"),
            ),
            (ScriptResultKind::State, _, _, _, _, _, _, _, _) => Err(serde::de::Error::custom(
                "state result must contain only state",
            )),
            (ScriptResultKind::Complete, _, _, _, _, _, _, _, _) => Err(serde::de::Error::custom(
                "complete result must contain only an optional summary",
            )),
            (ScriptResultKind::Message, _, _, _, _, _, _, _, _) => Err(serde::de::Error::custom(
                "message result must contain only a level and message",
            )),
            (ScriptResultKind::Report, _, _, _, _, _, _, _, _) => Err(serde::de::Error::custom(
                "report result must contain only a report",
            )),
        }
    }
}

/// Explicit script error that keeps host state unchanged.
#[derive(Debug, Serialize, Deserialize)]
pub struct ScriptError {
    /// Stable script-defined machine-readable error code.
    pub code: String,
    /// Safe user-facing error summary.
    pub message: String,
}

/// Provider identifier supplied by the host catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    /// Wraps a provider identifier.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrows the provider identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Model identifier supplied by the host catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    /// Wraps a model identifier.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrows the model identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reasoning-effort identifier supplied by the host catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReasoningEffort(String);

impl ReasoningEffort {
    /// Wraps a reasoning-effort identifier.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrows the reasoning-effort identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One provider, model, and reasoning-effort selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Route {
    /// Provider identifier.
    pub provider_id: ProviderId,
    /// Model identifier.
    pub model: ModelId,
    /// Reasoning-effort identifier.
    pub reasoning_effort: ReasoningEffort,
}

/// One bounded model-classifier request returned by a router script.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassifierRequest {
    /// Opaque continuation returned to the script with the classifier output.
    pub continuation: OpaqueId,
    /// Eligible Xedoc model route used for the classifier call.
    pub route: Route,
    /// Bounded classifier prompt authored by the router script.
    pub input: String,
}

/// Script-owned policy state used by the model router.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouterState {
    /// Shared routing mode selected by the script policy.
    pub mode: String,
    /// Shared approval mode selected by the script policy.
    pub approval: String,
    /// Whether router feedback is enabled by the script policy.
    pub feedback: bool,
    /// Shared similarity preset selected by the script policy.
    pub similarity_preset: String,
    /// Xedoc route used for the router's LLM classifier, when configured.
    pub classifier_route: Option<Route>,
    /// Xedoc route used to normalize reporting, when configured.
    pub reporting_baseline: Option<Route>,
    /// Script-owned policy revision.
    pub policy_revision: String,
}

/// One model route eligible for a script to select.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EligibleRoute {
    /// Provider identifier.
    pub provider_id: ProviderId,
    /// Model identifier.
    pub model: ModelId,
    /// Reasoning efforts currently allowed for this provider and model.
    pub reasoning_efforts: Vec<ReasoningEffort>,
}

/// Whether the host should apply a route or retain the current one.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RouteDisposition {
    /// The host may apply the returned route after eligibility checks.
    Apply,
    /// The host must retain its current route while reporting the proposed route.
    Shadow,
    /// The host must retain its current route.
    KeepCurrent,
}

/// Routing decision returned by a script.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteDecision {
    /// Script-defined decision identifier for host diagnostics.
    pub id: OpaqueId,
    /// Requested host disposition.
    pub disposition: RouteDisposition,
    /// Route required when the disposition is `apply`.
    pub route: Option<Route>,
    /// Script-owned route used to normalize cost reporting, when configured.
    #[serde(default)]
    pub reporting_baseline: Option<Route>,
    /// Compact prompt-free decision summary.
    pub summary: Option<String>,
    /// Compact prompt-free routing feedback rendered when the user enables it.
    pub feedback: RouteFeedback,
    /// Optional bounded model-facing guidance selected by the router.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_instructions: Option<String>,
}

/// Bounded prompt-free routing details supplied by a model-router script.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteFeedback {
    /// Whether the host should render this decision feedback in interactive clients.
    pub enabled: bool,
    /// Script-defined task classification.
    pub classification: String,
    /// Script confidence in the classification, from zero through one.
    pub confidence: f32,
    /// Script margin between its selected classification and runner-up, from zero through one.
    pub confidence_margin: f32,
    /// Compact script-defined routing or rating calculation.
    pub routing_calculation: String,
    /// Script-defined ranking score when ranking selected a candidate.
    pub ranking_score: Option<u16>,
    /// Script-defined lowest model class compatible with the classified work.
    pub ranking_minimum_class: Option<String>,
    /// Script-defined highest model class compatible with the classified work.
    pub ranking_maximum_class: Option<String>,
    /// Script-defined lowest eligible ranking ladder position.
    pub ranking_minimum_rank: Option<u16>,
    /// Script-defined highest eligible ranking ladder position.
    pub ranking_maximum_rank: Option<u16>,
    /// Script-defined target ranking ladder position.
    pub ranking_target_rank: Option<u16>,
    /// Script-defined selected ranking ladder position.
    pub ranking_selected_rank: Option<u16>,
}
