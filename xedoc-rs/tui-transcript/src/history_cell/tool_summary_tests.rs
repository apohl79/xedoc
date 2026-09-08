use super::*;
use pretty_assertions::assert_eq;

fn rendered_lines(cell: &ToolCallSummaryCell) -> Vec<String> {
    cell.display_lines(/*width*/ 120)
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn cap_ignores_late_completion_without_double_counting_or_ghost_progress() {
    let mut cell = ToolCallSummaryCell::new();
    cell.start_call("anchor".to_string(), "anchor".to_string());
    for index in 0..(MAX_TRACKED_CALLS + 64) {
        let id = format!("call-{index}");
        cell.start_call(id.clone(), id.clone());
        cell.complete_call(
            id,
            "completed".to_string(),
            ToolCallSummaryOutcome::Succeeded,
        );
    }
    cell.complete_call(
        "anchor".to_string(),
        "anchor".to_string(),
        ToolCallSummaryOutcome::Succeeded,
    );

    let expected = vec![
        "• Tool: anchor".to_string(),
        format!(
            "  Calls: {MAX_TRACKED_CALLS} · {MAX_TRACKED_CALLS} succeeded · 0 failed · 0 in progress · truncated"
        ),
    ];
    assert_eq!(rendered_lines(&cell), expected);

    let before_late_completion = rendered_lines(&cell);
    cell.complete_call(
        "call-overflow".to_string(),
        "late completion".to_string(),
        ToolCallSummaryOutcome::Failed,
    );
    assert_eq!(rendered_lines(&cell), before_late_completion);
}

#[test]
fn duplicate_events_remain_idempotent_after_summary_reaches_cap() {
    let mut cell = ToolCallSummaryCell::new();
    for index in 0..MAX_TRACKED_CALLS {
        let id = format!("call-{index}");
        cell.start_call(id.clone(), id.clone());
        cell.complete_call(
            id,
            "completed".to_string(),
            ToolCallSummaryOutcome::Succeeded,
        );
    }
    cell.start_call("overflow".to_string(), "overflow".to_string());
    cell.complete_call(
        "overflow".to_string(),
        "overflow".to_string(),
        ToolCallSummaryOutcome::Succeeded,
    );
    let at_cap = rendered_lines(&cell);

    cell.start_call("call-0".to_string(), "duplicate start".to_string());
    cell.complete_call(
        "call-0".to_string(),
        "duplicate completion".to_string(),
        ToolCallSummaryOutcome::Failed,
    );
    cell.start_call("untracked".to_string(), "untracked".to_string());
    cell.complete_call(
        "untracked".to_string(),
        "untracked".to_string(),
        ToolCallSummaryOutcome::Failed,
    );

    assert_eq!(rendered_lines(&cell), at_cap);
}

#[test]
fn labels_with_control_whitespace_stay_on_two_logical_rows() {
    let mut cell = ToolCallSummaryCell::new();
    cell.start_call("control".to_string(), "alpha\nbeta\tgamma".to_string());

    assert_eq!(
        rendered_lines(&cell),
        vec![
            "• Tool: alpha beta gamma".to_string(),
            "  Calls: 1 · 0 succeeded · 0 failed · 1 in progress".to_string(),
        ]
    );
}
