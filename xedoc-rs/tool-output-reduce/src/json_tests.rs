use super::json::crush;

#[test]
fn crushes_large_array_with_shape_summary_and_edges() {
    let input = format!(
        "[{}]",
        (0..80)
            .map(|index| format!(r#"{{"id":{index},"name":"same"}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );

    let output = crush(&input).expect("large array should reduce");
    let value: serde_json::Value = serde_json::from_str(&output).expect("reduced JSON is valid");
    let values = value.as_array().expect("array output");
    assert!(values.len() < 80);
    assert_eq!(values.first().and_then(|item| item["id"].as_i64()), Some(0));
    assert_eq!(values.get(7).and_then(|item| item["id"].as_i64()), Some(7));
    let marker = values
        .iter()
        .find_map(|item| item.get("_xedoc"))
        .expect("array includes an elision marker");
    assert_eq!(marker["type"], "elision");
    assert_eq!(marker["omitted"].as_u64(), Some(64));
    assert_eq!(marker["same_shape"], "id,name");
    assert_eq!(
        values.iter().rev().find_map(|item| item["id"].as_i64()),
        Some(79)
    );
}

#[test]
fn retains_signal_outlier_and_heterogeneous_shape() {
    let mut values = (0..50)
        .map(|index| serde_json::json!({"id": index, "status": "ok"}))
        .collect::<Vec<_>>();
    values[31] = serde_json::json!({"id": 31, "status": "error", "detail": "fatal"});
    let input = serde_json::to_string(&values).expect("input JSON");
    let output = crush(&input).expect("large array should reduce");
    let reduced: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
    assert!(
        reduced
            .as_array()
            .expect("array")
            .iter()
            .any(|item| item["status"] == "error")
    );
}

#[test]
fn preserves_tool_output_header_while_crushing_json_body() {
    let input = format!(
        "Wall time: 0.0001 seconds\nOutput:\n{}",
        serde_json::to_string(
            &(0..80)
                .map(|id| serde_json::json!({"id": id}))
                .collect::<Vec<_>>()
        )
        .expect("input JSON")
    );
    let output = crush(&input).expect("headered JSON should reduce");
    assert!(output.starts_with("Wall time: 0.0001 seconds\nOutput:\n"));
    assert!(output.len() < input.len());
}

#[test]
fn invalid_scalar_or_oversized_input_is_passthrough() {
    assert!(crush(r#"{"broken":"#).is_none());
    assert!(crush("42").is_none());
    assert!(crush(&" ".repeat(1_024 * 1_024 + 1)).is_none());
}
