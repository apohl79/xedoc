//! Process execution, sandbox adaptation, and child environment construction for Codex core.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod approval;
pub mod exec;
pub mod exec_env;
pub mod exec_policy;
pub mod sandboxing;
#[doc(hidden)]
pub mod tool_runtime;
pub mod tool_sandboxing;
pub mod unified_exec;

#[doc(hidden)]
pub mod spawn;
