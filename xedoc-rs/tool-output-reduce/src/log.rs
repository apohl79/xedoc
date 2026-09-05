//! Signal-preserving reduction for line-oriented logs.

const MIN_LINES: usize = 32;
const EDGE_LINES: usize = 6;
const CONTEXT_LINES: usize = 2;

const SIGNALS: &[&str] = &[
    "error",
    "fatal",
    "panic",
    "fail",
    "warn",
    "exception",
    "traceback",
];

/// Keeps signal lines and nearby context while eliding uninteresting runs.
///
/// The output is line based, bounded by the input size, and deterministic. A
/// signal is matched case-insensitively as a token substring so formats such
/// as `FATAL`, `ERROR:`, and `RuntimeError` remain visible.
pub(crate) fn reduce(input: &str) -> Option<String> {
    if input
        .lines()
        .any(|line| line.contains("log lines omitted]"))
    {
        return None;
    }
    let lines = input.lines().collect::<Vec<_>>();
    if lines.len() < MIN_LINES {
        return None;
    }

    let mut keep = vec![false; lines.len()];
    for index in 0..EDGE_LINES.min(lines.len()) {
        keep[index] = true;
    }
    for index in lines.len().saturating_sub(EDGE_LINES)..lines.len() {
        keep[index] = true;
    }
    for (index, line) in lines.iter().enumerate() {
        if is_signal(line) {
            let start = index.saturating_sub(CONTEXT_LINES);
            let end = (index + CONTEXT_LINES + 1).min(lines.len());
            keep[start..end].fill(true);
        }
    }

    let mut output_lines = Vec::with_capacity(lines.len());
    let mut omitted = 0usize;
    for (index, line) in lines.iter().enumerate() {
        if keep[index] {
            if omitted > 0 {
                output_lines.push(format!("… [{omitted} log lines omitted]"));
                omitted = 0;
            }
            output_lines.push((*line).to_owned());
        } else {
            omitted += 1;
        }
    }
    if omitted > 0 {
        output_lines.push(format!("… [{omitted} log lines omitted]"));
    }
    let mut output = output_lines.join("\n");
    if input.ends_with('\n') {
        output.push('\n');
    }
    (output.len() < input.len()).then_some(output)
}

fn is_signal(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    SIGNALS.iter().any(|signal| lower.contains(signal))
}
