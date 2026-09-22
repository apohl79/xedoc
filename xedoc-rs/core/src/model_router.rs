//! Host validation and catalog projection for the scripted model router.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use xedoc_core_config::config::Config;
use xedoc_models_manager::manager::RefreshStrategy;
use xedoc_models_manager::manager::SharedModelsManager;
use xedoc_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_script_protocol::EligibleRoute as ScriptEligibleRoute;
use xedoc_script_protocol::ModelId as ScriptModelId;
use xedoc_script_protocol::ProviderId as ScriptProviderId;
use xedoc_script_protocol::ReasoningEffort as ScriptReasoningEffort;
use xedoc_script_protocol::Route as ScriptRoute;

/// Appends bounded router guidance to the model-facing developer instructions for one turn.
pub(crate) fn append_script_model_instructions(
    developer_instructions: &mut Option<String>,
    model_instructions: Option<&str>,
) {
    let Some(model_instructions) = model_instructions else {
        return;
    };
    if model_instructions.trim().is_empty() {
        return;
    }
    let next = match developer_instructions.take() {
        Some(existing) if !existing.trim().is_empty() => {
            format!("{existing}\n\n{model_instructions}")
        }
        _ => model_instructions.to_string(),
    };
    *developer_instructions = Some(next);
}

/// Applies a script-selected route after validating it against current models.
pub(crate) async fn apply_script_route_to_config(
    config: &mut Config,
    models_manager: &SharedModelsManager,
    route: &ScriptRoute,
) -> bool {
    let Some(provider) = config
        .model_providers
        .get(route.provider_id.as_str())
        .cloned()
    else {
        return false;
    };
    let Ok(reasoning_effort) = route.reasoning_effort.as_str().parse::<ReasoningEffort>() else {
        return false;
    };
    let model_info = models_manager
        .get_model_info_for_provider(
            route.model.as_str(),
            route.provider_id.as_str(),
            &config.to_models_manager_config(),
        )
        .await;
    if model_info.used_fallback_model_metadata
        || !model_info
            .supported_reasoning_levels
            .iter()
            .any(|preset| preset.effort == reasoning_effort)
        || config.service_tier.as_deref().is_some_and(|tier| {
            tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE && !model_info.supports_service_tier(tier)
        })
    {
        return false;
    }

    config.model = Some(route.model.as_str().to_string());
    config.model_provider_id = route.provider_id.as_str().to_string();
    config.model_provider = provider;
    config.model_reasoning_effort = Some(reasoning_effort);
    true
}

/// Returns every currently usable provider/model/effort route for a script.
pub(crate) async fn eligible_script_routes(
    config: &Config,
    models_manager: &SharedModelsManager,
) -> Vec<ScriptEligibleRoute> {
    let models = models_manager
        .list_models(RefreshStrategy::Offline, config.http_client_factory())
        .await;
    let mut routes = BTreeMap::<(String, String), BTreeSet<String>>::new();
    for model in models {
        let provider_id = if !model.provider_id.is_empty() {
            model.provider_id
        } else {
            config.model_provider_id.clone()
        };
        let model_info = models_manager
            .get_model_info_for_provider(
                &model.model,
                &provider_id,
                &config.to_models_manager_config(),
            )
            .await;
        if model_info.used_fallback_model_metadata
            || config.service_tier.as_deref().is_some_and(|tier| {
                tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE
                    && !model_info.supports_service_tier(tier)
            })
        {
            continue;
        }
        let efforts = model_info
            .supported_reasoning_levels
            .iter()
            .map(|preset| preset.effort.as_str().to_string())
            .collect::<BTreeSet<_>>();
        if !efforts.is_empty() {
            routes.insert((provider_id, model.model), efforts);
        }
    }
    if let Some(current_route) = current_script_route(config) {
        let provider_id = current_route.provider_id.as_str();
        let model = current_route.model.as_str();
        let route_key = (provider_id.to_string(), model.to_string());
        if !routes.contains_key(&route_key) && config.model_providers.contains_key(provider_id) {
            let model_info = models_manager
                .get_model_info_for_provider(model, provider_id, &config.to_models_manager_config())
                .await;
            let efforts = model_info
                .supported_reasoning_levels
                .iter()
                .map(|preset| preset.effort.as_str().to_string())
                .collect::<BTreeSet<_>>();
            if !model_info.used_fallback_model_metadata
                && efforts.contains(current_route.reasoning_effort.as_str())
                && config.service_tier.as_deref().is_none_or(|tier| {
                    tier == SERVICE_TIER_DEFAULT_REQUEST_VALUE
                        || model_info.supports_service_tier(tier)
                })
            {
                routes.insert(route_key, efforts);
            }
        }
    }
    routes
        .into_iter()
        .map(
            |((provider_id, model), reasoning_efforts)| ScriptEligibleRoute {
                provider_id: ScriptProviderId::new(provider_id),
                model: ScriptModelId::new(model),
                reasoning_efforts: reasoning_efforts
                    .into_iter()
                    .map(ScriptReasoningEffort::new)
                    .collect(),
            },
        )
        .collect()
}

/// Represents the selected route in the script protocol.
pub(crate) fn current_script_route(config: &Config) -> Option<ScriptRoute> {
    Some(ScriptRoute {
        provider_id: ScriptProviderId::new(config.model_provider_id.clone()),
        model: ScriptModelId::new(config.model.clone()?),
        reasoning_effort: ScriptReasoningEffort::new(
            config.model_reasoning_effort.clone()?.as_str().to_string(),
        ),
    })
}
