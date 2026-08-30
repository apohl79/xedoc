#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod clipboard_paste;
pub mod key_hint;
pub mod keymap;
pub mod mention_codec;
pub mod textarea;
mod types;

pub use types::LocalImageAttachment;
pub use types::MentionBinding;
pub use types::QueuedInputAction;
pub use types::UserMessage;
pub use xedoc_tui_render::wrapping;
