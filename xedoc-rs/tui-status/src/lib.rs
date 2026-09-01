//! Status models and rendering shared by Xedoc TUI orchestration.

#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]

pub use xedoc_tui_input::key_hint;
pub use xedoc_tui_render::city_lights;
pub use xedoc_tui_render::render;
pub use xedoc_tui_render::terminal_hyperlinks;
pub use xedoc_tui_render::wrapping;
pub use xedoc_tui_transcript::exec_command;
pub use xedoc_tui_transcript::history_cell;
pub use xedoc_tui_transcript::line_truncation;
pub use xedoc_tui_transcript::motion;
pub use xedoc_tui_transcript::text_formatting;
pub use xedoc_tui_transcript::version;

pub mod app_event {
    pub use xedoc_tui_events::AppEvent;
}

pub mod app_event_sender {
    pub use xedoc_tui_events::AppEventSender;
}

pub mod legacy_core {
    pub use xedoc_core_config::config;
}

pub mod tui {
    pub use xedoc_tui_frame::FrameRequester;
}

pub mod goal_display;
pub mod rate_limit_labels;
pub mod status;
pub mod status_indicator_widget;
pub mod status_line_command;
pub mod token_usage;
