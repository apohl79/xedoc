//! Bounded recent activity state for sub-agent execution.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod activity;

#[doc(hidden)]
pub use activity::RecentSubAgentActivity;
