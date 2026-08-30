use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use codex_core_plugins::store::PLUGINS_CACHE_DIR;
use codex_file_watcher::DebouncedWatchReceiver;
use codex_file_watcher::FileWatcher;
use codex_file_watcher::FileWatcherSubscriber;
use codex_file_watcher::Receiver;
use codex_file_watcher::WatchPath;
use codex_file_watcher::WatchRegistration;
use tokio_util::sync::CancellationToken;
use tokio_util::sync::DropGuard;
use tracing::warn;

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
        codex_home: &Path,
        on_effective_plugins_changed: EffectivePluginsChangedCallback,
    ) -> Arc<Self> {
        let file_watcher = match FileWatcher::new() {
            Ok(file_watcher) => Arc::new(file_watcher),
            Err(err) => {
                warn!("failed to initialize plugin cache watcher: {err}");
                Arc::new(FileWatcher::noop())
            }
        };
        let plugin_cache = codex_home.join(PLUGINS_CACHE_DIR);
        if let Err(err) = std::fs::create_dir_all(&plugin_cache) {
            warn!(
                "failed to create plugin cache directory {}: {err}",
                plugin_cache.display()
            );
        }
        let (subscriber, rx) = file_watcher.add_subscriber();
        let registration = subscriber.register_paths(vec![WatchPath {
            path: plugin_cache,
            recursive: true,
        }]);
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

#[cfg(test)]
#[path = "plugin_watcher_tests.rs"]
mod tests;
