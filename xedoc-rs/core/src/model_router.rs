//! Host validation and catalog projection for the scripted model router.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use xedoc_core_config::config::Config;
use xedoc_login::AuthManager;
use xedoc_model_provider::configured_provider_has_credentials;
use xedoc_models_manager::manager::SharedModelsManager;
use xedoc_protocol::auth::AuthMode;
use xedoc_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use xedoc_protocol::openai_models::ModelAvailability;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_script_protocol::EligibleRoute as ScriptEligibleRoute;
use xedoc_script_protocol::ModelId as ScriptModelId;
use xedoc_script_protocol::ProviderId as ScriptProviderId;
use xedoc_script_protocol::ReasoningEffort as ScriptReasoningEffort;
use xedoc_script_protocol::Route as ScriptRoute;

/// Exposes authenticated backends and providers that require no authentication.
pub(crate) fn available_script_providers(
    config: &Config,
    auth_manager: &Arc<AuthManager>,
) -> Vec<String> {
    config
        .model_providers
        .iter()
        .filter(|(provider_id, provider)| {
            (!provider.requires_openai_auth
                && provider.env_key.is_none()
                && provider.auth.is_none()
                && provider.aws.is_none())
                || (provider.requires_openai_auth
                && auth_manager
                    .auth_mode()
                    .is_some_and(|mode| mode == AuthMode::ApiKey || mode.uses_xedoc_backend()))
                || provider.has_command_auth()
                || provider.uses_openai_actor_authorization()
                || provider
                    .experimental_bearer_token
                    .as_ref()
                    .is_some_and(|token| !token.trim().is_empty())
                || configured_provider_has_credentials(provider_id, provider, auth_manager)
                    .unwrap_or_else(|error| {
                        tracing::warn!(provider_id = %provider_id, %error, "could not inspect router backend availability");
                        false
                    })
        })
        .map(|(provider_id, _)| provider_id.clone())
        .collect()
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

    if config.model_provider_id != route.provider_id.as_str() {
        config.model_fast = config
            .provider_fast_models
            .get(route.provider_id.as_str())
            .cloned();
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
        .list_routing_models(config.http_client_factory())
        .await;
    let mut routes = BTreeMap::<(String, String), BTreeSet<String>>::new();
    for model in models {
        if !model.show_in_picker || model.availability == ModelAvailability::Unavailable {
            continue;
        }
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

/// Projects discovery evidence; the reserved `*` key classifies unlisted model IDs.
pub(crate) async fn script_model_availability(
    config: &Config,
    models_manager: &SharedModelsManager,
) -> BTreeMap<String, BTreeMap<String, ModelAvailability>> {
    let mut availability = BTreeMap::<String, BTreeMap<String, ModelAvailability>>::new();
    for model in models_manager
        .list_routing_models(config.http_client_factory())
        .await
        .into_iter()
        .take(256)
    {
        let provider_id = if model.provider_id.is_empty() {
            config.model_provider_id.clone()
        } else {
            model.provider_id
        };
        availability
            .entry(provider_id)
            .or_default()
            .insert(model.model, model.availability);
    }
    for provider_id in config.model_providers.keys().take(64) {
        availability.entry(provider_id.clone()).or_default().insert(
            "*".to_string(),
            models_manager.unlisted_model_availability_for_provider(provider_id),
        );
    }
    availability
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
