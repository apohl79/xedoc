use super::*;
use xedoc_app_server_protocol::ModelRouterDecisionNotification;
use xedoc_app_server_protocol::ModelRouterEffectiveRoute;
use xedoc_app_server_protocol::ModelRouterScope;
use xedoc_protocol::config_types::ServiceTier;

impl ChatWidget {
    pub(super) fn on_model_router_decision(
        &mut self,
        notification: ModelRouterDecisionNotification,
    ) {
        if notification.scope == ModelRouterScope::Root
            && let ModelRouterEffectiveRoute::Available {
                model_slug,
                reasoning_effort,
                ..
            } = &notification.effective_route
        {
            let fast = self.current_service_tier() == Some(ServiceTier::Fast.request_value());
            self.bottom_pane
                .set_runtime_context(model_slug, Some(reasoning_effort.clone()), fast);
            self.request_redraw();
        }
        if !notification.feedback_visible {
            return;
        }
        self.add_to_history(history_cell::new_model_router_decision(notification));
    }
}
