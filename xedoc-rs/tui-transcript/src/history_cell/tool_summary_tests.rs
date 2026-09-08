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
        String::new(),
        "• Ran anchor".to_string(),
        String::new(),
        format!(
            "  Calls: {MAX_TRACKED_CALLS} · {MAX_TRACKED_CALLS} succeeded · 0 failed · 0 in progress · truncated"
        ),
        String::new(),
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
            String::new(),
            "• Ran alpha beta gamma".to_string(),
            String::new(),
            "  Calls: 1 · 0 succeeded · 0 failed · 1 in progress".to_string(),
            String::new(),
        ]
    );
}

#[test]
fn persistent_summary_counts_file_and_web_activity() {
    let mut cell = ToolCallSummaryCell::new();
    cell.start_call("file".to_string(), "apply patch".to_string());
    cell.start_call(
        "search".to_string(),
        "Searched the web for \"ratatui\"".to_string(),
    );
    cell.start_call("fetch".to_string(), "Read https://ratatui.rs".to_string());

    let summary = ToolCallCountSummaryCell::new(cell.stats());
    let rendered = summary
        .display_lines(/*width*/ 20)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![
            "────────────────────".to_string(),
            "• 1 file edited. 1 web search performed. 1 web page fetched.".to_string(),
            "────────────────────".to_string(),
        ]
    );
}

#[test]
fn running_command_shows_the_live_output_tail() {
    let mut cell = ToolCallSummaryCell::new();
    cell.start_call_with_preview(
        "command".to_string(),
        "command".to_string(),
        Some(ToolCallSummaryPreview::Command {
            command: "cargo test".to_string(),
            output: Some(String::new()),
        }),
    );
    cell.append_command_output("command", "first\nsecond\nthird\nfourth\n");

    assert_eq!(
        rendered_lines(&cell),
        vec![
            String::new(),
            "• Running cargo test".to_string(),
            "  ... 1 more lines".to_string(),
            "  second".to_string(),
            "  third".to_string(),
            "  fourth".to_string(),
            String::new(),
            "  Calls: 1 · 0 succeeded · 0 failed · 1 in progress".to_string(),
            String::new(),
        ]
    );
}

#[test]
fn completed_command_preview_says_ran_when_another_call_is_in_progress() {
    let mut cell = ToolCallSummaryCell::new();
    cell.start_call_with_preview(
        "first".to_string(),
        "first".to_string(),
        Some(ToolCallSummaryPreview::Command {
            command: "first command".to_string(),
            output: Some(String::new()),
        }),
    );
    cell.start_call_with_preview(
        "second".to_string(),
        "second".to_string(),
        Some(ToolCallSummaryPreview::Command {
            command: "second command".to_string(),
            output: Some(String::new()),
        }),
    );
    cell.complete_call_with_preview(
        "first".to_string(),
        "first command".to_string(),
        ToolCallSummaryOutcome::Succeeded,
        Some(ToolCallSummaryPreview::Command {
            command: "first command".to_string(),
            output: Some("complete".to_string()),
        }),
    );

    assert_eq!(rendered_lines(&cell)[1], "• Ran first command");
}

#[test]
fn file_change_preview_uses_apply_patch_and_syntax_styles() {
    let preview = |path: &str| ToolCallSummaryPreview::FileChange {
        path: path.to_string(),
        added: 1,
        removed: 0,
        diff_lines: vec!["+let answer = 42;".to_string()],
    };
    let mut rust = ToolCallSummaryCell::new();
    rust.start_call_with_preview(
        "rust".to_string(),
        "apply patch".to_string(),
        Some(preview("src/main.rs")),
    );
    let rust_lines = rust.display_lines(/*width*/ 120);

    let mut plain = ToolCallSummaryCell::new();
    plain.start_call_with_preview(
        "plain".to_string(),
        "apply patch".to_string(),
        Some(preview("src/main.txt")),
    );
    let plain_lines = plain.display_lines(/*width*/ 120);

    assert_eq!(rust_lines[1].to_string(), "• Edited src/main.rs +1 -0");
    let rust_styles = rust_lines[2]
        .spans
        .iter()
        .map(|span| span.style)
        .collect::<Vec<_>>();
    let plain_styles = plain_lines[2]
        .spans
        .iter()
        .map(|span| span.style)
        .collect::<Vec<_>>();
    assert_ne!(rust_styles, plain_styles);
}

#[test]
fn command_header_is_limited_to_one_terminal_row() {
    let mut cell = ToolCallSummaryCell::new();
    cell.start_call_with_preview(
        "command".to_string(),
        "command".to_string(),
        Some(ToolCallSummaryPreview::Command {
            command: "cargo test --package xedoc-tui-transcript --test very-long-test-name"
                .to_string(),
            output: Some(String::new()),
        }),
    );

    let rendered = cell.display_lines(/*width*/ 30);
    assert!(rendered[1].to_string().ends_with('…'));
    assert!(rendered[1].width() <= 30);
}

#[test]
fn fallback_action_verb_uses_the_shared_accent_style() {
    let mut cell = ToolCallSummaryCell::new();
    cell.start_call("web".to_string(), "Searched the web".to_string());

    let line = cell.display_lines(/*width*/ 80).remove(1);
    assert_eq!(line.spans[1].content, "Ran ");
    assert_eq!(line.spans[1].style.fg, None);
    assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
}
