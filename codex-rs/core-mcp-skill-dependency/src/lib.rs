//! Resolves skill-declared MCP dependencies against configured servers.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod resolver;

#[doc(hidden)]
pub use resolver::canonical_mcp_server_key;
#[doc(hidden)]
pub use resolver::collect_missing_mcp_dependencies;
#[doc(hidden)]
pub use resolver::format_missing_mcp_dependencies;
