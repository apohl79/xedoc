use std::collections::HashSet;

use codex_core_skills::injection::ToolMentionKind;
use codex_core_skills::injection::extract_tool_mentions_with_sigil;
use codex_core_skills::injection::plugin_config_name_from_path;
use codex_core_skills::injection::tool_kind_for_path;
use codex_plugin::PluginCapabilitySummary;
use codex_protocol::user_input::UserInput;
use codex_utils_plugins::mention_syntax::PLUGIN_TEXT_MENTION_SIGIL;
use codex_utils_plugins::mention_syntax::TOOL_MENTION_SIGIL;

/// Mention names and paths extracted from user-visible text.
pub struct CollectedToolMentions {
    /// Plain mention names without a URI path.
    pub plain_names: HashSet<String>,
    /// Mention URI paths.
    pub paths: HashSet<String>,
}

/// Collects tool-style mentions from message text.
pub fn collect_tool_mentions_from_messages(messages: &[String]) -> CollectedToolMentions {
    collect_tool_mentions_from_messages_with_sigil(messages, TOOL_MENTION_SIGIL)
}

fn collect_tool_mentions_from_messages_with_sigil(
    messages: &[String],
    sigil: char,
) -> CollectedToolMentions {
    let mut plain_names = HashSet::new();
    let mut paths = HashSet::new();
    for message in messages {
        let mentions = extract_tool_mentions_with_sigil(message, sigil);
        plain_names.extend(mentions.plain_names().map(str::to_string));
        paths.extend(mentions.paths().map(str::to_string));
    }
    CollectedToolMentions { plain_names, paths }
}

/// Collect explicit structured or linked `plugin://...` mentions.
pub fn collect_explicit_plugin_mentions(
    input: &[UserInput],
    plugins: &[PluginCapabilitySummary],
) -> Vec<PluginCapabilitySummary> {
    if plugins.is_empty() {
        return Vec::new();
    }

    let messages = input
        .iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<String>>();

    let mentioned_config_names: HashSet<String> = input
        .iter()
        .filter_map(|item| match item {
            UserInput::Mention { path, .. } => Some(path.clone()),
            _ => None,
        })
        .chain(
            // Plugin plaintext links use `@`, not the default `$` tool sigil.
            collect_tool_mentions_from_messages_with_sigil(&messages, PLUGIN_TEXT_MENTION_SIGIL)
                .paths,
        )
        .filter(|path| tool_kind_for_path(path.as_str()) == ToolMentionKind::Plugin)
        .filter_map(|path| plugin_config_name_from_path(path.as_str()).map(str::to_string))
        .collect();

    if mentioned_config_names.is_empty() {
        return Vec::new();
    }

    plugins
        .iter()
        .filter(|plugin| mentioned_config_names.contains(plugin.config_name.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
