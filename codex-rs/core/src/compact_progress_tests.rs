use codex_analytics::CompactionReason;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn model_switch_reasons_map_to_one_cause() {
    let causes = [
        CompactionReason::UserRequested,
        CompactionReason::ContextLimit,
        CompactionReason::ModelDownshift,
        CompactionReason::CompHashChanged,
    ]
    .map(CompactionCause::from);

    assert_eq!(
        causes,
        [
            CompactionCause::UserRequested,
            CompactionCause::ContextLimit,
            CompactionCause::ModelSwitch,
            CompactionCause::ModelSwitch,
        ]
    );
}

#[test]
fn progress_messages_include_cause_and_stage() {
    let messages = [
        CompactionStage::Planning { chunks: 5 },
        CompactionStage::Mapping {
            completed: 2,
            total: 5,
        },
        CompactionStage::Reducing {
            layer: 1,
            groups: 3,
        },
        CompactionStage::Summarizing,
        CompactionStage::Complete,
        CompactionStage::Failed,
    ]
    .map(|stage| progress_message(CompactionCause::ModelSwitch, &stage));

    assert_eq!(
        messages,
        [
            "• Compacting... (model switch) planning 5 history chunks".to_string(),
            "• Compacting... (model switch) summarizing 2/5".to_string(),
            "• Compacting... (model switch) merging layer 1 (3 groups)".to_string(),
            "• Compacting... (model switch) summarizing history".to_string(),
            "• Compacting... (model switch) complete".to_string(),
            "• Compacting... (model switch) failed".to_string(),
        ]
    );
}

#[test]
fn causes_render_distinct_labels() {
    let messages = [
        CompactionCause::UserRequested,
        CompactionCause::ContextLimit,
        CompactionCause::ModelSwitch,
    ]
    .map(|cause| progress_message(cause, &CompactionStage::Summarizing));

    assert_eq!(
        messages,
        [
            "• Compacting... (requested) summarizing history".to_string(),
            "• Compacting... (context limit) summarizing history".to_string(),
            "• Compacting... (model switch) summarizing history".to_string(),
        ]
    );
}
