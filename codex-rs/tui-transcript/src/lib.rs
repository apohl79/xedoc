#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]

pub mod legacy_core {
    pub use codex_core_config::config;
}
pub use codex_tui_markdown as markdown_render;
pub use codex_tui_render::city_lights;
pub use codex_tui_render::color;
pub use codex_tui_render::diff_model;
pub use codex_tui_render::diff_render;
pub use codex_tui_render::render;
pub use codex_tui_render::style;
pub use codex_tui_render::terminal_hyperlinks;
pub use codex_tui_render::terminal_palette;
pub use codex_tui_render::wrapping;

pub mod custom_terminal;
pub mod exec_cell;
pub mod exec_command;
pub mod git_action_directives;
pub mod history_cell;
pub mod inline_visualization;
pub mod insert_history;
pub mod line_truncation;
pub mod live_wrap;
pub mod markdown;
pub mod markdown_stream;
pub mod motion;
pub mod replay;
pub mod session_state;
pub mod shimmer;
pub mod streaming;
pub mod table_detect;
pub mod text_formatting;
pub mod thread_transcript;
pub mod tooltips;
pub mod ui_consts;
pub mod update_action;
pub mod update_versions;
pub mod version;
pub mod width;

pub use custom_terminal::Terminal;
pub use insert_history::insert_history_lines;
pub use live_wrap::RowBuilder;
pub use update_action::UpdateAction;
pub use version::set_codex_cli_version;

#[cfg(test)]
pub mod test_backend {
    pub use codex_tui_test_support::VT100Backend;
}

#[cfg(test)]
pub mod test_support {
    pub use codex_tui_test_support::*;
}
