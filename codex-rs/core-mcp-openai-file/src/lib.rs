//! OpenAI file upload and argument rewriting for Codex Apps MCP tools.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod openai_file;

pub use openai_file::OpenAiFileUploadContext;
pub use openai_file::rewrite_mcp_tool_arguments_for_openai_files;

#[cfg(test)]
#[path = "openai_file_tests.rs"]
mod tests;
