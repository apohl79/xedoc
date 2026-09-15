//! JSON request and response shapes for extension scripts.

use crate::Interaction;
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
}

/// Supported extension method.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Requests a model route for a pending turn.
    #[serde(rename = "routing.decide")]
    RoutingDecide,
    /// Requests the initial settings interaction.
    #[serde(rename = "settings.open")]
    SettingsOpen,
    /// Submits a response to an interaction.
    #[serde(rename = "interaction.respond")]
    InteractionRespond,
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireScriptResult {
    kind: ScriptResultKind,
    decision: Option<RouteDecision>,
    interaction: Option<Interaction>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum ScriptResultKind {
    Route,
    Interaction,
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
        } = WireScriptResult::deserialize(deserializer)?;
        match (kind, decision, interaction) {
            (ScriptResultKind::Route, Some(decision), None) => Ok(Self::Route { decision }),
            (ScriptResultKind::Interaction, None, Some(interaction)) => {
                Ok(Self::Interaction { interaction })
            }
            (ScriptResultKind::Route, _, _) => Err(serde::de::Error::custom(
                "route result must contain only a decision",
            )),
            (ScriptResultKind::Interaction, _, _) => Err(serde::de::Error::custom(
                "interaction result must contain only an interaction",
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
