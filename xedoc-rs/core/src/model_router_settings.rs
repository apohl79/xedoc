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

/// A script-requested update to the live session's router state.
pub struct ModelRouterSettingsSessionUpdate {
    /// Mode override to retain until the session ends, or `None` for the shared policy mode.
    pub router_mode: Option<String>,
}

/// A validated script-owned settings interaction and optional session update.
pub struct ModelRouterSettingsResult {
    /// Renderable replacement interaction.
    pub interaction: Value,
    /// State that the caller applies to the selected live session.
    pub session_update: Option<ModelRouterSettingsSessionUpdate>,
}

/// Opens the configured script-owned model-router settings surface.
///
/// # Errors
///
/// Returns a prompt-free message when the configured script cannot provide a
/// valid settings surface.
pub async fn open(
    config: &Config,
    models_manager: &SharedModelsManager,
    session_mode: Option<&str>,
    supports_session_mode: bool,
) -> Result<ModelRouterSettingsResult, String> {
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
        "eligibleClassifierRoutes": eligible_routes,
        "currentRoute": current_script_route(config),
        "session": {
            "routerMode": session_mode,
            "supportsRouterMode": supports_session_mode,
        },
    });
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = cancellation.clone().drop_guard();
    let interaction = host
        .open_settings(context, &eligible_routes, cancellation)
        .await
        .map_err(settings_error)?;
    serde_json::to_value(interaction.interaction)
        .map(|interaction| ModelRouterSettingsResult {
            interaction,
            session_update: None,
        })
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
    session_mode: Option<&str>,
    supports_session_mode: bool,
) -> Result<ModelRouterSettingsResult, String> {
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
        "eligibleClassifierRoutes": eligible_routes,
        "currentRoute": current_script_route(config),
        "session": {
            "routerMode": session_mode,
            "supportsRouterMode": supports_session_mode,
        },
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
    let session_update =
        interaction
            .session_update
            .map(|update| ModelRouterSettingsSessionUpdate {
                router_mode: update.router_mode.map(|mode| mode.as_str().to_string()),
            });
    serde_json::to_value(interaction.interaction)
        .map(|surface| ModelRouterSettingsResult {
            interaction: surface,
            session_update,
        })
        .map_err(|_| "model-router script returned an invalid settings surface".to_string())
}

fn settings_error(error: ModelRouterScriptFailure) -> String {
    error.interaction_message().map_or_else(
        || "model-router settings script failed".to_string(),
        str::to_string,
    )
}
