use std::sync::Arc;

use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::SkillsChangedNotification;
use codex_core::ThreadManager;

use crate::config_manager::ConfigManager;
use crate::outgoing_message::OutgoingMessageSender;
use crate::request_processors::ConfigRequestProcessor;

pub(crate) type EffectivePluginsChangedCallback = Arc<dyn Fn() + Send + Sync + 'static>;

/// Refresh plugin consumers after the effective plugin set changed on disk or in config.
pub(crate) fn effective_plugins_changed_callback(
    thread_manager: Arc<ThreadManager>,
    outgoing: Arc<OutgoingMessageSender>,
    config_manager: ConfigManager,
    config_processor: ConfigRequestProcessor,
) -> EffectivePluginsChangedCallback {
    Arc::new(move || {
        thread_manager.plugins_manager().clear_cache();
        thread_manager.skills_service().clear_cache();

        let refresh_thread_manager = Arc::clone(&thread_manager);
        let refresh_outgoing = Arc::clone(&outgoing);
        let refresh_config_manager = config_manager.clone();
        let refresh_config_processor = config_processor.clone();
        tokio::spawn(async move {
            if !refresh_thread_manager.list_thread_ids().await.is_empty() {
                refresh_config_processor.reload_user_config().await;
                crate::mcp_refresh::queue_best_effort_refresh(
                    &refresh_thread_manager,
                    &refresh_config_manager,
                )
                .await;
            }
            refresh_outgoing
                .send_server_notification(ServerNotification::SkillsChanged(
                    SkillsChangedNotification {},
                ))
                .await;
        });
    })
}
