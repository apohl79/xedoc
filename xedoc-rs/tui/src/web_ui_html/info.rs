//! Browser gutters for terminal-origin informational lines.

use std::fmt::Write;

use ratatui::text::Line;

use super::css_style;
use super::flush_span;

pub(super) fn styled_info_lines_html(class: &str, lines: &[Line<'static>]) -> String {
    let mut html = format!("<div class=\"{class} history-info history-lines\">");
    for line in lines {
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let leading_spaces = text.len() - text.trim_start_matches(' ').len();
        let marker = text[leading_spaces..].chars().next().filter(|character| {
            matches!(
                character,
                '•' | '◦' | '■' | '⚠' | 'ⓘ' | '✘' | '✗' | '└' | '├' | '│'
            )
        });
        let branch = marker.is_some_and(|marker| matches!(marker, '└' | '├' | '│'));
        let prefix_end =
            leading_spaces
                + marker.map_or(0, char::len_utf8)
                + usize::from(marker.is_some_and(|marker| {
                    text[leading_spaces + marker.len_utf8()..].starts_with(' ')
                }));
        let indent = leading_spaces.saturating_sub(2);
        let _ = write!(
            html,
            "<div class=\"history-info-line\" style=\"{}\">",
            css_style(line.style)
        );
        if let Some(marker) = marker {
            let position = if branch {
                format!("left:{indent}ch;")
            } else {
                String::new()
            };
            let marker_style = line
                .spans
                .iter()
                .scan(0, |offset, span| {
                    *offset += span.content.len();
                    Some((*offset, span.style))
                })
                .find(|(end, _)| *end > leading_spaces)
                .map_or(line.style, |(_, style)| line.style.patch(style));
            let _ = write!(
                html,
                "<span class=\"history-info-marker\" style=\"{}{position}\">{marker}</span>",
                css_style(marker_style)
            );
        }
        let indent = indent + if branch { 2 } else { 0 };
        let _ = write!(
            html,
            "<div class=\"history-info-content\" style=\"padding-left:{indent}ch;\">"
        );
        let mut offset = 0;
        for span in &line.spans {
            let skip = prefix_end.saturating_sub(offset).min(span.content.len());
            offset += span.content.len();
            let mut text = span.content[skip..].to_string();
            flush_span(
                &mut html,
                &mut text,
                Some((line.style.patch(span.style), false)),
            );
        }
        html.push_str("</div></div>");
    }
    html.push_str("</div>");
    html
}
