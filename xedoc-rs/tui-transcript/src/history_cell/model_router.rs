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
        notification.classifications,
        notification.ranking_score,
        notification.ranking_minimum_class,
        notification.ranking_maximum_class,
        notification.ranking_minimum_rank,
        notification.ranking_maximum_rank,
        notification.effective_route,
    )
}

pub fn new_model_router_decision_item(
    scope: ModelRouterScope,
    disposition: ModelRouterDisposition,
    reason: ModelRouterDecisionReason,
    classifications: std::collections::BTreeMap<String, String>,
    ranking_score: Option<u16>,
    ranking_minimum_class: Option<String>,
    ranking_maximum_class: Option<String>,
    ranking_minimum_rank: Option<u16>,
    ranking_maximum_rank: Option<u16>,
    effective_route: ModelRouterEffectiveRoute,
) -> PlainHistoryCell {
    PlainHistoryCell::new(model_router_decision_lines(
        scope,
        disposition,
        reason,
        classifications,
        ranking_score,
        ranking_minimum_class,
        ranking_maximum_class,
        ranking_minimum_rank,
        ranking_maximum_rank,
        effective_route,
    ))
}

pub fn model_router_decision_lines(
    scope: ModelRouterScope,
    disposition: ModelRouterDisposition,
    reason: ModelRouterDecisionReason,
    classifications: std::collections::BTreeMap<String, String>,
    ranking_score: Option<u16>,
    ranking_minimum_class: Option<String>,
    ranking_maximum_class: Option<String>,
    ranking_minimum_rank: Option<u16>,
    ranking_maximum_rank: Option<u16>,
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
    let classifications = classifications
        .into_iter()
        .map(|(axis, value)| format!("{axis}={value}"))
        .collect::<Vec<_>>()
        .join(", ");
    let ranking = ranking_score.map_or_else(String::new, |score| {
        format!(
            " · rank {score} [{}-{}; {}-{}]",
            ranking_minimum_class.unwrap_or_default(),
            ranking_maximum_class.unwrap_or_default(),
            ranking_minimum_rank.map_or_else(String::new, |rank| rank.to_string()),
            ranking_maximum_rank.map_or_else(String::new, |rank| rank.to_string()),
        )
    });
    let mut lines = vec![
        vec![
            "  model router ".magenta(),
            format!("{scope:?} {disposition:?}: {route}").dim(),
            format!(" ({reason:?})").dim(),
        ]
        .into(),
    ];
    if !classifications.is_empty() {
        lines.push(format!("  {classifications}{ranking}").dim().into());
    }
    lines
}
