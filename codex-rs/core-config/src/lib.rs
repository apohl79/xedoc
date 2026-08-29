//! Configuration loading and platform configuration for Codex core.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod config;

#[doc(hidden)]
pub mod config_lock;

mod path_utils {
    pub use codex_utils_path::*;
}

mod skills {
    pub use codex_core_skills::service;
}

mod unified_exec {
    pub(crate) const MIN_EMPTY_YIELD_TIME_MS: u64 = 5_000;
    pub(crate) const DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS: u64 = 300_000;
}

impl codex_rollout::RolloutConfigView for config::Config {
    fn codex_home(&self) -> &std::path::Path {
        self.codex_home.as_path()
    }

    fn sqlite_home(&self) -> &std::path::Path {
        self.sqlite_home.as_path()
    }

    fn cwd(&self) -> &std::path::Path {
        self.cwd.as_path()
    }

    fn model_provider_id(&self) -> &str {
        self.model_provider_id.as_str()
    }
}
