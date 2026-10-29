//! Plugin mention capability enrichment for the TUI.
//!
//! Mention inventory comes from app-server `plugin/list`, matching the GUI
//! client. The current API exposes plugin-level mention metadata there, but not
//! effective per-session capability summaries.

use super::background_requests::request_plugin_list;
use super::*;
use xedoc_app_server_protocol::PluginListResponse;
use xedoc_app_server_protocol::PluginSummary;
use xedoc_plugin::PluginCapabilitySummary;

pub(super) async fn fetch_plugin_mentions(
    request_handle: AppServerRequestHandle,
    cwd: PathBuf,
) -> Result<Vec<PluginCapabilitySummary>> {
    let response = request_plugin_list(request_handle, cwd).await?;
    Ok(plugin_mentions_from_list_response(response))
}

fn plugin_mentions_from_list_response(
    response: PluginListResponse,
) -> Vec<PluginCapabilitySummary> {
    response
        .marketplaces
        .into_iter()
        .flat_map(|marketplace| {
            let marketplace_name = marketplace.name;
            marketplace
                .plugins
                .into_iter()
                .filter_map(move |plugin| plugin_mention_from_summary(&marketplace_name, plugin))
        })
        .collect()
}

fn plugin_is_eligible_for_mentions(plugin: &PluginSummary) -> bool {
    plugin.installed && plugin.enabled
}

fn plugin_mention_from_summary(
    marketplace_name: &str,
    plugin: PluginSummary,
) -> Option<PluginCapabilitySummary> {
    if !plugin_is_eligible_for_mentions(&plugin) {
        return None;
    }

    Some(PluginCapabilitySummary {
        config_name: plugin.id.clone(),
        display_name: plugin_mention_display_name(&plugin),
        description: plugin_mention_description(marketplace_name, &plugin),
        has_skills: false,
        mcp_server_names: Vec::new(),
    })
}

fn plugin_mention_display_name(plugin: &PluginSummary) -> String {
    plugin
        .interface
        .as_ref()
        .and_then(|interface| interface.display_name.as_deref())
        .map(str::trim)
        .filter(|display_name| !display_name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| plugin.name.clone())
}

fn plugin_mention_description(marketplace_name: &str, plugin: &PluginSummary) -> Option<String> {
    plugin
        .interface
        .as_ref()
        .and_then(|interface| interface.short_description.as_deref())
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let marketplace_name = marketplace_name.trim();
            (!marketplace_name.is_empty()).then(|| marketplace_name.to_string())
        })
}
