//! Selection views for managing the user-owned model-router policy.

use super::*;

impl ChatWidget {
    pub fn open_model_router_policy_manager(
        &mut self,
        policy: xedoc_app_server_protocol::ModelRouterPolicy,
    ) {
        let presets = [
            ("Strict", 0.50, 0.15, "Fewer automatic classifications."),
            (
                "Balanced",
                0.35,
                0.08,
                "Bundled default confidence thresholds.",
            ),
            (
                "Permissive",
                0.20,
                0.04,
                "More classifications; review approval prompts carefully.",
            ),
        ];
        let mut items = presets
            .into_iter()
            .map(|(name, minimum_score, minimum_margin, description)| {
                let mut next = policy.clone();
                next.minimum_score = minimum_score;
                next.minimum_margin = minimum_margin;
                SelectionItem {
                    name: format!("Confidence: {name} ({minimum_score:.2} / {minimum_margin:.2})"),
                    description: Some(description.to_string()),
                    is_current: (policy.minimum_score - minimum_score).abs() < f64::EPSILON
                        && (policy.minimum_margin - minimum_margin).abs() < f64::EPSILON,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::UpdateModelRouterPolicy {
                            policy: model_router_policy_update(&next),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();
        for class in &policy.classes {
            let mut next = policy.clone();
            let Some(next_class) = next
                .classes
                .iter_mut()
                .find(|candidate| candidate.id == class.id)
            else {
                continue;
            };
            next_class.minimum_reasoning_effort =
                next_model_router_effort(&class.minimum_reasoning_effort);
            let capabilities = if class.required_capabilities.is_empty() {
                "no required capabilities".to_string()
            } else {
                format!("requires {}", class.required_capabilities.join(", "))
            };
            items.push(SelectionItem {
                name: format!(
                    "Class: {} — {}",
                    class.id, class.minimum_reasoning_effort
                ),
                description: Some(format!(
                    "{capabilities}. Select to cycle the minimum effort: low → medium → high → xhigh."
                )),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::UpdateModelRouterPolicy {
                        policy: model_router_policy_update(&next),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Model-router policy".to_string()),
            subtitle: Some(format!(
                "Policy {}; classifier {}. Select an item to tune it.",
                policy.policy_revision, policy.classifier_revision
            )),
            items,
            ..Default::default()
        });
    }

    pub fn update_model_router_mode(&mut self, mode: &str) {
        self.config.model_router.mode = match mode {
            "off" => xedoc_config::ModelRouterMode::Off,
            "shadow-subagents" => xedoc_config::ModelRouterMode::ShadowSubagents,
            "shadow-full" => xedoc_config::ModelRouterMode::ShadowFull,
            "subagents" => xedoc_config::ModelRouterMode::Subagents,
            "full" => xedoc_config::ModelRouterMode::Full,
            _ => return,
        };
    }

    pub fn update_model_router_approval(&mut self, approval: bool) {
        self.config.model_router.approval = approval;
    }
}

fn model_router_policy_update(
    policy: &xedoc_app_server_protocol::ModelRouterPolicy,
) -> xedoc_app_server_protocol::ModelRouterPolicyWriteParams {
    xedoc_app_server_protocol::ModelRouterPolicyWriteParams {
        minimum_score: policy.minimum_score,
        minimum_margin: policy.minimum_margin,
        classes: policy.classes.clone(),
    }
}

fn next_model_router_effort(
    effort: &xedoc_protocol::openai_models::ReasoningEffort,
) -> xedoc_protocol::openai_models::ReasoningEffort {
    match effort {
        xedoc_protocol::openai_models::ReasoningEffort::Low => {
            xedoc_protocol::openai_models::ReasoningEffort::Medium
        }
        xedoc_protocol::openai_models::ReasoningEffort::Medium => {
            xedoc_protocol::openai_models::ReasoningEffort::High
        }
        xedoc_protocol::openai_models::ReasoningEffort::High
        | xedoc_protocol::openai_models::ReasoningEffort::Max
        | xedoc_protocol::openai_models::ReasoningEffort::Ultra
        | xedoc_protocol::openai_models::ReasoningEffort::None
        | xedoc_protocol::openai_models::ReasoningEffort::Minimal
        | xedoc_protocol::openai_models::ReasoningEffort::Custom(_) => {
            xedoc_protocol::openai_models::ReasoningEffort::XHigh
        }
        xedoc_protocol::openai_models::ReasoningEffort::XHigh => {
            xedoc_protocol::openai_models::ReasoningEffort::Low
        }
    }
}
