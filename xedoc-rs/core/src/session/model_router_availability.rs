//! Latches confirmed table failures to this session, never the shared policy.

use serde_json::json;
use tokio_util::sync::CancellationToken;
use xedoc_protocol::protocol::Event;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::WarningEvent;

use super::Session;
use crate::model_router::available_script_providers;
use crate::model_router::eligible_script_routes;
use crate::model_router::script_model_availability;
use crate::model_router_script_host::ModelRouterScriptHost;

impl Session {
    pub(crate) async fn model_router_session_mode(&self) -> Option<String> {
        let mut mode = self
            .state
            .lock()
            .await
            .model_router_session_mode()
            .map(str::to_string);
        if mode.is_none()
            && let Some(state_db) = self.state_db()
        {
            match state_db
                .model_router_session_mode(&self.thread_id.to_string())
                .await
            {
                Ok(saved) => mode = saved,
                Err(error) => {
                    tracing::warn!(%error, thread_id = %self.thread_id, "failed to read model-router session mode");
                }
            }
        }
        if mode.as_deref() == Some("off") {
            return mode;
        }
        let (config, revision) = {
            let state = self.state.lock().await;
            (
                Self::build_effective_session_config(&state.session_configuration),
                state.model_router_mode_revision(),
            )
        };
        let Some(host) = ModelRouterScriptHost::from_config(&config) else {
            return mode;
        };
        let models = &self.services.models_manager;
        let context = json!({
            "thread": {"id": self.thread_id.to_string()},
            "eligibleRoutes": eligible_script_routes(&config, models).await,
            "modelAvailability": script_model_availability(&config, models).await,
            "modelAliases": models.routing_model_aliases(),
            "availableProviders": available_script_providers(&config, &self.services.auth_manager),
            "session": {"routerMode": mode},
        });
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let router = match host.state(context, cancellation).await {
            Ok(router) => router,
            Err(failure) => {
                self.send_event_raw(Event {
                    id: String::new(),
                    msg: EventMsg::Warning(WarningEvent {
                        message: failure.warning_message(),
                    }),
                })
                .await;
                return mode;
            }
        };
        if let Some(reason) = router.unavailable_reason {
            let mut state = self.state.lock().await;
            if state.model_router_mode_revision() != revision {
                return state.model_router_session_mode().map(str::to_string);
            }
            mode = Some("off".to_string());
            state.set_model_router_session_mode(mode.clone());
            if let Some(state_db) = self.state_db()
                && let Err(error) = state_db
                    .set_model_router_session_mode(&self.thread_id.to_string(), mode.as_deref())
                    .await
            {
                tracing::warn!(%error, "failed to persist unavailable model-router session mode");
            }
            tracing::info!(thread_id = %self.thread_id, %reason, "disabled unavailable routing table for session");
            drop(state);
            self.send_event_raw(Event {
                id: String::new(),
                msg: EventMsg::Warning(WarningEvent {
                    message: format!(
                        "Model router warning: routing is disabled for this session: {reason}"
                    ),
                }),
            })
            .await;
        }
        mode
    }
}
