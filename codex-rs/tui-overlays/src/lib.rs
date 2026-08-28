//! Interactive modal and selection overlays for the Codex TUI.

#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]

pub use codex_tui_completion::skills_helpers;
pub use codex_tui_input::clipboard_paste;
pub use codex_tui_input::key_hint;
pub use codex_tui_input::keymap;
pub use codex_tui_render::city_lights;
pub use codex_tui_render::color;
pub use codex_tui_render::diff_model;
pub use codex_tui_render::render;
pub use codex_tui_render::style;
pub use codex_tui_render::terminal_hyperlinks;
pub use codex_tui_render::terminal_palette;
pub use codex_tui_render::wrapping;
pub use codex_tui_transcript::exec_cell;
pub use codex_tui_transcript::exec_command;
pub use codex_tui_transcript::history_cell;
pub use codex_tui_transcript::inline_visualization;
pub use codex_tui_transcript::line_truncation;
pub use codex_tui_transcript::text_formatting;
pub use codex_tui_transcript::ui_consts;

pub mod app {
    pub mod app_server_requests {
        pub use codex_tui_events::ResolvedAppServerRequest;
    }
}

pub mod app_command {
    pub use codex_tui_events::AppCommand;
}

pub mod app_event {
    pub use codex_tui_events::*;
}

pub mod app_event_sender {
    pub use codex_tui_events::AppEventSender;
}

pub mod app_server_approval_conversions {
    pub use codex_tui_events::approval_conversions::*;
}

pub mod tui {
    pub use codex_tui_frame::FrameRequester;
}

#[cfg(test)]
pub mod test_backend {
    pub use codex_tui_test_support::VT100Backend;
}

#[cfg(test)]
pub mod test_support {
    pub use codex_tui_test_support::*;
}

pub mod bottom_pane;
pub mod pager_overlay;

#[cfg(test)]
mod pager_overlay_inline_visualization_tests;
