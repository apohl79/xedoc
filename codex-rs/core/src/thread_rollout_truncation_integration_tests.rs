use crate::session::tests::build_world_state_from_turn_context;
use crate::session::tests::make_session_and_context;
use crate::thread_rollout_truncation::truncate_rollout_before_nth_user_message_from_start;
use crate::thread_rollout_truncation::user_message_positions_in_rollout;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::RolloutItem;
use pretty_assertions::assert_eq;
use std::sync::Arc;

fn message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[tokio::test]
async fn ignores_session_prefix_messages_when_truncating_rollout_from_start() {
    let (session, turn_context) = make_session_and_context().await;
    let turn_context = Arc::new(turn_context);
    let world_state = build_world_state_from_turn_context(&session, &turn_context).await;
    let mut items = session
        .build_initial_context_with_world_state(&turn_context, &world_state)
        .await;
    let feature_request = message("user", "feature request");
    items.push(feature_request.clone());
    items.push(message("assistant", "ack"));
    items.push(message("user", "second question"));
    items.push(message("assistant", "answer"));

    let rollout_items: Vec<RolloutItem> = items
        .iter()
        .cloned()
        .map(RolloutItem::ResponseItem)
        .collect();
    let feature_request_index = items
        .iter()
        .position(|item| item == &feature_request)
        .expect("feature request should be present");
    let user_message_positions = user_message_positions_in_rollout(&rollout_items);
    let feature_request_number = user_message_positions
        .iter()
        .position(|index| *index == feature_request_index)
        .expect("feature request should be a user-message boundary");

    let truncated =
        truncate_rollout_before_nth_user_message_from_start(&rollout_items, feature_request_number);
    let expected: Vec<RolloutItem> = items[..feature_request_index]
        .iter()
        .cloned()
        .map(RolloutItem::ResponseItem)
        .collect();

    assert_eq!(
        serde_json::to_value(&truncated).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
}
