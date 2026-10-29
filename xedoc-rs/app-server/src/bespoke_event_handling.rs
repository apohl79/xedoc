use crate::auto_session_name::AutoSessionNameUpdate;
use crate::auto_session_name::maybe_spawn_auto_session_name_update;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use crate::outgoing_message::ClientRequestResult;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::parallel_approval;
use crate::request_processors::populate_thread_turns_from_history;
use crate::request_processors::thread_from_stored_thread;
use crate::request_processors::thread_settings_from_core_snapshot;
use crate::server_request_error::is_turn_transition_server_request_error;
use crate::session_extension_manager::SessionExtensionManager;
use crate::session_script_registry::RequestUserInputResponseMode;
use crate::session_script_registry::SessionScriptRegistry;
use crate::thread_state::ThreadState;
use crate::thread_state::TurnSummary;
use crate::thread_state::resolve_server_request_on_thread_listener;
use crate::thread_status::ThreadWatchActiveGuard;
use crate::thread_status::ThreadWatchManager;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tracing::error;
use xedoc_app_server_protocol::AccountRateLimitsUpdatedNotification;
use xedoc_app_server_protocol::AdditionalPermissionProfile as V2AdditionalPermissionProfile;
use xedoc_app_server_protocol::CommandAction as V2ParsedCommand;
use xedoc_app_server_protocol::CommandExecutionApprovalDecision;
use xedoc_app_server_protocol::CommandExecutionRequestApprovalParams;
use xedoc_app_server_protocol::CommandExecutionRequestApprovalResponse;
use xedoc_app_server_protocol::CommandExecutionSource;
use xedoc_app_server_protocol::CommandExecutionStatus;
use xedoc_app_server_protocol::CompactionProgressNotification;
use xedoc_app_server_protocol::DeprecationNoticeNotification;
use xedoc_app_server_protocol::DynamicToolCallParams;
use xedoc_app_server_protocol::EnvironmentConnectionNotification;
use xedoc_app_server_protocol::ErrorNotification;
use xedoc_app_server_protocol::ExecPolicyAmendment as V2ExecPolicyAmendment;
use xedoc_app_server_protocol::ExtensionInteractionOutcome;
use xedoc_app_server_protocol::ExtensionInteractionRequestParams;
use xedoc_app_server_protocol::ExtensionInteractionRequestResponse;
use xedoc_app_server_protocol::FileChangeApprovalDecision;
use xedoc_app_server_protocol::FileChangeRequestApprovalParams;
use xedoc_app_server_protocol::FileChangeRequestApprovalResponse;
use xedoc_app_server_protocol::GrantedPermissionProfile as V2GrantedPermissionProfile;
use xedoc_app_server_protocol::HookCompletedNotification;
use xedoc_app_server_protocol::HookStartedNotification;
use xedoc_app_server_protocol::ItemCompletedNotification;
use xedoc_app_server_protocol::ItemStartedNotification;
use xedoc_app_server_protocol::McpServerElicitationAction;
use xedoc_app_server_protocol::McpServerElicitationRequestParams;
use xedoc_app_server_protocol::McpServerElicitationRequestResponse;
use xedoc_app_server_protocol::McpServerStartupState;
use xedoc_app_server_protocol::McpServerStatusUpdatedNotification;
use xedoc_app_server_protocol::ModelReroutedNotification;
use xedoc_app_server_protocol::ModelRouterActivityNotification;
use xedoc_app_server_protocol::ModelRouterDecisionNotification;
use xedoc_app_server_protocol::ModelSafetyBufferingUpdatedNotification;
use xedoc_app_server_protocol::ModelVerificationNotification;
use xedoc_app_server_protocol::NetworkApprovalContext as V2NetworkApprovalContext;
use xedoc_app_server_protocol::NetworkPolicyAmendment as V2NetworkPolicyAmendment;
use xedoc_app_server_protocol::NetworkPolicyRuleAction as V2NetworkPolicyRuleAction;
use xedoc_app_server_protocol::PermissionsRequestApprovalParams;
use xedoc_app_server_protocol::PermissionsRequestApprovalResponse;
use xedoc_app_server_protocol::RawResponseCompletedNotification;
use xedoc_app_server_protocol::RawResponseItemCompletedNotification;
use xedoc_app_server_protocol::RequestId;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ServerRequestPayload;
use xedoc_app_server_protocol::SessionScriptPromptClosedReason;
use xedoc_app_server_protocol::SessionScriptPromptKind;
use xedoc_app_server_protocol::SessionScriptPromptRequest;
use xedoc_app_server_protocol::SessionScriptSession;
use xedoc_app_server_protocol::ThreadGoalUpdatedNotification;
use xedoc_app_server_protocol::ThreadItem;
use xedoc_app_server_protocol::ThreadRollbackResponse;
use xedoc_app_server_protocol::ThreadSettingsUpdatedNotification;
use xedoc_app_server_protocol::ThreadStatus;
use xedoc_app_server_protocol::ThreadTokenUsage;
use xedoc_app_server_protocol::ThreadTokenUsageUpdatedNotification;
use xedoc_app_server_protocol::ToolRequestUserInputOption;
use xedoc_app_server_protocol::ToolRequestUserInputParams;
use xedoc_app_server_protocol::ToolRequestUserInputQuestion;
use xedoc_app_server_protocol::ToolRequestUserInputResponse;
use xedoc_app_server_protocol::Turn;
use xedoc_app_server_protocol::TurnCompletedNotification;
use xedoc_app_server_protocol::TurnDiffUpdatedNotification;
use xedoc_app_server_protocol::TurnError;
use xedoc_app_server_protocol::TurnInterruptResponse;
use xedoc_app_server_protocol::TurnItemsView;
use xedoc_app_server_protocol::TurnModerationMetadataNotification;
use xedoc_app_server_protocol::TurnPlanStep;
use xedoc_app_server_protocol::TurnPlanUpdatedNotification;
use xedoc_app_server_protocol::TurnStartedNotification;
use xedoc_app_server_protocol::TurnStatus;
use xedoc_app_server_protocol::WarningNotification;
use xedoc_app_server_protocol::XedocErrorInfo as V2XedocErrorInfo;
use xedoc_app_server_protocol::item_event_to_server_notification;
use xedoc_core::ThreadManager;
use xedoc_core::XedocThread;
use xedoc_protocol::ThreadId;
use xedoc_protocol::items::CollabAgentTool as CoreCollabAgentTool;
use xedoc_protocol::items::TurnItem as CoreTurnItem;
use xedoc_protocol::models::AdditionalPermissionProfile as CoreAdditionalPermissionProfile;
use xedoc_protocol::plan_tool::UpdatePlanArgs;
use xedoc_protocol::protocol::COMPACTION_PROGRESS_PREFIX;
use xedoc_protocol::protocol::Event;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::ExecApprovalRequestEvent;
use xedoc_protocol::protocol::Op;
use xedoc_protocol::protocol::ReviewDecision;
use xedoc_protocol::protocol::ScriptedInteractionOutcome as CoreScriptedInteractionOutcome;
use xedoc_protocol::protocol::ScriptedInteractionResponse as CoreScriptedInteractionResponse;
use xedoc_protocol::protocol::SubAgentActivityKind;
use xedoc_protocol::protocol::TokenCountEvent;
use xedoc_protocol::protocol::TurnAbortedEvent;
use xedoc_protocol::protocol::TurnCompleteEvent;
use xedoc_protocol::protocol::TurnDiffEvent;
use xedoc_protocol::protocol::XedocErrorInfo as CoreXedocErrorInfo;
use xedoc_protocol::request_permissions::PermissionGrantScope as CorePermissionGrantScope;
use xedoc_protocol::request_permissions::RequestPermissionProfile as CoreRequestPermissionProfile;
use xedoc_protocol::request_permissions::RequestPermissionsResponse as CoreRequestPermissionsResponse;
use xedoc_protocol::request_user_input::RequestUserInputAnswer as CoreRequestUserInputAnswer;
use xedoc_protocol::request_user_input::RequestUserInputResponse as CoreRequestUserInputResponse;
use xedoc_sandboxing::policy_transforms::intersect_permission_profiles;
use xedoc_shell_command::parse_command::shlex_join;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_path_uri::LegacyAppPathString;

enum CommandExecutionApprovalPresentation {
    Network(V2NetworkApprovalContext),
    Command(CommandExecutionCompletionItem),
}

#[derive(Debug, PartialEq)]
struct CommandExecutionCompletionItem {
    command: String,
    cwd: LegacyAppPathString,
    command_actions: Vec<V2ParsedCommand>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_bespoke_event_handling(
    event: Event,
    conversation_id: ThreadId,
    conversation: Arc<XedocThread>,
    thread_manager: Arc<ThreadManager>,
    outgoing: ThreadScopedOutgoingMessageSender,
    session_script_registry: SessionScriptRegistry,
    session_extension_manager: Option<SessionExtensionManager>,
    session_script_outgoing: Arc<crate::outgoing_message::OutgoingMessageSender>,
    thread_state: Arc<tokio::sync::Mutex<ThreadState>>,
    thread_watch_manager: ThreadWatchManager,
    thread_list_state_permit: Arc<tokio::sync::Semaphore>,
    fallback_model_provider: String,
) {
    let Event {
        id: event_turn_id,
        msg,
    } = event;
    match msg {
        EventMsg::TurnStarted(payload) => {
            // While not technically necessary as it was already done on TurnComplete, be extra cautios and abort any pending server requests.
            outgoing.abort_pending_server_requests().await;
            thread_watch_manager
                .note_turn_started(&conversation_id.to_string())
                .await;
            let turn = {
                let state = thread_state.lock().await;
                let mut turn = state.active_turn_snapshot().unwrap_or_else(|| Turn {
                    id: payload.turn_id.clone(),
                    items: Vec::new(),
                    items_view: TurnItemsView::NotLoaded,
                    error: None,
                    status: TurnStatus::InProgress,
                    started_at: payload.started_at,
                    completed_at: None,
                    duration_ms: None,
                });
                turn.items.clear();
                turn.items_view = TurnItemsView::NotLoaded;
                turn
            };
            let notification = TurnStartedNotification {
                thread_id: conversation_id.to_string(),
                turn,
            };
            outgoing
                .send_server_notification(ServerNotification::TurnStarted(notification.clone()))
                .await;
            session_script_registry
                .publish_turn_started(
                    &session_script_outgoing,
                    conversation_id,
                    ServerNotification::TurnStarted(notification),
                )
                .await;
        }
        EventMsg::TurnComplete(turn_complete_event) => {
            // All per-thread requests are bound to a turn, so abort them.
            outgoing.abort_pending_server_requests().await;
            session_script_registry
                .close_prompts_for_turn(
                    &session_script_outgoing,
                    conversation_id,
                    &event_turn_id,
                    SessionScriptPromptClosedReason::TurnEnded,
                )
                .await;
            respond_to_pending_interrupts(&thread_state, &outgoing).await;
            let turn_failed = thread_state.lock().await.turn_summary.last_error.is_some();
            thread_watch_manager
                .note_turn_completed(&conversation_id.to_string(), turn_failed)
                .await;
            let (completed_turn_count, refresh_after_mid_turn_initial_name) = {
                let state = thread_state.lock().await;
                (
                    state.completed_turn_count,
                    state.completed_turn_had_mid_turn_auto_session_name(&event_turn_id),
                )
            };
            let notification = handle_turn_complete(
                conversation_id,
                event_turn_id,
                turn_complete_event,
                &outgoing,
                &thread_state,
            )
            .await;
            session_script_registry
                .publish_turn_completed(
                    &session_script_outgoing,
                    conversation_id,
                    ServerNotification::TurnCompleted(notification),
                )
                .await;
            if !turn_failed {
                maybe_spawn_auto_session_name_update(
                    conversation_id,
                    conversation,
                    thread_manager,
                    outgoing,
                    session_script_registry,
                    session_script_outgoing,
                    thread_state,
                    thread_list_state_permit,
                    AutoSessionNameUpdate::turn_completed(
                        completed_turn_count,
                        refresh_after_mid_turn_initial_name,
                    ),
                );
            }
        }
        EventMsg::McpStartupUpdate(update) => {
            let (status, error, failure_reason) = match update.status {
                xedoc_protocol::protocol::McpStartupStatus::Starting => {
                    (McpServerStartupState::Starting, None, None)
                }
                xedoc_protocol::protocol::McpStartupStatus::Ready => {
                    (McpServerStartupState::Ready, None, None)
                }
                xedoc_protocol::protocol::McpStartupStatus::Failed { error, reason } => (
                    McpServerStartupState::Failed,
                    Some(error),
                    reason.map(Into::into),
                ),
                xedoc_protocol::protocol::McpStartupStatus::Cancelled => {
                    (McpServerStartupState::Cancelled, None, None)
                }
            };
            let notification = McpServerStatusUpdatedNotification {
                thread_id: Some(conversation_id.to_string()),
                name: update.server,
                status,
                error,
                failure_reason,
            };
            outgoing
                .send_server_notification(ServerNotification::McpServerStatusUpdated(notification))
                .await;
        }
        EventMsg::EnvironmentConnected(event) => {
            outgoing
                .send_server_notification(ServerNotification::EnvironmentConnected(
                    EnvironmentConnectionNotification {
                        thread_id: conversation_id.to_string(),
                        environment_id: event.environment_id,
                    },
                ))
                .await;
        }
        EventMsg::EnvironmentDisconnected(event) => {
            outgoing
                .send_server_notification(ServerNotification::EnvironmentDisconnected(
                    EnvironmentConnectionNotification {
                        thread_id: conversation_id.to_string(),
                        environment_id: event.environment_id,
                    },
                ))
                .await;
        }
        EventMsg::Warning(warning_event) => {
            if let Some(stage) = warning_event
                .message
                .strip_prefix(COMPACTION_PROGRESS_PREFIX)
            {
                outgoing
                    .send_server_notification(ServerNotification::CompactionProgress(
                        CompactionProgressNotification {
                            thread_id: conversation_id.to_string(),
                            stage: stage.trim().to_string(),
                        },
                    ))
                    .await;
            } else {
                let notification = WarningNotification {
                    thread_id: Some(conversation_id.to_string()),
                    message: warning_event.message,
                };
                outgoing
                    .send_server_notification(ServerNotification::Warning(notification))
                    .await;
            }
        }
        EventMsg::ModelReroute(event) => {
            let notification = ModelReroutedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                from_model: event.from_model,
                to_model: event.to_model,
                reason: event.reason.into(),
            };
            outgoing
                .send_server_notification(ServerNotification::ModelRerouted(notification))
                .await;
        }
        EventMsg::ModelRouterActivity(event) => {
            let notification = ModelRouterActivityNotification {
                thread_id: event.thread_id,
                turn_id: event.turn_id,
                scope: event.scope.into(),
                state: event.state.into(),
            };
            outgoing
                .send_server_notification(ServerNotification::ModelRouterActivity(notification))
                .await;
        }
        EventMsg::ModelRouterDecision(event) => {
            let notification = ModelRouterDecisionNotification {
                decision_id: event.decision_id,
                thread_id: event.thread_id,
                turn_id: event.turn_id,
                scope: event.scope.into(),
                disposition: event.disposition.into(),
                reason: event.reason.into(),
                feedback_visible: event.feedback_visible,
                diagnostic: event.diagnostic,
                summary: event.summary,
                policy_revision: event.policy_revision,
                classifications: event.classifications,
                confidence_score: event.confidence_score.into(),
                confidence_margin: event.confidence_margin.into(),
                ranking_score: event.ranking_score,
                ranking_minimum_class: event.ranking_minimum_class,
                ranking_maximum_class: event.ranking_maximum_class,
                ranking_minimum_rank: event.ranking_minimum_rank,
                ranking_maximum_rank: event.ranking_maximum_rank,
                ranking_target_rank: event.ranking_target_rank,
                ranking_selected_rank: event.ranking_selected_rank,
                proposed_provider_id: event.proposed_provider_id,
                proposed_model_slug: event.proposed_model_slug,
                proposed_reasoning_effort: event.proposed_reasoning_effort,
                effective_route: event.effective_route.into(),
                prompt_sha256: event.prompt_sha256,
                prompt_original_bytes: event.prompt_original_bytes,
                prompt_truncated: event.prompt_truncated,
                created_at: event.created_at,
            };
            outgoing
                .send_server_notification(ServerNotification::ModelRouterDecision(notification))
                .await;
        }
        EventMsg::ScriptedInteractionRequest(event) => {
            let request_id = event.request_id.clone();
            let extension_id = event.extension_id.clone();
            let interaction_id = event.interaction_id.clone();
            let continuation = event.continuation.clone();
            let state_revision = event.state_revision.clone();
            let surface = match serde_json::from_value(event.surface) {
                Ok(surface) => surface,
                Err(error) => {
                    error!("failed to deserialize scripted interaction surface: {error}");
                    let response = CoreScriptedInteractionResponse {
                        extension_id,
                        interaction_id,
                        continuation,
                        state_revision,
                        outcome: CoreScriptedInteractionOutcome::Cancelled,
                        action: None,
                        values: serde_json::Value::Null,
                    };
                    if let Err(error) = conversation
                        .submit(Op::ScriptedInteractionResponse {
                            request_id,
                            response,
                        })
                        .await
                    {
                        error!("failed to submit ScriptedInteractionResponse: {error}");
                    }
                    return;
                }
            };
            let params = ExtensionInteractionRequestParams {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                request_id: request_id.clone(),
                extension_id: event.extension_id,
                interaction_id: event.interaction_id,
                continuation: event.continuation,
                state_revision: event.state_revision,
                expires_at: event.expires_at,
                surface,
            };
            let script_prompt = session_script_registry
                .open_approval_prompt(
                    &session_script_outgoing,
                    conversation_id,
                    SessionScriptPromptKind::ExtensionInteraction,
                    Some(event_turn_id.clone()),
                    Some(request_id.clone()),
                    SessionScriptPromptRequest {
                        method: "item/extensionInteraction/request".to_string(),
                        params: serde_json::to_value(&params).unwrap_or(serde_json::Value::Null),
                    },
                )
                .await;
            let script_registry = session_script_registry.clone();
            let script_outgoing = session_script_outgoing.clone();
            tokio::spawn(async move {
                let script_prompt_id = script_prompt.prompt_id.clone();
                let (pending_request_id, receiver) = parallel_approval::await_response(
                    script_prompt,
                    ServerRequestPayload::ExtensionInteractionRequest(params),
                    outgoing,
                )
                .await;
                on_extension_interaction_response(
                    request_id,
                    extension_id,
                    interaction_id,
                    continuation,
                    state_revision,
                    pending_request_id,
                    receiver,
                    conversation,
                    thread_state,
                )
                .await;
                script_registry
                    .close_prompt(
                        &script_outgoing,
                        &script_prompt_id,
                        SessionScriptPromptClosedReason::Answered,
                    )
                    .await;
            });
        }
        EventMsg::ModelVerification(event) => {
            let notification = ModelVerificationNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                verifications: event.verifications.into_iter().map(Into::into).collect(),
            };
            outgoing
                .send_server_notification(ServerNotification::ModelVerification(notification))
                .await;
        }
        EventMsg::TurnModerationMetadata(event) => {
            let notification = TurnModerationMetadataNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                metadata: event.metadata,
            };
            outgoing
                .send_server_notification(ServerNotification::TurnModerationMetadata(notification))
                .await;
        }
        EventMsg::SafetyBuffering(event) => {
            let notification = ModelSafetyBufferingUpdatedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                model: event.model,
                use_cases: event.use_cases,
                reasons: event.reasons,
                show_buffering_ui: event.show_buffering_ui,
                faster_model: event.faster_model,
            };
            outgoing
                .send_server_notification(ServerNotification::ModelSafetyBufferingUpdated(
                    notification,
                ))
                .await;
        }
        EventMsg::ApplyPatchApprovalRequest(event) => {
            let permission_guard = thread_watch_manager
                .note_permission_requested(&conversation_id.to_string())
                .await;
            let item_id = event.call_id.clone();

            let params = FileChangeRequestApprovalParams {
                thread_id: conversation_id.to_string(),
                turn_id: event.turn_id.clone(),
                item_id: item_id.clone(),
                started_at_ms: event.started_at_ms,
                reason: event.reason.clone(),
                grant_root: event.grant_root.clone(),
            };
            let script_prompt = session_script_registry
                .open_approval_prompt(
                    &session_script_outgoing,
                    conversation_id,
                    SessionScriptPromptKind::FileChangeApproval,
                    Some(event.turn_id),
                    Some(item_id.clone()),
                    SessionScriptPromptRequest {
                        method: "item/fileChange/requestApproval".to_string(),
                        params: serde_json::to_value(&params).unwrap_or(serde_json::Value::Null),
                    },
                )
                .await;
            let script_registry = session_script_registry.clone();
            let script_outgoing = session_script_outgoing.clone();
            tokio::spawn(async move {
                let script_prompt_id = script_prompt.prompt_id.clone();
                let (pending_request_id, rx) = parallel_approval::await_response(
                    script_prompt,
                    ServerRequestPayload::FileChangeRequestApproval(params),
                    outgoing,
                )
                .await;
                on_file_change_request_approval_response(
                    item_id,
                    pending_request_id,
                    rx,
                    conversation,
                    thread_state.clone(),
                    permission_guard,
                )
                .await;
                script_registry
                    .close_prompt(
                        &script_outgoing,
                        &script_prompt_id,
                        SessionScriptPromptClosedReason::Answered,
                    )
                    .await;
            });
        }
        EventMsg::ExecApprovalRequest(ev) => {
            let permission_guard = thread_watch_manager
                .note_permission_requested(&conversation_id.to_string())
                .await;
            let available_decisions = ev
                .effective_available_decisions()
                .into_iter()
                .map(CommandExecutionApprovalDecision::from)
                .collect::<Vec<_>>();
            let ExecApprovalRequestEvent {
                call_id,
                approval_id,
                turn_id,
                environment_id,
                started_at_ms,
                command,
                cwd,
                reason,
                network_approval_context,
                proposed_execpolicy_amendment,
                proposed_network_policy_amendments,
                additional_permissions,
                parsed_cmd,
                ..
            } = ev;
            let command_actions = parsed_cmd
                .iter()
                .cloned()
                .map(|parsed| V2ParsedCommand::from_core_with_cwd(parsed, &cwd))
                .collect::<Vec<_>>();
            let presentation = if let Some(network_approval_context) =
                network_approval_context.map(V2NetworkApprovalContext::from)
            {
                CommandExecutionApprovalPresentation::Network(network_approval_context)
            } else {
                let command_string = shlex_join(&command);
                let completion_item = CommandExecutionCompletionItem {
                    command: command_string,
                    cwd: cwd.clone().into(),
                    command_actions: command_actions.clone(),
                };
                CommandExecutionApprovalPresentation::Command(completion_item)
            };
            let (network_approval_context, command, cwd, command_actions, completion_item) =
                match presentation {
                    CommandExecutionApprovalPresentation::Network(network_approval_context) => {
                        (Some(network_approval_context), None, None, None, None)
                    }
                    CommandExecutionApprovalPresentation::Command(completion_item) => (
                        None,
                        Some(completion_item.command.clone()),
                        Some(completion_item.cwd.clone()),
                        Some(completion_item.command_actions.clone()),
                        Some(completion_item),
                    ),
                };
            if approval_id.is_none()
                && let Some(completion_item) = completion_item.as_ref()
            {
                start_command_execution_item(
                    &conversation_id,
                    event_turn_id.clone(),
                    call_id.clone(),
                    completion_item.command.clone(),
                    completion_item.cwd.clone(),
                    completion_item.command_actions.clone(),
                    CommandExecutionSource::Agent,
                    &outgoing,
                    &thread_state,
                )
                .await;
            }
            let proposed_execpolicy_amendment_v2 =
                proposed_execpolicy_amendment.map(V2ExecPolicyAmendment::from);
            let proposed_network_policy_amendments_v2 =
                proposed_network_policy_amendments.map(|amendments| {
                    amendments
                        .into_iter()
                        .map(V2NetworkPolicyAmendment::from)
                        .collect()
                });
            let additional_permissions =
                additional_permissions.map(V2AdditionalPermissionProfile::from);

            let params = CommandExecutionRequestApprovalParams {
                thread_id: conversation_id.to_string(),
                turn_id: turn_id.clone(),
                item_id: call_id.clone(),
                started_at_ms,
                approval_id: approval_id.clone(),
                environment_id,
                reason,
                network_approval_context,
                command,
                cwd,
                command_actions,
                additional_permissions,
                proposed_execpolicy_amendment: proposed_execpolicy_amendment_v2,
                proposed_network_policy_amendments: proposed_network_policy_amendments_v2,
                available_decisions: Some(available_decisions),
            };
            let script_prompt = session_script_registry
                .open_approval_prompt(
                    &session_script_outgoing,
                    conversation_id,
                    SessionScriptPromptKind::CommandExecutionApproval,
                    Some(turn_id),
                    Some(call_id.clone()),
                    SessionScriptPromptRequest {
                        method: "item/commandExecution/requestApproval".to_string(),
                        params: serde_json::to_value(&params).unwrap_or(serde_json::Value::Null),
                    },
                )
                .await;
            let script_registry = session_script_registry.clone();
            let script_outgoing = session_script_outgoing.clone();
            tokio::spawn(async move {
                let script_prompt_id = script_prompt.prompt_id.clone();
                let (pending_request_id, rx) = parallel_approval::await_response(
                    script_prompt,
                    ServerRequestPayload::CommandExecutionRequestApproval(params),
                    outgoing.clone(),
                )
                .await;
                on_command_execution_request_approval_response(
                    event_turn_id,
                    conversation_id,
                    approval_id,
                    call_id,
                    completion_item,
                    pending_request_id,
                    rx,
                    conversation,
                    outgoing,
                    thread_state.clone(),
                    permission_guard,
                )
                .await;
                script_registry
                    .close_prompt(
                        &script_outgoing,
                        &script_prompt_id,
                        SessionScriptPromptClosedReason::Answered,
                    )
                    .await;
            });
        }
        EventMsg::RequestUserInput(request) => {
            let user_input_guard = thread_watch_manager
                .note_user_input_requested(&conversation_id.to_string())
                .await;
            let questions = request
                .questions
                .into_iter()
                .map(|question| ToolRequestUserInputQuestion {
                    id: question.id,
                    header: question.header,
                    question: question.question,
                    is_other: question.is_other,
                    is_secret: question.is_secret,
                    options: question.options.map(|options| {
                        options
                            .into_iter()
                            .map(|option| ToolRequestUserInputOption {
                                label: option.label,
                                description: option.description,
                            })
                            .collect()
                    }),
                })
                .collect();
            let params = ToolRequestUserInputParams {
                thread_id: conversation_id.to_string(),
                turn_id: request.turn_id,
                item_id: request.call_id,
                questions,
                auto_resolution_ms: request.auto_resolution_ms,
            };
            let script_prompt = session_script_registry
                .open_request_user_input(&session_script_outgoing, conversation_id, params.clone())
                .await;
            match script_prompt.response_mode {
                RequestUserInputResponseMode::Parallel => {
                    let prompt_id = script_prompt.prompt_id.clone();
                    let (pending_request_id, rx) =
                        parallel_approval::await_request_user_input_response(
                            script_prompt,
                            ServerRequestPayload::ToolRequestUserInput(params),
                            outgoing.clone(),
                        )
                        .await;
                    let script_registry = session_script_registry.clone();
                    let script_outgoing = session_script_outgoing.clone();
                    tokio::spawn(async move {
                        on_observed_request_user_input_response(
                            event_turn_id,
                            pending_request_id,
                            rx,
                            conversation,
                            thread_state,
                            user_input_guard,
                            prompt_id,
                            script_registry,
                            script_outgoing,
                        )
                        .await;
                    });
                }
                RequestUserInputResponseMode::Exclusive => {
                    if let Some(response_receiver) = script_prompt.response_receiver {
                        let script_registry = session_script_registry.clone();
                        let script_outgoing = session_script_outgoing.clone();
                        let response_timeout = script_prompt
                            .response_timeout
                            .expect("a delegated requestUserInput prompt has a response timeout");
                        tokio::spawn(async move {
                            on_session_script_request_user_input_response(
                                event_turn_id,
                                response_receiver,
                                script_prompt.prompt_id,
                                params,
                                response_timeout,
                                conversation,
                                thread_state,
                                user_input_guard,
                                outgoing,
                                script_registry,
                                script_outgoing,
                            )
                            .await;
                        });
                    } else {
                        let (pending_request_id, rx) = outgoing
                            .send_request(ServerRequestPayload::ToolRequestUserInput(params))
                            .await;
                        let script_registry = session_script_registry.clone();
                        let script_outgoing = session_script_outgoing.clone();
                        tokio::spawn(async move {
                            on_observed_request_user_input_response(
                                event_turn_id,
                                pending_request_id,
                                rx,
                                conversation,
                                thread_state,
                                user_input_guard,
                                script_prompt.prompt_id,
                                script_registry,
                                script_outgoing,
                            )
                            .await;
                        });
                    }
                }
            }
        }
        EventMsg::ElicitationRequest(request) => {
            let permission_guard = thread_watch_manager
                .note_permission_requested(&conversation_id.to_string())
                .await;
            let turn_id = match request.turn_id.clone() {
                Some(turn_id) => Some(turn_id),
                None => {
                    let state = thread_state.lock().await;
                    state.active_turn_snapshot().map(|turn| turn.id)
                }
            };
            let server_name = request.server_name.clone();
            let request_body = match request.request.try_into() {
                Ok(request_body) => request_body,
                Err(err) => {
                    error!(
                        error = %err,
                        server_name,
                        request_id = ?request.id,
                        "failed to parse typed MCP elicitation schema"
                    );
                    if let Err(err) = conversation
                        .submit(Op::ResolveElicitation {
                            server_name: request.server_name,
                            request_id: request.id,
                            decision: xedoc_protocol::approvals::ElicitationAction::Cancel,
                            content: None,
                            meta: None,
                        })
                        .await
                    {
                        error!("failed to submit ResolveElicitation: {err}");
                    }
                    return;
                }
            };
            let params = McpServerElicitationRequestParams {
                thread_id: conversation_id.to_string(),
                turn_id,
                server_name: request.server_name.clone(),
                request: request_body,
            };
            let script_prompt_id = session_script_registry
                .open_observed_prompt(
                    &session_script_outgoing,
                    conversation_id,
                    SessionScriptPromptKind::McpElicitation,
                    params.turn_id.clone(),
                    None,
                    SessionScriptPromptRequest {
                        method: "mcpServer/elicitation/request".to_string(),
                        params: serde_json::to_value(&params).unwrap_or(serde_json::Value::Null),
                    },
                )
                .await;
            let (pending_request_id, rx) = outgoing
                .send_request(ServerRequestPayload::McpServerElicitationRequest(params))
                .await;
            let script_registry = session_script_registry.clone();
            let script_outgoing = session_script_outgoing.clone();
            tokio::spawn(async move {
                on_mcp_server_elicitation_response(
                    request.server_name,
                    request.id,
                    pending_request_id,
                    rx,
                    conversation,
                    thread_state,
                    permission_guard,
                )
                .await;
                script_registry
                    .close_prompt(
                        &script_outgoing,
                        &script_prompt_id,
                        SessionScriptPromptClosedReason::Answered,
                    )
                    .await;
            });
        }
        EventMsg::RequestPermissions(request) => {
            let permission_guard = thread_watch_manager
                .note_permission_requested(&conversation_id.to_string())
                .await;
            let requested_permissions = request.permissions.clone();
            let request_cwd = match request.cwd.clone() {
                Some(cwd) => cwd,
                None => conversation.config_snapshot().await.cwd().clone(),
            };
            let params = PermissionsRequestApprovalParams {
                thread_id: conversation_id.to_string(),
                turn_id: request.turn_id.clone(),
                item_id: request.call_id.clone(),
                environment_id: request.environment_id.clone(),
                started_at_ms: request.started_at_ms,
                cwd: request_cwd.clone(),
                reason: request.reason,
                permissions: request.permissions.into(),
            };
            let script_prompt = session_script_registry
                .open_approval_prompt(
                    &session_script_outgoing,
                    conversation_id,
                    SessionScriptPromptKind::PermissionsApproval,
                    Some(request.turn_id.clone()),
                    Some(request.call_id.clone()),
                    SessionScriptPromptRequest {
                        method: "item/permissions/requestApproval".to_string(),
                        params: serde_json::to_value(&params).unwrap_or(serde_json::Value::Null),
                    },
                )
                .await;
            let script_registry = session_script_registry.clone();
            let script_outgoing = session_script_outgoing.clone();
            tokio::spawn(async move {
                let script_prompt_id = script_prompt.prompt_id.clone();
                let (pending_request_id, rx) = parallel_approval::await_response(
                    script_prompt,
                    ServerRequestPayload::PermissionsRequestApproval(params),
                    outgoing.clone(),
                )
                .await;
                let pending_response = PendingRequestPermissionsResponse {
                    call_id: request.call_id,
                    conversation_id,
                    turn_id: request.turn_id,
                    requested_permissions,
                    request_cwd,
                    pending_request_id,
                    outgoing,
                    receiver: rx,
                    request_permissions_guard: permission_guard,
                };
                on_request_permissions_response(pending_response, conversation, thread_state).await;
                script_registry
                    .close_prompt(
                        &script_outgoing,
                        &script_prompt_id,
                        SessionScriptPromptClosedReason::Answered,
                    )
                    .await;
            });
        }
        EventMsg::DynamicToolCallRequest(_)
        | EventMsg::DynamicToolCallResponse(_)
        | EventMsg::CollabAgentSpawnBegin(_)
        | EventMsg::CollabAgentSpawnEnd(_)
        | EventMsg::CollabAgentInteractionBegin(_)
        | EventMsg::CollabAgentInteractionEnd(_)
        | EventMsg::CollabWaitingBegin(_)
        | EventMsg::CollabWaitingEnd(_)
        | EventMsg::CollabCloseBegin(_)
        | EventMsg::CollabCloseEnd(_)
        | EventMsg::CollabResumeBegin(_)
        | EventMsg::CollabResumeEnd(_)
        | EventMsg::ExecCommandBegin(_)
        | EventMsg::ExecCommandEnd(_)
        | EventMsg::EnteredReviewMode(_)
        | EventMsg::ExitedReviewMode(_) => {
            // Deprecated item lifecycle events are still fanned out for raw-event and rollout
            // compatibility consumers.
            // App-server v2 receives TurnItem lifecycle instead, and dispatches dynamic tool
            // requests from DynamicToolCall starts.
        }
        EventMsg::McpToolCallBegin(_) | EventMsg::McpToolCallEnd(_) => {
            // Deprecated MCP tool-call events are still fanned out for raw-event and rollout
            // compatibility consumers.
            // App-server v2 receives the canonical TurnItem::McpToolCall lifecycle instead.
        }
        EventMsg::SubAgentActivity(activity) => {
            if activity.kind == SubAgentActivityKind::Interrupted {
                remove_missing_thread_watch(
                    &thread_manager,
                    &thread_watch_manager,
                    activity.agent_thread_id,
                )
                .await;
            }
            // Forward activity summaries from sub-agent threads to the parent thread
            // so the parent's TUI can display what the sub-agent is currently doing.
            let parent_thread_id = conversation.session_source().parent_thread_id();
            match (activity.current_activity.as_ref(), parent_thread_id) {
                (Some(current_activity), Some(parent_thread_id)) => {
                    match thread_manager.get_thread(parent_thread_id).await {
                        Ok(parent_thread) => {
                            tracing::info!(
                                child_thread_id = %activity.agent_thread_id,
                                parent_thread_id = %parent_thread_id,
                                kind = ?activity.kind,
                                summary_chars = current_activity.chars().count(),
                                "Forwarding sub-agent activity summary to parent"
                            );
                            parent_thread
                                .emit_event(EventMsg::SubAgentActivity(activity.clone()))
                                .await;
                        }
                        Err(err) => {
                            tracing::warn!(
                                child_thread_id = %activity.agent_thread_id,
                                parent_thread_id = %parent_thread_id,
                                error = %err,
                                "Unable to forward sub-agent activity summary to parent"
                            );
                        }
                    }
                }
                (current_activity, parent_thread_id) => {
                    tracing::debug!(
                        child_thread_id = %activity.agent_thread_id,
                        has_current_activity = current_activity.is_some(),
                        has_parent_thread = parent_thread_id.is_some(),
                        "Skipped forwarding sub-agent activity summary"
                    );
                }
            }
            let notification = item_event_to_server_notification(
                EventMsg::SubAgentActivity(activity),
                &conversation_id.to_string(),
                &event_turn_id,
            );
            outgoing.send_server_notification(notification).await;
        }
        msg @ (EventMsg::AgentMessageContentDelta(_)
        | EventMsg::PlanDelta(_)
        | EventMsg::ReasoningContentDelta(_)
        | EventMsg::ReasoningRawContentDelta(_)
        | EventMsg::AgentReasoningSectionBreak(_)) => {
            if let EventMsg::AgentMessageContentDelta(event) = &msg
                && let Some(request) = thread_state
                    .lock()
                    .await
                    .note_agent_message_delta_for_auto_session_name(event)
            {
                maybe_spawn_auto_session_name_update(
                    conversation_id,
                    conversation.clone(),
                    thread_manager.clone(),
                    outgoing.clone(),
                    session_script_registry.clone(),
                    session_script_outgoing.clone(),
                    thread_state.clone(),
                    thread_list_state_permit.clone(),
                    AutoSessionNameUpdate::mid_turn(request),
                );
            }
            let is_agent_message_delta = matches!(&msg, EventMsg::AgentMessageContentDelta(_));
            let notification = item_event_to_server_notification(
                msg,
                &conversation_id.to_string(),
                &event_turn_id,
            );
            outgoing
                .send_server_notification(notification.clone())
                .await;
            if is_agent_message_delta {
                session_script_registry
                    .publish_model_response_delta(
                        &session_script_outgoing,
                        conversation_id,
                        notification,
                    )
                    .await;
            }
        }
        EventMsg::ContextCompacted(..) => {
            // Core still fans out this deprecated event for raw-event and rollout compatibility
            // consumers;
            // v2 clients receive the canonical ContextCompaction item instead.
        }
        EventMsg::DeprecationNotice(event) => {
            let notification = DeprecationNoticeNotification {
                summary: event.summary,
                details: event.details,
            };
            outgoing
                .send_server_notification(ServerNotification::DeprecationNotice(notification))
                .await;
        }
        EventMsg::TokenCount(token_count_event) => {
            handle_token_count_event(conversation_id, event_turn_id, token_count_event, &outgoing)
                .await;
        }
        EventMsg::Error(ev) => {
            thread_watch_manager
                .note_system_error(&conversation_id.to_string())
                .await;

            let message = ev.message.clone();
            let xedoc_error_info = ev.xedoc_error_info.clone();
            // If this error belongs to an in-flight `thread/rollback` request, fail that request
            // (and clear pending state) so subsequent rollbacks are unblocked.
            //
            // Don't send a notification for this error.
            if matches!(
                xedoc_error_info,
                Some(CoreXedocErrorInfo::ThreadRollbackFailed)
            ) {
                return handle_thread_rollback_failed(
                    conversation_id,
                    message,
                    &thread_state,
                    &outgoing,
                )
                .await;
            };

            if !ev.affects_turn_status() {
                return;
            }

            let turn_error = TurnError {
                message: ev.message,
                xedoc_error_info: ev.xedoc_error_info.map(V2XedocErrorInfo::from),
                additional_details: None,
            };
            handle_error_notification(
                conversation_id,
                &event_turn_id,
                turn_error,
                &outgoing,
                &thread_state,
            )
            .await;
        }
        EventMsg::StreamError(ev) => {
            // We don't need to update the turn summary store for stream errors as they are intermediate error states for retries,
            // but we notify the client.
            let turn_error = TurnError {
                message: ev.message,
                xedoc_error_info: ev.xedoc_error_info.map(V2XedocErrorInfo::from),
                additional_details: ev.additional_details,
            };
            outgoing
                .send_server_notification(ServerNotification::Error(ErrorNotification {
                    error: turn_error,
                    will_retry: true,
                    thread_id: conversation_id.to_string(),
                    turn_id: event_turn_id.clone(),
                }))
                .await;
        }
        EventMsg::ViewImageToolCall(_) => {}
        EventMsg::ItemStarted(event) => {
            let should_emit = match &event.item {
                // Approval flows can emit the command start notification before core
                // emits the canonical item. Reuse the same set to suppress that duplicate.
                CoreTurnItem::CommandExecution(item) => thread_state
                    .lock()
                    .await
                    .turn_summary
                    .command_execution_started
                    .insert(item.id.clone()),
                _ => true,
            };
            let dynamic_tool_call_params = match &event.item {
                CoreTurnItem::DynamicToolCall(item) => Some(DynamicToolCallParams {
                    thread_id: conversation_id.to_string(),
                    turn_id: event.turn_id.clone(),
                    call_id: item.id.clone(),
                    namespace: item.namespace.clone(),
                    tool: item.tool.clone(),
                    arguments: item.arguments.clone(),
                }),
                _ => None,
            };
            if should_emit {
                let notification = item_event_to_server_notification(
                    EventMsg::ItemStarted(event),
                    &conversation_id.to_string(),
                    &event_turn_id,
                );
                outgoing.send_server_notification(notification).await;
            }
            if let Some(params) = dynamic_tool_call_params {
                if let Some(session_extension_manager) = session_extension_manager.as_ref()
                    && session_extension_manager
                        .dispatch_remote_dynamic_tool(params.clone(), conversation.clone())
                        .await
                {
                    return;
                }
                let call_id = params.call_id.clone();
                let (_pending_request_id, rx) = outgoing
                    .send_request(ServerRequestPayload::DynamicToolCall(params))
                    .await;
                tokio::spawn(async move {
                    crate::dynamic_tools::on_call_response(call_id, rx, conversation).await;
                });
            }
        }
        EventMsg::ItemCompleted(event) => {
            let is_agent_message = matches!(&event.item, CoreTurnItem::AgentMessage(_));
            let is_user_message = matches!(&event.item, CoreTurnItem::UserMessage(_));
            let is_file_change = matches!(&event.item, CoreTurnItem::FileChange(_));
            if let CoreTurnItem::AgentMessage(agent_message) = &event.item {
                let message = agent_message
                    .content
                    .iter()
                    .map(|content| match content {
                        xedoc_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
                    })
                    .collect::<String>();
                if let Some(request) = thread_state
                    .lock()
                    .await
                    .note_agent_message_for_auto_session_name(&event_turn_id, &message)
                {
                    maybe_spawn_auto_session_name_update(
                        conversation_id,
                        conversation.clone(),
                        thread_manager.clone(),
                        outgoing.clone(),
                        session_script_registry.clone(),
                        session_script_outgoing.clone(),
                        thread_state.clone(),
                        thread_list_state_permit.clone(),
                        AutoSessionNameUpdate::mid_turn(request),
                    );
                }
            }
            apply_canonical_item_completed_side_effects(
                &thread_manager,
                &thread_watch_manager,
                &thread_state,
                &event.item,
            )
            .await;
            let notification = item_event_to_server_notification(
                EventMsg::ItemCompleted(event),
                &conversation_id.to_string(),
                &event_turn_id,
            );
            outgoing
                .send_server_notification(notification.clone())
                .await;
            if is_agent_message {
                session_script_registry
                    .publish_model_response_completed(
                        &session_script_outgoing,
                        conversation_id,
                        notification,
                    )
                    .await;
            } else if is_user_message {
                session_script_registry
                    .publish_user_message(&session_script_outgoing, conversation_id, notification)
                    .await;
            } else if is_file_change {
                session_script_registry
                    .publish_file_change(&session_script_outgoing, conversation_id, notification)
                    .await;
            }
        }
        msg @ (EventMsg::PatchApplyUpdated(_) | EventMsg::TerminalInteraction(_)) => {
            let notification = item_event_to_server_notification(
                msg,
                &conversation_id.to_string(),
                &event_turn_id,
            );
            outgoing.send_server_notification(notification).await;
        }
        EventMsg::HookStarted(event) => {
            let notification = HookStartedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event.turn_id,
                run: event.run.into(),
            };
            outgoing
                .send_server_notification(ServerNotification::HookStarted(notification))
                .await;
        }
        EventMsg::HookCompleted(event) => {
            let notification = HookCompletedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event.turn_id,
                run: event.run.into(),
            };
            outgoing
                .send_server_notification(ServerNotification::HookCompleted(notification))
                .await;
        }
        EventMsg::RawResponseItem(raw_response_item_event) => {
            maybe_emit_raw_response_item_completed(
                conversation_id,
                &event_turn_id,
                raw_response_item_event.item,
                &outgoing,
            )
            .await;
        }
        EventMsg::RawResponseCompleted(raw_response_completed_event) => {
            let notification = RawResponseCompletedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id,
                response_id: raw_response_completed_event.response_id,
                usage: raw_response_completed_event.token_usage.map(Into::into),
            };
            outgoing
                .send_server_notification(ServerNotification::RawResponseCompleted(notification))
                .await;
        }
        EventMsg::PatchApplyBegin(_) | EventMsg::PatchApplyEnd(_) => {
            // Core still fans out these deprecated events for raw-event and rollout compatibility
            // consumers;
            // v2 clients receive the canonical FileChange item instead.
        }
        EventMsg::ExecCommandOutputDelta(exec_command_output_delta_event) => {
            let notification = item_event_to_server_notification(
                EventMsg::ExecCommandOutputDelta(exec_command_output_delta_event),
                &conversation_id.to_string(),
                &event_turn_id,
            );
            outgoing.send_server_notification(notification).await;
        }
        // If this is a TurnAborted, reply to any pending interrupt requests.
        EventMsg::TurnAborted(turn_aborted_event) => {
            // All per-thread requests are bound to a turn, so abort them.
            outgoing.abort_pending_server_requests().await;
            session_script_registry
                .close_prompts_for_turn(
                    &session_script_outgoing,
                    conversation_id,
                    &event_turn_id,
                    SessionScriptPromptClosedReason::TurnEnded,
                )
                .await;
            respond_to_pending_interrupts(&thread_state, &outgoing).await;

            thread_watch_manager
                .note_turn_interrupted(&conversation_id.to_string())
                .await;
            handle_turn_interrupted(
                conversation_id,
                event_turn_id,
                turn_aborted_event,
                &outgoing,
                &thread_state,
            )
            .await;
        }
        EventMsg::ThreadRolledBack(_rollback_event) => {
            let pending = {
                let mut state = thread_state.lock().await;
                state.pending_rollbacks.take()
            };

            if let Some(request_id) = pending {
                let _thread_list_state_permit = match thread_list_state_permit.acquire().await {
                    Ok(permit) => permit,
                    Err(err) => {
                        outgoing
                            .send_error(
                                request_id,
                                internal_error(format!(
                                    "failed to acquire thread list state permit: {err}"
                                )),
                            )
                            .await;
                        return;
                    }
                };
                let fallback_cwd = conversation.config_snapshot().await.cwd().clone();
                let stored_thread = match conversation
                    .read_thread(
                        /*include_archived*/ true, /*include_history*/ true,
                    )
                    .await
                {
                    Ok(stored_thread) => stored_thread,
                    Err(err) => {
                        outgoing
                            .send_error(
                                request_id.clone(),
                                internal_error(format!(
                                    "failed to read thread {conversation_id} after rollback: {err}"
                                )),
                            )
                            .await;
                        return;
                    }
                };
                let loaded_status = thread_watch_manager
                    .loaded_status_for_thread(&conversation_id.to_string())
                    .await;
                let response = match thread_rollback_response_from_stored_thread(
                    stored_thread,
                    conversation.session_configured().session_id.to_string(),
                    fallback_model_provider.as_str(),
                    &fallback_cwd,
                    loaded_status,
                ) {
                    Ok(response) => response,
                    Err(err) => {
                        outgoing
                            .send_error(request_id.clone(), internal_error(err))
                            .await;
                        return;
                    }
                };

                outgoing.send_response(request_id, response).await;
            }
        }
        EventMsg::ThreadGoalUpdated(thread_goal_event) => {
            let notification = ThreadGoalUpdatedNotification {
                thread_id: thread_goal_event.thread_id.to_string(),
                turn_id: thread_goal_event.turn_id,
                goal: thread_goal_event.goal.clone().into(),
            };
            outgoing
                .send_global_server_notification(ServerNotification::ThreadGoalUpdated(
                    notification,
                ))
                .await;
        }
        EventMsg::ThreadSettingsApplied(thread_settings_event) => {
            let thread_settings =
                thread_settings_from_core_snapshot(thread_settings_event.thread_settings);
            let changed = {
                let mut state = thread_state.lock().await;
                state.note_thread_settings(thread_settings.clone())
            };
            if changed {
                let session =
                    session_script_session_from_cwd(conversation_id, thread_settings.cwd.as_path());
                outgoing
                    .send_server_notification(ServerNotification::ThreadSettingsUpdated(
                        ThreadSettingsUpdatedNotification {
                            thread_id: conversation_id.to_string(),
                            thread_settings,
                        },
                    ))
                    .await;
                session_script_registry
                    .publish_session_updated(&session_script_outgoing, conversation_id, session)
                    .await;
            }
        }
        EventMsg::TurnDiff(turn_diff_event) => {
            handle_turn_diff(conversation_id, &event_turn_id, turn_diff_event, &outgoing).await;
        }
        EventMsg::PlanUpdate(plan_update_event) => {
            handle_turn_plan_update(
                conversation_id,
                &event_turn_id,
                plan_update_event,
                &outgoing,
            )
            .await;
        }
        EventMsg::ShutdownComplete => {
            thread_watch_manager
                .note_thread_shutdown(&conversation_id.to_string())
                .await;
        }

        _ => {}
    }
}

fn session_script_session_from_cwd(thread_id: ThreadId, cwd: &Path) -> SessionScriptSession {
    let project_root = xedoc_git_utils::get_git_repo_root(cwd);
    let project_name = project_root
        .as_deref()
        .or(Some(cwd))
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_string());
    let thread_id = thread_id.to_string();
    SessionScriptSession {
        session_id: thread_id.clone(),
        thread_id,
        title: None,
        project_name,
        project_root: project_root.map(|path| path.display().to_string()),
        cwd: cwd.display().to_string(),
    }
}

async fn handle_turn_diff(
    conversation_id: ThreadId,
    event_turn_id: &str,
    turn_diff_event: TurnDiffEvent,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    let notification = TurnDiffUpdatedNotification {
        thread_id: conversation_id.to_string(),
        turn_id: event_turn_id.to_string(),
        diff: turn_diff_event.unified_diff,
    };
    outgoing
        .send_server_notification(ServerNotification::TurnDiffUpdated(notification))
        .await;
}

async fn handle_turn_plan_update(
    conversation_id: ThreadId,
    event_turn_id: &str,
    plan_update_event: UpdatePlanArgs,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    // `update_plan` is a todo/checklist tool; it is not related to plan-mode updates
    let notification = TurnPlanUpdatedNotification {
        thread_id: conversation_id.to_string(),
        turn_id: event_turn_id.to_string(),
        explanation: plan_update_event.explanation,
        plan: plan_update_event
            .plan
            .into_iter()
            .map(TurnPlanStep::from)
            .collect(),
    };
    outgoing
        .send_server_notification(ServerNotification::TurnPlanUpdated(notification))
        .await;
}

struct TurnCompletionMetadata {
    status: TurnStatus,
    error: Option<TurnError>,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    duration_ms: Option<i64>,
}

async fn emit_turn_completed_with_status(
    conversation_id: ThreadId,
    event_turn_id: String,
    turn_completion_metadata: TurnCompletionMetadata,
    outgoing: &ThreadScopedOutgoingMessageSender,
) -> TurnCompletedNotification {
    let notification = TurnCompletedNotification {
        thread_id: conversation_id.to_string(),
        turn: Turn {
            id: event_turn_id,
            items: vec![],
            items_view: TurnItemsView::NotLoaded,
            error: turn_completion_metadata.error,
            status: turn_completion_metadata.status,
            started_at: turn_completion_metadata.started_at,
            completed_at: turn_completion_metadata.completed_at,
            duration_ms: turn_completion_metadata.duration_ms,
        },
    };
    outgoing
        .send_server_notification(ServerNotification::TurnCompleted(notification.clone()))
        .await;
    notification
}

async fn apply_canonical_item_completed_side_effects(
    thread_manager: &Arc<ThreadManager>,
    thread_watch_manager: &ThreadWatchManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    item: &CoreTurnItem,
) {
    match item {
        CoreTurnItem::CommandExecution(item) => {
            thread_state
                .lock()
                .await
                .turn_summary
                .command_execution_started
                .remove(&item.id);
        }
        CoreTurnItem::SubAgentActivity(activity)
            if activity.kind == SubAgentActivityKind::Interrupted =>
        {
            remove_missing_thread_watch(
                thread_manager,
                thread_watch_manager,
                activity.agent_thread_id,
            )
            .await;
        }
        CoreTurnItem::CollabAgentToolCall(item) if item.tool == CoreCollabAgentTool::CloseAgent => {
            for thread_id in &item.receiver_thread_ids {
                remove_missing_thread_watch(thread_manager, thread_watch_manager, *thread_id).await;
            }
        }
        _ => {}
    }
}

async fn remove_missing_thread_watch(
    thread_manager: &Arc<ThreadManager>,
    thread_watch_manager: &ThreadWatchManager,
    thread_id: ThreadId,
) {
    if thread_manager.get_thread(thread_id).await.is_err() {
        thread_watch_manager
            .remove_thread(&thread_id.to_string())
            .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_command_execution_item(
    conversation_id: &ThreadId,
    turn_id: String,
    item_id: String,
    command: String,
    cwd: LegacyAppPathString,
    command_actions: Vec<V2ParsedCommand>,
    source: CommandExecutionSource,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) -> bool {
    let first_start = {
        let mut state = thread_state.lock().await;
        state
            .turn_summary
            .command_execution_started
            .insert(item_id.clone())
    };
    if first_start {
        let notification = ItemStartedNotification {
            thread_id: conversation_id.to_string(),
            turn_id,
            started_at_ms: now_unix_timestamp_ms(),
            item: ThreadItem::CommandExecution {
                id: item_id,
                command,
                cwd,
                process_id: None,
                source,
                status: CommandExecutionStatus::InProgress,
                command_actions,
                aggregated_output: None,
                exit_code: None,
                duration_ms: None,
            },
        };
        outgoing
            .send_server_notification(ServerNotification::ItemStarted(notification))
            .await;
    }
    first_start
}

#[allow(clippy::too_many_arguments)]
async fn complete_command_execution_item(
    conversation_id: &ThreadId,
    turn_id: String,
    item_id: String,
    command: String,
    cwd: LegacyAppPathString,
    process_id: Option<String>,
    source: CommandExecutionSource,
    command_actions: Vec<V2ParsedCommand>,
    status: CommandExecutionStatus,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    let should_emit = thread_state
        .lock()
        .await
        .turn_summary
        .command_execution_started
        .remove(&item_id);
    if !should_emit {
        return;
    }

    let item = ThreadItem::CommandExecution {
        id: item_id,
        command,
        cwd,
        process_id,
        source,
        status,
        command_actions,
        aggregated_output: None,
        exit_code: None,
        duration_ms: None,
    };
    let notification = ItemCompletedNotification {
        thread_id: conversation_id.to_string(),
        turn_id,
        completed_at_ms: now_unix_timestamp_ms(),
        item,
    };
    outgoing
        .send_server_notification(ServerNotification::ItemCompleted(notification))
        .await;
}

async fn maybe_emit_raw_response_item_completed(
    conversation_id: ThreadId,
    turn_id: &str,
    item: xedoc_protocol::models::ResponseItem,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    let notification = RawResponseItemCompletedNotification {
        thread_id: conversation_id.to_string(),
        turn_id: turn_id.to_string(),
        item,
    };
    outgoing
        .send_server_notification(ServerNotification::RawResponseItemCompleted(notification))
        .await;
}

async fn find_and_remove_turn_summary(
    _conversation_id: ThreadId,
    thread_state: &Arc<Mutex<ThreadState>>,
) -> TurnSummary {
    let mut state = thread_state.lock().await;
    std::mem::take(&mut state.turn_summary)
}

async fn handle_turn_complete(
    conversation_id: ThreadId,
    event_turn_id: String,
    turn_complete_event: TurnCompleteEvent,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) -> TurnCompletedNotification {
    let turn_summary = find_and_remove_turn_summary(conversation_id, thread_state).await;

    let (status, error) = match turn_summary.last_error {
        Some(error) => (TurnStatus::Failed, Some(error)),
        None => (TurnStatus::Completed, None),
    };

    emit_turn_completed_with_status(
        conversation_id,
        event_turn_id,
        TurnCompletionMetadata {
            status,
            error,
            started_at: turn_summary.started_at,
            completed_at: turn_complete_event.completed_at,
            duration_ms: turn_complete_event.duration_ms,
        },
        outgoing,
    )
    .await
}

async fn handle_turn_interrupted(
    conversation_id: ThreadId,
    event_turn_id: String,
    turn_aborted_event: TurnAbortedEvent,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    let turn_summary = find_and_remove_turn_summary(conversation_id, thread_state).await;

    let _ = emit_turn_completed_with_status(
        conversation_id,
        event_turn_id,
        TurnCompletionMetadata {
            status: TurnStatus::Interrupted,
            error: None,
            started_at: turn_summary.started_at,
            completed_at: turn_aborted_event.completed_at,
            duration_ms: turn_aborted_event.duration_ms,
        },
        outgoing,
    )
    .await;
}

async fn handle_thread_rollback_failed(
    _conversation_id: ThreadId,
    message: String,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    let pending_rollback = thread_state.lock().await.pending_rollbacks.take();

    if let Some(request_id) = pending_rollback {
        outgoing
            .send_error(request_id, invalid_request(message))
            .await;
    }
}

fn thread_rollback_response_from_stored_thread(
    stored_thread: xedoc_thread_store::StoredThread,
    session_id: String,
    fallback_model_provider: &str,
    fallback_cwd: &AbsolutePathBuf,
    loaded_status: ThreadStatus,
) -> std::result::Result<ThreadRollbackResponse, String> {
    let thread_id = stored_thread.thread_id;
    let (mut thread, history) =
        thread_from_stored_thread(stored_thread, fallback_model_provider, fallback_cwd);
    thread.session_id = session_id;
    let Some(history) = history else {
        return Err(format!(
            "thread {thread_id} did not include persisted history after rollback"
        ));
    };
    populate_thread_turns_from_history(&mut thread, &history.items, /*active_turn*/ None);
    thread.status = loaded_status;
    Ok(ThreadRollbackResponse { thread })
}

async fn respond_to_pending_interrupts(
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    let pending = {
        let mut state = thread_state.lock().await;
        std::mem::take(&mut state.pending_interrupts)
    };

    for request_id in pending {
        outgoing
            .send_response(request_id, TurnInterruptResponse {})
            .await;
    }
}

async fn handle_token_count_event(
    conversation_id: ThreadId,
    turn_id: String,
    token_count_event: TokenCountEvent,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    let TokenCountEvent {
        info,
        rate_limits,
        session_cost_usd,
        token_optimizer,
    } = token_count_event;
    if let Some(mut token_usage) = info.map(ThreadTokenUsage::from) {
        token_usage.session_cost_usd = session_cost_usd;
        token_usage.token_optimizer = token_optimizer;
        let notification = ThreadTokenUsageUpdatedNotification {
            thread_id: conversation_id.to_string(),
            turn_id,
            token_usage,
        };
        outgoing
            .send_server_notification(ServerNotification::ThreadTokenUsageUpdated(notification))
            .await;
    }
    if let Some(rate_limits) = rate_limits {
        outgoing
            .send_server_notification(ServerNotification::AccountRateLimitsUpdated(
                AccountRateLimitsUpdatedNotification {
                    rate_limits: rate_limits.into(),
                },
            ))
            .await;
    }
}

async fn handle_error(
    _conversation_id: ThreadId,
    error: TurnError,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    let mut state = thread_state.lock().await;
    state.turn_summary.last_error = Some(error);
}

async fn handle_error_notification(
    conversation_id: ThreadId,
    event_turn_id: &str,
    error: TurnError,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    handle_error(conversation_id, error.clone(), thread_state).await;
    outgoing
        .send_server_notification(ServerNotification::Error(ErrorNotification {
            error,
            will_retry: false,
            thread_id: conversation_id.to_string(),
            turn_id: event_turn_id.to_string(),
        }))
        .await;
}

async fn on_request_user_input_response(
    event_turn_id: String,
    pending_request_id: RequestId,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<XedocThread>,
    thread_state: Arc<Mutex<ThreadState>>,
    user_input_guard: ThreadWatchActiveGuard,
) {
    let response = receiver.await;
    resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
    drop(user_input_guard);
    let value = match response {
        Ok(Ok(value)) => value,
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => return,
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            let empty = CoreRequestUserInputResponse {
                answers: HashMap::new(),
            };
            if let Err(err) = conversation
                .submit(Op::UserInputAnswer {
                    id: event_turn_id,
                    response: empty,
                })
                .await
            {
                error!("failed to submit UserInputAnswer: {err}");
            }
            return;
        }
        Err(err) => {
            error!("request failed: {err:?}");
            let empty = CoreRequestUserInputResponse {
                answers: HashMap::new(),
            };
            if let Err(err) = conversation
                .submit(Op::UserInputAnswer {
                    id: event_turn_id,
                    response: empty,
                })
                .await
            {
                error!("failed to submit UserInputAnswer: {err}");
            }
            return;
        }
    };

    let response =
        serde_json::from_value::<ToolRequestUserInputResponse>(value).unwrap_or_else(|err| {
            error!("failed to deserialize ToolRequestUserInputResponse: {err}");
            ToolRequestUserInputResponse {
                answers: HashMap::new(),
            }
        });
    let response = CoreRequestUserInputResponse {
        answers: response
            .answers
            .into_iter()
            .map(|(id, answer)| {
                (
                    id,
                    CoreRequestUserInputAnswer {
                        answers: answer.answers,
                    },
                )
            })
            .collect(),
    };

    submit_request_user_input_response(event_turn_id, response, conversation).await;
}

async fn on_observed_request_user_input_response(
    event_turn_id: String,
    pending_request_id: RequestId,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<XedocThread>,
    thread_state: Arc<Mutex<ThreadState>>,
    user_input_guard: ThreadWatchActiveGuard,
    prompt_id: String,
    session_script_registry: SessionScriptRegistry,
    session_script_outgoing: Arc<crate::outgoing_message::OutgoingMessageSender>,
) {
    on_request_user_input_response(
        event_turn_id,
        pending_request_id,
        receiver,
        conversation,
        thread_state,
        user_input_guard,
    )
    .await;
    session_script_registry
        .close_prompt(
            &session_script_outgoing,
            &prompt_id,
            SessionScriptPromptClosedReason::Answered,
        )
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn on_session_script_request_user_input_response(
    event_turn_id: String,
    mut response_receiver: oneshot::Receiver<ToolRequestUserInputResponse>,
    prompt_id: String,
    params: ToolRequestUserInputParams,
    response_timeout: Duration,
    conversation: Arc<XedocThread>,
    thread_state: Arc<Mutex<ThreadState>>,
    user_input_guard: ThreadWatchActiveGuard,
    outgoing: ThreadScopedOutgoingMessageSender,
    session_script_registry: SessionScriptRegistry,
    session_script_outgoing: Arc<crate::outgoing_message::OutgoingMessageSender>,
) {
    let response_timeout = params
        .auto_resolution_ms
        .map(Duration::from_millis)
        .map_or(response_timeout, |auto_resolution_timeout| {
            auto_resolution_timeout.min(response_timeout)
        });
    let response = tokio::select! {
        response = &mut response_receiver => Some(response),
        _ = tokio::time::sleep(response_timeout) => {
            let lease_expired = session_script_registry
                .close_prompt(
                    &session_script_outgoing,
                    &prompt_id,
                    SessionScriptPromptClosedReason::Expired,
                )
                .await;
            if lease_expired {
                None
            } else {
                Some(response_receiver.await)
            }
        }
    };
    match response {
        Some(Ok(response)) => {
            drop(user_input_guard);
            let response = CoreRequestUserInputResponse {
                answers: response
                    .answers
                    .into_iter()
                    .map(|(id, answer)| {
                        (
                            id,
                            CoreRequestUserInputAnswer {
                                answers: answer.answers,
                            },
                        )
                    })
                    .collect(),
            };
            submit_request_user_input_response(event_turn_id, response, conversation).await;
        }
        None | Some(Err(_)) => {
            let (pending_request_id, receiver) = outgoing
                .send_request(ServerRequestPayload::ToolRequestUserInput(params))
                .await;
            on_observed_request_user_input_response(
                event_turn_id,
                pending_request_id,
                receiver,
                conversation,
                thread_state,
                user_input_guard,
                prompt_id,
                session_script_registry,
                session_script_outgoing,
            )
            .await;
        }
    }
}

async fn submit_request_user_input_response(
    event_turn_id: String,
    response: CoreRequestUserInputResponse,
    conversation: Arc<XedocThread>,
) {
    if let Err(err) = conversation
        .submit(Op::UserInputAnswer {
            id: event_turn_id,
            response,
        })
        .await
    {
        error!("failed to submit UserInputAnswer: {err}");
    }
}

async fn on_extension_interaction_response(
    request_id: String,
    extension_id: String,
    interaction_id: String,
    continuation: String,
    state_revision: Option<String>,
    pending_request_id: Option<RequestId>,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<XedocThread>,
    thread_state: Arc<Mutex<ThreadState>>,
) {
    let cancelled_response = ExtensionInteractionRequestResponse {
        extension_id,
        interaction_id,
        continuation,
        state_revision,
        outcome: ExtensionInteractionOutcome::Cancelled,
        action: None,
        values: serde_json::Value::Null,
    };
    let response = match receiver.await {
        Ok(Ok(value)) => serde_json::from_value::<ExtensionInteractionRequestResponse>(value)
            .unwrap_or_else(|error| {
                error!("failed to deserialize ExtensionInteractionRequestResponse: {error}");
                cancelled_response.clone()
            }),
        Ok(Err(error)) if is_turn_transition_server_request_error(&error) => return,
        Ok(Err(error)) => {
            error!("extension interaction request failed with client error: {error:?}");
            cancelled_response.clone()
        }
        Err(error) => {
            error!("extension interaction request failed: {error:?}");
            cancelled_response
        }
    };
    if let Some(pending_request_id) = pending_request_id {
        resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
    }
    let response = CoreScriptedInteractionResponse {
        extension_id: response.extension_id,
        interaction_id: response.interaction_id,
        continuation: response.continuation,
        state_revision: response.state_revision,
        outcome: match response.outcome {
            ExtensionInteractionOutcome::Accepted => CoreScriptedInteractionOutcome::Accepted,
            ExtensionInteractionOutcome::Cancelled => CoreScriptedInteractionOutcome::Cancelled,
            ExtensionInteractionOutcome::Dismissed => CoreScriptedInteractionOutcome::Dismissed,
        },
        action: response.action.map(|action| {
            xedoc_protocol::protocol::ScriptedInteractionSelectedAction { id: action.id }
        }),
        values: response.values,
    };
    if let Err(error) = conversation
        .submit(Op::ScriptedInteractionResponse {
            request_id,
            response,
        })
        .await
    {
        error!("failed to submit ScriptedInteractionResponse: {error}");
    }
}

async fn on_mcp_server_elicitation_response(
    server_name: String,
    request_id: xedoc_protocol::mcp::RequestId,
    pending_request_id: RequestId,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<XedocThread>,
    thread_state: Arc<Mutex<ThreadState>>,
    permission_guard: ThreadWatchActiveGuard,
) {
    let response = receiver.await;
    resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
    drop(permission_guard);
    let response = mcp_server_elicitation_response_from_client_result(response);

    if let Err(err) = conversation
        .submit(Op::ResolveElicitation {
            server_name,
            request_id,
            decision: response.action.to_core(),
            content: response.content,
            meta: response.meta,
        })
        .await
    {
        error!("failed to submit ResolveElicitation: {err}");
    }
}

fn mcp_server_elicitation_response_from_client_result(
    response: std::result::Result<ClientRequestResult, oneshot::error::RecvError>,
) -> McpServerElicitationRequestResponse {
    match response {
        Ok(Ok(value)) => serde_json::from_value::<McpServerElicitationRequestResponse>(value)
            .unwrap_or_else(|err| {
                error!("failed to deserialize McpServerElicitationRequestResponse: {err}");
                McpServerElicitationRequestResponse {
                    action: McpServerElicitationAction::Decline,
                    content: None,
                    meta: None,
                }
            }),
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => {
            McpServerElicitationRequestResponse {
                action: McpServerElicitationAction::Cancel,
                content: None,
                meta: None,
            }
        }
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            McpServerElicitationRequestResponse {
                action: McpServerElicitationAction::Decline,
                content: None,
                meta: None,
            }
        }
        Err(err) => {
            error!("request failed: {err:?}");
            McpServerElicitationRequestResponse {
                action: McpServerElicitationAction::Decline,
                content: None,
                meta: None,
            }
        }
    }
}

async fn on_request_permissions_response(
    pending_response: PendingRequestPermissionsResponse,
    conversation: Arc<XedocThread>,
    thread_state: Arc<Mutex<ThreadState>>,
) {
    let PendingRequestPermissionsResponse {
        call_id,
        conversation_id,
        turn_id,
        requested_permissions,
        request_cwd,
        pending_request_id,
        outgoing,
        receiver,
        request_permissions_guard,
    } = pending_response;
    let response = receiver.await;
    if let Some(pending_request_id) = pending_request_id {
        resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
    }
    drop(request_permissions_guard);
    let response = match request_permissions_response_from_client_result(
        requested_permissions,
        response,
        request_cwd.as_path(),
    ) {
        Ok(Some(response)) => response,
        Ok(None) => return,
        // TODO(anp): Remove this native-path localization error path once core permission paths
        // remain PathUri after crossing the app-server boundary.
        Err(err) => {
            let message = format!("failed to localize granted filesystem paths: {err}");
            handle_error_notification(
                conversation_id,
                &turn_id,
                TurnError {
                    message,
                    xedoc_error_info: None,
                    additional_details: None,
                },
                &outgoing,
                &thread_state,
            )
            .await;
            if let Err(err) = conversation.submit(Op::Interrupt).await {
                error!("failed to interrupt turn after invalid permission paths: {err}");
            }
            return;
        }
    };

    if let Err(err) = conversation
        .submit(Op::RequestPermissionsResponse {
            id: call_id,
            response,
        })
        .await
    {
        error!("failed to submit RequestPermissionsResponse: {err}");
    }
}

struct PendingRequestPermissionsResponse {
    call_id: String,
    conversation_id: ThreadId,
    turn_id: String,
    requested_permissions: CoreRequestPermissionProfile,
    request_cwd: AbsolutePathBuf,
    pending_request_id: Option<RequestId>,
    outgoing: ThreadScopedOutgoingMessageSender,
    receiver: oneshot::Receiver<ClientRequestResult>,
    request_permissions_guard: ThreadWatchActiveGuard,
}

fn request_permissions_response_from_client_result(
    requested_permissions: CoreRequestPermissionProfile,
    response: std::result::Result<ClientRequestResult, oneshot::error::RecvError>,
    cwd: &std::path::Path,
) -> std::io::Result<Option<CoreRequestPermissionsResponse>> {
    let value = match response {
        Ok(Ok(value)) => value,
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => return Ok(None),
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            return Ok(Some(CoreRequestPermissionsResponse {
                permissions: Default::default(),
                scope: CorePermissionGrantScope::Turn,
            }));
        }
        Err(err) => {
            error!("request failed: {err:?}");
            return Ok(Some(CoreRequestPermissionsResponse {
                permissions: Default::default(),
                scope: CorePermissionGrantScope::Turn,
            }));
        }
    };

    let response = serde_json::from_value::<PermissionsRequestApprovalResponse>(value)
        .unwrap_or_else(|err| {
            error!("failed to deserialize PermissionsRequestApprovalResponse: {err}");
            PermissionsRequestApprovalResponse {
                permissions: V2GrantedPermissionProfile::default(),
                scope: xedoc_app_server_protocol::PermissionGrantScope::Turn,
            }
        });
    let granted_permissions: CoreAdditionalPermissionProfile = response.permissions.try_into()?;
    let permissions = if granted_permissions.is_empty() {
        CoreRequestPermissionProfile::default()
    } else {
        intersect_permission_profiles(requested_permissions.into(), granted_permissions, cwd).into()
    };
    Ok(Some(CoreRequestPermissionsResponse {
        permissions,
        scope: response.scope.to_core(),
    }))
}

fn map_file_change_approval_decision(decision: FileChangeApprovalDecision) -> ReviewDecision {
    match decision {
        FileChangeApprovalDecision::Accept => ReviewDecision::Approved,
        FileChangeApprovalDecision::AcceptForSession => ReviewDecision::ApprovedForSession,
        FileChangeApprovalDecision::Decline => ReviewDecision::denied("rejected by user"),
        FileChangeApprovalDecision::Cancel => ReviewDecision::Abort,
    }
}

#[allow(clippy::too_many_arguments)]
async fn on_file_change_request_approval_response(
    item_id: String,
    pending_request_id: Option<RequestId>,
    receiver: oneshot::Receiver<ClientRequestResult>,
    xedoc: Arc<XedocThread>,
    thread_state: Arc<Mutex<ThreadState>>,
    permission_guard: ThreadWatchActiveGuard,
) {
    let response = receiver.await;
    if let Some(pending_request_id) = pending_request_id {
        resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
    }
    drop(permission_guard);
    let decision = match response {
        Ok(Ok(value)) => match serde_json::from_value::<FileChangeRequestApprovalResponse>(value) {
            Ok(response) => map_file_change_approval_decision(response.decision),
            Err(err) => {
                error!("failed to deserialize FileChangeRequestApprovalResponse: {err}");
                ReviewDecision::denied("approval request failed")
            }
        },
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => return,
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            ReviewDecision::denied("approval request failed")
        }
        Err(err) => {
            error!("request failed: {err:?}");
            ReviewDecision::denied("approval request failed")
        }
    };

    if let Err(err) = xedoc
        .submit(Op::PatchApproval {
            id: item_id,
            decision,
        })
        .await
    {
        error!("failed to submit PatchApproval: {err}");
    }
}

#[allow(clippy::too_many_arguments)]
async fn on_command_execution_request_approval_response(
    event_turn_id: String,
    conversation_id: ThreadId,
    approval_id: Option<String>,
    item_id: String,
    completion_item: Option<CommandExecutionCompletionItem>,
    pending_request_id: Option<RequestId>,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<XedocThread>,
    outgoing: ThreadScopedOutgoingMessageSender,
    thread_state: Arc<Mutex<ThreadState>>,
    permission_guard: ThreadWatchActiveGuard,
) {
    let response = receiver.await;
    if let Some(pending_request_id) = pending_request_id {
        resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
    }
    drop(permission_guard);
    let (decision, completion_status) = match response {
        Ok(Ok(value)) => {
            match serde_json::from_value::<CommandExecutionRequestApprovalResponse>(value) {
                Ok(response) => match response.decision {
                    CommandExecutionApprovalDecision::Accept => (ReviewDecision::Approved, None),
                    CommandExecutionApprovalDecision::AcceptForSession => {
                        (ReviewDecision::ApprovedForSession, None)
                    }
                    CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
                        execpolicy_amendment,
                    } => (
                        ReviewDecision::ApprovedExecpolicyAmendment {
                            proposed_execpolicy_amendment: execpolicy_amendment.into_core(),
                        },
                        None,
                    ),
                    CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                        network_policy_amendment,
                    } => {
                        let completion_status = match network_policy_amendment.action {
                            V2NetworkPolicyRuleAction::Allow => None,
                            V2NetworkPolicyRuleAction::Deny => {
                                Some(CommandExecutionStatus::Declined)
                            }
                        };
                        (
                            ReviewDecision::NetworkPolicyAmendment {
                                network_policy_amendment: network_policy_amendment.into_core(),
                            },
                            completion_status,
                        )
                    }
                    CommandExecutionApprovalDecision::Decline => (
                        ReviewDecision::denied("rejected by user"),
                        Some(CommandExecutionStatus::Declined),
                    ),
                    CommandExecutionApprovalDecision::Cancel => (
                        ReviewDecision::Abort,
                        Some(CommandExecutionStatus::Declined),
                    ),
                },
                Err(err) => {
                    error!("failed to deserialize CommandExecutionRequestApprovalResponse: {err}");
                    (
                        ReviewDecision::denied("approval request failed"),
                        Some(CommandExecutionStatus::Failed),
                    )
                }
            }
        }
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => return,
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            (
                ReviewDecision::denied("approval request failed"),
                Some(CommandExecutionStatus::Failed),
            )
        }
        Err(err) => {
            error!("request failed: {err:?}");
            (
                ReviewDecision::denied("approval request failed"),
                Some(CommandExecutionStatus::Failed),
            )
        }
    };

    let suppress_subcommand_completion_item = {
        // For regular shell/unified_exec approvals, approval_id is null.
        // For zsh-fork subcommand approvals, approval_id is present and
        // item_id points to the parent command item.
        if approval_id.is_some() {
            let state = thread_state.lock().await;
            state
                .turn_summary
                .command_execution_started
                .contains(&item_id)
        } else {
            false
        }
    };

    if let Some(status) = completion_status
        && !suppress_subcommand_completion_item
        && let Some(completion_item) = completion_item
    {
        complete_command_execution_item(
            &conversation_id,
            event_turn_id.clone(),
            item_id.clone(),
            completion_item.command,
            completion_item.cwd,
            /*process_id*/ None,
            CommandExecutionSource::Agent,
            completion_item.command_actions,
            status,
            &outgoing,
            &thread_state,
        )
        .await;
    }

    if let Err(err) = conversation
        .submit(Op::ExecApproval {
            id: approval_id.unwrap_or_else(|| item_id.clone()),
            turn_id: Some(event_turn_id),
            decision,
        })
        .await
    {
        error!("failed to submit ExecApproval: {err}");
    }
}

fn now_unix_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
