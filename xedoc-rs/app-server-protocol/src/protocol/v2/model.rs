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
