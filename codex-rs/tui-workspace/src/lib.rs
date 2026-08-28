//! Workspace-facing integrations used by the Codex terminal UI.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod branch_summary;
pub mod get_git_diff;
pub mod git_action_directives;
pub mod ide_context;
pub mod workspace_command;
