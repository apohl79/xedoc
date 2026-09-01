//! Built-in `ContextContributor` that injects plugin context from `plugin.json`
//! manifests. Unlike hooks, these instructions are permanent and position-aware
//! relative to the world-state (AGENTS.md) user message.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use xedoc_core_plugins::PluginLoadOutcome;
use xedoc_core_plugins::manifest::load_plugin_manifest;
use xedoc_extension_api::ContextContributor;
use xedoc_extension_api::ExtensionData;
use xedoc_extension_api::ExtensionFuture;
use xedoc_extension_api::ExtensionRegistryBuilder;
use xedoc_extension_api::PluginContextPosition;
use xedoc_extension_api::PromptFragment;
use xedoc_plugin::manifest::PluginContextSlot;
use xedoc_plugin::manifest::PluginManifestContext;
use xedoc_plugin::manifest::PluginThreadContextEntry;
use xedoc_utils_absolute_path::AbsolutePathBuf;

use crate::shell::Shell;

const MAX_CONTEXT_ENTRIES: usize = 128;
const MAX_CONTEXT_ENTRY_CHARS: usize = 8_000;
const MAX_CONTEXT_TEXT_CHARS: usize = 32_000;
const CONDITION_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// A cache of plugin context declarations, keyed by plugin name, built once
/// during plugin loading and shared across all threads.
#[derive(Clone, Debug, Default)]
pub struct PluginContextCache {
    entries: Arc<Vec<(String, PluginManifestContext)>>,
}

impl PluginContextCache {
    pub fn new(entries: Vec<(String, PluginManifestContext)>) -> Self {
        let mut bounded_entries = Vec::new();
        let mut total_entries = 0;
        let mut total_text_chars = 0;
        for (plugin_name, context) in entries {
            let mut thread = Vec::new();
            for entry in context.thread {
                if total_entries >= MAX_CONTEXT_ENTRIES {
                    break;
                }
                let text_chars = entry.text.chars().count();
                if text_chars > MAX_CONTEXT_ENTRY_CHARS {
                    continue;
                }
                if total_text_chars + text_chars > MAX_CONTEXT_TEXT_CHARS {
                    break;
                }
                total_text_chars += text_chars;
                total_entries += 1;
                thread.push(entry);
            }
            if !thread.is_empty() {
                bounded_entries.push((plugin_name, PluginManifestContext { thread }));
            }
            if total_text_chars >= MAX_CONTEXT_TEXT_CHARS {
                break;
            }
        }
        Self {
            entries: Arc::new(bounded_entries),
        }
    }

    pub(crate) fn from_plugin_outcome(outcome: &PluginLoadOutcome) -> Self {
        let entries = outcome
            .plugins()
            .iter()
            .filter(|plugin| plugin.is_active())
            .filter_map(|plugin| {
                load_plugin_manifest(plugin.root.as_path()).and_then(|manifest| {
                    manifest
                        .paths
                        .context
                        .map(|context| (plugin.config_name.clone(), context))
                })
            })
            .collect();
        Self::new(entries)
    }
}

/// Registers a `ContextContributor` that emits plugin context declarations as
/// `PromptFragment` entries on every `contribute_thread_context` call.
pub fn register_plugin_context_contributor<C>(
    registry: &mut ExtensionRegistryBuilder<C>,
    cache: PluginContextCache,
    shell: Arc<Shell>,
    cwd: AbsolutePathBuf,
) where
    C: Send + Sync + 'static,
{
    registry.prompt_contributor(Arc::new(PluginManifestContextContributor {
        cache,
        shell,
        cwd,
    }));
}

struct PluginManifestContextContributor {
    cache: PluginContextCache,
    shell: Arc<Shell>,
    cwd: AbsolutePathBuf,
}

impl ContextContributor for PluginManifestContextContributor {
    fn contribute_thread_context<'a>(
        &'a self,
        _session_store: &'a ExtensionData,
        _thread_store: &'a ExtensionData,
    ) -> ExtensionFuture<'a, Vec<PromptFragment>> {
        Box::pin(async move {
            let mut fragments = Vec::new();
            for (plugin_name, context) in self.cache.entries.iter() {
                for entry in &context.thread {
                    let Some(condition_shell) = entry.condition_shell.as_deref() else {
                        fragments.push(Self::prompt_fragment(entry));
                        continue;
                    };
                    let mut shell_args = self
                        .shell
                        .derive_exec_args(condition_shell, /*use_login_shell*/ false);
                    let shell_program = shell_args.remove(0);
                    let mut command = Command::new(shell_program);
                    command
                        .args(shell_args)
                        .current_dir(self.cwd.as_path())
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .kill_on_drop(true);
                    let condition_applies = match timeout(CONDITION_SHELL_TIMEOUT, command.status())
                        .await
                    {
                        Ok(Ok(status)) => status.success(),
                        Ok(Err(error)) => {
                            tracing::warn!(plugin = %plugin_name, %error, "plugin context condition could not start; skipping entry");
                            false
                        }
                        Err(_) => {
                            tracing::warn!(plugin = %plugin_name, "plugin context condition timed out; skipping entry");
                            false
                        }
                    };
                    if condition_applies {
                        fragments.push(Self::prompt_fragment(entry));
                    }
                }
            }
            fragments
        })
    }
}

impl PluginManifestContextContributor {
    fn prompt_fragment(entry: &PluginThreadContextEntry) -> PromptFragment {
        let slot = match entry.slot {
            PluginContextSlot::DeveloperPolicy => xedoc_extension_api::PromptSlot::DeveloperPolicy,
            PluginContextSlot::DeveloperCapabilities => {
                xedoc_extension_api::PromptSlot::DeveloperCapabilities
            }
            PluginContextSlot::ContextualUser => xedoc_extension_api::PromptSlot::ContextualUser,
            PluginContextSlot::SeparateDeveloper => {
                xedoc_extension_api::PromptSlot::SeparateDeveloper
            }
        };
        let position = match entry.position {
            xedoc_plugin::manifest::PluginContextPosition::Preamble => {
                PluginContextPosition::Preamble
            }
            xedoc_plugin::manifest::PluginContextPosition::Supplement => {
                PluginContextPosition::Supplement
            }
        };
        PromptFragment::new(slot, entry.text.clone()).with_position(position)
    }
}

#[cfg(test)]
#[path = "plugin_context_tests.rs"]
mod tests;
