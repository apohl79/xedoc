//! Prompt composition, history, and footer rendering for the Xedoc TUI.

#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]

pub use xedoc_tui_input::clipboard_paste;
pub use xedoc_tui_input::key_hint;
pub use xedoc_tui_input::keymap;
pub use xedoc_tui_input::mention_codec;
pub use xedoc_tui_render::city_lights;
pub use xedoc_tui_render::color;
pub use xedoc_tui_render::render;
pub use xedoc_tui_render::style;
pub use xedoc_tui_render::terminal_palette;
pub use xedoc_tui_render::wrapping;
pub use xedoc_tui_transcript::history_cell;
pub use xedoc_tui_transcript::line_truncation;
pub use xedoc_tui_transcript::text_formatting;
pub use xedoc_tui_transcript::ui_consts;

pub use xedoc_tui_completion::skills_helpers;
pub use xedoc_tui_completion::slash_command;

pub mod app_event {
    pub use xedoc_tui_events::*;
}

pub mod app_event_sender {
    pub use xedoc_tui_events::AppEventSender;
}

pub mod onboarding {
    pub use xedoc_tui_render::terminal_hyperlinks::mark_underlined_hyperlink;
}

pub mod status {
    pub use xedoc_tui_transcript::text_formatting::format_tokens_compact;
}

pub mod status_indicator_widget {
    pub use xedoc_tui_transcript::text_formatting::fmt_elapsed_compact;
}

pub mod tui {
    pub use xedoc_tui_frame::FrameRequester;
}

#[cfg(test)]
pub mod test_backend {
    pub use xedoc_tui_test_support::VT100Backend;
}

#[cfg(test)]
pub mod test_support {
    pub use xedoc_tui_test_support::*;
}

pub mod bottom_pane;
