//! Browser-flowing transcript content and escaped terminal overlays.

use std::fmt::Write;

use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::FinalMessageSeparator;
use crate::history_cell::HistoryCell;
use crate::history_cell::ProposedPlanCell;
use crate::history_cell::ToolCallCountSummaryCell;
use crate::history_cell::UserHistoryCell;

const HISTORY_LINES_PER_PAGE: usize = 240;
const MARKDOWN_PAGE_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum StreamHtmlKind {
    Assistant,
    Plan,
}

pub(crate) fn history_cell_page_count(cell: &dyn HistoryCell) -> u16 {
    let markdown_page_count = cell
        .as_any()
        .downcast_ref::<UserHistoryCell>()
        .map(|user| markdown_pages(&user.message).len())
        .or_else(|| {
            cell.as_any()
                .downcast_ref::<AgentMarkdownCell>()
                .map(|message| markdown_pages(message.markdown_source()).len())
        })
        .or_else(|| {
            cell.as_any()
                .downcast_ref::<ProposedPlanCell>()
                .map(|plan| markdown_pages(plan.markdown_source()).len())
        });
    if let Some(pages) = markdown_page_count {
        return u16::try_from(pages).unwrap_or(u16::MAX);
    }
    if cell.as_any().is::<ToolCallCountSummaryCell>() || cell.as_any().is::<FinalMessageSeparator>()
    {
        return 1;
    }
    let pages = cell
        .display_lines(u16::MAX)
        .len()
        .div_ceil(HISTORY_LINES_PER_PAGE)
        .max(1);
    u16::try_from(pages).unwrap_or(u16::MAX)
}

pub(crate) fn history_cell_html(cell: &dyn HistoryCell, page_start: u16, page_end: u16) -> String {
    if let Some(user) = cell.as_any().downcast_ref::<UserHistoryCell>() {
        let pages = markdown_pages(&user.message);
        let start = usize::from(page_start);
        let end = usize::from(page_end).min(pages.len());
        let mut html = String::new();
        for (index, page) in pages
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            html.push_str("<div class=\"history-user\"><div>");
            if index == 0 {
                for (image_index, _) in user.remote_image_urls.iter().enumerate() {
                    let _ = write!(
                        html,
                        "<div class=\"image-anchor\">[Image #{}]</div>",
                        image_index + 1
                    );
                }
            }
            html.push_str(&markdown_html(page));
            html.push_str("</div></div>");
        }
        return html;
    }
    if let Some(message) = cell.as_any().downcast_ref::<AgentMarkdownCell>() {
        return paged_markdown_html(
            "history-markdown",
            message.markdown_source(),
            page_start,
            page_end,
            None,
        );
    }
    if let Some(plan) = cell.as_any().downcast_ref::<ProposedPlanCell>() {
        return paged_markdown_html(
            "history-plan",
            plan.markdown_source(),
            page_start,
            page_end,
            Some("Proposed Plan"),
        );
    }
    let (class, lines) =
        if let Some(summary) = cell.as_any().downcast_ref::<ToolCallCountSummaryCell>() {
            ("history-edit-summary", summary.summary_lines())
        } else if let Some(summary) = cell.as_any().downcast_ref::<FinalMessageSeparator>() {
            ("history-turn-summary", summary.summary_lines())
        } else {
            let class = if cell.as_any().is::<AgentMessageCell>() {
                "history-agent"
            } else {
                "history-tool"
            };
            let lines = cell.display_lines(u16::MAX);
            let start = usize::from(page_start).saturating_mul(HISTORY_LINES_PER_PAGE);
            let end = usize::from(page_end)
                .saturating_mul(HISTORY_LINES_PER_PAGE)
                .min(lines.len());
            (
                class,
                lines
                    .into_iter()
                    .skip(start)
                    .take(end.saturating_sub(start))
                    .collect(),
            )
        };
    styled_lines_html(class, &lines)
}

fn paged_markdown_html(
    class: &str,
    source: &str,
    page_start: u16,
    page_end: u16,
    heading: Option<&str>,
) -> String {
    let pages = markdown_pages(source);
    let start = usize::from(page_start);
    let end = usize::from(page_end).min(pages.len());
    let mut html = String::new();
    for (index, page) in pages
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
    {
        let _ = write!(html, "<div class=\"{class}\">");
        if index == 0
            && let Some(heading) = heading
        {
            let _ = write!(html, "<div class=\"plan-heading\">{heading}</div>");
        }
        html.push_str(&markdown_html(page));
        html.push_str("</div>");
    }
    html
}

pub(crate) fn markdown_pages(source: &str) -> Vec<&str> {
    let mut pages = Vec::new();
    let mut page_start = 0;
    let mut offset = 0;
    let mut fence = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed
            .bytes()
            .take_while(|byte| matches!(byte, b'`' | b'~'))
            .count();
        if marker >= 3 {
            let character = trimmed.as_bytes()[0];
            if fence.is_some_and(|(active, length)| active == character && marker >= length) {
                fence = None;
            } else if fence.is_none() {
                fence = Some((character, marker));
            }
        }
        offset += line.len();
        if fence.is_none() && line.trim().is_empty() && offset - page_start >= MARKDOWN_PAGE_BYTES {
            pages.push(&source[page_start..offset]);
            page_start = offset;
        }
    }
    if page_start < source.len() || pages.is_empty() {
        pages.push(&source[page_start..]);
    }
    pages
}

pub(crate) fn stream_page_html(kind: StreamHtmlKind, source: &str, first_page: bool) -> String {
    match kind {
        StreamHtmlKind::Assistant => format!(
            "<div class=\"history-markdown history-streaming\">{}</div>",
            markdown_html(source)
        ),
        StreamHtmlKind::Plan => format!(
            "<div class=\"history-plan\">{}{}</div>",
            if first_page {
                "<div class=\"plan-heading\">Proposed Plan</div>"
            } else {
                ""
            },
            markdown_html(source)
        ),
    }
}

fn styled_lines_html(class: &str, lines: &[ratatui::text::Line<'static>]) -> String {
    let mut html = format!("<div class=\"{class} history-lines\">");
    for line in lines {
        let _ = write!(html, "<div style=\"{}\">", css_style(line.style));
        for span in &line.spans {
            let mut text = span.content.to_string();
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
                        let mut open = String::from(
                            "<a target=\"_blank\" rel=\"noopener noreferrer\" href=\"",
                        );
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
