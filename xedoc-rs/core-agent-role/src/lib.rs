//! Agent-role configuration resolution and presentation.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod role;

pub use role::DEFAULT_ROLE_NAME;
pub use role::apply_role_to_config;
pub use role::resolve_role_config;
pub use role::spawn_tool_spec;
