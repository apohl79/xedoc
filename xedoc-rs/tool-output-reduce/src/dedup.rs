//! Exact, conservative repeated-line reduction.

/// Collapse only consecutive identical non-empty lines.
pub(crate) fn dedup_exact(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let trailing_newline = input.ends_with('\n');
    let mut lines = input.lines().peekable();
    while let Some(line) = lines.next() {
        let mut count = 1usize;
        while lines.peek().is_some_and(|next| *next == line) {
            count += 1;
            lines.next();
        }
        if count > 1 && !line.is_empty() {
            output.push_str(line);
            output.push_str(&format!("  [×{count}]"));
        } else {
            output.push_str(line);
        }
        if lines.peek().is_some() {
            output.push('\n');
        }
    }
    if trailing_newline {
        output.push('\n');
    }
    output
}
