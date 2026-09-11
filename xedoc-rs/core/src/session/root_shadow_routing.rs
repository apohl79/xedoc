//! Observational routing for accepted root-turn user input.

use std::sync::Arc;

use xedoc_model_router::RouteDecision;
use xedoc_protocol::user_input::UserInput;

use crate::agent::control::render_input_preview;
use crate::model_router::ModelRouterService;
use crate::model_router::route_for_model;
use crate::session::Session;
use crate::session::turn_context::TurnContext;

/// Classifies an accepted idle root request without mutating the session route.
pub(super) async fn decide_for_accepted_input(
    session: &Arc<Session>,
    turn_context: &TurnContext,
    input: &[UserInput],
    explicit_override: bool,
) -> Option<RouteDecision> {
    let Some(reasoning_effort) = turn_context.reasoning_effort.clone() else {
        return None;
    };
    let route = route_for_model(
        turn_context.config.as_ref(),
        turn_context.model_info.slug.as_str(),
        reasoning_effort,
    );
    let prompt = render_input_preview(input);
    ModelRouterService::decide_root(
        turn_context.config.as_ref(),
        &session.services.models_manager,
        &prompt,
        route,
        explicit_override,
    )
    .await
}
