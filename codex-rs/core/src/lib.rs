//! Root of the `codex-core` library.

// Prevent accidental direct writes to stdout/stderr in library code. All
// user-visible output must go through the appropriate abstraction (e.g.,
// the TUI or the tracing stack).
#![deny(clippy::print_stdout, clippy::print_stderr)]

mod apply_patch;
mod audio_preparation;
pub(crate) use codex_core_client::client;
pub(crate) use codex_core_client::client_common;
mod realtime_context;
mod realtime_conversation;
mod realtime_prompt;
pub(crate) use codex_core_client::responses_metadata;
mod responses_retry;
pub(crate) mod session;
pub use codex_core_client::CodexResponsesMetadata;
pub use codex_core_turn_metadata::detached_memory_responses_metadata;
pub use session::SteerInputError;
mod codex_thread;
mod compact_hierarchical;
mod compact_model_fallback;
mod compact_progress;
mod compact_remote;
mod compact_remote_v2;
mod compact_token_budget;
pub(crate) use codex_core_config::config_lock;
pub use codex_thread::BackgroundTerminalInfo;
pub use codex_thread::CodexThread;
pub use codex_thread::CodexThreadSettingsOverrides;
pub use codex_thread::ThreadConfigSnapshot;
pub use codex_thread::TryStartTurnIfIdleError;
pub use codex_thread::TryStartTurnIfIdleRejectionReason;
pub use session::turn_context::TurnContext;
mod agent;
mod agent_communication;
mod codex_delegate;
mod command_canonicalization;
pub use codex_core_config::config;
#[cfg(test)]
mod config_requirements_exec_policy_tests;
#[cfg(test)]
mod config_test_support;
pub mod context;
mod context_manager;
mod current_time;
mod elicitation;
pub(crate) mod environment_selection {
    pub(crate) use codex_core_environment::StartingTurnEnvironment;
    pub(crate) use codex_core_environment::ThreadEnvironments;
    pub(crate) use codex_core_environment::TurnEnvironmentSnapshot;
    pub(crate) use codex_core_environment::TurnEnvironmentState;
    pub(crate) use codex_core_environment::default_thread_environment_selections;
}
pub mod exec;
pub mod exec_env;
mod exec_policy;
#[cfg(test)]
mod git_info_tests;
mod guardian;
mod hook_runtime;
mod image_preparation;
mod installation_id;
pub(crate) mod mcp;
mod mcp_skill_dependencies;
mod mcp_tool_approval_templates;
mod mcp_tool_exposure;
mod network_policy_decision;
pub use mcp::McpManager;
mod original_image_detail;
pub use codex_mcp::SandboxState;
mod mcp_tool_call;
pub(crate) mod mention_syntax;
pub(crate) mod utils;
pub use mention_syntax::PLUGIN_TEXT_MENTION_SIGIL;
pub use mention_syntax::TOOL_MENTION_SIGIL;
pub use utils::path_utils;
pub(crate) mod plugin_context;
pub(crate) mod plugins;
#[doc(hidden)]
pub(crate) mod prompt_debug;
#[doc(hidden)]
pub use prompt_debug::build_prompt_input;
pub(crate) mod mentions {
    pub(crate) use crate::plugins::collect_explicit_plugin_mentions;
}
mod sandbox_tags {
    pub(crate) use codex_core_turn_metadata::permission_profile_policy_tag;
    pub(crate) use codex_core_turn_metadata::permission_profile_sandbox_tag;
}
pub mod sandboxing;
mod session_prefix;
mod session_startup_prewarm;
pub mod skills;
pub(crate) use skills::SkillInjections;
pub(crate) use skills::SkillMetadata;
pub(crate) use skills::SkillsService;
pub(crate) use skills::build_available_skills;
pub(crate) use skills::build_skill_injections;
pub(crate) use skills::build_skill_name_counts;
pub(crate) use skills::collect_explicit_skill_mentions;
pub(crate) use skills::default_skill_metadata_budget;
pub(crate) use skills::maybe_emit_implicit_skill_invocation;
pub(crate) use skills::skills_load_input_from_config;
mod stream_events_utils;
pub mod test_support;
mod unified_exec;
pub use codex_core_client::X_RESPONSESAPI_INCLUDE_TIMING_METRICS_HEADER;
pub use codex_protocol::config_types::ModelProviderAuthInfo;
mod event_mapping;
pub use codex_prompts as review_prompts;
mod thread_manager;
pub use codex_core_response_items::web_search_action_detail;
pub use thread_manager::ForkSnapshot;
pub use thread_manager::NewThread;
pub use thread_manager::StartThreadOptions;
pub use thread_manager::ThreadManager;
pub use thread_manager::ThreadShutdownReport;
pub use thread_manager::build_models_manager;
pub use thread_manager::local_agent_graph_store_from_state_db;
pub use thread_manager::thread_store_from_config;
#[deprecated(note = "use ThreadManager")]
pub type ConversationManager = ThreadManager;
#[deprecated(note = "use NewThread")]
pub type NewConversation = NewThread;
#[deprecated(note = "use CodexThread")]
pub type CodexConversation = CodexThread;
pub(crate) mod agents_md;
mod agents_md_manager;
pub use agents_md::DEFAULT_AGENTS_MD_FILENAME;
pub use agents_md::LOCAL_AGENTS_MD_FILENAME;
pub use agents_md::LoadedAgentsMd;
mod rollout;
mod rollout_budget;
pub(crate) mod safety;
mod session_rollout_init_error;
pub mod shell {
    pub use codex_core_environment::shell::*;
}
pub(crate) mod shell_snapshot {
    pub(crate) use codex_core_environment::ShellSnapshot;
    pub(crate) use codex_core_environment::ShellSnapshotFile;
}
pub mod spawn;
pub(crate) mod state_db_bridge;
pub use state_db_bridge::StateDbHandle;
pub use state_db_bridge::init_state_db;
mod thread_rollout_truncation;
pub use thread_rollout_truncation::truncate_rollout_after_turn_id;
pub use thread_rollout_truncation::truncate_rollout_before_turn_id;
mod tools;
pub(crate) mod turn_diff_tracker {
    pub(crate) use codex_core_turn_diff::TurnDiffTracker;
}
mod turn_metadata {
    pub(crate) use codex_core_turn_metadata::McpTurnMetadataContext;
    pub(crate) use codex_core_turn_metadata::TurnMetadataState;
}
mod turn_timing;
pub use rollout::ARCHIVED_SESSIONS_SUBDIR;
pub use rollout::Cursor;
pub use rollout::INTERACTIVE_SESSION_SOURCES;
pub use rollout::RolloutRecorder;
pub use rollout::RolloutRecorderParams;
pub use rollout::SESSIONS_SUBDIR;
pub use rollout::SessionMeta;
pub use rollout::SortDirection;
pub use rollout::ThreadItem;
pub use rollout::ThreadSortKey;
pub use rollout::ThreadsPage;
pub use rollout::append_thread_name;
pub use rollout::find_archived_thread_path_by_id_str;
#[deprecated(note = "use find_thread_path_by_id_str")]
pub use rollout::find_conversation_path_by_id_str;
pub use rollout::find_thread_meta_by_name_str;
pub use rollout::find_thread_name_by_id;
pub use rollout::find_thread_names_by_ids;
pub use rollout::find_thread_path_by_id_str;
pub use rollout::parse_cursor;
pub use rollout::read_head_for_summary;
pub use rollout::read_session_meta_line;
pub use rollout::rollout_date_parts;
mod function_tool;
mod state;
mod tasks;
mod user_shell_command;
pub mod util {
    pub use codex_core_utils::backoff;
    pub(crate) use codex_core_utils::error_or_panic;
    pub use codex_core_utils::normalize_thread_name;
}

pub use codex_core_client::ModelClient;
pub use codex_core_client::ModelClientSession;
pub use codex_core_client::Prompt;
pub use codex_core_client::ResponseEvent;
pub use codex_core_client::ResponseStream;
pub use codex_core_client::X_CODEX_INSTALLATION_ID_HEADER;
pub use codex_core_client::X_CODEX_TURN_METADATA_HEADER;
pub use codex_core_context_manager::content_items_to_text;
pub use codex_prompts::REVIEW_PROMPT;
pub use current_time::SleepFuture;
pub use current_time::TimeFuture;
pub use current_time::TimeProvider;
pub use event_mapping::parse_turn_item;
pub use exec_policy::ExecPolicyError;
pub use exec_policy::check_execpolicy_for_warnings;
pub use exec_policy::format_exec_policy_error_with_source;
pub use exec_policy::load_exec_policy;
pub use installation_id::resolve_installation_id;
pub mod compact;
mod memory_usage;
pub mod otel_init;
