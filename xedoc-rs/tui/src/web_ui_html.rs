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
use crate::history_cell::ToolCallSummaryCell;
use crate::history_cell::UserHistoryCell;
use xedoc_tui_transcript::exec_cell::ExecCell;

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
            html.push_str("<div class=\"history-user\"");
            if index == 0 {
                html.push_str(" data-prompt=\"");
                escape_html(&mut html, &user.message);
                html.push('"');
            }
            html.push_str("><div>");
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
            let lines = cell.display_lines(u16::MAX);
            let (class, lines) = if cell.as_any().is::<AgentMessageCell>() {
                ("history-agent", lines)
            } else if let Some(exec) = cell.as_any().downcast_ref::<ExecCell>() {
                if exec.is_exploring_cell() {
                    ("history-tool history-command", lines)
                } else {
                    let has_command_continuations = lines.iter().skip(1).any(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                            .starts_with("  │ ")
                    });
                    let mut command_lines = Vec::with_capacity(lines.len());
                    for (index, mut line) in lines.into_iter().enumerate() {
                        let is_command_continuation = line
                            .spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                            .starts_with("  │ ");
                        if index == 0 {
                            if has_command_continuations {
                                line.spans.push(ratatui::text::Span::styled(
                                    " …",
                                    Style::default().add_modifier(Modifier::DIM),
                                ));
                            }
                            command_lines.push(line);
                        } else if !is_command_continuation {
                            command_lines.push(line);
                        }
                    }
                    (
                        "history-tool history-command history-tool-box",
                        command_lines,
                    )
                }
            } else if cell.as_any().is::<ToolCallSummaryCell>() {
                ("history-tool history-tool-call history-command", lines)
            } else {
                ("history-tool", lines)
            };
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
    let command_cell = class
        .split_ascii_whitespace()
        .any(|class| class == "history-command");
    for (index, line) in lines.iter().enumerate() {
        let mut style = css_style(line.style);
        if command_cell && index == 0 {
            style.push_str("overflow:hidden;text-overflow:ellipsis;white-space:pre;");
        }
        let _ = write!(html, "<div style=\"{style}\">");
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
    let source = normalize_blockquote_continuations(source);
    let mut blockquote_depth = 0;
    let mut suppress_next_block_margin = false;
    for event in Parser::new_ext(
        &source,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    ) {
        match event {
            Event::Start(tag) => {
                let starts_blockquote = matches!(&tag, Tag::BlockQuote);
                if starts_blockquote {
                    blockquote_depth += 1;
                }
                let removes_following_margin = suppress_next_block_margin
                    && matches!(
                        &tag,
                        Tag::Paragraph
                            | Tag::Heading { .. }
                            | Tag::BlockQuote
                            | Tag::CodeBlock(_)
                            | Tag::List(_)
                            | Tag::Table(_)
                    );
                if removes_following_margin {
                    suppress_next_block_margin = false;
                }
                let (open, close) = match tag {
                    Tag::Paragraph if blockquote_depth > 0 => {
                        ("<p style=\"margin-block: 0\">".to_string(), "</p>")
                    }
                    Tag::Paragraph if removes_following_margin => {
                        ("<p style=\"margin-block-start: 0\">".to_string(), "</p>")
                    }
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
                    Tag::BlockQuote => (
                        "<blockquote style=\"margin-block: 0; padding-block: 0\">".to_string(),
                        "</blockquote>",
                    ),
                    Tag::CodeBlock(_) => {
                        let margin = if removes_following_margin {
                            " style=\"margin-block-start: 0\""
                        } else {
                            ""
                        };
                        (format!("<pre{margin}><code>"), "</code></pre>")
                    }
                    Tag::List(Some(start)) => {
                        let margin = if removes_following_margin {
                            "margin-block-start: 0; "
                        } else {
                            ""
                        };
                        (
                            format!(
                                "<ol start=\"{start}\" style=\"{margin}padding-left: calc(1.6em - 10px)\">"
                            ),
                            "</ol>",
                        )
                    }
                    Tag::List(None) => {
                        let margin = if removes_following_margin {
                            "margin-block-start: 0; "
                        } else {
                            ""
                        };
                        (
                            format!("<ul style=\"{margin}padding-left: calc(1.6em - 10px)\">"),
                            "</ul>",
                        )
                    }
                    Tag::Item => ("<li>".to_string(), "</li>"),
                    Tag::Table(_) => {
                        let margin = if removes_following_margin {
                            " style=\"margin-block-start: 0\""
                        } else {
                            ""
                        };
                        (
                            format!("<div class=\"table-scroll\"{margin}><table>"),
                            "</table></div>",
                        )
                    }
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
            Event::End(tag) => {
                html.push_str(closing.pop().unwrap_or_default());
                if matches!(tag, pulldown_cmark::TagEnd::BlockQuote) {
                    blockquote_depth -= 1;
                    suppress_next_block_margin = true;
                }
            }
            Event::Rule if suppress_next_block_margin => {
                html.push_str("<hr style=\"margin-block-start: 0\">");
                suppress_next_block_margin = false;
            }
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

fn normalize_blockquote_continuations(source: &str) -> String {
    let mut normalized = String::with_capacity(source.len());
    let mut previous_line_was_blockquote = false;
    let mut fence: Option<(u8, usize, usize)> = None;

    let fence_run = |line: &str| {
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        if indent > 3 {
            return None;
        }
        let remainder = &line[indent..];
        let marker = *remainder.as_bytes().first()?;
        if marker != b'`' && marker != b'~' {
            return None;
        }
        let length = remainder.bytes().take_while(|byte| *byte == marker).count();
        (length >= 3).then_some((marker, length))
    };

    let fence_closes = |line: &str, marker: u8, length: usize| {
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        if indent > 3 {
            return false;
        }
        let remainder = &line[indent..];
        let marker_count = remainder.bytes().take_while(|byte| *byte == marker).count();
        marker_count >= length && remainder[marker_count..].trim().is_empty()
    };

    for line in source.split_inclusive('\n') {
        let source_line = line
            .strip_suffix('\n')
            .unwrap_or(line)
            .trim_end_matches('\r');
        let mut remainder = source_line;
        let mut quote_depth = 0;
        loop {
            let indent = remainder.bytes().take_while(|byte| *byte == b' ').count();
            if indent > 3 {
                break;
            }
            remainder = &remainder[indent..];
            let Some(after_marker) = remainder.strip_prefix('>') else {
                break;
            };
            quote_depth += 1;
            remainder = after_marker.strip_prefix(' ').unwrap_or(after_marker);
        }

        let (is_blockquote_line, fence_content) = match fence {
            Some((_, _, fence_quote_depth)) if fence_quote_depth == 0 => (false, source_line),
            Some((_, _, fence_quote_depth)) => (
                quote_depth >= fence_quote_depth,
                if quote_depth >= fence_quote_depth {
                    remainder
                } else {
                    source_line
                },
            ),
            None => (quote_depth > 0, remainder),
        };

        if previous_line_was_blockquote && !is_blockquote_line && !source_line.trim().is_empty() {
            normalized.push('\n');
        }
        normalized.push_str(line);

        match fence {
            Some((_, _, fence_quote_depth)) if fence_quote_depth > 0 && !is_blockquote_line => {
                fence = fence_run(fence_content).map(|(marker, length)| (marker, length, 0));
            }
            Some((marker, length, _)) if fence_closes(fence_content, marker, length) => {
                fence = None;
            }
            Some(_) => {}
            None => {
                fence =
                    fence_run(fence_content).map(|(marker, length)| (marker, length, quote_depth));
            }
        }
        previous_line_was_blockquote = is_blockquote_line;
    }

    normalized
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
        let mut cells = Vec::new();
        while column < buffer.area.right() {
            let cell = &buffer[(column, row)];
            cells.push((
                cell.symbol().to_string(),
                cell.style(),
                cursor == Some((column, row)),
            ));
            column =
                column.saturating_add(u16::try_from(cell.symbol().width().max(1)).unwrap_or(1));
        }
        let mut text = String::new();
        let mut previous = None;
        let mut cell_index = 0;
        while cell_index < cells.len() {
            let replacement = match (
                cells.get(cell_index).map(|cell| cell.0.as_str()),
                cells.get(cell_index + 1).map(|cell| cell.0.as_str()),
                cells.get(cell_index + 2).map(|cell| cell.0.as_str()),
            ) {
                (Some("="), Some("="), Some("=")) => Some(("≡", 3)),
                (Some("="), Some(">"), _) => Some(("⇒", 2)),
                (Some("-"), Some(">"), _) => Some(("→", 2)),
                _ => None,
            };
            let (symbol, style, at_cursor, consumed) = match replacement {
                Some((symbol, consumed)) => (
                    symbol,
                    cells[cell_index].1,
                    cells[cell_index..cell_index + consumed]
                        .iter()
                        .any(|cell| cell.2),
                    consumed,
                ),
                None => (
                    cells[cell_index].0.as_str(),
                    cells[cell_index].1,
                    cells[cell_index].2,
                    1,
                ),
            };
            if previous != Some((style, at_cursor)) {
                flush_span(&mut html, &mut text, previous);
                previous = Some((style, at_cursor));
            }
            text.push_str(symbol);
            cell_index += consumed;
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
    let foreground = foreground.and_then(rgb);
    let background = background.and_then(rgb);
    for (property, color) in [
        ("color", foreground.as_deref()),
        ("background-color", background.as_deref()),
    ] {
        if let Some(color) = color {
            if property == "color" && style.add_modifier.contains(Modifier::DIM) {
                css.push_str("color:var(--fg-dim);");
            } else {
                let _ = write!(css, "{property}:{color};");
            }
        }
    }
    if foreground.is_none() && style.add_modifier.contains(Modifier::DIM) {
        css.push_str("color:var(--fg-dim);");
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
