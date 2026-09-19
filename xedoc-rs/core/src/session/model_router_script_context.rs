//! Bounded host-owned context supplied to scripted model routing.

use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use xedoc_protocol::models::MessagePhase;
use xedoc_protocol::models::ResponseItem;
use xedoc_script_protocol::EligibleRoute;
use xedoc_script_protocol::Route;

use super::Session;
use super::TurnContext;
use crate::content_items_to_text;
use crate::context_manager::is_user_turn_boundary;

/// Lifecycle state for one scripted routing decision.
pub(crate) enum RoutingTurnState {
    /// A new root turn whose route may still change.
    PendingRoot,
    /// An active root turn whose route is immutable.
    ActiveRoot,
    /// A subagent whose route may still change before spawn.
    PendingSubagent,
}

/// Inputs needed to build one bounded scripted-routing context.
pub(crate) struct RoutingContextInput<'a> {
    pub(crate) session: &'a Session,
    pub(crate) turn: &'a TurnContext,
    pub(crate) turn_state: RoutingTurnState,
    pub(crate) current_route: &'a Route,
    pub(crate) eligible_routes: &'a [EligibleRoute],
}

/// Builds the bounded host-owned context shared by all routing entry points.
pub(crate) async fn build(input: RoutingContextInput<'_>) -> Value {
    let (thread_name, token_info, history, session_mode) = {
        let state = input.session.state.lock().await;
        (
            state.session_configuration.thread_name.clone(),
            state.token_info(),
            state.clone_history(),
            state.model_router_session_mode().map(str::to_string),
        )
    };
    let message_limit = input
        .turn
        .config
        .model_router
        .context
        .bounded_recent_messages();
    let byte_limit = input
        .turn
        .config
        .model_router
        .context
        .bounded_max_recent_message_bytes();
    let (recent_messages, truncated) =
        recent_messages(history.raw_items(), message_limit, byte_limit);
    let (scope, active, route_mutable) = match input.turn_state {
        RoutingTurnState::PendingRoot => ("root", false, true),
        RoutingTurnState::ActiveRoot => ("root", true, false),
        RoutingTurnState::PendingSubagent => ("subagent", false, true),
    };

    let mut thread = Map::from_iter([
        (
            "id".to_string(),
            Value::String(input.session.thread_id.to_string()),
        ),
        (
            "cwd".to_string(),
            Value::String(input.turn.config.cwd.as_path().display().to_string()),
        ),
        (
            "tokenUsage".to_string(),
            json!({
                "contextWindow": token_info
                    .as_ref()
                    .and_then(|info| info.model_context_window),
                "used": token_info
                    .as_ref()
                    .map_or(0, |info| info.last_token_usage.total_tokens),
            }),
        ),
    ]);
    if let Some(thread_name) = thread_name {
        thread.insert("name".to_string(), Value::String(thread_name));
    }

    json!({
        "turn": {
            "id": input.turn.sub_id,
            "scope": scope,
            "active": active,
            "routeMutable": route_mutable,
        },
        "currentRoute": input.current_route,
        "eligibleRoutes": input.eligible_routes,
        "eligibleClassifierRoutes": input.eligible_routes,
        "thread": thread,
        "session": {
            "routerMode": session_mode,
        },
        "conversation": {
            "recentMessages": recent_messages,
            "truncated": truncated,
        },
    })
}

fn recent_messages(
    history: &[ResponseItem],
    message_limit: usize,
    byte_limit: usize,
) -> (Vec<Value>, bool) {
    let mut messages = history
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message {
                role,
                content,
                phase,
                ..
            } if (role == "user" && is_user_turn_boundary(item))
                || (role == "assistant" && *phase == Some(MessagePhase::FinalAnswer)) =>
            {
                content_items_to_text(content)
                    .filter(|text| !text.is_empty())
                    .map(|text| (role.as_str(), text))
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut truncated = messages.len() > message_limit;
    let retained_start = messages.len().saturating_sub(message_limit);
    let messages = messages
        .drain(retained_start..)
        .map(|(role, mut text)| {
            truncated |= truncate_utf8(&mut text, byte_limit);
            json!({ "role": role, "text": text })
        })
        .collect();
    (messages, truncated)
}

fn truncate_utf8(value: &mut String, max_bytes: usize) -> bool {
    if value.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    true
}
