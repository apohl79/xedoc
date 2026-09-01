//! Project-document discovery and session-level caching.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod agents_md;
mod manager;

pub use agents_md::DEFAULT_AGENTS_MD_FILENAME;
pub use agents_md::LOCAL_AGENTS_MD_FILENAME;
pub use agents_md::LoadedAgentsMd;
pub use manager::AgentsMdManager;
