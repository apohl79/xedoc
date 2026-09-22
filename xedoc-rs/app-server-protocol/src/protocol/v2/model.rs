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
use xedoc_protocol::protocol::ModelRouterActivityState as CoreModelRouterActivityState;
use xedoc_protocol::protocol::ModelRouterDecisionReason as CoreModelRouterDecisionReason;
use xedoc_protocol::protocol::ModelRouterDisposition as CoreModelRouterDisposition;
use xedoc_protocol::protocol::ModelRouterEffectiveRoute as CoreModelRouterEffectiveRoute;
use xedoc_protocol::protocol::ModelRouterScope as CoreModelRouterScope;
use xedoc_protocol::protocol::ModelVerification as CoreModelVerification;

/// A generic constrained interaction requested by an extension script.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionRequestParams {
    pub thread_id: String,
    pub turn_id: String,
    pub request_id: String,
    pub extension_id: String,
    pub interaction_id: String,
    pub continuation: String,
    pub state_revision: Option<String>,
    pub expires_at: i64,
    pub surface: ExtensionInteractionSurface,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterSettingsOpenParams {
    #[ts(optional = nullable)]
    pub thread_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterSettingsInteraction {
    pub interaction_id: String,
    pub continuation: String,
    pub state_revision: String,
    pub surface: ExtensionInteractionSurface,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterSettingsOpenResponse {
    pub interaction: Option<ModelRouterSettingsInteraction>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterSettingsRespondParams {
    #[ts(optional = nullable)]
    pub thread_id: Option<String>,
    pub response: ExtensionInteractionRequestResponse,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterSettingsRespondResponse {
    pub interaction: Option<ModelRouterSettingsInteraction>,
    pub error: Option<String>,
}

/// The constrained declarative surface an extension client may render.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ExtensionInteractionSurface {
    Menu {
        title: String,
        subtitle: Option<String>,
        items: Vec<ExtensionInteractionMenuItem>,
    },
    Form {
        id: String,
        title: String,
        subtitle: Option<String>,
        fields: Vec<ExtensionInteractionField>,
        submit: ExtensionInteractionAction,
        cancel: Option<ExtensionInteractionAction>,
    },
    Confirmation {
        title: String,
        body: String,
        details: Vec<ExtensionInteractionDetail>,
        #[serde(default)]
        sections: Vec<ExtensionInteractionSection>,
        actions: Vec<ExtensionInteractionAction>,
        #[serde(rename = "override")]
        #[ts(rename = "override")]
        override_form: Option<ExtensionInteractionForm>,
    },
    Notice {
        title: String,
        body: String,
        level: ExtensionInteractionNoticeLevel,
    },
}

/// A selectable row in an extension interaction menu.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionMenuItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub action: ExtensionInteractionAction,
    pub current: Option<bool>,
    pub disabled: Option<bool>,
}

/// Script-controlled filtering for a select field.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionSelectSearch {
    pub placeholder: Option<String>,
}

/// An opaque action supplied by an extension interaction.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionAction {
    pub id: String,
    pub opens: Option<String>,
    pub host_action: Option<ModelRouterSettingsHostAction>,
    pub label: Option<String>,
    pub key_bindings: Vec<String>,
    pub context: Option<String>,
    pub value: Option<JsonValue>,
}

/// A titled group of script-owned confirmation rows.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionSection {
    pub title: Option<String>,
    pub rows: Vec<ExtensionInteractionRow>,
}

/// One script-owned confirmation row.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionRow {
    pub text: String,
    #[serde(default)]
    pub indent: u8,
}

/// Explicit host capabilities available to model-router settings scripts.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(rename_all = "kebab-case")]
#[ts(export_to = "v2/")]
pub enum ModelRouterSettingsHostAction {
    OpenReport,
    ArmAb,
    DisableAb,
}

/// A field in an extension interaction form.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export_to = "v2/")]
pub enum ExtensionInteractionField {
    #[schemars(rename_all = "camelCase")]
    Select {
        id: String,
        label: String,
        description: Option<String>,
        value: Option<String>,
        current: Option<String>,
        options: Vec<ExtensionInteractionOption>,
        search: Option<ExtensionInteractionSelectSearch>,
    },
    #[schemars(rename_all = "camelCase")]
    Boolean {
        id: String,
        label: String,
        description: Option<String>,
        value: bool,
    },
    #[schemars(rename_all = "camelCase")]
    Text {
        id: String,
        label: String,
        description: Option<String>,
        value: String,
        max_bytes: u32,
        #[serde(default)]
        sensitive: bool,
    },
    #[schemars(rename_all = "camelCase")]
    ModelRoute {
        id: String,
        label: String,
        description: Option<String>,
        value: Option<ExtensionInteractionRoute>,
        eligible_routes: Vec<ExtensionInteractionEligibleRoute>,
    },
    #[schemars(rename_all = "camelCase")]
    Action {
        id: String,
        label: String,
        description: Option<String>,
        action: ExtensionInteractionAction,
    },
}

/// An opaque selectable option in an extension interaction form.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub disabled: Option<bool>,
}

/// A form in an extension interaction surface.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionForm {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub fields: Vec<ExtensionInteractionField>,
    pub submit: ExtensionInteractionAction,
    pub cancel: Option<ExtensionInteractionAction>,
}

/// A selected model route in an extension interaction.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionRoute {
    pub provider_id: String,
    pub model: String,
    pub reasoning_effort: String,
}

/// A model route eligible for selection in an extension interaction.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionEligibleRoute {
    pub provider_id: String,
    pub model: String,
    pub reasoning_efforts: Vec<String>,
}

/// A visible label/value pair in an extension interaction confirmation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionDetail {
    pub label: String,
    pub value: String,
}

/// Presentation severity for an extension interaction notice.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ExtensionInteractionNoticeLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// A response to a generic scripted extension interaction.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionRequestResponse {
    pub extension_id: String,
    pub interaction_id: String,
    pub continuation: String,
    pub state_revision: Option<String>,
    pub outcome: ExtensionInteractionOutcome,
    pub action: Option<ExtensionInteractionSelectedAction>,
    pub values: JsonValue,
}

/// An opaque action selected by the client.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExtensionInteractionSelectedAction {
    pub id: String,
}

/// How the client concluded an extension interaction.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ExtensionInteractionOutcome {
    Accepted,
    Cancelled,
    Dismissed,
}

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
    pub enum ModelRouterActivityState from CoreModelRouterActivityState {
        Started,
        Finished
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
        SteeringBypass,
        LowConfidence,
        NoClass,
        EmbeddingFailed,
        RouteUnavailable,
        ExplicitOverride
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
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

/// Transient local model-router classification lifecycle update.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterActivityNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub scope: ModelRouterScope,
    pub state: ModelRouterActivityState,
}

/// Experimental metadata-only record of one local model-router decision.
///
/// This notification never contains prompt or output text.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ModelRouterDecisionNotification {
    pub decision_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub scope: ModelRouterScope,
    pub disposition: ModelRouterDisposition,
    pub reason: ModelRouterDecisionReason,
    /// Whether interactive clients should render this decision's feedback.
    #[serde(default)]
    pub feedback_visible: bool,
    /// Prompt-free diagnostic retained when a local router dependency fails.
    pub diagnostic: Option<String>,
    /// Prompt-free multiline decision summary supplied by the router.
    pub summary: Option<String>,
    pub policy_revision: String,
    pub classifications: std::collections::BTreeMap<String, String>,
    /// Classifier confidence score from the task embedding.
    #[serde(default)]
    #[ts(type = "number")]
    pub confidence_score: f64,
    /// Margin between the selected classifier head and its runner-up.
    #[serde(default)]
    #[ts(type = "number")]
    pub confidence_margin: f64,
    pub ranking_score: Option<u16>,
    pub ranking_minimum_class: Option<String>,
    pub ranking_maximum_class: Option<String>,
    pub ranking_minimum_rank: Option<u16>,
    pub ranking_maximum_rank: Option<u16>,
    pub ranking_target_rank: Option<u16>,
    pub ranking_selected_rank: Option<u16>,
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
