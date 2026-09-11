//! Compact history rendering for immutable model-router decisions.

use super::*;
use xedoc_app_server_protocol::ModelRouterDecisionNotification;
use xedoc_app_server_protocol::ModelRouterDecisionReason;
use xedoc_app_server_protocol::ModelRouterDisposition;
use xedoc_app_server_protocol::ModelRouterEffectiveRoute;
use xedoc_app_server_protocol::ModelRouterScope;

pub fn new_model_router_decision(
    notification: ModelRouterDecisionNotification,
) -> PlainHistoryCell {
    new_model_router_decision_item(
        notification.scope,
        notification.disposition,
        notification.reason,
        notification.effective_route,
    )
}

pub fn new_model_router_decision_item(
    scope: ModelRouterScope,
    disposition: ModelRouterDisposition,
    reason: ModelRouterDecisionReason,
    effective_route: ModelRouterEffectiveRoute,
) -> PlainHistoryCell {
    PlainHistoryCell::new(model_router_decision_lines(
        scope,
        disposition,
        reason,
        effective_route,
    ))
}

pub fn model_router_decision_lines(
    scope: ModelRouterScope,
    disposition: ModelRouterDisposition,
    reason: ModelRouterDecisionReason,
    effective_route: ModelRouterEffectiveRoute,
) -> Vec<Line<'static>> {
    let route = match effective_route {
        ModelRouterEffectiveRoute::Available {
            provider_id,
            model_slug,
            reasoning_effort,
        } => format!("{provider_id}/{model_slug}/{reasoning_effort}"),
        ModelRouterEffectiveRoute::Unavailable => "unavailable".to_string(),
    };
    vec![
        vec![
            "  model router ".magenta(),
            format!("{scope:?} {disposition:?}: {route}").dim(),
            format!(" ({reason:?})").dim(),
        ]
        .into(),
    ]
}
