use super::log;

#[test]
fn preserves_fatal_and_context_buried_in_log() {
    let mut lines = (0..100)
        .map(|index| format!("2026-01-01T00:00:{index:02}Z INFO setup"))
        .collect::<Vec<_>>();
    lines[67] = "2026-01-01T00:01:07Z FATAL disk unavailable".to_owned();
    let input = lines.join("\n");
    let reduced = log::reduce(&input).expect("large logs should reduce");
    let mut expected = lines[..6].iter().map(String::as_str).collect::<Vec<_>>();
    expected.push("… [59 log lines omitted]");
    expected.extend(lines[65..70].iter().map(String::as_str));
    expected.push("… [24 log lines omitted]");
    expected.extend(lines[94..].iter().map(String::as_str));
    let expected = expected.join("\n");
    assert_eq!(reduced, expected);
}

#[test]
fn line_numbered_search_output_is_not_a_log() {
    let input = (0..40)
        .map(|index| format!("src/main.rs:{index}: INFO setup"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(crate::detect_payload_kind(&input) != crate::PayloadKind::Log);
}
