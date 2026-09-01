use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tokio_util::sync::DropGuard;
use tracing::warn;
use xedoc_core_plugins::legacy_plugin_home;
use xedoc_core_plugins::store::PLUGINS_CACHE_DIR;
use xedoc_file_watcher::DebouncedWatchReceiver;
use xedoc_file_watcher::FileWatcher;
use xedoc_file_watcher::FileWatcherSubscriber;
use xedoc_file_watcher::Receiver;
use xedoc_file_watcher::WatchPath;
use xedoc_file_watcher::WatchRegistration;

#[cfg(not(test))]
const WATCHER_DEBOUNCE_INTERVAL: Duration = Duration::from_millis(250);
#[cfg(test)]
const WATCHER_DEBOUNCE_INTERVAL: Duration = Duration::from_millis(50);

use crate::effective_plugin_change::EffectivePluginsChangedCallback;

pub(crate) struct PluginWatcher {
    _subscriber: FileWatcherSubscriber,
    _registration: WatchRegistration,
    shutdown_token: CancellationToken,
    _shutdown_drop_guard: DropGuard,
}

impl PluginWatcher {
    pub(crate) fn new(
        xedoc_home: &Path,
        on_effective_plugins_changed: EffectivePluginsChangedCallback,
    ) -> Arc<Self> {
        Self::new_with_legacy_plugin_home(
            xedoc_home,
            legacy_plugin_home(xedoc_home).as_deref(),
            on_effective_plugins_changed,
        )
    }

    fn new_with_legacy_plugin_home(
        xedoc_home: &Path,
        legacy_plugin_home: Option<&Path>,
        on_effective_plugins_changed: EffectivePluginsChangedCallback,
    ) -> Arc<Self> {
        let file_watcher = match FileWatcher::new() {
            Ok(file_watcher) => Arc::new(file_watcher),
            Err(err) => {
                warn!("failed to initialize plugin cache watcher: {err}");
                Arc::new(FileWatcher::noop())
            }
        };
        let primary_plugin_cache = xedoc_home.join(PLUGINS_CACHE_DIR);
        if let Err(err) = std::fs::create_dir_all(&primary_plugin_cache) {
            warn!(
                "failed to create plugin cache directory {}: {err}",
                primary_plugin_cache.display()
            );
        }
        let (subscriber, rx) = file_watcher.add_subscriber();
        let registration = subscriber.register_paths(
            plugin_cache_paths(xedoc_home, legacy_plugin_home)
                .into_iter()
                .map(|path| WatchPath {
                    path,
                    recursive: true,
                })
                .collect(),
        );
        let shutdown_token = CancellationToken::new();
        let shutdown_drop_guard = shutdown_token.clone().drop_guard();
        Self::spawn_event_loop(
            rx,
            on_effective_plugins_changed,
            shutdown_token.child_token(),
        );
        Arc::new(Self {
            _subscriber: subscriber,
            _registration: registration,
            shutdown_token,
            _shutdown_drop_guard: shutdown_drop_guard,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_with_legacy_plugin_home_for_tests(
        xedoc_home: &Path,
        legacy_plugin_home: &Path,
        on_effective_plugins_changed: EffectivePluginsChangedCallback,
    ) -> Arc<Self> {
        Self::new_with_legacy_plugin_home(
            xedoc_home,
            Some(legacy_plugin_home),
            on_effective_plugins_changed,
        )
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown_token.cancel();
    }

    fn spawn_event_loop(
        rx: Receiver,
        on_effective_plugins_changed: EffectivePluginsChangedCallback,
        shutdown_token: CancellationToken,
    ) {
        let mut rx = DebouncedWatchReceiver::new(rx, WATCHER_DEBOUNCE_INTERVAL);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!("plugin cache watcher listener skipped: no Tokio runtime available");
            return;
        };
        handle.spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = shutdown_token.cancelled() => break,
                    event = rx.recv() => event,
                };
                if event.is_none() {
                    break;
                }
                on_effective_plugins_changed();
            }
        });
    }
}

fn plugin_cache_paths(xedoc_home: &Path, legacy_plugin_home: Option<&Path>) -> Vec<PathBuf> {
    std::iter::once(xedoc_home)
        .chain(legacy_plugin_home)
        .map(|home| home.join(PLUGINS_CACHE_DIR))
        .collect()
}

#[cfg(test)]
#[path = "plugin_watcher_tests.rs"]
mod tests;
