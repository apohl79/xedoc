//! Signal-preserving reduction for unified git diffs.

const MAX_CONTEXT_LINES_PER_HUNK: usize = 8;

/// Removes low-value git metadata and caps unchanged context in each hunk.
pub(crate) fn reduce(input: &str) -> Option<String> {
    if input
        .lines()
        .any(|line| line.contains("unchanged diff lines omitted]"))
    {
        return None;
    }
    let lines = input.lines().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut in_hunk = false;
    let mut context = 0usize;
    let mut omitted = 0usize;

    for line in lines {
        if line.starts_with("index ") || line.starts_with("similarity index ") {
            continue;
        }
        if line.starts_with("@@ ") {
            flush_omitted(&mut output, &mut omitted);
            output.push_str(line);
            output.push('\n');
            in_hunk = true;
            context = 0;
            continue;
        }
        if line.starts_with("diff --git ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with('\\')
        {
            flush_omitted(&mut output, &mut omitted);
            output.push_str(line);
            output.push('\n');
            continue;
        }

        let is_context = in_hunk && line.starts_with(' ');
        if is_context {
            context += 1;
            if context > MAX_CONTEXT_LINES_PER_HUNK {
                omitted += 1;
                continue;
            }
        } else if line.starts_with('+') || line.starts_with('-') {
            flush_omitted(&mut output, &mut omitted);
            // Context is capped per contiguous run, not across unrelated
            // changes in the same hunk.
            context = 0;
        }
        output.push_str(line);
        output.push('\n');
    }
    flush_omitted(&mut output, &mut omitted);

    if !input.ends_with('\n') {
        output.pop();
    }
    (output.len() < input.len()).then_some(output)
}

fn flush_omitted(output: &mut String, omitted: &mut usize) {
    if *omitted > 0 {
        output.push_str(&format!("… [{omitted} unchanged diff lines omitted]\n"));
        *omitted = 0;
    }
}
