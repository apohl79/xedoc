use xedoc_model_provider_info::AMAZON_BEDROCK_GPT_5_6_TERRA_MODEL_ID;
use xedoc_models_manager::bundled_models_response;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ModelVisibility;
use xedoc_protocol::openai_models::ModelsResponse;
use xedoc_protocol::openai_models::ReasoningEffort;

const GPT_5_BEDROCK_CONTEXT_WINDOW: i64 = 272_000;
const GPT_5_6_TERRA_OPENAI_MODEL_ID: &str = "gpt-5.6-terra";

pub(crate) fn static_model_catalog() -> ModelsResponse {
    with_default_only_service_tier(ModelsResponse {
        models: vec![gpt_5_bedrock_model(
            GPT_5_6_TERRA_OPENAI_MODEL_ID,
            AMAZON_BEDROCK_GPT_5_6_TERRA_MODEL_ID,
            "GPT-5.6 Terra",
            /*priority*/ 0,
        )],
    })
}

pub(crate) fn with_default_only_service_tier(mut catalog: ModelsResponse) -> ModelsResponse {
    for model in &mut catalog.models {
        // Amazon Bedrock currently only supports the implicit "default" tier for GPT models.
        model.additional_speed_tiers.clear();
        model.service_tiers.clear();
        model.default_service_tier = None;
    }
    catalog
}

fn gpt_5_bedrock_model(
    openai_slug: &str,
    bedrock_slug: &str,
    display_name: &str,
    priority: i32,
) -> ModelInfo {
    let mut model = bundled_openai_model(openai_slug);
    model.slug = bedrock_slug.to_string();
    model.display_name = display_name.to_string();
    model.priority = priority;
    model.context_window = Some(GPT_5_BEDROCK_CONTEXT_WINDOW);
    model.max_context_window = Some(GPT_5_BEDROCK_CONTEXT_WINDOW);
    // Bedrock does not offer the `ultra` reasoning effort.
    model
        .supported_reasoning_levels
        .retain(|preset| preset.effort != ReasoningEffort::Ultra);
    model.visibility = ModelVisibility::List;
    model.availability_nux = None;
    model.upgrade = None;
    model
}

fn bundled_openai_model(slug: &str) -> ModelInfo {
    bundled_models_response()
        .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"))
        .models
        .into_iter()
        .find(|model| model.slug == slug)
        .unwrap_or_else(|| panic!("bundled models.json should include {slug}"))
}
