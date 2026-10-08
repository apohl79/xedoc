//! Styled rendering of a remote session's transcript text.

use ratatui::style::Stylize;
use ratatui::text::Line;

/// Headings the remote host puts on each transcript item.
const SECTION_LABELS: [&str; 8] = [
    "User",
    "Remote agent",
    "Reasoning",
    "Plan",
    "Command",
    "File change",
    "Tool call",
    "Sub-agent",
];

/// Renders transcript text, emphasizing item headings and dimming reasoning and commands.
pub(super) fn remote_output_lines(text: &str) -> Vec<Line<'static>> {
    let mut dim_body = false;
    text.lines()
        .map(|line| {
            if let Some(label) = line
                .strip_suffix(':')
                .filter(|label| SECTION_LABELS.contains(label))
            {
                dim_body = matches!(label, "Reasoning" | "Command");
                line.to_string().bold().into()
            } else if dim_body {
                line.to_string().dim().into()
            } else {
                line.to_string().into()
            }
        })
        .collect()
}
