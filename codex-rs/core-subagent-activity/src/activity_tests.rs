use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemReasoningSummary;

#[test]
fn tracker_collects_message_reasoning_and_tool_activity() {
    let mut activity = RecentSubAgentActivity::default();
    activity.record_response_item(&ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "Reviewing the migration".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    });
    activity.record_response_item(&ResponseItem::Reasoning {
        id: None,
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "Checking compatibility".to_string(),
        }],
        content: None,
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    });
    activity.record_response_item(&ResponseItem::FunctionCall {
        id: None,
        name: "exec_command".to_string(),
        namespace: Some("functions".to_string()),
        arguments: r#"{"cmd":"just test"}"#.to_string(),
        call_id: "call-1".to_string(),
        internal_chat_message_metadata_passthrough: None,
    });

    assert_eq!(
        activity.snapshot_if_changed(),
        Some(
            "Assistant: Reviewing the migration\nReasoning: Checking compatibility\nTool functions/exec_command: {\"cmd\":\"just test\"}"
                .to_string()
        )
    );
    assert_eq!(activity.snapshot_if_changed(), None);
}

#[test]
fn tracker_retries_failed_summary_with_same_bounded_history() {
    let mut activity = RecentSubAgentActivity::default();
    for index in 0..=MAX_RECENT_ACTIVITY_ITEMS {
        activity.record_response_item(&ResponseItem::FunctionCall {
            id: None,
            name: format!("tool-{index}"),
            namespace: None,
            arguments: "{}".to_string(),
            call_id: format!("call-{index}"),
            internal_chat_message_metadata_passthrough: None,
        });
    }

    let first = activity
        .snapshot_if_changed()
        .expect("new activity should produce a snapshot");
    activity.retry();
    let retried = activity
        .snapshot_if_changed()
        .expect("failed summary should remain eligible");

    assert_eq!(
        (
            first.lines().count(),
            first.lines().next(),
            first.lines().last(),
            retried,
        ),
        (
            MAX_RECENT_ACTIVITY_ITEMS,
            Some("Tool tool-1: {}"),
            Some("Tool tool-8: {}"),
            first.clone(),
        )
    );
}
