use std::time::Duration;

pub use codex_tui_completion::command_popup;
pub use codex_tui_completion::file_search_popup;
pub use codex_tui_completion::mentions_v2;
pub use codex_tui_completion::prompt_args;
pub use codex_tui_completion::skill_popup;
pub use codex_tui_completion::slash_commands;
pub use codex_tui_events::AppEventSender;
pub use codex_tui_input::LocalImageAttachment;
pub use codex_tui_input::MentionBinding;
pub use codex_tui_input::QueuedInputAction;
pub use codex_tui_input::textarea;

pub mod chat_composer;
pub mod chat_composer_history;
pub mod effort_ignition;
mod effort_status_line;
pub mod footer;
pub mod paste_burst;
mod pending_input_preview;

pub use chat_composer::ChatComposer;
pub use chat_composer::ChatComposerConfig;
pub use chat_composer::InputResult;
pub use chat_composer_history::HistoryEntry;
pub use footer::CollaborationModeIndicator;
pub use footer::GoalStatusIndicator;
pub use footer::goal_status_indicator_line;
pub use pending_input_preview::PendingInputPreview;

/// How long the "press again to quit" hint stays visible.
pub const QUIT_SHORTCUT_TIMEOUT: Duration = Duration::from_secs(1);
