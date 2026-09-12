//! Selection views for managing the user-owned model-router policy.

use super::*;

const ROUTE_TAG_PREFIX: &str = "__model_router_route__";

impl ChatWidget {
    pub fn open_model_router_policy_manager(
        &mut self,
        policy: xedoc_app_server_protocol::ModelRouterPolicy,
    ) {
        self.bottom_pane
            .dismiss_active_view_if_id("model-router-policy-loading");
        let mut items = Vec::new();
        let confidence_policy = policy.clone();
        items.push(SelectionItem {
            name: format!(
                "Confidence: {:.2} / {:.2}",
                policy.minimum_score, policy.minimum_margin
            ),
            description: Some("Choose the classifier confidence thresholds.".to_string()),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenModelRouterPolicyConfidenceMenu {
                    policy: confidence_policy.clone(),
                })
            })],
            dismiss_on_select: false,
            dismiss_parent_on_child_accept: true,
            ..Default::default()
        });
        for class in &policy.classes {
            let class_policy = policy.clone();
            let class_id = class.id.clone();
            let route = configured_route(&policy, class);
            items.push(SelectionItem {
                name: format!("Class: {}", class.id),
                description: Some(format!(
                    "Effort: {}; model: {}",
                    class.minimum_reasoning_effort,
                    route.as_deref().unwrap_or("Automatic")
                )),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenModelRouterPolicyClassMenu {
                        policy: class_policy.clone(),
                        class_id: class_id.clone(),
                    })
                })],
                dismiss_on_select: false,
                dismiss_parent_on_child_accept: true,
                ..Default::default()
            });
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Model-router policy".to_string()),
            subtitle: Some(format!(
                "Policy {}; classifier {}",
                policy.policy_revision, policy.classifier_revision
            )),
            items,
            ..Default::default()
        });
    }

    pub fn open_model_router_policy_confidence_menu(
        &mut self,
        policy: xedoc_app_server_protocol::ModelRouterPolicy,
    ) {
        let presets = [
            ("Strict", 0.50, 0.15),
            ("Balanced", 0.35, 0.08),
            ("Permissive", 0.20, 0.04),
        ];
        let items = presets
            .into_iter()
            .map(|(name, minimum_score, minimum_margin)| {
                let mut next = policy.clone();
                next.minimum_score = minimum_score;
                next.minimum_margin = minimum_margin;
                SelectionItem {
                    name: format!("{name} ({minimum_score:.2} / {minimum_margin:.2})"),
                    is_current: (policy.minimum_score - minimum_score).abs() < f64::EPSILON
                        && (policy.minimum_margin - minimum_margin).abs() < f64::EPSILON,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::UpdateModelRouterPolicy {
                            policy: model_router_policy_update(&next),
                        })
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Policy confidence".to_string()),
            items,
            ..Default::default()
        });
    }

    pub fn open_model_router_policy_class_menu(
        &mut self,
        policy: xedoc_app_server_protocol::ModelRouterPolicy,
        class_id: String,
    ) {
        let Some(class) = policy.classes.iter().find(|class| class.id == class_id) else {
            return;
        };
        let mut items = Vec::new();
        let effort_policy = policy.clone();
        let effort_class_id = class_id.clone();
        items.push(SelectionItem {
            name: format!("Effort: {}", class.minimum_reasoning_effort),
            description: Some("Choose the minimum reasoning effort.".to_string()),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenModelRouterPolicyClassEffortMenu {
                    policy: effort_policy.clone(),
                    class_id: effort_class_id.clone(),
                })
            })],
            dismiss_on_select: false,
            dismiss_parent_on_child_accept: true,
            ..Default::default()
        });
        let route_policy = policy.clone();
        let route_class_id = class_id.clone();
        items.push(SelectionItem {
            name: format!(
                "Model: {}",
                configured_route(&policy, class)
                    .as_deref()
                    .unwrap_or("Automatic")
            ),
            description: Some("Pin this class to a user-enabled provider/model route.".to_string()),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenModelRouterPolicyRouteMenu {
                    policy: route_policy.clone(),
                    class_id: route_class_id.clone(),
                })
            })],
            dismiss_on_select: false,
            dismiss_parent_on_child_accept: true,
            ..Default::default()
        });
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("Policy class: {class_id}")),
            items,
            ..Default::default()
        });
    }

    pub fn open_model_router_policy_class_effort_menu(
        &mut self,
        policy: xedoc_app_server_protocol::ModelRouterPolicy,
        class_id: String,
    ) {
        let efforts = [
            xedoc_protocol::openai_models::ReasoningEffort::Low,
            xedoc_protocol::openai_models::ReasoningEffort::Medium,
            xedoc_protocol::openai_models::ReasoningEffort::High,
            xedoc_protocol::openai_models::ReasoningEffort::XHigh,
        ];
        let items = efforts
            .into_iter()
            .map(|effort| {
                let mut next = policy.clone();
                if let Some(class) = next.classes.iter_mut().find(|class| class.id == class_id) {
                    class.minimum_reasoning_effort = effort.clone();
                }
                SelectionItem {
                    name: effort.to_string(),
                    is_current: policy
                        .classes
                        .iter()
                        .find(|class| class.id == class_id)
                        .is_some_and(|class| class.minimum_reasoning_effort == effort),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::UpdateModelRouterPolicy {
                            policy: model_router_policy_update(&next),
                        })
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Minimum effort".to_string()),
            items,
            ..Default::default()
        });
    }

    pub fn open_model_router_policy_route_menu(
        &mut self,
        policy: xedoc_app_server_protocol::ModelRouterPolicy,
        class_id: String,
    ) {
        let mut items = vec![SelectionItem {
            name: "Automatic".to_string(),
            description: Some("Let the router choose among eligible models.".to_string()),
            is_current: policy
                .classes
                .iter()
                .find(|class| class.id == class_id)
                .is_some_and(|class| configured_route(&policy, class).is_none()),
            actions: vec![Box::new({
                let policy = policy.clone();
                let class_id = class_id.clone();
                move |tx| {
                    tx.send(AppEvent::UpdateModelRouterPolicy {
                        policy: route_policy_update(&policy, &class_id, None),
                    })
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        }];
        let mut models = self
            .model_catalog
            .try_list_models()
            .unwrap_or_default()
            .into_iter()
            .filter(|model| model.show_in_picker && model.supported_in_api)
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            left.provider_id
                .cmp(&right.provider_id)
                .then_with(|| left.model.cmp(&right.model))
        });
        for model in models {
            let provider = model.provider_id.clone();
            let slug = model.model.clone();
            let policy_for_action = policy.clone();
            let class_for_action = class_id.clone();
            let current = policy
                .classes
                .iter()
                .find(|class| class.id == class_id)
                .and_then(|class| configured_route(&policy, class))
                .is_some_and(|route| route == format!("{provider}/{slug}"));
            items.push(SelectionItem {
                name: format!("{provider}/{slug}"),
                is_current: current,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::UpdateModelRouterPolicy {
                        policy: route_policy_update(
                            &policy_for_action,
                            &class_for_action,
                            Some((provider.clone(), slug.clone())),
                        ),
                    })
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Class model".to_string()),
            items,
            ..Default::default()
        });
    }

    pub fn show_model_router_policy_loading(&mut self) {
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some("model-router-policy-loading"),
            title: Some("Model-router policy".to_string()),
            items: vec![SelectionItem {
                name: "Loading…".to_string(),
                is_disabled: true,
                ..Default::default()
            }],
            ..Default::default()
        });
    }

    pub fn dismiss_model_router_policy_loading(&mut self) {
        self.bottom_pane
            .dismiss_active_view_if_id("model-router-policy-loading");
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

    pub fn update_model_router_decision_feedback(&mut self, enabled: bool) {
        self.config.model_router.decision_feedback = enabled;
    }
}

fn model_router_policy_update(
    policy: &xedoc_app_server_protocol::ModelRouterPolicy,
) -> xedoc_app_server_protocol::ModelRouterPolicyWriteParams {
    xedoc_app_server_protocol::ModelRouterPolicyWriteParams {
        minimum_score: policy.minimum_score,
        minimum_margin: policy.minimum_margin,
        capabilities: policy.capabilities.clone(),
        classes: policy.classes.clone(),
    }
}

fn route_tag(class_id: &str) -> String {
    format!("{ROUTE_TAG_PREFIX}{class_id}")
}

fn configured_route(
    policy: &xedoc_app_server_protocol::ModelRouterPolicy,
    class: &xedoc_app_server_protocol::ModelRouterPolicyClass,
) -> Option<String> {
    let tag = route_tag(&class.id);
    policy
        .capabilities
        .iter()
        .find(|capability| capability.tags.iter().any(|candidate| candidate == &tag))
        .map(|capability| format!("{}/{}", capability.provider, capability.model))
}

fn route_policy_update(
    policy: &xedoc_app_server_protocol::ModelRouterPolicy,
    class_id: &str,
    route: Option<(String, String)>,
) -> xedoc_app_server_protocol::ModelRouterPolicyWriteParams {
    let tag = route_tag(class_id);
    let is_pinned = route.is_some();
    let mut next = policy.clone();
    for capability in &mut next.capabilities {
        capability.tags.retain(|candidate| candidate != &tag);
    }
    next.capabilities
        .retain(|capability| !capability.tags.is_empty());
    if let Some((provider, model)) = route {
        if let Some(capability) = next
            .capabilities
            .iter_mut()
            .find(|capability| capability.provider == provider && capability.model == model)
        {
            capability.tags.push(tag.clone());
        } else {
            next.capabilities
                .push(xedoc_app_server_protocol::ModelRouterCapability {
                    provider,
                    model,
                    tags: vec![tag.clone()],
                });
        }
    }
    if let Some(class) = next.classes.iter_mut().find(|class| class.id == class_id) {
        class
            .required_capabilities
            .retain(|candidate| candidate != &tag);
        if is_pinned {
            class.required_capabilities.push(tag);
        }
    }
    model_router_policy_update(&next)
}
