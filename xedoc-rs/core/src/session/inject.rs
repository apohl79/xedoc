use super::input_queue::TurnInput;
use super::session::Session;
use super::turn_context::TurnContext;
use crate::model_router_script_host::ModelRouterScriptDecisionOutcome;
use crate::model_router_script_host::ModelRouterScriptHost;
use crate::model_router_script_host::ModelRouterScriptInteractionOutcome;
use crate::state::ActiveTurn;
use crate::state::TurnState;
use crate::tasks::RegularTask;
use crate::xedoc_thread::TryStartTurnIfIdleError;
use crate::xedoc_thread::TryStartTurnIfIdleRejectionReason;
use std::sync::Arc;
use xedoc_protocol::config_types::ModeKind;
use xedoc_protocol::models::ResponseItem;

impl Session {
    /// Returns the input if there is no active turn to inject into.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn inject_if_running(
        &self,
        input: Vec<ResponseItem>,
    ) -> Result<(), Vec<ResponseItem>> {
        let mut active = self.active_turn.lock().await;
        match active.as_mut() {
            Some(active_turn) => {
                self.input_queue
                    .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                        active_turn.turn_state.as_ref(),
                        input.into_iter().map(TurnInput::ResponseItem).collect(),
                    )
                    .await;
                Ok(())
            }
            None => Err(input),
        }
    }

    /// Starts a regular turn with the provided items only if automatic idle work
    /// is allowed for the current session state.
    ///
    /// This is the shared gate for extension-initiated idle work. It refuses to
    /// start a turn when user/client-triggered work is queued, any task is still
    /// active, or the session is currently in Plan mode. Active Review tasks are
    /// covered by the active-task check because Review turns are not steerable.
    pub(crate) async fn try_start_turn_if_idle(
        self: &Arc<Self>,
        input: Vec<ResponseItem>,
    ) -> Result<(), TryStartTurnIfIdleError> {
        self.try_start_turn_if_idle_inner(input, uuid::Uuid::new_v4().to_string())
            .await
    }

    pub(crate) async fn try_start_turn_if_idle_with_response(
        self: &Arc<Self>,
        input: Vec<ResponseItem>,
        sub_id: String,
    ) -> Result<(), TryStartTurnIfIdleError> {
        self.try_start_turn_if_idle_inner(input, sub_id).await
    }

    async fn try_start_turn_if_idle_inner(
        self: &Arc<Self>,
        input: Vec<ResponseItem>,
        sub_id: String,
    ) -> Result<(), TryStartTurnIfIdleError> {
        if input.is_empty() {
            return Ok(());
        }
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PendingTriggerTurn,
                input,
            ));
        }
        if self.collaboration_mode().await.mode == ModeKind::Plan {
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PlanMode,
                input,
            ));
        }

        let turn_state = {
            let mut active_turn = self.active_turn.lock().await;
            if active_turn.is_some() {
                return Err(TryStartTurnIfIdleError::new(
                    TryStartTurnIfIdleRejectionReason::Busy,
                    input,
                ));
            }
            let active_turn = active_turn.get_or_insert_with(ActiveTurn::default);
            Arc::clone(&active_turn.turn_state)
        };

        if self.input_queue.has_trigger_turn_mailbox_items().await {
            self.clear_reserved_idle_turn(&turn_state).await;
            self.maybe_start_turn_for_pending_work().await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PendingTriggerTurn,
                input,
            ));
        }

        let mut turn_context = self.new_default_turn_with_sub_id(sub_id.clone()).await;
        if turn_context.mode == ModeKind::Plan {
            self.clear_reserved_idle_turn(&turn_state).await;
            self.maybe_start_turn_for_pending_work().await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PlanMode,
                input,
            ));
        }
        if let Some(script_host) = ModelRouterScriptHost::from_config(turn_context.config.as_ref())
            && let Some(baseline_route) =
                crate::model_router::current_script_route(turn_context.config.as_ref())
        {
            let eligible_routes = crate::model_router::eligible_script_routes(
                turn_context.config.as_ref(),
                &self.services.models_manager,
            )
            .await;
            let (current_route, router_instructions) = self
                .state
                .lock()
                .await
                .model_router_applied_route(&baseline_route)
                .filter(|(route, _)| {
                    eligible_routes.iter().any(|eligible| {
                        eligible.provider_id == route.provider_id
                            && eligible.model == route.model
                            && eligible.reasoning_efforts.contains(&route.reasoning_effort)
                    })
                })
                .unwrap_or_else(|| (baseline_route.clone(), None));
            self.begin_model_router_ab_root_turn(baseline_route.clone())
                .await;
            let cancellation = self.begin_model_router_script_invocation().await;
            let context = crate::session::model_router_script_context::build(
                crate::session::model_router_script_context::RoutingContextInput {
                    session: self,
                    turn: turn_context.as_ref(),
                    turn_state:
                        crate::session::model_router_script_context::RoutingTurnState::PendingRoot,
                    current_route: &current_route,
                    eligible_routes: &eligible_routes,
                },
            )
            .await;
            let outcome = if let Some(response) =
                self.take_scripted_interaction_response(&sub_id).await
            {
                script_host
                    .respond(
                        context,
                        crate::model_router_script_host::interaction_response(response.response),
                        &eligible_routes,
                        /*route_mutable*/ true,
                        response.automated,
                        cancellation.child_token(),
                    )
                    .await
            } else {
                self.emit_model_router_activity(
                    turn_context.as_ref(),
                    xedoc_protocol::protocol::ModelRouterScope::Root,
                    xedoc_protocol::protocol::ModelRouterActivityState::Started,
                )
                .await;
                let outcome = script_host
                    .decide(
                        self,
                        turn_context.as_ref(),
                        turn_context.config.as_ref(),
                        context,
                        serde_json::json!({
                            "prompt": input.iter().filter_map(|item| match item {
                                ResponseItem::Message { content, .. } => crate::content_items_to_text(content),
                                _ => None,
                            }).collect::<Vec<_>>().join("\n"),
                            "explicitRouteOverride": false,
                        }),
                        &eligible_routes,
                        /*route_mutable*/ true,
                        cancellation.child_token(),
                    )
                    .await;
                self.emit_model_router_activity(
                    turn_context.as_ref(),
                    xedoc_protocol::protocol::ModelRouterScope::Root,
                    xedoc_protocol::protocol::ModelRouterActivityState::Finished,
                )
                .await;
                match outcome {
                    ModelRouterScriptDecisionOutcome::Apply { decision, route } => {
                        ModelRouterScriptInteractionOutcome::Apply { decision, route }
                    }
                    ModelRouterScriptDecisionOutcome::KeepCurrent { decision, failure } => {
                        ModelRouterScriptInteractionOutcome::KeepCurrent { decision, failure }
                    }
                    ModelRouterScriptDecisionOutcome::Interaction(interaction) => {
                        ModelRouterScriptInteractionOutcome::Interaction(interaction)
                    }
                }
            };
            if cancellation.is_cancelled() {
                self.clear_reserved_idle_turn(&turn_state).await;
                return Err(TryStartTurnIfIdleError::new(
                    TryStartTurnIfIdleRejectionReason::Busy,
                    input,
                ));
            }
            match outcome {
                ModelRouterScriptInteractionOutcome::Apply { decision, route } => {
                    let model_instructions = decision.model_instructions.clone();
                    if let Some(routed_context) = self
                        .new_script_routed_turn_from_current_settings_with_sub_id(
                            turn_context.sub_id.clone(),
                            /*final_output_json_schema*/ None,
                            &route,
                            model_instructions.as_deref(),
                        )
                        .await
                    {
                        self.emit_and_remember_model_router_decision(
                            turn_context.as_ref(),
                            crate::model_router_script_host::decision_event(
                                decision,
                                &route,
                                self.thread_id.to_string(),
                                turn_context.sub_id.clone(),
                                xedoc_protocol::protocol::ModelRouterScope::Root,
                                /*applied*/ true,
                                None,
                            ),
                        )
                        .await;
                        if let Some(startup_prewarm) = self.take_session_startup_prewarm().await {
                            startup_prewarm.abort().await;
                        }
                        if route != current_route {
                            self.force_full_context_replay().await;
                        }
                        self.state.lock().await.set_model_router_applied_route(
                            baseline_route,
                            route,
                            model_instructions,
                        );
                        turn_context = routed_context;
                    }
                }
                ModelRouterScriptInteractionOutcome::KeepCurrent { decision, failure } => {
                    if (current_route != baseline_route || router_instructions.is_some())
                        && let Some(retained_context) = self
                            .new_script_routed_turn_from_current_settings_with_sub_id(
                                turn_context.sub_id.clone(),
                                /*final_output_json_schema*/ None,
                                &current_route,
                                router_instructions.as_deref(),
                            )
                            .await
                    {
                        if let Some(startup_prewarm) = self.take_session_startup_prewarm().await {
                            startup_prewarm.abort().await;
                        }
                        turn_context = retained_context;
                    }
                    if let Some(decision) = decision {
                        self.emit_and_remember_model_router_decision(
                            turn_context.as_ref(),
                            crate::model_router_script_host::decision_event(
                                decision,
                                &current_route,
                                self.thread_id.to_string(),
                                turn_context.sub_id.clone(),
                                xedoc_protocol::protocol::ModelRouterScope::Root,
                                /*applied*/ false,
                                failure.as_ref(),
                            ),
                        )
                        .await;
                    }
                }
                ModelRouterScriptInteractionOutcome::Interaction(interaction) => {
                    let pending = crate::model_router_script_host::interaction_request(interaction)
                        .ok()
                        .and_then(|(request, extension_id, interaction_id, continuation, state_revision)| {
                            serde_json::from_value(request.surface.clone()).ok().map(|surface| {
                                (request.clone(), crate::session::session::PendingScriptedInteraction {
                                    extension_id,
                                    interaction_id,
                                    script_continuation: continuation,
                                    state_revision,
                                    expires_at: request.expires_at,
                                    surface,
                                    continuation: crate::session::session::PendingScriptedInteractionContinuation::Idle {
                                        sub_id: sub_id.clone(),
                                        input: input.clone(),
                                    },
                                })
                            })
                        });
                    self.clear_reserved_idle_turn(&turn_state).await;
                    if let Some((request, pending)) = pending
                        && self
                            .request_scripted_interaction(&turn_context, request, pending)
                            .await
                    {
                        return Ok(());
                    }
                    return Err(TryStartTurnIfIdleError::new(
                        TryStartTurnIfIdleRejectionReason::Busy,
                        input,
                    ));
                }
                ModelRouterScriptInteractionOutcome::Failure(failure) => {
                    tracing::warn!(failure = %failure.diagnostic(), "scripted model-router interaction failed; retaining current route");
                }
            }
        }
        self.maybe_emit_model_warnings_for_turn(turn_context.as_ref())
            .await;
        if self.input_queue.has_trigger_turn_mailbox_items().await {
            self.clear_reserved_idle_turn(&turn_state).await;
            self.maybe_start_turn_for_pending_work().await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::PendingTriggerTurn,
                input,
            ));
        }
        let still_reserved = {
            let active_turn = self.active_turn.lock().await;
            active_turn.as_ref().is_some_and(|active_turn| {
                active_turn.task.is_none() && Arc::ptr_eq(&active_turn.turn_state, &turn_state)
            })
        };
        if !still_reserved {
            self.clear_reserved_idle_turn(&turn_state).await;
            return Err(TryStartTurnIfIdleError::new(
                TryStartTurnIfIdleRejectionReason::Busy,
                input,
            ));
        }

        self.input_queue
            .extend_pending_input_for_turn_state(
                turn_state.as_ref(),
                input.into_iter().map(TurnInput::ResponseItem).collect(),
            )
            .await;
        self.start_task(turn_context, Vec::new(), RegularTask::new())
            .await;
        Ok(())
    }

    async fn clear_reserved_idle_turn(&self, turn_state: &Arc<tokio::sync::Mutex<TurnState>>) {
        let mut active_turn_guard = self.active_turn.lock().await;
        if let Some(active_turn) = active_turn_guard.as_ref()
            && active_turn.task.is_none()
            && Arc::ptr_eq(&active_turn.turn_state, turn_state)
        {
            *active_turn_guard = None;
        }
    }

    /// Injects items into active work, or records them without starting a turn.
    pub(crate) async fn inject_no_new_turn(
        &self,
        items: Vec<ResponseItem>,
        current_turn_context: Option<&TurnContext>,
    ) {
        let Err(items) = self.inject_if_running(items).await else {
            return;
        };
        let default_turn_context;
        let turn_context = match current_turn_context {
            Some(turn_context) => turn_context,
            None => {
                default_turn_context = self.new_default_turn().await;
                default_turn_context.as_ref()
            }
        };
        self.record_conversation_items(turn_context, &items).await;
    }
}
