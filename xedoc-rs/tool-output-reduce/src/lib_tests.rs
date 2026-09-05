use super::*;
use pretty_assertions::assert_eq;

#[test]
fn reduce_is_pass_through() {
    let config = ReductionConfig {
        level: ReductionLevel::Balanced,
        budget: TruncationPolicy::Bytes(128),
        spill_dir: None,
    };
    let output = reduce(
        ReductionInput {
            tool_name: "shell",
            call_id: "call-1",
            text: "line one\nline two",
            command_hash: None,
        },
        &config,
    );

    assert_eq!(
        output,
        ReductionOutput {
            text: "line one\nline two".to_owned(),
            record: ReductionRecord {
                thread_id: None,
                turn_id: None,
                call_id: "call-1".to_owned(),
                command_hash: None,
                tool_name: "shell".to_owned(),
                kind: PayloadKind::Prose,
                level: ReductionLevel::Balanced,
                reducers_applied: Vec::new(),
                bytes_in: 17,
                bytes_out: 17,
                est_tokens_in: 5,
                est_tokens_out: 5,
                duration_us: output.record.duration_us,
                spilled: false,
                spill_path: None,
                recorded_at: output.record.recorded_at,
            },
        }
    );
    assert!(output.record.duration_us < 1_000_000);
}

#[test]
fn conservative_normalizes_ansi_whitespace_and_deduplicates_exact_lines() {
    let input = "\u{1b}[31mwarning\u{1b}[0m  \n\n\nwarning  \nwarning  ";
    let normalized = normalize::normalize(input);
    assert_eq!(normalized, "warning\n\nwarning\nwarning");
    assert_eq!(dedup::dedup_exact(&normalized), "warning\n\nwarning  [×2]");

    // A terminal newline is part of the payload and must survive reduction.
    assert_eq!(dedup::dedup_exact("warning\nwarning\n"), "warning  [×2]\n");
}

#[test]
fn reduction_is_idempotent_for_exact_dedup() {
    let config = ReductionConfig {
        level: ReductionLevel::Conservative,
        budget: TruncationPolicy::Bytes(512),
        spill_dir: None,
    };
    let input = ReductionInput {
        tool_name: "shell",
        call_id: "call-3",
        text: "same\nsame\nsame",
        command_hash: None,
    };
    let first = reduce(input, &config);
    let second = reduce(
        ReductionInput {
            text: &first.text,
            ..input
        },
        &config,
    );
    assert_eq!(first.text, second.text);
}

#[test]
fn structured_json_and_diff_are_not_lossily_reduced() {
    let config = ReductionConfig {
        level: ReductionLevel::Conservative,
        budget: TruncationPolicy::Bytes(512),
        spill_dir: None,
    };
    let json = reduce(
        ReductionInput {
            tool_name: "shell",
            call_id: "call-a",
            text: r#"{"items":[{"id":1},{"id":2}]}"#,
            command_hash: None,
        },
        &config,
    );
    let diff = reduce(
        ReductionInput {
            tool_name: "shell",
            call_id: "call-b",
            text: "diff --git a/a b/a\n@@ -1 +1 @@\n-old\n+new\n",
            command_hash: None,
        },
        &config,
    );
    assert_eq!(json.text, r#"{"items":[{"id":1},{"id":2}]}"#);
    assert_eq!(diff.text, "diff --git a/a b/a\n@@ -1 +1 @@\n-old\n+new\n");
    assert!(json.record.reducers_applied.is_empty());
    assert!(diff.record.reducers_applied.is_empty());
}

#[test]
fn rg_line_numbered_output_remains_passthrough() {
    let config = ReductionConfig {
        level: ReductionLevel::Aggressive,
        budget: TruncationPolicy::Bytes(4096),
        spill_dir: None,
    };
    let input_text = (1..=64)
        .map(|line| format!("src/main.rs:{line}: INFO setup"))
        .collect::<Vec<_>>()
        .join("\n");
    let output = reduce(
        ReductionInput {
            tool_name: "shell",
            call_id: "call-rg",
            text: &input_text,
            command_hash: None,
        },
        &config,
    );
    assert_eq!(output.text, input_text);
    assert_eq!(output.record.kind, PayloadKind::Prose);
}
