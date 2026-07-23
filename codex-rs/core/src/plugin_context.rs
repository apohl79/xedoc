//! Built-in `ContextContributor` that injects static plugin context from
//! `plugin.json` manifests. Unlike hooks, these instructions are permanent
//! (regenerated every turn at zero overhead) and position-aware relative to
//! the world-state (AGENTS.md) user message.

use codex_extension_api::ContextContributor;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::PluginContextPosition;
use codex_extension_api::PromptFragment;
use codex_plugin::manifest::PluginContextSlot;
use codex_plugin::manifest::PluginManifestContext;
use codex_plugin::manifest::PluginThreadContextEntry;
use std::sync::Arc;

/// A cache of plugin context declarations, keyed by plugin name, built once
/// during plugin loading and shared across all threads.
#[derive(Clone, Debug, Default)]
pub struct PluginContextCache {
    entries: Arc<Vec<(String, PluginManifestContext)>>,
}

impl PluginContextCache {
    pub fn new(entries: Vec<(String, PluginManifestContext)>) -> Self {
        Self {
            entries: Arc::new(entries),
        }
    }

    fn fragments(&self) -> Vec<PluginThreadContextEntry> {
        self.entries
            .iter()
            .flat_map(|(_, context)| context.thread.iter().cloned())
            .collect()
    }
}

/// Registers a `ContextContributor` that emits plugin context declarations as
/// `PromptFragment` entries on every `contribute_thread_context` call.
pub fn register_plugin_context_contributor<C>(
    registry: &mut ExtensionRegistryBuilder<C>,
    cache: PluginContextCache,
) where
    C: Send + Sync + 'static,
{
    registry.prompt_contributor(Arc::new(PluginManifestContextContributor { cache }));
}

struct PluginManifestContextContributor {
    cache: PluginContextCache,
}

impl ContextContributor for PluginManifestContextContributor {
    fn contribute_thread_context<'a>(
        &'a self,
        _session_store: &'a ExtensionData,
        _thread_store: &'a ExtensionData,
    ) -> ExtensionFuture<'a, Vec<PromptFragment>> {
        Box::pin(async move {
            self.cache
                .fragments()
                .into_iter()
                .map(|entry| {
                    let slot = match entry.slot {
                        PluginContextSlot::DeveloperPolicy => {
                            codex_extension_api::PromptSlot::DeveloperPolicy
                        }
                        PluginContextSlot::DeveloperCapabilities => {
                            codex_extension_api::PromptSlot::DeveloperCapabilities
                        }
                        PluginContextSlot::ContextualUser => {
                            codex_extension_api::PromptSlot::ContextualUser
                        }
                        PluginContextSlot::SeparateDeveloper => {
                            codex_extension_api::PromptSlot::SeparateDeveloper
                        }
                    };
                    let position = match entry.position {
                        codex_plugin::manifest::PluginContextPosition::Preamble => {
                            PluginContextPosition::Preamble
                        }
                        codex_plugin::manifest::PluginContextPosition::Supplement => {
                            PluginContextPosition::Supplement
                        }
                    };
                    PromptFragment::new(slot, entry.text).with_position(position)
                })
                .collect()
        })
    }
}
