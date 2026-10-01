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
                    dismiss_on_select: true,
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
        let current_kind = self
            .config
            .provider_budgets
            .get(&provider_id)
            .map_or(ProviderBudgetKind::None, |budget| budget.kind);
        let options = [
            (ProviderBudgetKind::None, "None", "Do not track a budget"),
            (
                ProviderBudgetKind::Monthly,
                "Monthly",
                "Set a USD amount and the day of the month it refills",
            ),
            (
                ProviderBudgetKind::Prepaid,
                "Prepaid",
                "Set a USD amount; tracking restarts now and on every refill",
            ),
        ];
        let initial_selected_idx = options
            .iter()
            .position(|(kind, _, _)| *kind == current_kind);
        let items = options
            .into_iter()
            .map(|(kind, name, description)| {
                let tx = self.app_event_tx.clone();
                let provider_id = provider_id.clone();
                SelectionItem {
                    name: name.to_string(),
                    description: Some(description.to_string()),
                    is_current: kind == current_kind,
                    actions: vec![Box::new(move |_| {
                        tx.send(AppEvent::OpenProviderBudgetValue {
                            provider_id: provider_id.clone(),
                            kind,
                            error: None,
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("{provider_id} budget")),
            subtitle: Some("Select a budget type.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    pub fn open_provider_budget_value_prompt(
        &mut self,
        provider_id: String,
        kind: ProviderBudgetKind,
        error: Option<String>,
    ) {
        if let Some(error) = error {
            self.add_error_message(error);
        }
        let placeholder = match kind {
            ProviderBudgetKind::None => {
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
            ProviderBudgetKind::Monthly => "Monthly budget in USD, for example 100",
            ProviderBudgetKind::Prepaid => "Prepaid balance in USD, for example 25",
        };
        let initial_text = self
            .config
            .provider_budgets
            .get(&provider_id)
            .filter(|budget| budget.kind == kind)
            .and_then(|budget| budget.usd)
            .map(|usd| usd.to_string())
            .unwrap_or_default();
        let title = match kind {
            ProviderBudgetKind::Monthly => format!("{provider_id} monthly budget (USD)"),
            ProviderBudgetKind::Prepaid => format!("{provider_id} prepaid balance (USD)"),
            ProviderBudgetKind::None => unreachable!("handled above"),
        };
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            title,
            placeholder.to_string(),
            initial_text,
            None,
            Box::new(move |value| {
                let usd = value
                    .trim()
                    .trim_start_matches('$')
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|usd| usd.is_finite() && *usd > 0.0);
                let Some(usd) = usd else {
                    tx.send(AppEvent::OpenProviderBudgetValue {
                        provider_id: provider_id.clone(),
                        kind,
                        error: Some(format!(
                            "Invalid budget {value:?}: enter a positive USD amount, for example 25."
                        )),
                    });
                    return;
                };
                match kind {
                    ProviderBudgetKind::Monthly => {
                        tx.send(AppEvent::OpenProviderBudgetResetDay {
                            provider_id: provider_id.clone(),
                            usd,
                        });
                    }
                    ProviderBudgetKind::Prepaid => {
                        tx.send(AppEvent::ProviderBudgetUpdate {
                            provider_id: provider_id.clone(),
                            budget: ProviderBudget {
                                kind,
                                usd: Some(usd),
                                reset_day: None,
                                started_at: Some(chrono::Utc::now().timestamp()),
                            },
                        });
                    }
                    ProviderBudgetKind::None => {}
                }
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    pub fn open_provider_budget_reset_day(&mut self, provider_id: String, usd: f64) {
        let current_day = self
            .config
            .provider_budgets
            .get(&provider_id)
            .and_then(|budget| budget.reset_day)
            .filter(|day| (1..=31).contains(day))
            .unwrap_or(1);
        let items = (1..=31u8)
            .map(|day| {
                let tx = self.app_event_tx.clone();
                let provider_id = provider_id.clone();
                let description = match day {
                    1 => Some("Default".to_string()),
                    29..=31 => Some("Uses the last day in shorter months".to_string()),
                    _ => None,
                };
                SelectionItem {
                    name: format!("Day {day}"),
                    description,
                    is_current: day == current_day,
                    actions: vec![Box::new(move |_| {
                        tx.send(AppEvent::ProviderBudgetUpdate {
                            provider_id: provider_id.clone(),
                            budget: ProviderBudget {
                                kind: ProviderBudgetKind::Monthly,
                                usd: Some(usd),
                                reset_day: Some(day),
                                started_at: None,
                            },
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("{provider_id} monthly reset day")),
            subtitle: Some(format!("${usd:.2} refills on this day each month.")),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx: Some(usize::from(current_day - 1)),
            ..Default::default()
        });
    }
}
