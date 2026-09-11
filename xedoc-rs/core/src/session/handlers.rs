use async_channel::Receiver;
use tracing::Instrument;
use tracing::info_span;
use xedoc_otel::set_parent_from_w3c_trace_context;
use xedoc_protocol::protocol::Submission;

use crate::session::SteerInputError;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::session::SessionSettingsUpdate;

use crate::config::Config;
use crate::review_prompts::resolve_review_request;
use crate::session::spawn_review_thread;
use crate::tasks::CompactTask;
use crate::tasks::UserShellCommandMode;
use crate::tasks::UserShellCommandTask;
use crate::tasks::execute_user_shell_command;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::protocol::ErrorEvent;
use xedoc_protocol::protocol::Event;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::InterAgentCommunication;
use xedoc_protocol::protocol::McpServerRefreshConfig;
use xedoc_protocol::protocol::Op;
use xedoc_protocol::protocol::ReviewDecision;
use xedoc_protocol::protocol::ReviewRequest;
use xedoc_protocol::protocol::RolloutItem;
use xedoc_protocol::protocol::ThreadRolledBackEvent;
use xedoc_protocol::protocol::ThreadSettingsAppliedEvent;
use xedoc_protocol::protocol::ThreadSettingsOverrides;
use xedoc_protocol::protocol::ThreadSettingsSnapshot;
use xedoc_protocol::protocol::TurnAbortReason;
use xedoc_protocol::protocol::WarningEvent;
use xedoc_protocol::protocol::XedocErrorInfo;
use xedoc_protocol::request_permissions::RequestPermissionsResponse;
use xedoc_protocol::request_user_input::RequestUserInputResponse;

use crate::context_manager::is_user_turn_boundary;
use serde_json::Value;
use std::sync::Arc;
use tracing::debug;
use tracing::info;
use tracing::warn;
use xedoc_protocol::dynamic_tools::DynamicToolResponse;
use xedoc_protocol::mcp::RequestId as ProtocolRequestId;
use xedoc_rmcp_client::ElicitationAction;
use xedoc_rmcp_client::ElicitationResponse;

pub async fn interrupt(sess: &Arc<Session>) {
    sess.interrupt_task().await;
}

pub async fn clean_background_terminals(sess: &Arc<Session>) {
    sess.close_unified_exec_processes().await;
}

pub async fn user_input_or_turn(
    sess: &Arc<Session>,
    sub_id: String,
    op: Op,
    client_user_message_id: Option<String>,
) {
    user_input_or_turn_inner(sess, sub_id, op, client_user_message_id).await;
}

pub async fn update_thread_settings(
    sess: &Arc<Session>,
    sub_id: String,
    thread_settings: ThreadSettingsOverrides,
) {
    let updates = thread_settings_update(sess, thread_settings).await;
    match sess.update_settings(updates).await {
        Ok(()) => {
            if sess.refresh_model_context_window().await {
                sess.send_event_raw_without_materializing_rollout(Event {
                    id: sub_id.clone(),
                    msg: sess.token_count_event().await,
                })
                .await;
            }
            sess.send_event_raw_without_materializing_rollout(Event {
                id: sub_id,
                msg: thread_settings_applied_event(sess).await,
            })
            .await;
        }
        Err(err) => {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: format!("invalid thread settings override: {err}"),
                    xedoc_error_info: Some(XedocErrorInfo::BadRequest),
                }),
            })
            .await;
        }
    }
}

async fn thread_settings_update(
    sess: &Session,
    thread_settings: ThreadSettingsOverrides,
) -> SessionSettingsUpdate {
    let ThreadSettingsOverrides {
        environments,
        profile_workspace_roots,
        approval_policy,
        sandbox_policy,
        permission_profile,
        active_permission_profile,
        model,
        effort,
        summary,
        service_tier,
        collaboration_mode,
        personality,
        model_provider_id,
    } = thread_settings;
    let current_collaboration_mode = sess.collaboration_mode().await;
    let collaboration_mode = collaboration_mode.unwrap_or_else(|| {
        // Model and reasoning effort live in CollaborationMode settings today, so
        // partial thread-settings updates refresh those fields on the active mode.
        current_collaboration_mode.with_updates(model, effort, /*developer_instructions*/ None)
    });
    let selected_provider_id = match model_provider_id.as_ref() {
        Some(provider_id) => provider_id.clone(),
        None => sess.get_config().await.model_provider_id.clone(),
    };
    let base_instructions = if collaboration_mode.model() != current_collaboration_mode.model()
        || model_provider_id.is_some()
    {
        sess.base_instructions_for_model(collaboration_mode.model(), &selected_provider_id)
            .await
    } else {
        None
    };
    SessionSettingsUpdate {
        environments,
        profile_workspace_roots,
        approval_policy,
        sandbox_policy,
        permission_profile,
        active_permission_profile,
        collaboration_mode: Some(collaboration_mode),
        base_instructions,
        reasoning_summary: summary,
        service_tier,
        personality,
        model_provider_id,
        ..Default::default()
    }
}

async fn thread_settings_applied_event(sess: &Session) -> EventMsg {
    let snapshot = {
        let state = sess.state.lock().await;
        state.session_configuration.thread_config_snapshot()
    };
    let cwd = snapshot.cwd().clone();
    EventMsg::ThreadSettingsApplied(ThreadSettingsAppliedEvent {
        thread_settings: ThreadSettingsSnapshot {
            model: snapshot.model,
            model_provider_id: snapshot.model_provider_id,
            service_tier: snapshot.service_tier,
            approval_policy: snapshot.approval_policy,
            permission_profile: snapshot.permission_profile,
            active_permission_profile: snapshot.active_permission_profile,
            cwd,
            reasoning_effort: snapshot.reasoning_effort,
            reasoning_summary: snapshot.reasoning_summary,
            personality: snapshot.personality,
            collaboration_mode: snapshot.collaboration_mode,
        },
    })
}

pub(super) async fn user_input_or_turn_inner(
    sess: &Arc<Session>,
    sub_id: String,
    op: Op,
    client_user_message_id: Option<String>,
) {
    let Op::UserInput {
        items,
        final_output_json_schema,
        responsesapi_client_metadata,
        additional_context,
        thread_settings,
    } = op
    else {
        unreachable!();
    };
    let explicit_route_override = thread_settings.model.is_some()
        || thread_settings.effort.is_some()
        || thread_settings.service_tier.is_some()
        || thread_settings.collaboration_mode.is_some()
        || thread_settings.model_provider_id.is_some();
    let emit_thread_settings_applied = thread_settings != ThreadSettingsOverrides::default();
    let updates = if emit_thread_settings_applied {
        thread_settings_update(sess, thread_settings).await
    } else {
        SessionSettingsUpdate::default()
    };
    if let Err(err) = sess.update_settings(updates).await {
        sess.send_event_raw(Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: format!("invalid thread settings override: {err}"),
                xedoc_error_info: Some(XedocErrorInfo::BadRequest),
            }),
        })
        .await;
        return;
    }
    if emit_thread_settings_applied {
        sess.send_event_raw_without_materializing_rollout(Event {
            id: sub_id.clone(),
            msg: thread_settings_applied_event(sess).await,
        })
        .await;
    }
    match sess
        .steer_input(
            items.clone(),
            additional_context.clone(),
            /*expected_turn_id*/ None,
            client_user_message_id.clone(),
            responsesapi_client_metadata.clone(),
        )
        .await
    {
        Ok(_) => {
            sess.services.session_telemetry.user_prompt(&items);
        }
        Err(SteerInputError::NoActiveTurn(items)) => {
            let mut current_context = sess
                .new_turn_from_current_settings_with_sub_id(
                    sub_id.clone(),
                    final_output_json_schema.clone(),
                )
                .await;
            if !current_context.session_source.is_non_root_agent()
                && let Some(orchestrator_route) =
                    crate::model_router::current_route(current_context.config.as_ref())
            {
                sess.begin_model_router_ab_root_turn(orchestrator_route)
                    .await;
            }
            sess.maybe_emit_model_warnings_for_turn(current_context.as_ref())
                .await;
            if let Some(mut decision) = super::root_shadow_routing::decide_for_accepted_input(
                sess,
                current_context.as_ref(),
                &items,
                explicit_route_override,
            )
            .await
            {
                let approval_response = sess
                    .model_router_approval_responses
                    .lock()
                    .await
                    .remove(&sub_id);
                if let Some(response) = approval_response.as_ref() {
                    if response.action
                        == xedoc_protocol::protocol::ModelRouterApprovalAction::Override
                        && let Some(label) = response.classification.as_deref()
                    {
                        let feedback_path = sess.model_router_feedback_path().await;
                        let prompt = crate::agent::control::render_input_preview(&items);
                        if let Err(error) = xedoc_model_router::append_classifier_feedback(
                            &feedback_path,
                            &decision.prompt.sha256,
                            crate::turn_timing::now_unix_timestamp_ms() / 1_000,
                            label,
                            &prompt,
                        ) {
                            tracing::warn!(%error, "failed to persist model-router classifier feedback");
                        } else if let Err(error) =
                            crate::model_router::ModelRouterService::recalibrate_from_feedback(
                                current_context.config.as_ref(),
                                &feedback_path,
                            )
                        {
                            tracing::warn!(%error, "failed to recalibrate model-router classifier");
                        }
                    }
                    match response.action {
                        xedoc_protocol::protocol::ModelRouterApprovalAction::Approve => {}
                        xedoc_protocol::protocol::ModelRouterApprovalAction::Reject => {
                            crate::model_router::fallback_to_original_route(&mut decision);
                        }
                        xedoc_protocol::protocol::ModelRouterApprovalAction::Override => {
                            if let Some(route) = response
                                .route
                                .as_ref()
                                .and_then(crate::model_router::route_from_approval)
                            {
                                decision.effective_route = Some(route);
                                decision.disposition =
                                    xedoc_model_router::RouteDisposition::Applied;
                            }
                        }
                    }
                }
                if current_context.config.model_router.approval
                    && approval_response.is_none()
                    && decision.disposition == xedoc_model_router::RouteDisposition::Applied
                    && decision.effective_route != decision.original_route
                {
                    let approval = crate::model_router::approval_event(
                        &decision,
                        sess.thread_id.to_string(),
                        current_context.sub_id.clone(),
                        xedoc_protocol::protocol::ModelRouterScope::Root,
                    );
                    sess.pending_model_router_approvals.lock().await.insert(
                        approval.approval_id.clone(),
                        crate::session::session::PendingModelRouterApproval {
                            sub_id: sub_id.clone(),
                            op: Op::UserInput {
                                items,
                                final_output_json_schema,
                                responsesapi_client_metadata,
                                additional_context,
                                thread_settings: ThreadSettingsOverrides::default(),
                            },
                            client_user_message_id,
                        },
                    );
                    sess.send_event(
                        current_context.as_ref(),
                        EventMsg::ModelRouterApprovalRequest(approval),
                    )
                    .await;
                    return;
                }
                if decision.disposition == xedoc_model_router::RouteDisposition::Applied
                    && decision.effective_route != decision.original_route
                {
                    let routed_context = match decision.effective_route.as_ref() {
                        Some(route) => {
                            sess.new_routed_turn_from_current_settings_with_sub_id(
                                sub_id.clone(),
                                final_output_json_schema,
                                route,
                            )
                            .await
                        }
                        None => None,
                    };
                    if let Some(routed_context) = routed_context {
                        if let Some(startup_prewarm) = sess.take_session_startup_prewarm().await {
                            startup_prewarm.abort().await;
                        }
                        sess.force_full_context_replay().await;
                        current_context = routed_context;
                        sess.maybe_emit_model_warnings_for_turn(current_context.as_ref())
                            .await;
                    } else {
                        crate::model_router::fallback_to_original_route(&mut decision);
                    }
                }
                let router_event = crate::model_router::decision_event(
                    decision,
                    sess.thread_id.to_string(),
                    current_context.sub_id.clone(),
                    xedoc_protocol::protocol::ModelRouterScope::Root,
                    crate::turn_timing::now_unix_timestamp_ms() / 1_000,
                );
                sess.remember_model_router_decision(
                    &current_context.sub_id,
                    router_event.decision_id.clone(),
                )
                .await;
                sess.send_event(
                    current_context.as_ref(),
                    EventMsg::ModelRouterDecision(router_event),
                )
                .await;
            }
            if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
                current_context
                    .turn_metadata_state
                    .set_responsesapi_client_metadata(responsesapi_client_metadata);
            }
            current_context.session_telemetry.user_prompt(&items);
            sess.refresh_mcp_servers_if_requested(&current_context)
                .await;
            let additional_context_input = {
                let mut state = sess.state.lock().await;
                state.additional_context.merge(additional_context)
            };
            let mut task_input = additional_context_input
                .into_iter()
                .map(ResponseItem::from)
                .map(TurnInput::ResponseItem)
                .collect::<Vec<_>>();
            if !items.is_empty() {
                task_input.push(TurnInput::UserInput {
                    content: items,
                    client_id: client_user_message_id,
                });
            }
            sess.spawn_task(
                Arc::clone(&current_context),
                task_input,
                crate::tasks::RegularTask::new(),
            )
            .await;
        }
        Err(err) => {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::Error(err.to_error_event()),
            })
            .await;
        }
    }
}

/// Queues an inter-agent message, then lets the shared pending-work scheduler
/// decide whether an idle session should start a regular turn.
pub async fn inter_agent_communication(
    sess: &Arc<Session>,
    sub_id: String,
    communication: InterAgentCommunication,
) {
    let trigger_turn = communication.trigger_turn;
    sess.input_queue
        .enqueue_mailbox_communication(communication)
        .await;
    crate::agent_communication::emit_agent_communication_receive(&sub_id);
    if trigger_turn {
        sess.maybe_start_turn_for_pending_work_with_sub_id(sub_id)
            .await;
    }
}

pub async fn stage_inter_agent_communication(
    sess: &Arc<Session>,
    communication: InterAgentCommunication,
    barrier_id: String,
) {
    sess.input_queue
        .stage_mailbox_communication(barrier_id, communication)
        .await;
}

pub async fn release_inter_agent_communication(
    sess: &Arc<Session>,
    sub_id: String,
    barrier_id: String,
) {
    let Some(mut communication) = sess
        .input_queue
        .release_staged_mailbox_communication(&barrier_id)
        .await
    else {
        return;
    };
    communication.trigger_turn = true;
    sess.input_queue
        .enqueue_mailbox_communication(communication)
        .await;
    crate::agent_communication::emit_agent_communication_receive(&sub_id);
}

pub async fn run_user_shell_command(sess: &Arc<Session>, sub_id: String, command: String) {
    if let Some((turn_context, cancellation_token)) =
        sess.active_turn_context_and_cancellation_token().await
    {
        let session = Arc::clone(sess);
        tokio::spawn(async move {
            execute_user_shell_command(
                session,
                turn_context,
                command,
                cancellation_token,
                UserShellCommandMode::ActiveTurnAuxiliary,
            )
            .await;
        });
        return;
    }

    let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
    sess.spawn_task(
        Arc::clone(&turn_context),
        Vec::new(),
        UserShellCommandTask::new(command),
    )
    .await;
}

pub async fn resolve_elicitation(
    sess: &Arc<Session>,
    server_name: String,
    request_id: ProtocolRequestId,
    decision: xedoc_protocol::approvals::ElicitationAction,
    content: Option<Value>,
    meta: Option<Value>,
) {
    let action = match decision {
        xedoc_protocol::approvals::ElicitationAction::Accept => ElicitationAction::Accept,
        xedoc_protocol::approvals::ElicitationAction::Decline => ElicitationAction::Decline,
        xedoc_protocol::approvals::ElicitationAction::Cancel => ElicitationAction::Cancel,
    };
    let content = match action {
        // Preserve the legacy fallback for clients that only send an action.
        ElicitationAction::Accept => Some(content.unwrap_or_else(|| serde_json::json!({}))),
        ElicitationAction::Decline | ElicitationAction::Cancel => None,
    };
    let response = ElicitationResponse {
        action,
        content,
        meta,
    };
    let request_id = match request_id {
        ProtocolRequestId::String(value) => {
            rmcp::model::NumberOrString::String(std::sync::Arc::from(value))
        }
        ProtocolRequestId::Integer(value) => rmcp::model::NumberOrString::Number(value),
    };
    if let Err(err) = sess
        .resolve_elicitation(server_name, request_id, response)
        .await
    {
        warn!(
            error = %err,
            "failed to resolve elicitation request in session"
        );
    }
}

/// Propagate a user's exec approval decision to the session.
/// Also optionally applies an execpolicy amendment.
pub async fn exec_approval(
    sess: &Arc<Session>,
    approval_id: String,
    turn_id: Option<String>,
    decision: ReviewDecision,
) {
    let event_turn_id = turn_id.unwrap_or_else(|| approval_id.clone());
    if let ReviewDecision::ApprovedExecpolicyAmendment {
        proposed_execpolicy_amendment,
    } = &decision
    {
        match sess
            .persist_execpolicy_amendment(proposed_execpolicy_amendment)
            .await
        {
            Ok(()) => {
                sess.record_execpolicy_amendment_message(
                    &event_turn_id,
                    proposed_execpolicy_amendment,
                )
                .await;
            }
            Err(err) => {
                let message = format!("Failed to apply execpolicy amendment: {err}");
                tracing::warn!("{message}");
                let warning = EventMsg::Warning(WarningEvent { message });
                sess.send_event_raw(Event {
                    id: event_turn_id.clone(),
                    msg: warning,
                })
                .await;
            }
        }
    }
    match decision {
        ReviewDecision::Abort => {
            sess.interrupt_task().await;
        }
        other => sess.notify_approval(&approval_id, other).await,
    }
}

pub async fn patch_approval(sess: &Arc<Session>, id: String, decision: ReviewDecision) {
    match decision {
        ReviewDecision::Abort => {
            sess.interrupt_task().await;
        }
        other => sess.notify_approval(&id, other).await,
    }
}

pub async fn request_user_input_response(
    sess: &Arc<Session>,
    id: String,
    response: RequestUserInputResponse,
) {
    sess.notify_user_input_response(&id, response).await;
}

pub async fn model_router_approval_response(
    sess: &Arc<Session>,
    approval_id: String,
    response: xedoc_protocol::protocol::ModelRouterApprovalResponse,
) {
    if let Some(sender) = sess
        .pending_model_router_tool_approvals
        .lock()
        .await
        .remove(&approval_id)
    {
        let _ = sender.send(response);
        return;
    }
    let pending = sess
        .pending_model_router_approvals
        .lock()
        .await
        .remove(&approval_id);
    if let Some(pending) = pending {
        sess.model_router_approval_responses
            .lock()
            .await
            .insert(pending.sub_id.clone(), response);
        user_input_or_turn(
            sess,
            pending.sub_id,
            pending.op,
            pending.client_user_message_id,
        )
        .await;
    }
}

pub async fn request_permissions_response(
    sess: &Arc<Session>,
    id: String,
    response: RequestPermissionsResponse,
) {
    sess.notify_request_permissions_response(&id, response)
        .await;
}

pub async fn dynamic_tool_response(sess: &Arc<Session>, id: String, response: DynamicToolResponse) {
    sess.notify_dynamic_tool_response(&id, response).await;
}

pub async fn refresh_mcp_servers(sess: &Arc<Session>, refresh_config: McpServerRefreshConfig) {
    let mut guard = sess.pending_mcp_server_refresh_config.lock().await;
    *guard = Some(refresh_config);
}

pub async fn reload_user_config(sess: &Arc<Session>) {
    sess.reload_user_config_layer().await;
}

pub async fn compact(sess: &Arc<Session>, sub_id: String) {
    let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;

    sess.spawn_task(Arc::clone(&turn_context), Vec::new(), CompactTask)
        .await;
}

pub async fn thread_rollback(sess: &Arc<Session>, sub_id: String, num_turns: u32) {
    if num_turns == 0 {
        sess.send_event_raw(Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: "num_turns must be >= 1".to_string(),
                xedoc_error_info: Some(XedocErrorInfo::ThreadRollbackFailed),
            }),
        })
        .await;
        return;
    }

    let has_active_turn = { sess.active_turn.lock().await.is_some() };
    if has_active_turn {
        sess.send_event_raw(Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: "Cannot rollback while a turn is in progress.".to_string(),
                xedoc_error_info: Some(XedocErrorInfo::ThreadRollbackFailed),
            }),
        })
        .await;
        return;
    }

    let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
    let live_thread = match sess.live_thread_for_persistence("rollback thread") {
        Ok(live_thread) => live_thread,
        Err(_) => {
            sess.send_event_raw(Event {
                id: turn_context.sub_id.clone(),
                msg: EventMsg::Error(ErrorEvent {
                    message: "thread rollback requires persisted thread history".to_string(),
                    xedoc_error_info: Some(XedocErrorInfo::ThreadRollbackFailed),
                }),
            })
            .await;
            return;
        }
    };
    if let Err(err) = live_thread.flush().await {
        sess.send_event_raw(Event {
            id: turn_context.sub_id.clone(),
            msg: EventMsg::Error(ErrorEvent {
                message: format!("failed to flush thread persistence for rollback replay: {err}"),
                xedoc_error_info: Some(XedocErrorInfo::ThreadRollbackFailed),
            }),
        })
        .await;
        return;
    }

    let stored_history = match live_thread.load_history(/*include_archived*/ false).await {
        Ok(history) => history,
        Err(err) => {
            sess.send_event_raw(Event {
                id: turn_context.sub_id.clone(),
                msg: EventMsg::Error(ErrorEvent {
                    message: format!("failed to load thread history for rollback replay: {err}"),
                    xedoc_error_info: Some(XedocErrorInfo::ThreadRollbackFailed),
                }),
            })
            .await;
            return;
        }
    };

    let rollback_event = ThreadRolledBackEvent { num_turns };
    let rollback_msg = EventMsg::ThreadRolledBack(rollback_event.clone());
    let replay_items = stored_history
        .items
        .into_iter()
        .chain(std::iter::once(RolloutItem::EventMsg(rollback_msg.clone())))
        .collect::<Vec<_>>();
    sess.apply_rollout_reconstruction(turn_context.as_ref(), replay_items.as_slice())
        .await;
    sess.services
        .agent_control
        .rollout_budget()
        .rearm_reminder(sess.thread_id());
    sess.recompute_token_usage(turn_context.as_ref()).await;

    sess.persist_rollout_items(&[RolloutItem::EventMsg(rollback_msg.clone())])
        .await;
    if let Err(err) = sess.flush_rollout().await {
        sess.send_event(
            turn_context.as_ref(),
            EventMsg::Warning(WarningEvent {
                message: format!(
                    "Rolled the thread back, but failed to save the rollback marker. Xedoc will continue retrying. Error: {err}"
                ),
            }),
        )
        .await;
    }

    sess.deliver_event_raw(Event {
        id: turn_context.sub_id.clone(),
        msg: rollback_msg,
    })
    .await;
}

async fn shutdown_session_runtime(sess: &Arc<Session>) {
    if let Some(startup_prewarm) = sess.take_session_startup_prewarm().await {
        startup_prewarm.abort().await;
    }
    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
    sess.services
        .unified_exec_manager
        .terminate_all_processes()
        .await;
    sess.services.mcp_runtime.shutdown().await;

    crate::hook_runtime::run_session_end_hooks(sess).await;
}

async fn emit_thread_stop_lifecycle(sess: &Session) {
    for contributor in sess.services.extensions.thread_lifecycle_contributors() {
        contributor
            .on_thread_stop(xedoc_extension_api::ThreadStopInput {
                session_store: &sess.services.session_extension_data,
                thread_store: &sess.services.thread_extension_data,
            })
            .await;
    }
}

pub async fn shutdown(sess: &Arc<Session>, sub_id: String) -> bool {
    shutdown_session_runtime(sess).await;
    info!("Shutting down Xedoc instance");
    let history = sess.clone_history().await;
    let turn_count = history
        .raw_items()
        .iter()
        .filter(|item| is_user_turn_boundary(item))
        .count();
    sess.services.session_telemetry.counter(
        "xedoc.conversation.turn.count",
        i64::try_from(turn_count).unwrap_or(0),
        &[],
    );

    emit_thread_stop_lifecycle(sess.as_ref()).await;

    // Gracefully flush and shutdown thread persistence on session end so tests
    // that inspect durable state do not race with the background writer.
    if let Some(live_thread) = sess.live_thread()
        && let Err(e) = live_thread.shutdown().await
    {
        warn!("failed to shutdown thread persistence: {e}");
        let event = Event {
            id: sub_id.clone(),
            msg: EventMsg::Error(ErrorEvent {
                message: "Failed to shutdown thread persistence".to_string(),
                xedoc_error_info: Some(XedocErrorInfo::Other),
            }),
        };
        sess.send_event_raw(event).await;
    }

    let event = Event {
        id: sub_id,
        msg: EventMsg::ShutdownComplete,
    };
    sess.deliver_event_raw(event).await;
    true
}

pub async fn review(
    sess: &Arc<Session>,
    config: &Arc<Config>,
    sub_id: String,
    review_request: ReviewRequest,
) {
    let turn_context = sess.new_default_turn_with_sub_id(sub_id.clone()).await;
    sess.maybe_emit_model_warnings_for_turn(turn_context.as_ref())
        .await;
    sess.refresh_mcp_servers_if_requested(&turn_context).await;
    #[allow(deprecated)]
    match resolve_review_request(review_request, &turn_context.cwd) {
        Ok(resolved) => {
            spawn_review_thread(
                Arc::clone(sess),
                Arc::clone(config),
                turn_context.clone(),
                sub_id,
                resolved,
            )
            .await;
        }
        Err(err) => {
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: err.to_string(),
                    xedoc_error_info: Some(XedocErrorInfo::Other),
                }),
            };
            sess.send_event(&turn_context, event.msg).await;
        }
    }
}

pub(super) async fn submission_loop(
    sess: Arc<Session>,
    config: Arc<Config>,
    rx_sub: Receiver<Submission>,
) {
    // To break out of this loop, send Op::Shutdown.
    let mut shutdown_received = false;
    while let Ok(sub) = rx_sub.recv().await {
        debug!(?sub, "Submission");
        let dispatch_span = submission_dispatch_span(&sub);
        let should_exit = async {
            match sub.op.clone() {
                Op::Interrupt => {
                    interrupt(&sess).await;
                    false
                }
                Op::CleanBackgroundTerminals => {
                    clean_background_terminals(&sess).await;
                    false
                }
                Op::UserInput { .. } => {
                    user_input_or_turn(&sess, sub.id.clone(), sub.op, sub.client_user_message_id)
                        .await;
                    false
                }
                Op::ThreadSettings { thread_settings } => {
                    update_thread_settings(&sess, sub.id.clone(), thread_settings).await;
                    false
                }
                Op::InterAgentCommunication { communication } => {
                    inter_agent_communication(&sess, sub.id.clone(), communication).await;
                    false
                }
                Op::StageInterAgentCommunication {
                    communication,
                    barrier_id,
                } => {
                    stage_inter_agent_communication(&sess, communication, barrier_id).await;
                    false
                }
                Op::ReleaseInterAgentCommunication { barrier_id } => {
                    release_inter_agent_communication(&sess, sub.id.clone(), barrier_id).await;
                    false
                }
                Op::ExecApproval {
                    id: approval_id,
                    turn_id,
                    decision,
                } => {
                    exec_approval(&sess, approval_id, turn_id, decision).await;
                    false
                }
                Op::PatchApproval { id, decision } => {
                    patch_approval(&sess, id, decision).await;
                    false
                }
                Op::UserInputAnswer { id, response } => {
                    request_user_input_response(&sess, id, response).await;
                    false
                }
                Op::ModelRouterApprovalResponse {
                    approval_id,
                    response,
                } => {
                    model_router_approval_response(&sess, approval_id, response).await;
                    false
                }
                Op::RequestPermissionsResponse { id, response } => {
                    request_permissions_response(&sess, id, response).await;
                    false
                }
                Op::DynamicToolResponse { id, response } => {
                    dynamic_tool_response(&sess, id, response).await;
                    false
                }
                Op::RefreshMcpServers { config } => {
                    refresh_mcp_servers(&sess, config).await;
                    false
                }
                Op::ReloadUserConfig => {
                    reload_user_config(&sess).await;
                    false
                }
                Op::Compact => {
                    compact(&sess, sub.id.clone()).await;
                    false
                }
                Op::ThreadRollback { num_turns } => {
                    thread_rollback(&sess, sub.id.clone(), num_turns).await;
                    false
                }
                Op::RunUserShellCommand { command } => {
                    run_user_shell_command(&sess, sub.id.clone(), command).await;
                    false
                }
                Op::ResolveElicitation {
                    server_name,
                    request_id,
                    decision,
                    content,
                    meta,
                } => {
                    resolve_elicitation(&sess, server_name, request_id, decision, content, meta)
                        .await;
                    false
                }
                Op::Shutdown => shutdown(&sess, sub.id.clone()).await,
                Op::Review { review_request } => {
                    review(&sess, &config, sub.id.clone(), review_request).await;
                    false
                }
                _ => false, // Ignore unknown ops; enum is non_exhaustive to allow extensions.
            }
        }
        .instrument(dispatch_span)
        .await;
        if should_exit {
            shutdown_received = true;
            break;
        }
    }
    // If the submission loop exits because the channel closed without an
    // explicit shutdown op, still run session teardown.
    if !shutdown_received {
        shutdown_session_runtime(&sess).await;
        emit_thread_stop_lifecycle(sess.as_ref()).await;
        if let Some(live_thread) = sess.live_thread()
            && let Err(err) = live_thread.shutdown().await
        {
            warn!("failed to shutdown thread persistence after submission channel closed: {err}");
        }
    }
    debug!("Agent loop exited");
}

pub(super) fn submission_dispatch_span(sub: &Submission) -> tracing::Span {
    let op_name = sub.op.kind();
    let span_name = format!("op.dispatch.{op_name}");
    let dispatch_span = info_span!(
        "submission_dispatch",
        otel.name = span_name.as_str(),
        submission.id = sub.id.as_str(),
        xedoc.op = op_name
    );
    if let Some(trace) = sub.trace.as_ref()
        && !set_parent_from_w3c_trace_context(&dispatch_span, trace)
    {
        warn!(
            submission.id = sub.id.as_str(),
            "ignoring invalid submission trace carrier"
        );
    }
    dispatch_span
}
