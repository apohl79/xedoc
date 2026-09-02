use xedoc_app_server_protocol::ManagedModelSettings;
use xedoc_app_server_protocol::ManagedProviderSettings;
use xedoc_app_server_protocol::ModelManagerUpdateParams;

use super::AppEvent;
use super::ChatWidget;
use super::CustomPromptView;
use crate::app_event::ModelManagerSetting;
use crate::app_event::ProviderApiKey;
use crate::app_event_sender::AppEventSender;

impl ChatWidget {
    pub(super) fn open_model_manager_api_key_prompt(&mut self, provider: ManagedProviderSettings) {
        let provider_id = provider.id;
        let app_event_tx = self.app_event_tx.clone();
        let view = CustomPromptView::new_secret(
            format!("{} API key", provider.display_name),
            "Paste an API key and press Enter".to_string(),
            Some("The key is masked and stored encrypted.".to_string()),
            Box::new(move |api_key| {
                app_event_tx.send(AppEvent::SetProviderApiKey {
                    provider_id: provider_id.clone(),
                    api_key: ProviderApiKey::new(api_key),
                });
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    pub(super) fn open_model_manager_setting_prompt(
        &mut self,
        provider: ManagedProviderSettings,
        model: ManagedModelSettings,
        setting: ModelManagerSetting,
    ) {
        let (title, placeholder, initial_text) = match setting {
            ModelManagerSetting::ContextWindow => (
                "Context window",
                "Enter the normal context-window token count",
                model.context_window.to_string(),
            ),
            ModelManagerSetting::MaxContextWindow => (
                "Maximum context window",
                "Enter the provider's maximum token count",
                model.max_context_window.to_string(),
            ),
            ModelManagerSetting::AutoCompactTokenLimit => (
                "Automatic compaction limit",
                "Enter the token count that triggers compaction",
                model.auto_compact_token_limit.to_string(),
            ),
            ModelManagerSetting::BaseInstructions => (
                "Base instructions",
                "Enter the model system prompt/base instructions",
                model.base_instructions.clone(),
            ),
        };
        let context_label = Some(format!(
            "{} · {}",
            provider.display_name, model.display_name
        ));
        let app_event_tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            title.to_string(),
            placeholder.to_string(),
            initial_text,
            context_label,
            Box::new(move |value| {
                let mut updated = model.clone();
                match setting {
                    ModelManagerSetting::ContextWindow => update_token_setting(
                        &app_event_tx,
                        &provider.id,
                        updated,
                        setting,
                        &value,
                        |model, tokens| model.context_window = tokens,
                    ),
                    ModelManagerSetting::MaxContextWindow => update_token_setting(
                        &app_event_tx,
                        &provider.id,
                        updated,
                        setting,
                        &value,
                        |model, tokens| model.max_context_window = tokens,
                    ),
                    ModelManagerSetting::AutoCompactTokenLimit => update_token_setting(
                        &app_event_tx,
                        &provider.id,
                        updated,
                        setting,
                        &value,
                        |model, tokens| model.auto_compact_token_limit = tokens,
                    ),
                    ModelManagerSetting::BaseInstructions => {
                        updated.base_instructions = value;
                        send_model_update(&app_event_tx, &provider.id, updated);
                    }
                }
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }
}

fn update_token_setting(
    app_event_tx: &AppEventSender,
    provider_id: &str,
    mut model: ManagedModelSettings,
    setting: ModelManagerSetting,
    value: &str,
    update: impl FnOnce(&mut ManagedModelSettings, i64),
) {
    match value.parse::<i64>() {
        Ok(tokens) if tokens > 0 => {
            update(&mut model, tokens);
            send_model_update(app_event_tx, provider_id, model);
        }
        _ => {
            app_event_tx.send(AppEvent::ModelManagerError(format!(
                "{} must be a positive whole number",
                setting_label(setting)
            )));
        }
    }
}

fn send_model_update(
    app_event_tx: &AppEventSender,
    provider_id: &str,
    model: ManagedModelSettings,
) {
    app_event_tx.send(AppEvent::UpdateModelManager(
        ModelManagerUpdateParams::ModelSettings {
            provider_id: provider_id.to_string(),
            model_id: model.id,
            context_window: model.context_window,
            max_context_window: model.max_context_window,
            auto_compact_token_limit: model.auto_compact_token_limit,
            base_instructions: model.base_instructions,
        },
    ));
}

fn setting_label(setting: ModelManagerSetting) -> &'static str {
    match setting {
        ModelManagerSetting::ContextWindow => "Context window",
        ModelManagerSetting::MaxContextWindow => "Maximum context window",
        ModelManagerSetting::AutoCompactTokenLimit => "Automatic compaction limit",
        ModelManagerSetting::BaseInstructions => "Base instructions",
    }
}
