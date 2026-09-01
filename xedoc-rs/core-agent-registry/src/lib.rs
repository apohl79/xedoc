//! Shared registry and lifecycle state for multi-agent execution.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod registry;
mod status;

pub use registry::AgentMetadata;
pub use registry::AgentRegistry;
pub use registry::SpawnReservation;
pub use registry::exceeds_thread_spawn_depth_limit;
pub use registry::next_thread_spawn_depth;
pub use status::agent_status_from_event;
pub use status::is_final;
