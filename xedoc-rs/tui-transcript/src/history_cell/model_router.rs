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
        notification.diagnostic,
        notification.classifications,
        notification.confidence_score,
        notification.confidence_margin,
        notification.ranking_score,
        notification.ranking_minimum_class,
        notification.ranking_maximum_class,
        notification.ranking_minimum_rank,
        notification.ranking_maximum_rank,
        notification.ranking_target_rank,
        notification.ranking_selected_rank,
        notification.effective_route,
    )
}

pub fn new_model_router_decision_item(
    scope: ModelRouterScope,
    disposition: ModelRouterDisposition,
    reason: ModelRouterDecisionReason,
    diagnostic: Option<String>,
    classifications: std::collections::BTreeMap<String, String>,
    confidence_score: f64,
    confidence_margin: f64,
    ranking_score: Option<u16>,
    ranking_minimum_class: Option<String>,
    ranking_maximum_class: Option<String>,
    ranking_minimum_rank: Option<u16>,
    ranking_maximum_rank: Option<u16>,
    ranking_target_rank: Option<u16>,
    ranking_selected_rank: Option<u16>,
    effective_route: ModelRouterEffectiveRoute,
) -> PlainHistoryCell {
    PlainHistoryCell::new(model_router_decision_lines(
        scope,
        disposition,
        reason,
        diagnostic,
        classifications,
        confidence_score,
        confidence_margin,
        ranking_score,
        ranking_minimum_class,
        ranking_maximum_class,
        ranking_minimum_rank,
        ranking_maximum_rank,
        ranking_target_rank,
        ranking_selected_rank,
        effective_route,
    ))
}

pub fn model_router_decision_lines(
    scope: ModelRouterScope,
    disposition: ModelRouterDisposition,
    reason: ModelRouterDecisionReason,
    diagnostic: Option<String>,
    classifications: std::collections::BTreeMap<String, String>,
    confidence_score: f64,
    confidence_margin: f64,
    ranking_score: Option<u16>,
    ranking_minimum_class: Option<String>,
    ranking_maximum_class: Option<String>,
    ranking_minimum_rank: Option<u16>,
    ranking_maximum_rank: Option<u16>,
    ranking_target_rank: Option<u16>,
    ranking_selected_rank: Option<u16>,
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
    let ranking = ranking_score
        .zip(ranking_minimum_class)
        .zip(ranking_maximum_class)
        .zip(ranking_minimum_rank)
        .zip(ranking_maximum_rank)
        .zip(ranking_target_rank)
        .zip(ranking_selected_rank)
        .map_or_else(
            String::new,
            |(
                (((((score, minimum_class), maximum_class), minimum_rank), maximum_rank), target_rank),
                selected_rank,
            )| {
                format!(
                    " · rating {score} → {minimum_class}-{maximum_class} → ranks {minimum_rank}-{maximum_rank} → target {target_rank} → selected {selected_rank}"
                )
            },
        );
    let mut lines = vec![
        vec![
            "  model router ".magenta(),
            format!("{scope:?} {disposition:?}: {route}").dim(),
            format!(" ({reason:?})").dim(),
        ]
        .into(),
    ];
    if !classifications.is_empty() {
        lines.push(
            format!(
                "  {classifications} · confidence {confidence_score:.3} (margin {confidence_margin:.3}){ranking}"
            )
            .dim()
            .into(),
        );
    } else {
        lines.push(
            format!("  confidence {confidence_score:.3} (margin {confidence_margin:.3})")
                .dim()
                .into(),
        );
    }
    if let Some(diagnostic) = diagnostic {
        lines.push(format!("  {diagnostic}").dim().into());
    }
    lines
}
