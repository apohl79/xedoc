use super::*;
use xedoc_app_server_protocol::ModelRouterActivityNotification;
use xedoc_app_server_protocol::ModelRouterActivityState;
use xedoc_app_server_protocol::ModelRouterDecisionNotification;
use xedoc_app_server_protocol::ModelRouterEffectiveRoute;
use xedoc_app_server_protocol::ModelRouterScope;
use xedoc_protocol::config_types::ServiceTier;

impl ChatWidget {
    pub(super) fn on_model_router_activity(
        &mut self,
        notification: ModelRouterActivityNotification,
    ) {
        match notification.state {
            ModelRouterActivityState::Started => {
                self.bottom_pane.ensure_status_indicator();
                self.bottom_pane
                    .set_interrupt_hint_visible(/*visible*/ false);
                self.status_state.terminal_title_status_kind = TerminalTitleStatusKind::Working;
                self.set_status_header(String::from("Routing"));
            }
            ModelRouterActivityState::Finished => {
                if self.status_state.current_status.header == "Routing" {
                    if self.bottom_pane.is_task_running() {
                        self.restore_reasoning_status_header();
                    } else {
                        self.hide_status_indicator();
                        self.status_state.terminal_title_status_kind =
                            TerminalTitleStatusKind::Thinking;
                        self.refresh_status_surfaces();
                    }
                }
            }
        }
        self.request_redraw();
    }

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
