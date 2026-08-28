//! Plugin mention, discovery, and prompt-context assembly.

mod discovery;
mod injection;
mod mentions;
mod render;
#[doc(hidden)]
pub mod test_support;

pub use codex_core_skills::build_skill_name_counts;
pub use codex_plugin::PluginCapabilitySummary;
pub use discovery::list_tool_suggest_discoverable_plugins;
pub use injection::build_plugin_injections;
pub use mentions::build_connector_slug_counts;
pub use mentions::collect_explicit_app_ids;
pub use mentions::collect_explicit_plugin_mentions;
pub use mentions::collect_tool_mentions_from_messages;
pub use render::render_explicit_plugin_instructions;
