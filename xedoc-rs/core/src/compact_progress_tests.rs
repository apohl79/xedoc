use pretty_assertions::assert_eq;

use super::*;

#[test]
fn progress_messages_include_stage() {
    let messages = [
        CompactionStage::Preparing,
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
        CompactionStage::WritingSummary,
        CompactionStage::InstallingSummary,
        CompactionStage::Complete,
        CompactionStage::Failed,
    ]
    .map(|stage| progress_message(&stage));

    assert_eq!(
        messages,
        [
            "• Compacting preparing compaction".to_string(),
            "• Compacting planning 5 history chunks".to_string(),
            "• Compacting summarizing 2/5".to_string(),
            "• Compacting merging layer 1 (3 groups)".to_string(),
            "• Compacting summarizing history".to_string(),
            "• Compacting writing summary".to_string(),
            "• Compacting installing summary".to_string(),
            "• Compacting complete".to_string(),
            "• Compacting failed".to_string(),
        ]
    );
}
