//! Contextless scripted model-router settings operations.

use serde_json::Value;
use tokio_util::sync::CancellationToken;
use xedoc_models_manager::manager::SharedModelsManager;

use crate::config::Config;
use crate::model_router::current_script_route;
use crate::model_router::eligible_script_routes;
use crate::model_router_script_host::ModelRouterScriptFailure;
use crate::model_router_script_host::ModelRouterScriptHost;
use crate::model_router_script_host::interaction_response;

/// Opens the configured script-owned model-router settings surface.
///
/// # Errors
///
/// Returns a prompt-free message when the configured script cannot provide a
/// valid settings surface.
pub async fn open(config: &Config, models_manager: &SharedModelsManager) -> Result<Value, String> {
    let Some(host) = ModelRouterScriptHost::from_config(config) else {
        return Err("model-router script is not configured".to_string());
    };
    let eligible_routes = eligible_script_routes(config, models_manager).await;
    let context = serde_json::json!({
        "client": {
            "kind": "tui",
            "surfaces": ["menu", "form", "confirmation"],
        },
        "eligibleRoutes": eligible_routes,
        "currentRoute": current_script_route(config),
    });
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = cancellation.clone().drop_guard();
    let interaction = host
        .open_settings(context, &eligible_routes, cancellation)
        .await
        .map_err(settings_error)?;
    serde_json::to_value(interaction.interaction)
        .map_err(|_| "model-router script returned an invalid settings surface".to_string())
}

/// Submits one rendered script-owned settings action.
///
/// # Errors
///
/// Returns a prompt-free message when the configured script rejects the action
/// or cannot provide its replacement surface.
pub async fn respond(
    config: &Config,
    models_manager: &SharedModelsManager,
    response: Value,
) -> Result<Value, String> {
    let Some(host) = ModelRouterScriptHost::from_config(config) else {
        return Err("model-router script is not configured".to_string());
    };
    let response = serde_json::from_value(response)
        .map_err(|_| "invalid model-router settings response".to_string())?;
    let eligible_routes = eligible_script_routes(config, models_manager).await;
    let context = serde_json::json!({
        "client": {
            "kind": "tui",
            "surfaces": ["menu", "form", "confirmation"],
        },
        "eligibleRoutes": eligible_routes,
        "currentRoute": current_script_route(config),
    });
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = cancellation.clone().drop_guard();
    let interaction = host
        .respond_settings(
            context,
            interaction_response(response),
            &eligible_routes,
            cancellation,
        )
        .await
        .map_err(settings_error)?;
    serde_json::to_value(interaction.interaction)
        .map_err(|_| "model-router script returned an invalid settings surface".to_string())
}

fn settings_error(error: ModelRouterScriptFailure) -> String {
    error.interaction_message().map_or_else(
        || "model-router settings script failed".to_string(),
        str::to_string,
    )
}
