//! Plugin mention and prompt-context assembly.

mod injection;
mod mentions;
mod render;
#[doc(hidden)]
pub mod test_support;

pub use codex_core_skills::build_skill_name_counts;
pub use codex_plugin::PluginCapabilitySummary;
pub use injection::build_plugin_injections;
pub use mentions::collect_explicit_plugin_mentions;
pub use mentions::collect_tool_mentions_from_messages;
pub use render::render_explicit_plugin_instructions;
