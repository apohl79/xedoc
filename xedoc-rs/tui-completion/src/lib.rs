//! Completion, slash-command, and mention popup primitives for the Xedoc TUI.

pub use xedoc_tui_input::key_hint;
pub use xedoc_tui_input::keymap;
pub use xedoc_tui_render::city_lights;
pub use xedoc_tui_render::render;
pub use xedoc_tui_render::style;
pub use xedoc_tui_render::wrapping;
pub use xedoc_tui_transcript::line_truncation;
pub use xedoc_tui_transcript::text_formatting;

pub mod command_popup;
pub mod file_search_popup;
pub mod mentions_v2;
pub mod popup_consts;
pub mod prompt_args;
pub mod scroll_state;
pub mod selection_popup_common;
pub mod skill_popup;
pub mod skills_helpers;
pub mod slash_command;
pub mod slash_commands;

pub mod bottom_pane {
    pub use crate::popup_consts;
    pub use crate::scroll_state;
}
