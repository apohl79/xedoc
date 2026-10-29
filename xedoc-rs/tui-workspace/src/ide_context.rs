//! IDE context data model and public helpers for TUI `/ide` support.

mod ipc;
mod prompt;
#[cfg(windows)]
mod windows_pipe;

pub use ipc::fetch_ide_context;
pub use prompt::apply_ide_context_to_user_input;
pub use prompt::extract_prompt_request_with_offset;
pub use prompt::has_prompt_context;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IdeContext {
    active_file: Option<ActiveFile>,
    #[serde(default)]
    open_tabs: Vec<FileDescriptor>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ActiveFile {
    #[serde(flatten)]
    descriptor: FileDescriptor,
    selection: Range,
    #[serde(default)]
    active_selection_content: String,
    #[serde(default)]
    selections: Vec<Range>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct FileDescriptor {
    label: String,
    path: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Range {
    start: Position,
    end: Position,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Position {
    line: u32,
    character: u32,
}
