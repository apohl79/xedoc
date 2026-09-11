use super::shared::v2_enum_from_core;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use ts_rs::TS;
use xedoc_protocol::openai_models::InputModality;
use xedoc_protocol::openai_models::ModelAvailabilityNux as CoreModelAvailabilityNux;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::openai_models::default_input_modalities;
use xedoc_protocol::protocol::ModelRerouteReason as CoreModelRerouteReason;
use xedoc_protocol::protocol::ModelRouterDecisionReason as CoreModelRouterDecisionReason;
use xedoc_protocol::protocol::ModelRouterDisposition as CoreModelRouterDisposition;
use xedoc_protocol::protocol::ModelRouterEffectiveRoute as CoreModelRouterEffectiveRoute;
use xedoc_protocol::protocol::ModelRouterScope as CoreModelRouterScope;
use xedoc_protocol::protocol::ModelVerification as CoreModelVerification;

v2_enum_from_core!(
    pub enum ModelRerouteReason from CoreModelRerouteReason {
        HighRiskCyberActivity
    }
);

v2_enum_from_core!(
    pub enum ModelVerification from CoreModelVerification {
        TrustedAccessForCyber
    }
);

v2_enum_from_core!(
    pub enum ModelRouterScope from CoreModelRouterScope {
        Root,
        Subagent
    }
);

v2_enum_from_core!(
    pub enum ModelRouterDisposition from CoreModelRouterDisposition {
        Applied,
        Shadow,
        Fallback
    }
);

v2_enum_from_core!(
    pub enum ModelRouterDecisionReason from CoreModelRouterDecisionReason {
        Classified,
        LowConfidence,
        NoClass,
        EmbeddingFailed,
        RouteUnavailable,
        ExplicitOverride
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ModelRouterEffectiveRoute {
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Available {
        provider_id: String,
        model_slug: String,
        reasoning_effort: String,
    },
    Unavailable,
}

/// A selectable model route proposed by the model router.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterRoute {
    pub provider_id: String,
    pub model_slug: String,
    pub reasoning_effort: String,
}

/// A direct user control for the next root-turn A/B experiment.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ModelRouterAbControlAction {
    ArmNext,
    Disable,
}

/// Request a root-thread model-router A/B control action.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterAbControlParams {
    pub thread_id: String,
    pub action: ModelRouterAbControlAction,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterAbControlResponse {}

/// The action a user takes on a model-router proposal.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ModelRouterApprovalAction {
    Approve,
    Reject,
    Override,
}

/// A dedicated, structured request to approve a proposed model route.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterApprovalParams {
    pub approval_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub scope: ModelRouterScope,
    pub predicted_classification: String,
    pub proposed_route: ModelRouterRoute,
    pub current_route: ModelRouterEffectiveRoute,
    #[ts(type = "number")]
    pub score: f64,
    #[ts(type = "number")]
    pub margin: f64,
    pub classifier_revision: String,
    pub policy_revision: String,
    pub prompt_sha256: String,
}

/// The response to a model-router approval request.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterApprovalResponse {
    pub action: ModelRouterApprovalAction,
    pub classification: Option<String>,
    pub route: Option<ModelRouterRoute>,
}

impl From<CoreModelRouterEffectiveRoute> for ModelRouterEffectiveRoute {
    fn from(value: CoreModelRouterEffectiveRoute) -> Self {
        match value {
            CoreModelRouterEffectiveRoute::Available {
                provider_id,
                model_slug,
                reasoning_effort,
            } => Self::Available {
                provider_id,
                model_slug,
                reasoning_effort,
            },
            CoreModelRouterEffectiveRoute::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderCapabilitiesReadParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderCapabilitiesReadResponse {
    pub namespace_tools: bool,
    pub image_generation: bool,
    pub web_search: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelManagerReadParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelManagerReadResponse {
    pub providers: Vec<ManagedProviderSettings>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ManagedProviderSettings {
    pub id: String,
    pub display_name: String,
    pub default_model: String,
    pub fast_model: String,
    pub default_reasoning_effort: ReasoningEffort,
    pub api_key_configured: bool,
    pub oauth_supported: bool,
    pub oauth_configured: bool,
    pub models: Vec<ManagedModelSettings>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ManagedModelSettings {
    pub id: String,
    pub display_name: String,
    pub context_window: i64,
    pub max_context_window: i64,
    pub auto_compact_token_limit: i64,
    pub base_instructions: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ModelManagerUpdateParams {
    ProviderDefaults {
        provider_id: String,
        default_model: String,
        fast_model: String,
        default_reasoning_effort: ReasoningEffort,
    },
    ModelSettings {
        provider_id: String,
        model_id: String,
        context_window: i64,
        max_context_window: i64,
        auto_compact_token_limit: i64,
        base_instructions: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelManagerUpdateResponse {}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderApiKeySetParams {
    pub provider_id: String,
    pub api_key: String,
}

impl std::fmt::Debug for ModelProviderApiKeySetParams {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelProviderApiKeySetParams")
            .field("provider_id", &self.provider_id)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderApiKeySetResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderApiKeyDeleteParams {
    pub provider_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderApiKeyDeleteResponse {
    pub deleted: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderOauthDeleteParams {
    pub provider_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderOauthDeleteResponse {
    pub deleted: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderOauthStartParams {
    pub provider_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelProviderOauthStartResponse {
    pub auth_url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelListParams {
    /// Opaque pagination cursor returned by a previous call.
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
    /// Optional page size; defaults to a reasonable server-side value.
    #[ts(optional = nullable)]
    pub limit: Option<u32>,
    /// When true, include models that are hidden from the default picker list.
    #[ts(optional = nullable)]
    pub include_hidden: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelAvailabilityNux {
    pub message: String,
}

impl From<CoreModelAvailabilityNux> for ModelAvailabilityNux {
    fn from(value: CoreModelAvailabilityNux) -> Self {
        Self {
            message: value.message,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelServiceTier {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct Model {
    pub id: String,
    pub model: String,
    /// Identifier of the provider that serves this model.
    pub provider_id: String,
    pub upgrade: Option<String>,
    pub upgrade_info: Option<ModelUpgradeInfo>,
    pub availability_nux: Option<ModelAvailabilityNux>,
    pub display_name: String,
    pub description: String,
    pub hidden: bool,
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    pub default_reasoning_effort: ReasoningEffort,
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
    #[serde(default)]
    pub supports_personality: bool,
    /// Deprecated: use `serviceTiers` instead.
    #[serde(default)]
    pub additional_speed_tiers: Vec<String>,
    #[serde(default)]
    pub service_tiers: Vec<ModelServiceTier>,
    /// Catalog default service tier id for this model, when one is configured.
    #[serde(default)]
    pub default_service_tier: Option<String>,
    // Only one model should be marked as default.
    pub is_default: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelUpgradeInfo {
    pub model: String,
    pub upgrade_copy: Option<String>,
    pub model_link: Option<String>,
    pub migration_markdown: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: ReasoningEffort,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelListResponse {
    pub data: Vec<Model>,
    /// Opaque cursor to pass to the next call to continue after the last item.
    /// If None, there are no more items to return.
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelReroutedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub from_model: String,
    pub to_model: String,
    pub reason: ModelRerouteReason,
}

/// Experimental metadata-only record of one local model-router decision.
///
/// This notification never contains prompt or output text.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterDecisionNotification {
    pub decision_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub scope: ModelRouterScope,
    pub disposition: ModelRouterDisposition,
    pub reason: ModelRouterDecisionReason,
    pub policy_revision: String,
    pub proposed_provider_id: String,
    pub proposed_model_slug: String,
    pub proposed_reasoning_effort: String,
    pub effective_route: ModelRouterEffectiveRoute,
    pub prompt_sha256: String,
    #[ts(type = "number")]
    pub prompt_original_bytes: u64,
    pub prompt_truncated: bool,
    /// Unix timestamp in whole seconds when this immutable decision was made.
    #[ts(type = "number")]
    pub created_at: i64,
}

/// Bounded input for reading the local model-router report.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterReportReadParams {
    /// Inclusive UTC-day start, as whole Unix seconds.
    #[ts(optional = nullable, type = "number")]
    pub from_day: Option<i64>,
    /// Inclusive UTC-day end, as whole Unix seconds.
    #[ts(optional = nullable, type = "number")]
    pub through_day: Option<i64>,
    /// Maximum recent decisions to return, capped by the server.
    #[ts(optional = nullable)]
    pub recent_limit: Option<u32>,
}

/// One persisted UTC-day aggregate for the local model router.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterReportDay {
    /// UTC midnight as whole Unix seconds.
    #[ts(type = "number")]
    pub day: i64,
    pub provider_id: String,
    pub model_slug: String,
    pub scope: String,
    pub reasoning_effort: Option<String>,
    #[ts(type = "number")]
    pub decisions: i64,
    #[ts(type = "number")]
    pub invocations: i64,
    #[ts(type = "number | null")]
    pub input_tokens: Option<i64>,
    #[ts(type = "number | null")]
    pub cached_input_tokens: Option<i64>,
    #[ts(type = "number | null")]
    pub output_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub normalized_baseline_usd: Option<f64>,
    pub estimated_savings_usd: Option<f64>,
    pub ab_experiment_overhead_usd: Option<f64>,
    #[ts(type = "number")]
    pub attributed_invocations: i64,
    #[ts(type = "number")]
    pub unattributed_invocations: i64,
    #[ts(type = "number")]
    pub missing_usage_invocations: i64,
    #[ts(type = "number")]
    pub unknown_price_invocations: i64,
    #[ts(type = "number")]
    pub classified_decisions: i64,
    #[ts(type = "number")]
    pub fallback_decisions: i64,
    pub average_score: Option<f64>,
    pub average_margin: Option<f64>,
}

/// Metadata-only view of one recent local model-router decision.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterReportDecision {
    pub decision_id: String,
    pub scope: String,
    pub class_id: Option<String>,
    pub score: Option<f64>,
    pub margin: Option<f64>,
    pub disposition: String,
    pub reason: String,
    pub fallback: bool,
    pub policy_revision: String,
    pub proposed_provider_id: Option<String>,
    pub proposed_model_slug: Option<String>,
    pub proposed_reasoning_effort: Option<String>,
    pub effective_provider_id: Option<String>,
    pub effective_model_slug: Option<String>,
    pub effective_reasoning_effort: Option<String>,
    /// Creation time as whole Unix seconds.
    #[ts(type = "number")]
    pub created_at: i64,
}

/// Bounded report assembled from local model-router state.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterReportReadResponse {
    /// Inclusive UTC-day start used for `days`.
    #[ts(type = "number")]
    pub from_day: i64,
    /// Inclusive UTC-day end used for `days`.
    #[ts(type = "number")]
    pub through_day: i64,
    pub days: Vec<ModelRouterReportDay>,
    /// Whether aggregate rows exceeded the report's fixed response limit.
    pub days_truncated: bool,
    pub recent_decisions: Vec<ModelRouterReportDecision>,
}

/// Starts a local browser report for model-router activity.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterReportOpenParams {}

/// A short-lived local URL for the model-router browser report.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterReportOpenResponse {
    /// Loopback URL whose fragment contains the read-only capability.
    pub url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelVerificationNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub verifications: Vec<ModelVerification>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnModerationMetadataNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub metadata: JsonValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelSafetyBufferingUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub model: String,
    pub use_cases: Vec<String>,
    pub reasons: Vec<String>,
    pub show_buffering_ui: bool,
    pub faster_model: Option<String>,
}
