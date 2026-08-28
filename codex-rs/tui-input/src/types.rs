use codex_protocol::user_input::TextElement;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalImageAttachment {
    pub placeholder: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionBinding {
    /// Visible mention sigil (`$` or `@`).
    pub sigil: char,
    /// Mention token text without the leading sigil (`$` or `@`).
    pub mention: String,
    /// Canonical mention target (for example `app://...` or absolute SKILL.md path).
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedInputAction {
    Plain,
    ParseSlash,
    RunShell,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserMessage {
    pub text: String,
    pub local_images: Vec<LocalImageAttachment>,
    /// Remote image attachments represented as URLs (for example data URLs)
    /// provided by app-server clients.
    ///
    /// Unlike `local_images`, these are not created by TUI image attach/paste
    /// flows. The TUI can restore and remove them while editing/backtracking.
    pub remote_image_urls: Vec<String>,
    pub text_elements: Vec<TextElement>,
    pub mention_bindings: Vec<MentionBinding>,
}

impl From<String> for UserMessage {
    fn from(text: String) -> Self {
        Self {
            text,
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        }
    }
}

impl From<&str> for UserMessage {
    fn from(text: &str) -> Self {
        Self::from(text.to_string())
    }
}
