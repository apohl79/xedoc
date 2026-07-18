//! City Lights (Doom Emacs) color palette for the TUI.
//!
//! Provides an extension trait [`CityLightsStylize`] that mirrors ratatui's
//! [`Stylize`] trait but emits City Lights RGB colors instead of terminal ANSI
//! color names. Import `CityLightsStylize` and replace `.cyan()` / `.green()` /
//! `.red()` / `.magenta()` with `.cl_cyan()` / `.cl_green()` / `.cl_red()` /
//! `.cl_magenta()`.

use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Span;

// -- City Lights palette (Doom Emacs) ----------------------------------------

/// Background / base.
pub const CL_BG: (u8, u8, u8) = (0x1D, 0x25, 0x2C);
/// Foreground / primary text.
pub const CL_FG: (u8, u8, u8) = (0xA8, 0xB7, 0xC5);
/// Comments / dim text.
pub const CL_COMMENT: (u8, u8, u8) = (0x41, 0x50, 0x5E);
/// Strings / success.
pub const CL_GREEN: (u8, u8, u8) = (0x5C, 0xD6, 0xB6);
/// Keywords / blue.
pub const CL_BLUE: (u8, u8, u8) = (0x70, 0xA5, 0xE0);
/// Functions / bright blue.
pub const CL_BRIGHT_BLUE: (u8, u8, u8) = (0x53, 0x9A, 0xFC);
/// Types / magenta-purple.
pub const CL_PURPLE: (u8, u8, u8) = (0xA0, 0x6B, 0xEA);
/// Constants / teal-cyan.
pub const CL_TEAL: (u8, u8, u8) = (0x00, 0x8B, 0x94);
/// Warnings / warm orange.
pub const CL_WARNING: (u8, u8, u8) = (0xD9, 0x8E, 0x48);
/// Errors.
pub const CL_RED: (u8, u8, u8) = (0xD9, 0x54, 0x68);

// -- RGB helpers --------------------------------------------------------------

#[allow(clippy::disallowed_methods)]
fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

// -- Extension trait ----------------------------------------------------------

/// Extension trait that provides City Lights color shortcuts for ratatui
/// [`Style`] and [`Span`]. Use `.cl_cyan()` instead of `.cyan()`, etc.
pub trait CityLightsStylize {
    type Output;
    fn cl_cyan(self) -> Self::Output;
    fn cl_green(self) -> Self::Output;
    fn cl_red(self) -> Self::Output;
    fn cl_magenta(self) -> Self::Output;
}

impl CityLightsStylize for Style {
    type Output = Style;
    fn cl_cyan(self) -> Self::Output {
        self.fg(rgb(CL_TEAL))
    }
    fn cl_green(self) -> Self::Output {
        self.fg(rgb(CL_GREEN))
    }
    fn cl_red(self) -> Self::Output {
        self.fg(rgb(CL_RED))
    }
    fn cl_magenta(self) -> Self::Output {
        self.fg(rgb(CL_PURPLE))
    }
}

impl CityLightsStylize for Span<'_> {
    type Output = Span<'static>;
    fn cl_cyan(self) -> Self::Output {
        Span::from(self.content.into_owned()).style(Style::default().fg(rgb(CL_TEAL)))
    }
    fn cl_green(self) -> Self::Output {
        Span::from(self.content.into_owned()).style(Style::default().fg(rgb(CL_GREEN)))
    }
    fn cl_red(self) -> Self::Output {
        Span::from(self.content.into_owned()).style(Style::default().fg(rgb(CL_RED)))
    }
    fn cl_magenta(self) -> Self::Output {
        Span::from(self.content.into_owned()).style(Style::default().fg(rgb(CL_PURPLE)))
    }
}

impl CityLightsStylize for &str {
    type Output = Span<'static>;
    fn cl_cyan(self) -> Self::Output {
        Span::from(self).cl_cyan()
    }
    fn cl_green(self) -> Self::Output {
        Span::from(self).cl_green()
    }
    fn cl_red(self) -> Self::Output {
        Span::from(self).cl_red()
    }
    fn cl_magenta(self) -> Self::Output {
        Span::from(self).cl_magenta()
    }
}

impl CityLightsStylize for String {
    type Output = Span<'static>;
    fn cl_cyan(self) -> Self::Output {
        Span::from(self).cl_cyan()
    }
    fn cl_green(self) -> Self::Output {
        Span::from(self).cl_green()
    }
    fn cl_red(self) -> Self::Output {
        Span::from(self).cl_red()
    }
    fn cl_magenta(self) -> Self::Output {
        Span::from(self).cl_magenta()
    }
}

// -- City Lights-specific style helpers ---------------------------------------

/// Returns a City Lights-tinted background color for the user
/// message/composer area. Dark terminals get a subtle light tint; light
/// terminals get a subtle dark tint.
pub fn user_message_bg_cl(terminal_bg: Option<(u8, u8, u8)>) -> Color {
    let use_light_tint = terminal_bg.map_or(true, |bg| crate::color::is_light(bg));
    if use_light_tint {
        rgb(crate::color::blend(
            (0, 0, 0),
            terminal_bg.unwrap_or(CL_BG),
            0.04,
        ))
    } else {
        rgb(crate::color::blend(
            (255, 255, 255),
            terminal_bg.unwrap_or(CL_BG),
            0.08,
        ))
    }
}

/// Accent style for selected / active elements.
pub fn accent_style_cl() -> Style {
    Style::default().fg(rgb(CL_TEAL)).bold()
}

/// Style for the composer top border.
pub fn composer_border_style() -> Style {
    Style::default().fg(rgb(CL_COMMENT))
}

// -- Widget-type impls --------------------------------------------------------

impl<'a> CityLightsStylize for ratatui::text::Line<'a> {
    type Output = ratatui::text::Line<'a>;
    fn cl_cyan(self) -> Self::Output {
        self.style(Style::default().fg(rgb(CL_TEAL)))
    }
    fn cl_green(self) -> Self::Output {
        self.style(Style::default().fg(rgb(CL_GREEN)))
    }
    fn cl_red(self) -> Self::Output {
        self.style(Style::default().fg(rgb(CL_RED)))
    }
    fn cl_magenta(self) -> Self::Output {
        self.style(Style::default().fg(rgb(CL_PURPLE)))
    }
}

impl<'a> CityLightsStylize for ratatui::widgets::Paragraph<'a> {
    type Output = ratatui::widgets::Paragraph<'a>;
    fn cl_cyan(self) -> Self::Output {
        self.style(Style::default().fg(rgb(CL_TEAL)))
    }
    fn cl_green(self) -> Self::Output {
        self.style(Style::default().fg(rgb(CL_GREEN)))
    }
    fn cl_red(self) -> Self::Output {
        self.style(Style::default().fg(rgb(CL_RED)))
    }
    fn cl_magenta(self) -> Self::Output {
        self.style(Style::default().fg(rgb(CL_PURPLE)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::style::Modifier;

    #[test]
    fn cl_cyan_uses_teal() {
        let span: Span = "text".cl_cyan();
        assert_eq!(span.style.fg, Some(rgb(CL_TEAL)));
    }

    #[test]
    fn cl_green_uses_mint() {
        let span: Span = "text".cl_green();
        assert_eq!(span.style.fg, Some(rgb(CL_GREEN)));
    }

    #[test]
    fn cl_red_uses_rose() {
        let span: Span = "text".cl_red();
        assert_eq!(span.style.fg, Some(rgb(CL_RED)));
    }

    #[test]
    fn cl_magenta_uses_purple() {
        let span: Span = "text".cl_magenta();
        assert_eq!(span.style.fg, Some(rgb(CL_PURPLE)));
    }

    #[test]
    fn user_message_bg_dark_terminal_uses_light_tint() {
        let dark_bg = Some((0x1D, 0x25, 0x2C));
        let bg = user_message_bg_cl(dark_bg);
        assert_ne!(bg, rgb(CL_BG));
    }

    #[test]
    fn accent_style_uses_teal() {
        let style = accent_style_cl();
        assert_eq!(style.fg, Some(rgb(CL_TEAL)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }
}
