//! Conservative, payload-agnostic text normalization.

/// Normalize presentation-only differences without changing content tokens.
pub(crate) fn normalize(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut blank = false;
    for source_line in input.lines() {
        let line = source_line.trim_end_matches(char::is_whitespace);
        if line.is_empty() {
            if blank {
                continue;
            }
            blank = true;
            output.push('\n');
            continue;
        }
        blank = false;
        output.push_str(&strip_ansi(line));
        output.push('\n');
    }
    if !input.ends_with('\n') {
        output.pop();
    }
    output
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for control in chars.by_ref() {
            if ('@'..='~').contains(&control) {
                break;
            }
        }
    }
    output
}
