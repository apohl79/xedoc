//! Request metadata and permission telemetry tags for a Codex turn.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod sandbox_tags;
mod turn_metadata;

pub use sandbox_tags::permission_profile_policy_tag;
pub use sandbox_tags::permission_profile_sandbox_tag;
pub use turn_metadata::McpTurnMetadataContext;
pub use turn_metadata::TurnMetadataState;
pub use turn_metadata::detached_memory_responses_metadata;
