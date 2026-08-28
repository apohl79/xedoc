//! User-configurable terminal UI settings and their selection flows.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod collaboration_modes;
pub mod keymap_setup;
pub mod model_catalog;
pub mod service_tier_resolution;
pub mod theme_picker;

mod app_event {
    pub use codex_tui_events::*;
}

mod app_event_sender {
    pub use codex_tui_events::AppEventSender;
}

mod bottom_pane {
    pub use codex_tui_bottom_pane::*;
}

#[cfg(test)]
mod tui {
    pub use codex_tui_frame::FrameRequester;
}

use codex_tui_input::key_hint;
use codex_tui_input::keymap;
use codex_tui_render::city_lights;
use codex_tui_render::diff_render;
use codex_tui_render::render;
use codex_tui_render::style;
use codex_tui_status::status;
