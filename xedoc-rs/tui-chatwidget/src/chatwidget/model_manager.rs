use ratatui::style::Stylize;
use ratatui::text::Line;
use xedoc_app_server_protocol::ManagedModelSettings;
use xedoc_app_server_protocol::ManagedProviderSettings;
use xedoc_app_server_protocol::ModelManagerReadResponse;
use xedoc_app_server_protocol::ModelManagerUpdateParams;

use super::AppEvent;
use super::ChatWidget;
use super::ColumnRenderable;
use super::SelectionAction;
use super::SelectionItem;
use super::SelectionViewParams;
use super::standard_popup_hint_line;
use crate::app_event::ModelManagerSetting;
use crate::app_event::ModelManagerUiAction;

impl ChatWidget {
    pub fn open_model_manager(&mut self, response: ModelManagerReadResponse) {
        let items = response
            .providers
            .into_iter()
            .map(|provider| {
                let action_provider = provider.clone();
                SelectionItem {
                    name: provider.display_name.clone(),
                    description: Some(format!(
                        "smart: {} · fast: {} · auth: {}",
                        provider.default_model,
                        provider.fast_model,
                        provider_auth_status(&provider)
                    )),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::ModelManagerUi(
                            ModelManagerUiAction::OpenProvider(action_provider.clone()),
                        ));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.show_model_manager_selection(
            "Model Manager",
            "Manage provider credentials, defaults, instructions, and token limits.",
            items,
            /*is_searchable*/ false,
        );
    }

    pub fn handle_model_manager_ui(&mut self, action: ModelManagerUiAction) {
        match action {
            ModelManagerUiAction::OpenProvider(provider) => {
                self.open_model_manager_provider(provider)
            }
            ModelManagerUiAction::OpenAuthentication(provider) => {
                self.open_model_manager_authentication(provider)
            }
            ModelManagerUiAction::PromptApiKey(provider) => {
                self.open_model_manager_api_key_prompt(provider)
            }
            ModelManagerUiAction::OpenSmartModel(provider) => {
                self.open_model_manager_model_default(provider, ModelDefaultKind::Smart)
            }
            ModelManagerUiAction::OpenFastModel(provider) => {
                self.open_model_manager_model_default(provider, ModelDefaultKind::Fast)
            }
            ModelManagerUiAction::OpenReasoning(provider) => {
                self.open_model_manager_reasoning(provider)
            }
            ModelManagerUiAction::OpenModels(provider) => self.open_model_manager_models(provider),
            ModelManagerUiAction::OpenModel { provider, model } => {
                self.open_model_manager_model(provider, model)
            }
            ModelManagerUiAction::PromptModelSetting {
                provider,
                model,
                setting,
            } => self.open_model_manager_setting_prompt(provider, model, setting),
        }
    }

    fn open_model_manager_provider(&mut self, provider: ManagedProviderSettings) {
        let items = vec![
            navigation_item(
                "Authentication",
                format!("Current: {}", provider_auth_status(&provider)),
                ModelManagerUiAction::OpenAuthentication(provider.clone()),
            ),
            navigation_item(
                "Default smart model",
                provider.default_model.clone(),
                ModelManagerUiAction::OpenSmartModel(provider.clone()),
            ),
            navigation_item(
                "Default fast model",
                provider.fast_model.clone(),
                ModelManagerUiAction::OpenFastModel(provider.clone()),
            ),
            navigation_item(
                "Default reasoning effort",
                provider.default_reasoning_effort.to_string(),
                ModelManagerUiAction::OpenReasoning(provider.clone()),
            ),
            navigation_item(
                "Model settings",
                format!("{} configured models", provider.models.len()),
                ModelManagerUiAction::OpenModels(provider.clone()),
            ),
        ];
        self.show_model_manager_selection(
            &provider.display_name,
            "Choose what to configure.",
            items,
            /*is_searchable*/ false,
        );
    }

    fn open_model_manager_authentication(&mut self, provider: ManagedProviderSettings) {
        let mut items = Vec::new();
        let prompt_provider = provider.clone();
        items.push(SelectionItem {
            name: if provider.api_key_configured {
                "Replace API key".to_string()
            } else {
                "Set API key".to_string()
            },
            description: Some("Stored in Xedoc's encrypted credential store.".to_string()),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::ModelManagerUi(
                    ModelManagerUiAction::PromptApiKey(prompt_provider.clone()),
                ));
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        if provider.api_key_configured {
            let provider_id = provider.id.clone();
            items.push(SelectionItem {
                name: "Remove API key".to_string(),
                description: Some("Delete the stored key for this provider.".to_string()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::DeleteProviderApiKey {
                        provider_id: provider_id.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        if provider.oauth_supported {
            let provider_id = provider.id.clone();
            items.push(SelectionItem {
                name: if provider.oauth_configured {
                    "Log in again with OAuth".to_string()
                } else {
                    "Log in with OAuth".to_string()
                },
                description: Some("Continue authentication in your browser.".to_string()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::StartProviderOauth {
                        provider_id: provider_id.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        self.show_model_manager_selection(
            &format!("{} Authentication", provider.display_name),
            &format!("Current: {}", provider_auth_status(&provider)),
            items,
            /*is_searchable*/ false,
        );
    }

    fn open_model_manager_model_default(
        &mut self,
        provider: ManagedProviderSettings,
        kind: ModelDefaultKind,
    ) {
        let current = match kind {
            ModelDefaultKind::Smart => &provider.default_model,
            ModelDefaultKind::Fast => &provider.fast_model,
        };
        let items = provider
            .models
            .iter()
            .map(|model| {
                let mut updated = provider.clone();
                match kind {
                    ModelDefaultKind::Smart => {
                        updated.default_model.clone_from(&model.id);
                        if !model
                            .supported_reasoning_efforts
                            .contains(&updated.default_reasoning_effort)
                            && let Some(effort) = model.supported_reasoning_efforts.first()
                        {
                            updated.default_reasoning_effort.clone_from(effort);
                        }
                    }
                    ModelDefaultKind::Fast => updated.fast_model.clone_from(&model.id),
                }
                SelectionItem {
                    name: model.display_name.clone(),
                    description: Some(model.id.clone()),
                    is_current: model.id == *current,
                    actions: vec![provider_update_action(updated)],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        let title = match kind {
            ModelDefaultKind::Smart => "Default Smart Model",
            ModelDefaultKind::Fast => "Default Fast Model",
        };
        self.show_model_manager_selection(
            title,
            &format!("Provider: {}", provider.display_name),
            items,
            /*is_searchable*/ true,
        );
    }

    fn open_model_manager_reasoning(&mut self, provider: ManagedProviderSettings) {
        let mut efforts = provider
            .models
            .iter()
            .find(|model| model.id == provider.default_model)
            .map(|model| model.supported_reasoning_efforts.clone())
            .unwrap_or_default();
        efforts.sort_by_key(reasoning_rank);
        efforts.dedup();

        let items = efforts
            .into_iter()
            .map(|effort| {
                let mut updated = provider.clone();
                updated.default_reasoning_effort = effort.clone();
                SelectionItem {
                    name: effort.to_string(),
                    is_current: effort == provider.default_reasoning_effort,
                    actions: vec![provider_update_action(updated)],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.show_model_manager_selection(
            "Default Reasoning Effort",
            &format!("Provider: {}", provider.display_name),
            items,
            /*is_searchable*/ false,
        );
    }

    fn open_model_manager_models(&mut self, provider: ManagedProviderSettings) {
        let items = provider
            .models
            .iter()
            .map(|model| {
                let action_provider = provider.clone();
                let action_model = model.clone();
                SelectionItem {
                    name: model.display_name.clone(),
                    description: Some(format!(
                        "{} · context {} · compact {}",
                        model.id, model.context_window, model.auto_compact_token_limit
                    )),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::ModelManagerUi(ModelManagerUiAction::OpenModel {
                            provider: action_provider.clone(),
                            model: action_model.clone(),
                        }));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.show_model_manager_selection(
            &format!("{} Models", provider.display_name),
            "Edit model instructions and context limits.",
            items,
            /*is_searchable*/ true,
        );
    }

    fn open_model_manager_model(
        &mut self,
        provider: ManagedProviderSettings,
        model: ManagedModelSettings,
    ) {
        let items = vec![
            model_setting_item(
                "Context window",
                model.context_window.to_string(),
                &provider,
                &model,
                ModelManagerSetting::ContextWindow,
            ),
            model_setting_item(
                "Maximum context window",
                model.max_context_window.to_string(),
                &provider,
                &model,
                ModelManagerSetting::MaxContextWindow,
            ),
            model_setting_item(
                "Automatic compaction limit",
                model.auto_compact_token_limit.to_string(),
                &provider,
                &model,
                ModelManagerSetting::AutoCompactTokenLimit,
            ),
            model_setting_item(
                "Base instructions",
                format!("{} characters", model.base_instructions.chars().count()),
                &provider,
                &model,
                ModelManagerSetting::BaseInstructions,
            ),
        ];
        self.show_model_manager_selection(
            &model.display_name,
            &format!("{} · {}", provider.display_name, model.id),
            items,
            /*is_searchable*/ false,
        );
    }

    fn show_model_manager_selection(
        &mut self,
        title: &str,
        subtitle: &str,
        items: Vec<SelectionItem>,
        is_searchable: bool,
    ) {
        let mut header = ColumnRenderable::new();
        header.push(Line::from(title.to_string().bold()));
        header.push(Line::from(subtitle.to_string().dim()));
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable,
            search_placeholder: is_searchable.then(|| "Search models".to_string()),
            header: Box::new(header),
            ..Default::default()
        });
    }
}

#[derive(Clone, Copy)]
enum ModelDefaultKind {
    Smart,
    Fast,
}

fn navigation_item(name: &str, description: String, action: ModelManagerUiAction) -> SelectionItem {
    SelectionItem {
        name: name.to_string(),
        description: Some(description),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::ModelManagerUi(action.clone()));
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn provider_update_action(provider: ManagedProviderSettings) -> SelectionAction {
    Box::new(move |tx| {
        tx.send(AppEvent::UpdateModelManager(provider_update(&provider)));
    })
}

fn provider_update(provider: &ManagedProviderSettings) -> ModelManagerUpdateParams {
    ModelManagerUpdateParams::ProviderDefaults {
        provider_id: provider.id.clone(),
        default_model: provider.default_model.clone(),
        fast_model: provider.fast_model.clone(),
        default_reasoning_effort: provider.default_reasoning_effort.clone(),
    }
}

fn model_setting_item(
    name: &str,
    description: String,
    provider: &ManagedProviderSettings,
    model: &ManagedModelSettings,
    setting: ModelManagerSetting,
) -> SelectionItem {
    let provider = provider.clone();
    let model = model.clone();
    SelectionItem {
        name: name.to_string(),
        description: Some(description),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::ModelManagerUi(
                ModelManagerUiAction::PromptModelSetting {
                    provider: provider.clone(),
                    model: model.clone(),
                    setting,
                },
            ));
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn provider_auth_status(provider: &ManagedProviderSettings) -> &'static str {
    match (provider.api_key_configured, provider.oauth_configured) {
        (true, true) => "API key + OAuth",
        (true, false) => "API key",
        (false, true) => "OAuth",
        (false, false) => "not configured",
    }
}

fn reasoning_rank(effort: &xedoc_protocol::openai_models::ReasoningEffort) -> usize {
    use xedoc_protocol::openai_models::ReasoningEffort;

    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::XHigh => 5,
        ReasoningEffort::Max => 6,
        ReasoningEffort::Ultra => 7,
        ReasoningEffort::Custom(_) => 8,
    }
}
