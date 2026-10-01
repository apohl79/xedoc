//! Provider-budget selection controls.

use super::*;
use xedoc_config::types::ProviderBudget;
use xedoc_config::types::ProviderBudgetKind;

impl ChatWidget {
    pub(super) fn open_provider_budget_setup(&mut self) {
        let mut providers = self
            .config
            .model_providers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        providers.sort_unstable();
        let items = providers
            .into_iter()
            .map(|provider_id| {
                let budget = self.config.provider_budgets.get(&provider_id).cloned();
                let description = budget.as_ref().map_or_else(
                    || "No budget".to_string(),
                    |budget| match budget.kind {
                        ProviderBudgetKind::None => "No budget".to_string(),
                        ProviderBudgetKind::Monthly => format!(
                            "Monthly ${:.2}, resets on day {}",
                            budget.usd.unwrap_or_default(),
                            budget.reset_day.unwrap_or(1)
                        ),
                        ProviderBudgetKind::Prepaid => {
                            format!("Prepaid ${:.2}", budget.usd.unwrap_or_default())
                        }
                    },
                );
                let tx = self.app_event_tx.clone();
                SelectionItem {
                    name: provider_id.clone(),
                    description: Some(description),
                    actions: vec![Box::new(move |_| {
                        tx.send(AppEvent::OpenProviderBudget {
                            provider_id: provider_id.clone(),
                        });
                    })],
                    ..Default::default()
                }
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Provider budgets".to_string()),
            subtitle: Some("Select a provider to set its budget.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub fn open_provider_budget_kind(&mut self, provider_id: String) {
        let items = [
            (ProviderBudgetKind::None, "None", "Do not track a budget"),
            (
                ProviderBudgetKind::Monthly,
                "Monthly",
                "Resets on a chosen day each month",
            ),
            (
                ProviderBudgetKind::Prepaid,
                "Prepaid",
                "Starts now and resets when manually refilled",
            ),
        ]
        .into_iter()
        .map(|(kind, name, description)| {
            let tx = self.app_event_tx.clone();
            let provider_id = provider_id.clone();
            SelectionItem {
                name: name.to_string(),
                description: Some(description.to_string()),
                actions: vec![Box::new(move |_| {
                    tx.send(AppEvent::OpenProviderBudgetValue {
                        provider_id: provider_id.clone(),
                        kind,
                    });
                })],
                ..Default::default()
            }
        })
        .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("{provider_id} budget")),
            subtitle: Some(
                "Select a budget type. Budget values can be edited in config.toml.".to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub fn open_provider_budget_value_prompt(
        &mut self,
        provider_id: String,
        kind: ProviderBudgetKind,
    ) {
        if kind == ProviderBudgetKind::None {
            self.app_event_tx.send(AppEvent::ProviderBudgetUpdate {
                provider_id,
                budget: ProviderBudget {
                    kind,
                    usd: None,
                    reset_day: None,
                    started_at: None,
                },
            });
            return;
        }
        let existing = self.config.provider_budgets.get(&provider_id);
        let initial_text = match kind {
            ProviderBudgetKind::Monthly => format!(
                "{},{}",
                existing.and_then(|budget| budget.usd).unwrap_or_default(),
                existing.and_then(|budget| budget.reset_day).unwrap_or(1)
            ),
            ProviderBudgetKind::Prepaid => existing
                .and_then(|budget| budget.usd)
                .unwrap_or_default()
                .to_string(),
            ProviderBudgetKind::None => String::new(),
        };
        let placeholder = match kind {
            ProviderBudgetKind::Monthly => "USD budget, reset day (for example: 25,1)",
            ProviderBudgetKind::Prepaid => "USD budget (for example: 25)",
            ProviderBudgetKind::None => unreachable!(),
        };
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            format!("{provider_id} {kind:?} budget"),
            placeholder.to_string(),
            initial_text,
            None,
            Box::new(move |value| {
                let mut values = value.split(',').map(str::trim);
                let usd = values.next().and_then(|value| value.parse::<f64>().ok());
                let reset_day = values.next().and_then(|value| value.parse::<u8>().ok());
                let valid = usd.is_some_and(|usd| usd > 0.0)
                    && (kind != ProviderBudgetKind::Monthly
                        || reset_day.is_some_and(|day| (1..=31).contains(&day)));
                if valid {
                    tx.send(AppEvent::ProviderBudgetUpdate {
                        provider_id: provider_id.clone(),
                        budget: ProviderBudget {
                            kind,
                            usd,
                            reset_day: (kind == ProviderBudgetKind::Monthly)
                                .then_some(reset_day.unwrap_or(1)),
                            started_at: (kind == ProviderBudgetKind::Prepaid)
                                .then(|| chrono::Utc::now().timestamp()),
                        },
                    });
                }
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }
}
