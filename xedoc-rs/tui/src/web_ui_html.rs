//! Escaped HTML rendering of the same styled buffers used by the terminal.

use std::fmt::Write;

use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

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
            let _ = write!(css, "{property}:{color};");
        }
    }
    for (modifier, rule) in [
        (Modifier::BOLD, "font-weight:700;"),
        (Modifier::DIM, "opacity:.65;"),
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
