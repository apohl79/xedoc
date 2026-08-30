use super::*;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use tokio::sync::Notify;

#[tokio::test]
async fn plugin_version_installed_outside_codex_triggers_reload() {
    let codex_home = TempDir::new().expect("Codex home");
    let plugin_cache = codex_home.path().join(PLUGINS_CACHE_DIR);
    let reload_count = Arc::new(AtomicUsize::new(0));
    let reloaded = Arc::new(Notify::new());
    let callback_reload_count = Arc::clone(&reload_count);
    let callback_reloaded = Arc::clone(&reloaded);
    let watcher = PluginWatcher::new(
        codex_home.path(),
        Arc::new(move || {
            callback_reload_count.fetch_add(1, Ordering::Relaxed);
            callback_reloaded.notify_one();
        }),
    );

    let installed_manifest = plugin_cache.join("external/sample/2.0.0/.codex-plugin/plugin.json");
    std::fs::create_dir_all(installed_manifest.parent().expect("manifest parent"))
        .expect("externally installed plugin directory");
    std::fs::write(installed_manifest, r#"{"name":"sample","version":"2.0.0"}"#)
        .expect("externally installed plugin manifest");
    tokio::time::timeout(Duration::from_secs(5), reloaded.notified())
        .await
        .expect("plugin reload notification");
    watcher.shutdown();

    assert_eq!(reload_count.load(Ordering::Relaxed), 1);
}
