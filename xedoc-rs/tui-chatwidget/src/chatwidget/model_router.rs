use super::*;
use xedoc_app_server_protocol::ModelRouterDecisionNotification;

impl ChatWidget {
    pub(super) fn on_model_router_decision(
        &mut self,
        notification: ModelRouterDecisionNotification,
    ) {
        if !self.config.model_router.decision_feedback {
            return;
        }
        self.add_to_history(history_cell::new_model_router_decision(notification));
    }
}
