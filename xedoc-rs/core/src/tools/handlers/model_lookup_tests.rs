use super::model_supports_multi_agent_backend;
use crate::tools::handlers::multi_agents_common::find_spawn_agent_model;
use xedoc_protocol::openai_models::ModelPreset;
use xedoc_protocol::openai_models::ModelServiceTier;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::openai_models::ReasoningEffortPreset;
use xedoc_protocol::protocol::MultiAgentVersion;

#[test]
fn picker_visible_model_lookup_results_are_valid_spawn_overrides() {
    let version = MultiAgentVersion::V2;
    let models = ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
        .map(|slug| ModelPreset {
            id: slug.to_string(),
            model: slug.to_string(),
            display_name: slug.to_string(),
            description: String::new(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![ReasoningEffortPreset {
                effort: ReasoningEffort::Medium,
                description: String::new(),
            }],
            supports_personality: false,
            additional_speed_tiers: Vec::new(),
            service_tiers: vec![ModelServiceTier {
                id: String::new(),
                name: String::new(),
                description: String::new(),
            }],
            default_service_tier: None,
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            multi_agent_version: Some(MultiAgentVersion::V1),
            availability_nux: None,
            supported_in_api: true,
            input_modalities: Vec::new(),
            provider_id: String::new(),
        })
        .to_vec();

    for model in models
        .iter()
        .filter(|model| model_supports_multi_agent_backend(model, version))
    {
        find_spawn_agent_model(&models, &model.model, version)
            .expect("model_lookup result should be accepted by spawn_agent");
    }
}
