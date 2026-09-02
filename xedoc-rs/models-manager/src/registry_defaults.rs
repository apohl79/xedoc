use std::collections::BTreeMap;
use std::io;

use xedoc_protocol::config_types::ReasoningSummary;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ModelVisibility;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::openai_models::ReasoningEffortPreset;

use crate::bundled_models_response;
use crate::model_info::BASE_INSTRUCTIONS;
use crate::model_info::model_info_from_provider_catalog_slug;
use crate::registry::MODEL_REGISTRY_SCHEMA_VERSION;
use crate::registry::ManagedModel;
use crate::registry::ModelRegistry;
use crate::registry::ModelTokenPrices;
use crate::registry::ProviderModelConfig;

pub(crate) const DEFAULT_CONTEXT_WINDOW: i64 = 400_000;
pub(crate) const DEFAULT_COMPACT_LIMIT: i64 = 360_000;
const DEFAULT_MAX_CONTEXT_WINDOW: i64 = 1_000_000;

pub(crate) fn default_registry() -> io::Result<ModelRegistry> {
    let mut providers = BTreeMap::new();
    providers.insert("openai".to_string(), openai_defaults()?);
    providers.insert(
        "anthropic".to_string(),
        provider_defaults(
            "Anthropic",
            "claude-fable-5-1",
            "claude-haiku-4-5-20251001",
            &[
                "claude-opus-5",
                "claude-opus-4-8",
                "claude-fable-5-1",
                "claude-sonnet-5",
                "claude-haiku-4-5-20251001",
            ],
            anthropic_prices(),
            /*supports_max*/ true,
        ),
    );
    providers.insert(
        "google".to_string(),
        provider_defaults(
            "Google",
            "gemini-3.1-pro-preview",
            "gemini-3.6-flash",
            &[
                "gemini-3.6-flash",
                "gemini-3.5-flash",
                "gemini-3.1-pro-preview",
                "gemini-3-pro-preview",
            ],
            google_prices(),
            /*supports_max*/ false,
        ),
    );
    providers.insert(
        "deepseek".to_string(),
        provider_defaults(
            "DeepSeek",
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            &["deepseek-v4-pro", "deepseek-v4-flash"],
            deepseek_prices(),
            /*supports_max*/ false,
        ),
    );
    let registry = ModelRegistry {
        schema_version: MODEL_REGISTRY_SCHEMA_VERSION,
        providers,
    };
    registry.validate()?;
    Ok(registry)
}

fn openai_defaults() -> io::Result<ProviderModelConfig> {
    let catalog = bundled_models_response().map_err(io::Error::other)?;
    let prices = openai_prices();
    let models = catalog
        .models
        .into_iter()
        .map(|info| {
            let price = prices.get(&info.slug).cloned();
            (
                info.slug.clone(),
                ManagedModel {
                    info,
                    prices: price,
                },
            )
        })
        .collect();
    Ok(ProviderModelConfig {
        display_name: "OpenAI".to_string(),
        default_model: "gpt-5.6-sol".to_string(),
        fast_model: "gpt-5.6-luna".to_string(),
        default_reasoning_effort: ReasoningEffort::Medium,
        template: provider_template("OpenAI", /*supports_max*/ true),
        models,
    })
}

fn provider_defaults(
    display_name: &str,
    default_model: &str,
    fast_model: &str,
    model_ids: &[&str],
    prices: BTreeMap<String, ModelTokenPrices>,
    supports_max: bool,
) -> ProviderModelConfig {
    let models = model_ids
        .iter()
        .enumerate()
        .map(|(index, model_id)| {
            let mut info = configured_model_info(display_name, model_id, supports_max);
            info.priority = i32::try_from(index + 1).unwrap_or(i32::MAX);
            let prices = prices.get(*model_id).cloned();
            (info.slug.clone(), ManagedModel { info, prices })
        })
        .collect();
    ProviderModelConfig {
        display_name: display_name.to_string(),
        default_model: default_model.to_string(),
        fast_model: fast_model.to_string(),
        default_reasoning_effort: ReasoningEffort::Medium,
        template: provider_template(display_name, supports_max),
        models,
    }
}

fn configured_model_info(display_name: &str, model_id: &str, supports_max: bool) -> ModelInfo {
    let mut info = model_info_from_provider_catalog_slug(model_id, display_name);
    apply_provider_defaults(&mut info, supports_max);
    info
}

fn provider_template(display_name: &str, supports_max: bool) -> ManagedModel {
    let mut info = model_info_from_provider_catalog_slug("*", display_name);
    apply_provider_defaults(&mut info, supports_max);
    info.description = None;
    info.base_instructions = BASE_INSTRUCTIONS.to_string();
    ManagedModel { info, prices: None }
}

fn apply_provider_defaults(info: &mut ModelInfo, supports_max: bool) {
    info.visibility = ModelVisibility::List;
    info.context_window = Some(DEFAULT_CONTEXT_WINDOW);
    info.max_context_window = Some(DEFAULT_MAX_CONTEXT_WINDOW);
    info.auto_compact_token_limit = Some(DEFAULT_COMPACT_LIMIT);
    info.effective_context_window_percent = 100;
    info.default_reasoning_level = Some(ReasoningEffort::Medium);
    info.default_reasoning_summary = ReasoningSummary::Auto;
    info.supported_reasoning_levels = reasoning_levels(supports_max);
    info.used_fallback_model_metadata = false;
}

fn reasoning_levels(supports_max: bool) -> Vec<ReasoningEffortPreset> {
    let mut levels = vec![
        reasoning_level(
            ReasoningEffort::Low,
            "Fast responses with lighter reasoning",
        ),
        reasoning_level(ReasoningEffort::Medium, "Balanced reasoning for most tasks"),
        reasoning_level(ReasoningEffort::High, "Deeper reasoning for complex tasks"),
        reasoning_level(ReasoningEffort::XHigh, "Maximum fixed reasoning budget"),
    ];
    if supports_max {
        levels.push(reasoning_level(
            ReasoningEffort::Max,
            "Provider-managed maximum reasoning",
        ));
    }
    levels
}

fn reasoning_level(effort: ReasoningEffort, description: &str) -> ReasoningEffortPreset {
    ReasoningEffortPreset {
        effort,
        description: description.to_string(),
    }
}

fn price(input: f64, cached_input: Option<f64>, output: f64) -> ModelTokenPrices {
    ModelTokenPrices {
        input,
        cached_input,
        output,
        long_context_input: None,
        long_context_cached_input: None,
        long_context_output: None,
    }
}

fn tiered_price(
    input: f64,
    cached_input: Option<f64>,
    output: f64,
    long_context_input: f64,
    long_context_cached_input: Option<f64>,
    long_context_output: f64,
) -> ModelTokenPrices {
    ModelTokenPrices {
        input,
        cached_input,
        output,
        long_context_input: Some(long_context_input),
        long_context_cached_input,
        long_context_output: Some(long_context_output),
    }
}

fn anthropic_prices() -> BTreeMap<String, ModelTokenPrices> {
    [
        ("claude-opus-5", price(5.0, Some(0.5), 25.0)),
        ("claude-opus-4-8", price(5.0, Some(0.5), 25.0)),
        ("claude-fable-5-1", price(10.0, Some(1.0), 50.0)),
        ("claude-sonnet-5", price(2.0, Some(0.2), 10.0)),
        ("claude-haiku-4-5-20251001", price(1.0, Some(0.1), 5.0)),
    ]
    .into_iter()
    .map(|(model, prices)| (model.to_string(), prices))
    .collect()
}

fn google_prices() -> BTreeMap<String, ModelTokenPrices> {
    [
        ("gemini-3.6-flash", price(0.15, Some(0.015), 0.60)),
        ("gemini-3.5-flash", price(0.15, Some(0.015), 0.60)),
        ("gemini-3.1-pro-preview", price(1.25, Some(0.125), 5.0)),
        ("gemini-3-pro-preview", price(1.25, Some(0.125), 5.0)),
    ]
    .into_iter()
    .map(|(model, prices)| (model.to_string(), prices))
    .collect()
}

fn deepseek_prices() -> BTreeMap<String, ModelTokenPrices> {
    [
        ("deepseek-v4-pro", price(0.435, Some(0.003625), 0.87)),
        ("deepseek-v4-flash", price(0.14, Some(0.0028), 0.28)),
    ]
    .into_iter()
    .map(|(model, prices)| (model.to_string(), prices))
    .collect()
}

fn openai_prices() -> BTreeMap<String, ModelTokenPrices> {
    [
        (
            "gpt-5.6-sol",
            tiered_price(5.0, Some(0.5), 30.0, 10.0, Some(1.0), 45.0),
        ),
        (
            "gpt-5.6-terra",
            tiered_price(2.5, Some(0.25), 15.0, 5.0, Some(0.5), 22.5),
        ),
        (
            "gpt-5.6-luna",
            tiered_price(1.0, Some(0.1), 6.0, 2.0, Some(0.2), 9.0),
        ),
        (
            "gpt-5.5",
            tiered_price(5.0, Some(0.5), 30.0, 10.0, Some(1.0), 45.0),
        ),
        (
            "gpt-5.5-pro",
            tiered_price(30.0, None, 180.0, 60.0, None, 270.0),
        ),
        (
            "gpt-5.4",
            tiered_price(2.5, Some(0.25), 15.0, 5.0, Some(0.5), 22.5),
        ),
        (
            "gpt-5.4-mini",
            tiered_price(0.75, Some(0.075), 4.5, 1.5, Some(0.15), 9.0),
        ),
        (
            "gpt-5-codex",
            tiered_price(5.0, Some(0.5), 30.0, 10.0, Some(1.0), 45.0),
        ),
        (
            "gpt-5.1-codex-max",
            tiered_price(5.0, Some(0.5), 30.0, 10.0, Some(1.0), 45.0),
        ),
    ]
    .into_iter()
    .map(|(model, prices)| (model.to_string(), prices))
    .collect()
}
