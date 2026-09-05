//! Bounded, shape-preserving reduction for JSON tool output.

use serde_json::Map;
use serde_json::Value;
use std::collections::BTreeSet;
use xedoc_protocol::protocol::TruncationPolicy;

const MAX_INPUT_BYTES: usize = 1_024 * 1_024;
const MAX_DEPTH: usize = 64;
const ARRAY_THRESHOLD: usize = 32;
const EDGE_ITEMS: usize = 8;
const MAX_OUTLIERS: usize = 16;

/// Crushes large JSON arrays while preserving ordering and representative items.
///
/// The input must be one complete JSON object or array. Invalid, oversized, or
/// deeply nested values are rejected so callers can pass the original through
/// unchanged. Objects are never reordered and arrays are never reordered.
pub(crate) fn crush(input: &str) -> Option<String> {
    if input.len() > MAX_INPUT_BYTES {
        return None;
    }
    let (prefix, candidate) = split_output_header(input);
    let value = serde_json::from_str::<Value>(candidate.trim()).ok()?;
    if !matches!(value, Value::Object(_) | Value::Array(_)) {
        return None;
    }
    if depth(&value, 0) > MAX_DEPTH {
        return None;
    }

    let reduced = crush_value(value);
    let serialized = serde_json::to_string(&reduced).ok()?;
    let output = format!("{prefix}{serialized}");
    (output.len() < input.len()).then_some(output)
}

/// Fits a JSON payload to a byte budget while keeping the payload parseable.
///
/// The optional tool-output header is preserved when it fits. If the reduced
/// value is still too large, it is replaced with a compact elision object (or
/// a shortened JSON string for very small budgets).
pub(crate) fn fit_to_budget(input: &str, policy: TruncationPolicy) -> Option<String> {
    let budget = policy.byte_budget();
    let (prefix, candidate) = split_output_header(input);
    let value = serde_json::from_str::<Value>(candidate.trim()).ok()?;
    let serialized = serde_json::to_string(&value).ok()?;
    let (prefix, available) = budget
        .checked_sub(prefix.len())
        .map_or(("", budget), |available| (prefix, available));
    if serialized.len() <= available {
        return Some(format!("{prefix}{serialized}"));
    }

    let marker = serde_json::json!({"_xedoc":{"type":"elision"}});
    let marker = serde_json::to_string(&marker).ok()?;
    if marker.len() <= available {
        return Some(format!("{prefix}{marker}"));
    }

    // A JSON string is the smallest useful valid representation for tiny
    // budgets. Reserve two bytes for quotes and truncate at UTF-8 boundaries.
    if available < 2 {
        return None;
    }
    let max_content = available - 2;
    let mut content = candidate
        .trim()
        .chars()
        .take(max_content)
        .collect::<String>();
    while serde_json::to_string(&content).is_ok_and(|encoded| encoded.len() > available) {
        content.pop();
    }
    Some(format!("{prefix}{}", serde_json::to_string(&content).ok()?))
}

fn split_output_header(input: &str) -> (&str, &str) {
    input.find("\nOutput:\n").map_or(("", input), |offset| {
        input.split_at(offset + "\nOutput:\n".len())
    })
}

fn depth(value: &Value, current: usize) -> usize {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|value| depth(value, current.saturating_add(1)))
            .max()
            .unwrap_or(current),
        Value::Object(values) => values
            .values()
            .map(|value| depth(value, current.saturating_add(1)))
            .max()
            .unwrap_or(current),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => current,
    }
}

fn crush_value(value: Value) -> Value {
    match value {
        Value::Array(values) => {
            let values = values.into_iter().map(crush_value).collect::<Vec<_>>();
            if values.len() <= ARRAY_THRESHOLD {
                return Value::Array(values);
            }

            let signatures = values.iter().map(shape_signature).collect::<Vec<_>>();
            // Count shapes while retaining first-occurrence order so ties are
            // deterministic across runs (HashMap iteration order is not).
            let mut counts = Vec::new();
            for shape in &signatures {
                if let Some((_, count)) =
                    counts.iter_mut().find(|(seen, _)| *seen == shape.as_str())
                {
                    *count += 1;
                } else {
                    counts.push((shape.as_str(), 1usize));
                }
            }
            let dominant = counts
                .into_iter()
                .max_by_key(|(_, count)| *count)
                .map(|(shape, _)| shape);

            let mut selected = Vec::with_capacity(EDGE_ITEMS * 2 + MAX_OUTLIERS + 1);
            let mut selected_indices = BTreeSet::new();
            for index in 0..EDGE_ITEMS {
                selected_indices.insert(index);
            }
            for index in values.len().saturating_sub(EDGE_ITEMS)..values.len() {
                selected_indices.insert(index);
            }
            if let Some(dominant) = dominant {
                for (index, shape) in signatures.iter().enumerate() {
                    if shape != dominant && selected_indices.len() < EDGE_ITEMS * 2 + MAX_OUTLIERS {
                        selected_indices.insert(index);
                    }
                }
                for (index, value) in values.iter().enumerate() {
                    if selected_indices.len() >= EDGE_ITEMS * 2 + MAX_OUTLIERS {
                        break;
                    }
                    if is_signal_outlier(value) {
                        selected_indices.insert(index);
                    }
                }
            }

            let omitted = values.len().saturating_sub(selected_indices.len());
            for index in selected_indices {
                if let Some(value) = values.get(index) {
                    selected.push(value.clone());
                }
            }
            if omitted > 0 {
                let shape = dominant.unwrap_or("heterogeneous");
                let mut marker = Map::new();
                marker.insert(
                    "_xedoc".to_owned(),
                    serde_json::json!({
                        "type": "elision",
                        "omitted": omitted,
                        "same_shape": shape,
                    }),
                );
                selected.push(Value::Object(marker));
            }
            Value::Array(selected)
        }
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, crush_value(value)))
                .collect(),
        ),
        scalar => scalar,
    }
}

fn is_signal_outlier(value: &Value) -> bool {
    let text = value.to_string().to_ascii_lowercase();
    [
        "error",
        "fatal",
        "panic",
        "exception",
        "failed",
        "failure",
        "warn",
    ]
    .iter()
    .any(|signal| text.contains(signal))
}

fn shape_signature(value: &Value) -> String {
    match value {
        Value::Object(values) => {
            let mut keys = values.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            keys.join(",")
        }
        Value::Array(_) => "[]".to_owned(),
        Value::Null => "null".to_owned(),
        Value::Bool(_) => "bool".to_owned(),
        Value::Number(_) => "number".to_owned(),
        Value::String(_) => "string".to_owned(),
    }
}
