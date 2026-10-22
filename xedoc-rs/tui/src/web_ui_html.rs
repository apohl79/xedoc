//! Browser-flowing transcript content and escaped terminal overlays.

use std::fmt::Write;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use unicode_width::UnicodeWidthStr;

use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::UserHistoryCell;

pub(crate) fn history_cell_html(
    cell: &dyn HistoryCell,
    width: u16,
    row_start: u16,
    row_end: u16,
) -> String {
    // Retain terminal row coordinates for bounded slices of unusually large cells.
    if row_start != 0 || row_end < cell.desired_height(width) {
        let area = Rect::new(0, 0, width, row_end.saturating_sub(row_start));
        let mut buffer = Buffer::empty(area);
        if let Some(style) = cell.display_background_style() {
            buffer.set_style(area, style);
        }
        Paragraph::new(Text::from(cell.display_lines(width)))
            .wrap(Wrap { trim: false })
            .scroll((row_start, 0))
            .render(area, &mut buffer);
        let class = if cell.as_any().is::<UserHistoryCell>() {
            "history-user-slice"
        } else {
            "history-slice"
        };
        return format!(
            "<div class=\"{class}\">{}</div>",
            buffer_html(&buffer, None)
        );
    }
    if let Some(user) = cell.as_any().downcast_ref::<UserHistoryCell>() {
        let mut html = String::from("<div class=\"history-user\"><div>");
        for (index, _) in user.remote_image_urls.iter().enumerate() {
            let _ = write!(
                html,
                "<div class=\"image-anchor\">[Image #{}]</div>",
                index + 1
            );
        }
        html.push_str(&markdown_html(&user.message));
        html.push_str("</div></div>");
        return html;
    }
    if let Some(message) = cell.as_any().downcast_ref::<AgentMarkdownCell>() {
        return format!(
            "<div class=\"history-markdown\">{}</div>",
            markdown_html(message.markdown_source())
        );
    }
    let class = if cell.as_any().is::<AgentMessageCell>() {
        "history-agent"
    } else {
        "history-tool"
    };
    let mut html =
        format!("<div class=\"{class}\" style=\"white-space:pre-wrap;overflow-wrap:anywhere\">");
    for line in cell.display_lines(4096) {
        let _ = write!(
            html,
            "<div style=\"min-height:1.4em;{}\">",
            css_style(line.style)
        );
        for span in line.spans {
            let mut text = span.content.into_owned();
            flush_span(
                &mut html,
                &mut text,
                Some((line.style.patch(span.style), false)),
            );
        }
        html.push_str("</div>");
    }
    html.push_str("</div>");
    html
}

fn markdown_html(source: &str) -> String {
    use pulldown_cmark::Event;
    use pulldown_cmark::Options;
    use pulldown_cmark::Parser;
    use pulldown_cmark::Tag;

    let mut html = String::new();
    let mut closing = Vec::new();
    for event in Parser::new_ext(
        source,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    ) {
        match event {
            Event::Start(tag) => {
                let (open, close) = match tag {
                    Tag::Paragraph => ("<p>".to_string(), "</p>"),
                    Tag::Heading { level, .. } => (
                        format!("<{level}>"),
                        match level {
                            pulldown_cmark::HeadingLevel::H1 => "</h1>",
                            pulldown_cmark::HeadingLevel::H2 => "</h2>",
                            pulldown_cmark::HeadingLevel::H3 => "</h3>",
                            pulldown_cmark::HeadingLevel::H4 => "</h4>",
                            pulldown_cmark::HeadingLevel::H5 => "</h5>",
                            pulldown_cmark::HeadingLevel::H6 => "</h6>",
                        },
                    ),
                    Tag::BlockQuote => ("<blockquote>".to_string(), "</blockquote>"),
                    Tag::CodeBlock(_) => ("<pre><code>".to_string(), "</code></pre>"),
                    Tag::List(Some(start)) => (format!("<ol start=\"{start}\">"), "</ol>"),
                    Tag::List(None) => ("<ul>".to_string(), "</ul>"),
                    Tag::Item => ("<li>".to_string(), "</li>"),
                    Tag::Table(_) => (
                        "<div class=\"table-scroll\"><table>".to_string(),
                        "</table></div>",
                    ),
                    Tag::TableHead => ("<tr class=\"table-head\">".to_string(), "</tr>"),
                    Tag::TableRow => ("<tr>".to_string(), "</tr>"),
                    Tag::TableCell => ("<td>".to_string(), "</td>"),
                    Tag::Emphasis => ("<em>".to_string(), "</em>"),
                    Tag::Strong => ("<strong>".to_string(), "</strong>"),
                    Tag::Strikethrough => ("<del>".to_string(), "</del>"),
                    Tag::Link { dest_url, .. } => {
                        let destination = dest_url.trim().to_ascii_lowercase();
                        let safe = !destination.contains(':')
                            || destination.starts_with("https:")
                            || destination.starts_with("http:")
                            || destination.starts_with("mailto:");
                        let mut open = String::from("<a rel=\"noreferrer\" href=\"");
                        escape_html(&mut open, if safe { &dest_url } else { "" });
                        open.push_str("\">");
                        (open, "</a>")
                    }
                    Tag::HtmlBlock
                    | Tag::FootnoteDefinition(_)
                    | Tag::Image { .. }
                    | Tag::MetadataBlock(_) => (String::new(), ""),
                };
                html.push_str(&open);
                closing.push(close);
            }
            Event::End(_) => html.push_str(closing.pop().unwrap_or_default()),
            Event::Text(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::FootnoteReference(text) => escape_html(&mut html, &text),
            Event::Code(text) => {
                html.push_str("<code>");
                escape_html(&mut html, &text);
                html.push_str("</code>");
            }
            Event::SoftBreak => html.push('\n'),
            Event::HardBreak => html.push_str("<br>"),
            Event::Rule => html.push_str("<hr>"),
            Event::TaskListMarker(checked) => html.push_str(if checked { "☑ " } else { "☐ " }),
        }
    }
    html
}

fn escape_html(html: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            '\'' => html.push_str("&#39;"),
            ch => html.push(ch),
        }
    }
}

pub(crate) fn buffer_html(buffer: &Buffer, cursor: Option<(u16, u16)>) -> String {
    let mut html = String::new();
    for row in buffer.area.top()..buffer.area.bottom() {
        html.push_str("<div class=\"line\">");
        let mut column = buffer.area.left();
        let mut text = String::new();
        let mut previous = None;
        while column < buffer.area.right() {
            let cell = &buffer[(column, row)];
            let style = cell.style();
            let at_cursor = cursor == Some((column, row));
            if previous != Some((style, at_cursor)) {
                flush_span(&mut html, &mut text, previous);
                previous = Some((style, at_cursor));
            }
            text.push_str(cell.symbol());
            column =
                column.saturating_add(u16::try_from(cell.symbol().width().max(1)).unwrap_or(1));
        }
        flush_span(&mut html, &mut text, previous);
        html.push_str("</div>");
    }
    html
}

fn flush_span(html: &mut String, text: &mut String, style: Option<(Style, bool)>) {
    if text.is_empty() {
        return;
    }
    if let Some((style, cursor)) = style {
        let class = if cursor { " class=\"cursor\"" } else { "" };
        let _ = write!(html, "<span{class} style=\"{}\">", css_style(style));
        escape_html(html, text);
        html.push_str("</span>");
    }
    text.clear();
}

fn css_style(style: Style) -> String {
    let mut css = String::new();
    let reversed = style.add_modifier.contains(Modifier::REVERSED);
    let (foreground, background) = if reversed {
        (
            style.bg.or(Some(Color::Black)),
            style.fg.or(Some(Color::Gray)),
        )
    } else {
        (style.fg, style.bg)
    };
    for (property, color) in [("color", foreground), ("background-color", background)] {
        if let Some(color) = color
            && let Some(color) = rgb(color)
        {
            if property == "color" && style.add_modifier.contains(Modifier::DIM) {
                let _ = write!(css, "color:color-mix(in srgb,{color} 65%,transparent);");
            } else {
                let _ = write!(css, "{property}:{color};");
            }
        }
    }
    if foreground.is_none() && style.add_modifier.contains(Modifier::DIM) {
        css.push_str("color:color-mix(in srgb,var(--fg,#c5cdd5) 65%,transparent);");
    }
    for (modifier, rule) in [
        (Modifier::BOLD, "font-weight:700;"),
        (Modifier::ITALIC, "font-style:italic;"),
        (Modifier::UNDERLINED, "text-decoration:underline;"),
        (Modifier::CROSSED_OUT, "text-decoration:line-through;"),
        (Modifier::HIDDEN, "visibility:hidden;"),
    ] {
        if style.add_modifier.contains(modifier) {
            css.push_str(rule);
        }
    }
    css
}

fn rgb(color: Color) -> Option<String> {
    let (red, green, blue) = match color {
        Color::Reset => return None,
        Color::Rgb(red, green, blue) => (red, green, blue),
        Color::Indexed(index) if index >= 232 => {
            let level = 8 + (index - 232) * 10;
            (level, level, level)
        }
        Color::Indexed(index) if index >= 16 => {
            let index = index - 16;
            let level = |component| {
                if component == 0 {
                    0
                } else {
                    55 + component * 40
                }
            };
            (level(index / 36), level(index / 6 % 6), level(index % 6))
        }
        color => {
            let index = match color {
                Color::Black => 0,
                Color::Red => 1,
                Color::Green => 2,
                Color::Yellow => 3,
                Color::Blue => 4,
                Color::Magenta => 5,
                Color::Cyan => 6,
                Color::Gray => 7,
                Color::DarkGray => 8,
                Color::LightRed => 9,
                Color::LightGreen => 10,
                Color::LightYellow => 11,
                Color::LightBlue => 12,
                Color::LightMagenta => 13,
                Color::LightCyan => 14,
                Color::White => 15,
                Color::Indexed(index) => usize::from(index),
                Color::Reset | Color::Rgb(..) => return None,
            };
            return Some(
                [
                    "#1b1f23", "#e27878", "#85b695", "#eac18b", "#8cafd4", "#ba9cf3", "#70b9be",
                    "#c5cdd5", "#64717d", "#ff8a8a", "#a3d6a7", "#f1d49a", "#9dc9ff", "#d1b4ff",
                    "#9cd9df", "#edf1f5",
                ][index]
                    .to_string(),
            );
        }
    };
    Some(format!("#{red:02x}{green:02x}{blue:02x}"))
}
