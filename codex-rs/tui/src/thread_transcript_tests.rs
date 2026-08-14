use super::*;

#[test]
fn persisted_interacted_sub_agent_activity_has_no_transcript_cell() {
    let item = ThreadItem::SubAgentActivity {
        id: "activity-1".to_string(),
        kind: SubAgentActivityKind::Interacted,
        agent_thread_id: codex_protocol::ThreadId::new().to_string(),
        agent_path: "/root/child".to_string(),
        model_provider: None,
        model: None,
        reasoning_effort: None,
        current_activity: Some("Running tests".to_string()),
    };

    assert!(fallback_transcript_cell(&item).is_none());
}
