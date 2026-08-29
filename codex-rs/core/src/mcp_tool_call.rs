use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use crate::config::Config;
use crate::config::edit::ConfigEdit;
use crate::config::edit::ConfigEditsBuilder;
use crate::connectors;
use crate::guardian::GuardianApprovalRequest;
use crate::guardian::GuardianMcpAnnotations;
use crate::guardian::new_guardian_review_id;
use crate::guardian::review_approval_request;
use crate::guardian::routes_approval_to_guardian_with_reviewer;
use crate::hook_runtime::run_permission_request_hooks;
use crate::mcp_tool_approval_templates::render_mcp_tool_approval_template;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use crate::tools::hook_names::HookToolName;
use crate::tools::sandboxing::PermissionRequestPayload;
use crate::turn_metadata::McpTurnMetadataContext;
use codex_analytics::AppInvocation;
use codex_analytics::InvocationType;
use codex_analytics::build_track_events_context;
use codex_config::ConfigLayerSource;
use codex_config::types::AppToolApproval;
use codex_config::types::ApprovalsReviewer;
use codex_connectors::AppToolPolicy;
use codex_connectors::AppToolPolicyEvaluator;
use codex_connectors::AppToolPolicyInput;
pub(crate) use codex_core_approval_policy::MCP_TOOL_APPROVAL_ACCEPT;
pub(crate) use codex_core_approval_policy::MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION;
pub(crate) use codex_core_approval_policy::MCP_TOOL_APPROVAL_DECLINE_SYNTHETIC;
pub(crate) use codex_core_approval_policy::MCP_TOOL_APPROVAL_QUESTION_ID_PREFIX;
use codex_core_approval_policy::McpToolApprovalDecision;
use codex_core_approval_policy::McpToolApprovalElicitationRequest;
use codex_core_approval_policy::McpToolApprovalKey;
pub(crate) use codex_core_approval_policy::McpToolApprovalMetadata;
#[cfg(test)]
use codex_core_approval_policy::McpToolApprovalPromptOptions;
use codex_core_approval_policy::build_mcp_tool_approval_display_params;
use codex_core_approval_policy::build_mcp_tool_approval_elicitation_request;
use codex_core_approval_policy::build_mcp_tool_approval_question;
pub(crate) use codex_core_approval_policy::is_mcp_tool_approval_question_id;
use codex_core_approval_policy::mcp_tool_approval_prompt_options;
use codex_core_approval_policy::normalize_approval_decision_for_mode;
use codex_core_approval_policy::parse_mcp_tool_approval_elicitation_response;
use codex_core_approval_policy::parse_mcp_tool_approval_response;
use codex_core_approval_policy::persistent_mcp_tool_approval_key;
use codex_core_approval_policy::requires_mcp_tool_approval_for_mode;
use codex_core_approval_policy::session_mcp_tool_approval_key;
use codex_core_mcp_openai_file::OpenAiFileUploadContext;
use codex_core_mcp_openai_file::rewrite_mcp_tool_arguments_for_openai_files;
use codex_core_tool_output::sanitize_mcp_tool_result_for_model;
use codex_core_tool_output::truncate_mcp_tool_result_for_event;
use codex_features::Feature;
use codex_hooks::PermissionRequestDecision;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
use codex_mcp::MCP_TOOL_CODEX_APPS_META_KEY;
use codex_mcp::McpConnectionManager;
use codex_mcp::McpPermissionPromptAutoApproveContext;
use codex_mcp::SandboxState;
use codex_mcp::auth_elicitation_completed_result;
use codex_mcp::build_auth_elicitation_plan;
use codex_mcp::mcp_permission_prompt_is_auto_approved;
use codex_mcp::tool_is_model_visible;
use codex_protocol::approvals::ElicitationRequest;
use codex_protocol::items::McpToolCallError;
use codex_protocol::items::McpToolCallItem;
use codex_protocol::items::McpToolCallStatus;
use codex_protocol::items::TurnItem;
use codex_protocol::mcp::CallToolResult;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::McpInvocation;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::request_user_input::RequestUserInputArgs;
use codex_rmcp_client::ElicitationAction;
#[cfg(test)]
use codex_rmcp_client::ElicitationResponse;
use codex_rollout::state_db;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
#[cfg(test)]
use rmcp::model::ToolAnnotations;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::sync::Arc;
use toml_edit::value;
use tracing::Instrument;
use tracing::Span;
use tracing::error;

use codex_core_mcp_runtime::tool_call_telemetry::McpCallMetricOutcome;
use codex_core_mcp_runtime::tool_call_telemetry::McpToolCallSpanFields;
use codex_core_mcp_runtime::tool_call_telemetry::emit_mcp_call_metrics;
use codex_core_mcp_runtime::tool_call_telemetry::mcp_call_metric_outcome;
use codex_core_mcp_runtime::tool_call_telemetry::mcp_tool_call_span;
use codex_core_mcp_runtime::tool_call_telemetry::record_mcp_result_span_telemetry;
use codex_core_mcp_runtime::tool_item_metadata::McpToolCallItemMetadata;
use codex_core_mcp_runtime::tool_metadata::get_mcp_app_resource_uri;
use codex_core_mcp_runtime::tool_metadata::openai_file_input_optional_fields_for_server;
use codex_core_mcp_runtime::tool_request_metadata::build_mcp_tool_call_request_meta;
use codex_core_mcp_runtime::tool_request_metadata::with_mcp_tool_call_thread_id_meta;
const MCP_TOOL_CALL_EVENT_RESULT_MAX_BYTES: usize = codex_utils_pty::DEFAULT_OUTPUT_BYTES_CAP;

/// Handles the specified tool call and dispatches the appropriate MCP tool-call
/// item lifecycle events to the `Session`.
pub(crate) async fn handle_mcp_tool_call(
    sess: Arc<Session>,
    step_context: &Arc<StepContext>,
    call_id: String,
    server: String,
    tool_name: String,
    hook_tool_name: HookToolName,
    arguments: String,
) -> HandledMcpToolCall {
    let turn_context = &step_context.turn;
    let manager = step_context.mcp.manager();
    // Parse the `arguments` as JSON. An empty string is OK, but invalid JSON
    // is not.
    let arguments_value = if arguments.trim().is_empty() {
        None
    } else {
        match serde_json::from_str::<serde_json::Value>(&arguments) {
            Ok(value) => Some(value),
            Err(e) => {
                error!("failed to parse tool call arguments: {e}");
                return HandledMcpToolCall {
                    result: CallToolResult::from_error_text(format!("err: {e}")),
                    tool_input: JsonValue::Object(serde_json::Map::new()),
                };
            }
        }
    };

    let invocation = McpInvocation {
        server: server.clone(),
        tool: tool_name.clone(),
        arguments: arguments_value.clone(),
    };

    let metadata = lookup_mcp_tool_metadata(
        sess.as_ref(),
        turn_context.as_ref(),
        manager,
        &server,
        &tool_name,
    )
    .await;
    let item_metadata = McpToolCallItemMetadata::from_tool_metadata(&server, metadata.as_ref());
    if metadata.is_none() {
        let result = notify_mcp_tool_call_skip(
            sess.as_ref(),
            turn_context.as_ref(),
            &call_id,
            invocation,
            item_metadata,
            format!("MCP tool `{server}/{tool_name}` is not available to the model"),
            /*already_started*/ false,
        )
        .await;
        return HandledMcpToolCall {
            result: CallToolResult::from_result(result),
            tool_input: arguments_value
                .unwrap_or_else(|| JsonValue::Object(serde_json::Map::new())),
        };
    }
    let app_tool_policy = if server == CODEX_APPS_MCP_SERVER_NAME {
        let annotations = metadata
            .as_ref()
            .and_then(|metadata| metadata.annotations.as_ref());
        AppToolPolicyEvaluator::new(&turn_context.config.config_layer_stack).policy(
            AppToolPolicyInput {
                connector_id: metadata
                    .as_ref()
                    .and_then(|metadata| metadata.connector_id.as_deref()),
                tool_name: &tool_name,
                tool_title: metadata
                    .as_ref()
                    .and_then(|metadata| metadata.tool_title.as_deref()),
                destructive_hint: annotations.and_then(|annotations| annotations.destructive_hint),
                open_world_hint: annotations.and_then(|annotations| annotations.open_world_hint),
            },
        )
    } else {
        AppToolPolicy::default()
    };
    let approval_mode = if server == CODEX_APPS_MCP_SERVER_NAME {
        app_tool_policy.approval
    } else if let Some(approval_mode) = {
        // Selected-plugin registrations are absent from config.toml and the legacy plugin manager,
        // so their resolved catalog entry is the authoritative source for tool approval policy.
        manager
            .is_selected_plugin_mcp_server(&server)
            .then(|| manager.tool_approval_mode(&server, &tool_name))
    } {
        approval_mode
    } else {
        custom_mcp_tool_approval_mode(sess.as_ref(), turn_context.as_ref(), &server, &tool_name)
            .await
    };

    let connector_id = metadata
        .as_ref()
        .and_then(|metadata| metadata.connector_id.clone());
    let connector_name = metadata
        .as_ref()
        .and_then(|metadata| metadata.connector_name.clone());

    if server == CODEX_APPS_MCP_SERVER_NAME && !app_tool_policy.enabled {
        let result = notify_mcp_tool_call_skip(
            sess.as_ref(),
            turn_context.as_ref(),
            &call_id,
            invocation,
            item_metadata.clone(),
            "MCP tool call blocked by app configuration".to_string(),
            /*already_started*/ false,
        )
        .await;
        let status = if result.is_ok() { "ok" } else { "error" };
        let outcome = McpCallMetricOutcome::from_status(status);
        emit_mcp_call_metrics(
            &turn_context.session_telemetry,
            &outcome,
            &server,
            &tool_name,
            connector_id.as_deref(),
            connector_name.as_deref(),
            /*duration*/ None,
        );
        return HandledMcpToolCall {
            result: CallToolResult::from_result(result),
            tool_input: arguments_value
                .unwrap_or_else(|| JsonValue::Object(serde_json::Map::new())),
        };
    }
    notify_mcp_tool_call_started(
        sess.as_ref(),
        turn_context.as_ref(),
        &call_id,
        invocation.clone(),
        item_metadata.clone(),
    )
    .await;

    if let Some(decision) = maybe_request_mcp_tool_approval(
        &sess,
        step_context,
        &call_id,
        &invocation,
        &hook_tool_name,
        metadata.as_ref(),
        approval_mode,
    )
    .await
    {
        let result = match decision {
            McpToolApprovalDecision::Accept
            | McpToolApprovalDecision::AcceptForSession
            | McpToolApprovalDecision::AcceptAndRemember => {
                return handle_approved_mcp_tool_call(
                    sess.as_ref(),
                    step_context.as_ref(),
                    &call_id,
                    invocation,
                    metadata.as_ref(),
                    item_metadata,
                )
                .await;
            }
            McpToolApprovalDecision::Decline { message } => {
                let message = message.unwrap_or_else(|| "user rejected MCP tool call".to_string());
                notify_mcp_tool_call_skip(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &call_id,
                    invocation,
                    item_metadata.clone(),
                    message,
                    /*already_started*/ true,
                )
                .await
            }
            McpToolApprovalDecision::Cancel => {
                let message = "user cancelled MCP tool call".to_string();
                notify_mcp_tool_call_skip(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &call_id,
                    invocation,
                    item_metadata.clone(),
                    message,
                    /*already_started*/ true,
                )
                .await
            }
        };

        let status = if result.is_ok() { "ok" } else { "error" };
        let outcome = McpCallMetricOutcome::from_status(status);
        emit_mcp_call_metrics(
            &turn_context.session_telemetry,
            &outcome,
            &server,
            &tool_name,
            connector_id.as_deref(),
            connector_name.as_deref(),
            /*duration*/ None,
        );

        return HandledMcpToolCall {
            result: CallToolResult::from_result(result),
            tool_input: arguments_value
                .unwrap_or_else(|| JsonValue::Object(serde_json::Map::new())),
        };
    }

    handle_approved_mcp_tool_call(
        sess.as_ref(),
        step_context.as_ref(),
        &call_id,
        invocation,
        metadata.as_ref(),
        item_metadata,
    )
    .await
}

pub(crate) struct HandledMcpToolCall {
    pub(crate) result: CallToolResult,
    pub(crate) tool_input: JsonValue,
}

async fn handle_approved_mcp_tool_call(
    sess: &Session,
    step_context: &StepContext,
    call_id: &str,
    invocation: McpInvocation,
    metadata: Option<&McpToolApprovalMetadata>,
    item_metadata: McpToolCallItemMetadata,
) -> HandledMcpToolCall {
    let turn_context = step_context.turn.as_ref();
    let manager = step_context.mcp.manager();
    let server = invocation.server.clone();
    maybe_mark_thread_memory_mode_polluted(sess, turn_context, manager, &server).await;
    let tool_name = invocation.tool.clone();
    let arguments_value = invocation.arguments.clone();
    let connector_id = metadata.and_then(|metadata| metadata.connector_id.as_deref());
    let connector_name = metadata.and_then(|metadata| metadata.connector_name.as_deref());
    let server_origin = manager.server_origin(&server).map(str::to_string);

    let start = Instant::now();
    let auth = sess.services.auth_manager.auth().await;
    let http_client_factory = turn_context.config.http_client_factory();
    let file_upload_context = OpenAiFileUploadContext {
        auth: auth.as_ref(),
        primary_environment: turn_context.environments.primary(),
        chatgpt_base_url: turn_context.config.chatgpt_base_url.as_str(),
        http_client_factory: &http_client_factory,
    };
    let rewrite = rewrite_mcp_tool_arguments_for_openai_files(
        &file_upload_context,
        arguments_value.clone(),
        metadata.and_then(|metadata| metadata.openai_file_input_optional_fields.as_ref()),
    )
    .await;
    let tool_input = match &rewrite {
        Ok(Some(rewritten_arguments)) => rewritten_arguments.clone(),
        Ok(None) | Err(_) => arguments_value
            .clone()
            .unwrap_or_else(|| JsonValue::Object(serde_json::Map::new())),
    };
    let result = async {
        let result = async {
            let rewritten_arguments = rewrite?;
            let request_meta = build_mcp_tool_call_request_meta(
                crate::X_CODEX_TURN_METADATA_HEADER,
                turn_context
                    .turn_metadata_state
                    .current_meta_value_for_mcp_request(McpTurnMetadataContext {
                        model: turn_context.model_info.slug.as_str(),
                        reasoning_effort: turn_context.effective_reasoning_effort(),
                    }),
                &server,
                call_id,
                metadata,
            );
            execute_mcp_tool_call(
                sess,
                step_context,
                call_id,
                &invocation,
                rewritten_arguments,
                metadata,
                request_meta,
            )
            .await
        }
        .await;
        record_mcp_result_span_telemetry(&Span::current(), &result);
        result
    }
    .instrument(mcp_tool_call_span(
        &sess.thread_id,
        &sess.thread_id,
        turn_context.sub_id.as_str(),
        McpToolCallSpanFields {
            server_name: &server,
            tool_name: &tool_name,
            call_id,
            server_origin: server_origin.as_deref(),
            connector_id,
            connector_name,
        },
    ))
    .await;
    if let Err(error) = &result {
        tracing::warn!("MCP tool call error: {error:?}");
    }
    let duration = start.elapsed();
    notify_mcp_tool_call_completed(
        sess,
        turn_context,
        call_id,
        invocation,
        item_metadata,
        duration,
        truncate_mcp_tool_result_for_event(&result, MCP_TOOL_CALL_EVENT_RESULT_MAX_BYTES),
    )
    .await;
    maybe_track_codex_app_used(sess, turn_context, manager, &server, &tool_name).await;

    let outcome = mcp_call_metric_outcome(&result);
    emit_mcp_call_metrics(
        &turn_context.session_telemetry,
        &outcome,
        &server,
        &tool_name,
        connector_id,
        connector_name,
        Some(duration),
    );

    HandledMcpToolCall {
        result: CallToolResult::from_result(result),
        tool_input,
    }
}

async fn execute_mcp_tool_call(
    sess: &Session,
    step_context: &StepContext,
    call_id: &str,
    invocation: &McpInvocation,
    rewritten_arguments: Option<JsonValue>,
    metadata: Option<&McpToolApprovalMetadata>,
    request_meta: Option<JsonValue>,
) -> Result<CallToolResult, String> {
    let turn_context = step_context.turn.as_ref();
    let manager = step_context.mcp.manager();
    let request_meta = with_mcp_tool_call_thread_id_meta(request_meta, &sess.thread_id.to_string());
    let request_meta = augment_mcp_tool_request_meta_with_sandbox_state(
        step_context,
        manager,
        &invocation.server,
        request_meta,
    )
    .await
    .map_err(|e| format!("failed to build MCP tool request metadata: {e:#}"))?;
    let mcp_call_trace = sess
        .services
        .rollout_thread_trace
        .start_mcp_call_trace(call_id);
    let request_meta = mcp_call_trace.add_request_meta(request_meta);
    let result = manager
        .call_tool(
            &invocation.server,
            &invocation.tool,
            rewritten_arguments,
            request_meta,
        )
        .await
        .map_err(|e| format!("tool call error: {e:?}"))?;
    let result =
        sanitize_mcp_tool_result_for_model(&turn_context.model_info.input_modalities, Ok(result))?;
    Ok(maybe_request_codex_apps_auth_elicitation(
        sess,
        turn_context,
        manager,
        call_id,
        &invocation.server,
        metadata,
        result,
    )
    .await)
}

async fn maybe_request_codex_apps_auth_elicitation(
    sess: &Session,
    turn_context: &TurnContext,
    manager: &McpConnectionManager,
    call_id: &str,
    server: &str,
    metadata: Option<&McpToolApprovalMetadata>,
    result: CallToolResult,
) -> CallToolResult {
    if !manager.is_host_owned_codex_apps_server(server) {
        return result;
    }

    if !turn_context
        .config
        .features
        .enabled(Feature::AuthElicitation)
    {
        return result;
    }

    match turn_context.approval_policy.value() {
        AskForApproval::Never => return result,
        AskForApproval::Granular(granular_config) if !granular_config.allows_mcp_elicitations() => {
            return result;
        }
        AskForApproval::OnRequest | AskForApproval::UnlessTrusted | AskForApproval::Granular(_) => {
        }
    }

    let connector_id = metadata.and_then(|metadata| metadata.connector_id.as_deref());
    let connector_name = metadata.and_then(|metadata| metadata.connector_name.as_deref());
    let install_url = connector_id.map(|connector_id| {
        codex_connectors::metadata::connector_install_url(
            connector_name.unwrap_or(connector_id),
            connector_id,
        )
    });
    let Some(plan) =
        build_auth_elicitation_plan(call_id, &result, connector_id, connector_name, install_url)
    else {
        return result;
    };

    let request_id = rmcp::model::RequestId::String(plan.elicitation.elicitation_id.clone().into());
    let request = ElicitationRequest::Url {
        meta: Some(plan.elicitation.meta),
        message: plan.elicitation.message,
        url: plan.elicitation.url,
        elicitation_id: plan.elicitation.elicitation_id,
    };
    let response = sess
        .request_mcp_server_elicitation(
            turn_context,
            CODEX_APPS_MCP_SERVER_NAME.to_string(),
            request_id,
            request,
        )
        .await
        .response;
    if !response
        .as_ref()
        .is_some_and(|response| response.action == ElicitationAction::Accept)
    {
        return result;
    }

    refresh_codex_apps_after_connector_auth(sess, turn_context, manager).await;
    auth_elicitation_completed_result(&plan.auth_failure, result.meta)
}

async fn refresh_codex_apps_after_connector_auth(
    sess: &Session,
    turn_context: &TurnContext,
    manager: &McpConnectionManager,
) {
    let mcp_tools_result = manager.hard_refresh_codex_apps_tools_cache().await;

    match mcp_tools_result {
        Ok(mcp_tools) => {
            let auth = sess.services.auth_manager.auth().await;
            connectors::refresh_accessible_connectors_cache_from_mcp_tools(
                &turn_context.config,
                auth.as_ref(),
                &mcp_tools,
            );
        }
        Err(err) => {
            tracing::warn!("failed to refresh Codex Apps tools after connector auth: {err:#}");
        }
    }
}

async fn augment_mcp_tool_request_meta_with_sandbox_state(
    step_context: &StepContext,
    manager: &McpConnectionManager,
    server: &str,
    mut meta: Option<serde_json::Value>,
) -> anyhow::Result<Option<serde_json::Value>> {
    let turn_context = step_context.turn.as_ref();
    let supports_sandbox_state_meta = manager
        .server_supports_sandbox_state_meta_capability(server)
        .await
        .unwrap_or(false);
    if !supports_sandbox_state_meta {
        return Ok(meta);
    }

    let server_environment_id = manager
        .server_environment_id(server)
        .unwrap_or(codex_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID);
    let Some(sandbox_cwd) = sandbox_cwd_for_mcp_server(step_context, server_environment_id) else {
        return Ok(meta);
    };
    let permission_profile = turn_context.permission_profile();
    let sandbox_state = serde_json::to_value(SandboxState {
        permission_profile,
        codex_linux_sandbox_exe: step_context.mcp.config().codex_linux_sandbox_exe.clone(),
        sandbox_cwd,
        use_legacy_landlock: step_context.mcp.config().use_legacy_landlock,
    })?;

    match meta.as_mut() {
        Some(serde_json::Value::Object(map)) => {
            map.insert(
                codex_mcp::MCP_SANDBOX_STATE_META_CAPABILITY.to_string(),
                sandbox_state,
            );
        }
        Some(_) => {}
        None => {
            let mut map = serde_json::Map::new();
            map.insert(
                codex_mcp::MCP_SANDBOX_STATE_META_CAPABILITY.to_string(),
                sandbox_state,
            );
            meta = Some(serde_json::Value::Object(map));
        }
    }

    Ok(meta)
}

fn sandbox_cwd_for_mcp_server(step_context: &StepContext, environment_id: &str) -> Option<PathUri> {
    if let Some(environment) = step_context
        .environments
        .turn_environments()
        .find(|environment| environment.environment_id == environment_id)
    {
        return Some(environment.cwd().clone());
    }

    if environment_id == codex_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID {
        #[allow(deprecated)]
        return Some(PathUri::from_abs_path(&step_context.turn.cwd));
    }

    None
}

async fn maybe_mark_thread_memory_mode_polluted(
    sess: &Session,
    turn_context: &TurnContext,
    manager: &McpConnectionManager,
    server: &str,
) {
    if !turn_context.config.memories.disable_on_external_context {
        return;
    }
    let pollutes_memory = manager.server_pollutes_memory(server);
    if !pollutes_memory {
        return;
    }
    state_db::mark_thread_memory_mode_polluted(
        sess.services.state_db.as_deref(),
        sess.thread_id,
        "mcp_tool_call",
    )
    .await;
}

async fn notify_mcp_tool_call_started(
    sess: &Session,
    turn_context: &TurnContext,
    call_id: &str,
    invocation: McpInvocation,
    item_metadata: McpToolCallItemMetadata,
) {
    let McpInvocation {
        server,
        tool,
        arguments,
    } = invocation;
    let item = TurnItem::McpToolCall(McpToolCallItem {
        id: call_id.to_string(),
        server,
        tool,
        arguments: arguments.unwrap_or(JsonValue::Null),
        connector_id: item_metadata.connector_id,
        mcp_app_resource_uri: item_metadata.mcp_app_resource_uri,
        link_id: item_metadata.link_id,
        app_name: item_metadata.app_name,
        action_name: item_metadata.action_name,
        plugin_id: item_metadata.plugin_id,
        status: McpToolCallStatus::InProgress,
        result: None,
        error: None,
        duration: None,
    });
    sess.emit_turn_item_started(turn_context, &item).await;
}

async fn notify_mcp_tool_call_completed(
    sess: &Session,
    turn_context: &TurnContext,
    call_id: &str,
    invocation: McpInvocation,
    item_metadata: McpToolCallItemMetadata,
    duration: Duration,
    result: Result<CallToolResult, String>,
) {
    let (status, result, error) = match result {
        Ok(result) if result.is_error.unwrap_or(false) => {
            (McpToolCallStatus::Failed, Some(result), None)
        }
        Ok(result) => (McpToolCallStatus::Completed, Some(result), None),
        Err(message) => (
            McpToolCallStatus::Failed,
            None,
            Some(McpToolCallError { message }),
        ),
    };
    let McpInvocation {
        server,
        tool,
        arguments,
    } = invocation;
    let item = TurnItem::McpToolCall(McpToolCallItem {
        id: call_id.to_string(),
        server,
        tool,
        arguments: arguments.unwrap_or(JsonValue::Null),
        connector_id: item_metadata.connector_id,
        mcp_app_resource_uri: item_metadata.mcp_app_resource_uri,
        link_id: item_metadata.link_id,
        app_name: item_metadata.app_name,
        action_name: item_metadata.action_name,
        plugin_id: item_metadata.plugin_id,
        status,
        result,
        error,
        duration: Some(duration),
    });
    sess.emit_turn_item_completed(turn_context, item).await;
}

struct McpAppUsageMetadata {
    connector_id: Option<String>,
    app_name: Option<String>,
}

async fn maybe_track_codex_app_used(
    sess: &Session,
    turn_context: &TurnContext,
    manager: &McpConnectionManager,
    server: &str,
    tool_name: &str,
) {
    if server != CODEX_APPS_MCP_SERVER_NAME {
        return;
    }
    let metadata = lookup_mcp_app_usage_metadata(manager, server, tool_name).await;
    let (connector_id, app_name) = metadata
        .map(|metadata| (metadata.connector_id, metadata.app_name))
        .unwrap_or((None, None));
    let invocation_type = if let Some(connector_id) = connector_id.as_deref() {
        let mentioned_connector_ids = sess.get_connector_selection().await;
        if mentioned_connector_ids.contains(connector_id) {
            InvocationType::Explicit
        } else {
            InvocationType::Implicit
        }
    } else {
        InvocationType::Implicit
    };

    let tracking = build_track_events_context(
        turn_context.model_info.slug.clone(),
        sess.thread_id.to_string(),
        turn_context.sub_id.clone(),
        turn_context.originator.clone(),
    );
    sess.services.analytics_events_client.track_app_used(
        tracking,
        AppInvocation {
            connector_id,
            app_name,
            invocation_type: Some(invocation_type),
        },
    );
}

const MCP_TOOL_LINK_ID_META_KEY: &str = "link_id";
const MCP_TOOL_CONNECTED_ACCOUNT_EMAIL_META_KEY: &str = "connected_account_email";
async fn custom_mcp_tool_approval_mode(
    sess: &Session,
    turn_context: &TurnContext,
    server: &str,
    tool_name: &str,
) -> AppToolApproval {
    let user_configured_mode = turn_context
        .config
        .config_layer_stack
        .effective_config()
        .as_table()
        .and_then(|table| table.get("mcp_servers"))
        .cloned()
        .and_then(|value| {
            HashMap::<String, codex_config::types::McpServerConfig>::deserialize(value).ok()
        })
        .and_then(|servers| {
            let server_config = servers.get(server)?;
            Some(
                server_config
                    .tools
                    .get(tool_name)
                    .and_then(|tool| tool.approval_mode)
                    .or(server_config.default_tools_approval_mode)
                    .unwrap_or_default(),
            )
        });
    if let Some(user_configured_mode) = user_configured_mode {
        return user_configured_mode;
    }

    sess.services
        .plugins_manager
        .plugins_for_config(&turn_context.config.plugins_config_input())
        .await
        .plugins()
        .iter()
        .filter(|plugin| plugin.is_active())
        .find_map(|plugin| {
            let server_config = plugin.mcp_servers.get(server)?;
            server_config
                .tools
                .get(tool_name)
                .and_then(|tool| tool.approval_mode)
                .or(server_config.default_tools_approval_mode)
        })
        .unwrap_or_default()
}

async fn maybe_request_mcp_tool_approval(
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    call_id: &str,
    invocation: &McpInvocation,
    hook_tool_name: &HookToolName,
    metadata: Option<&McpToolApprovalMetadata>,
    approval_mode: AppToolApproval,
) -> Option<McpToolApprovalDecision> {
    let turn_context = &step_context.turn;
    let manager = step_context.mcp.manager();
    let approvals_reviewer = mcp_approvals_reviewer(turn_context, &invocation.server, metadata);
    if mcp_permission_prompt_is_auto_approved(
        turn_context.approval_policy.value(),
        &turn_context.permission_profile(),
        McpPermissionPromptAutoApproveContext {
            tool_approval_mode: Some(approval_mode),
        },
    ) {
        return None;
    }

    let annotations = metadata.and_then(|metadata| metadata.annotations.as_ref());
    if !requires_mcp_tool_approval_for_mode(annotations, approval_mode) {
        return None;
    }

    let session_approval_key = session_mcp_tool_approval_key(invocation, metadata, approval_mode);
    let persistent_approval_key = if manager.is_selected_plugin_mcp_server(&invocation.server) {
        None
    } else {
        persistent_mcp_tool_approval_key(invocation, metadata, approval_mode)
    };
    if let Some(key) = session_approval_key.as_ref()
        && mcp_tool_approval_is_remembered(sess, key).await
    {
        return Some(McpToolApprovalDecision::Accept);
    }

    match run_permission_request_hooks(
        sess,
        turn_context,
        call_id,
        PermissionRequestPayload {
            tool_name: hook_tool_name.clone(),
            tool_input: invocation
                .arguments
                .clone()
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        },
    )
    .await
    {
        Some(PermissionRequestDecision::Allow) => {
            return Some(McpToolApprovalDecision::Accept);
        }
        Some(PermissionRequestDecision::Deny { message }) => {
            return Some(McpToolApprovalDecision::Decline {
                message: Some(message),
            });
        }
        None => {}
    }

    let tool_call_mcp_elicitation_enabled = turn_context
        .config
        .features
        .enabled(Feature::ToolCallMcpElicitation);

    if routes_approval_to_guardian_with_reviewer(turn_context, approvals_reviewer) {
        let review_id = new_guardian_review_id();
        let decision = review_approval_request(
            sess,
            turn_context,
            review_id.clone(),
            build_guardian_mcp_tool_review_request(call_id, invocation, metadata),
            /*retry_reason*/ None,
        )
        .await;
        let decision = mcp_tool_approval_decision_from_guardian(decision);
        apply_mcp_tool_approval_decision(
            sess,
            turn_context,
            &decision,
            session_approval_key,
            persistent_approval_key,
        )
        .await;
        return Some(decision);
    }

    let prompt_options = mcp_tool_approval_prompt_options(
        session_approval_key.as_ref(),
        persistent_approval_key.as_ref(),
        tool_call_mcp_elicitation_enabled,
    );
    let question_id = format!("{MCP_TOOL_APPROVAL_QUESTION_ID_PREFIX}_{call_id}");
    let rendered_template = render_mcp_tool_approval_template(
        &invocation.server,
        metadata.and_then(|metadata| metadata.connector_id.as_deref()),
        metadata.and_then(|metadata| metadata.connector_name.as_deref()),
        metadata.and_then(|metadata| metadata.tool_title.as_deref()),
        invocation.arguments.as_ref(),
    );
    let tool_params_display = rendered_template
        .as_ref()
        .map(|rendered_template| rendered_template.tool_params_display.clone())
        .or_else(|| build_mcp_tool_approval_display_params(invocation.arguments.as_ref()));
    let question = build_mcp_tool_approval_question(
        question_id.clone(),
        &invocation.server,
        &invocation.tool,
        metadata.and_then(|metadata| metadata.connector_name.as_deref()),
        prompt_options,
        rendered_template
            .as_ref()
            .map(|rendered_template| rendered_template.question.as_str()),
    );
    if tool_call_mcp_elicitation_enabled {
        let request_id = rmcp::model::RequestId::String(
            format!("{MCP_TOOL_APPROVAL_QUESTION_ID_PREFIX}_{call_id}").into(),
        );
        let request =
            build_mcp_tool_approval_elicitation_request(McpToolApprovalElicitationRequest {
                server: &invocation.server,
                metadata,
                tool_params: rendered_template
                    .as_ref()
                    .and_then(|rendered_template| rendered_template.tool_params.as_ref())
                    .or(invocation.arguments.as_ref()),
                tool_params_display: tool_params_display.as_deref(),
                question,
                message_override: rendered_template
                    .as_ref()
                    .map(|rendered_template| rendered_template.elicitation_message.as_str()),
                prompt_options,
            });
        let decision = parse_mcp_tool_approval_elicitation_response(
            sess.request_mcp_server_elicitation(
                turn_context.as_ref(),
                invocation.server.clone(),
                request_id,
                request,
            )
            .await
            .response,
            &question_id,
        );
        let decision = normalize_approval_decision_for_mode(decision, approval_mode);
        apply_mcp_tool_approval_decision(
            sess,
            turn_context,
            &decision,
            session_approval_key,
            persistent_approval_key,
        )
        .await;
        return Some(decision);
    }

    let args = RequestUserInputArgs {
        questions: vec![question],
        auto_resolution_ms: None,
    };
    let response = sess
        .request_user_input(turn_context.as_ref(), call_id.to_string(), args)
        .await;
    let decision = normalize_approval_decision_for_mode(
        parse_mcp_tool_approval_response(response, &question_id),
        approval_mode,
    );
    apply_mcp_tool_approval_decision(
        sess,
        turn_context,
        &decision,
        session_approval_key,
        persistent_approval_key,
    )
    .await;
    Some(decision)
}

pub(crate) fn mcp_approvals_reviewer(
    turn_context: &TurnContext,
    server_name: &str,
    metadata: Option<&McpToolApprovalMetadata>,
) -> ApprovalsReviewer {
    connectors::mcp_approvals_reviewer(
        turn_context.config.as_ref(),
        server_name,
        metadata.and_then(|metadata| metadata.connector_id.as_deref()),
    )
}

pub(crate) fn build_guardian_mcp_tool_review_request(
    call_id: &str,
    invocation: &McpInvocation,
    metadata: Option<&McpToolApprovalMetadata>,
) -> GuardianApprovalRequest {
    GuardianApprovalRequest::McpToolCall {
        id: call_id.to_string(),
        server: invocation.server.clone(),
        tool_name: invocation.tool.clone(),
        arguments: invocation.arguments.clone(),
        connector_id: metadata.and_then(|metadata| metadata.connector_id.clone()),
        connector_name: metadata.and_then(|metadata| metadata.connector_name.clone()),
        connector_description: metadata.and_then(|metadata| metadata.connector_description.clone()),
        connected_account_email: (invocation.server == CODEX_APPS_MCP_SERVER_NAME)
            .then(|| metadata.and_then(|metadata| metadata.connected_account_email.clone()))
            .flatten(),
        tool_title: metadata.and_then(|metadata| metadata.tool_title.clone()),
        tool_description: metadata.and_then(|metadata| metadata.tool_description.clone()),
        annotations: metadata
            .and_then(|metadata| metadata.annotations.as_ref())
            .map(|annotations| GuardianMcpAnnotations {
                destructive_hint: annotations.destructive_hint,
                open_world_hint: annotations.open_world_hint,
                read_only_hint: annotations.read_only_hint,
            }),
    }
}

fn mcp_tool_approval_decision_from_guardian(decision: ReviewDecision) -> McpToolApprovalDecision {
    match decision {
        ReviewDecision::Approved
        | ReviewDecision::ApprovedExecpolicyAmendment { .. }
        | ReviewDecision::NetworkPolicyAmendment { .. } => McpToolApprovalDecision::Accept,
        ReviewDecision::ApprovedForSession => McpToolApprovalDecision::AcceptForSession,
        ReviewDecision::Denied { rejection } => McpToolApprovalDecision::Decline {
            message: Some(rejection),
        },
        ReviewDecision::TimedOut => McpToolApprovalDecision::Decline {
            message: Some(crate::guardian::guardian_timeout_message()),
        },
        ReviewDecision::Abort => McpToolApprovalDecision::Decline { message: None },
    }
}

pub(crate) async fn lookup_mcp_tool_metadata(
    sess: &Session,
    turn_context: &TurnContext,
    manager: &McpConnectionManager,
    server: &str,
    tool_name: &str,
) -> Option<McpToolApprovalMetadata> {
    let plugin_id = manager
        .plugin_id_for_mcp_server_name(server)
        .map(str::to_string);
    let tool_info = manager.tool_info(server, tool_name).await?;
    if !tool_is_model_visible(&tool_info) {
        return None;
    }
    let connector_description = if server == CODEX_APPS_MCP_SERVER_NAME {
        let connectors = match connectors::list_cached_accessible_connectors_from_mcp_tools(
            turn_context.config.as_ref(),
        )
        .await
        {
            Some(connectors) => Some(connectors),
            None => {
                connectors::list_accessible_connectors_from_mcp_tools_with_mcp_manager(
                    turn_context.config.as_ref(),
                    /*force_refetch*/ false,
                    sess.services.turn_environments.environment_manager(),
                    Arc::clone(&sess.services.mcp_manager),
                )
                .await
                .ok()
                .map(|status| status.connectors)
            }
        };
        connectors.and_then(|connectors| {
            let connector_id = tool_info.connector_id.as_deref()?;
            connectors
                .into_iter()
                .find(|connector| connector.id == connector_id)
                .and_then(|connector| connector.description)
        })
    } else {
        None
    };

    let codex_apps_meta = tool_info
        .tool
        .meta
        .as_ref()
        .and_then(|meta| meta.get(MCP_TOOL_CODEX_APPS_META_KEY))
        .and_then(serde_json::Value::as_object)
        .cloned();
    let connected_account_email = if server == CODEX_APPS_MCP_SERVER_NAME {
        codex_apps_meta
            .as_ref()
            .and_then(|meta| meta.get(MCP_TOOL_CONNECTED_ACCOUNT_EMAIL_META_KEY))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|email| !email.is_empty())
            .map(str::to_string)
    } else {
        None
    };

    Some(McpToolApprovalMetadata {
        annotations: tool_info.tool.annotations,
        connector_id: tool_info.connector_id,
        link_id: tool_info
            .tool
            .meta
            .as_ref()
            .and_then(|meta| meta.get(MCP_TOOL_LINK_ID_META_KEY))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        connector_name: tool_info.connector_name,
        connector_description,
        connected_account_email,
        plugin_id,
        tool_title: tool_info.tool.title,
        tool_description: tool_info.tool.description.map(std::borrow::Cow::into_owned),
        mcp_app_resource_uri: get_mcp_app_resource_uri(tool_info.tool.meta.as_deref()),
        codex_apps_meta,
        // Disallow custom MCPs from uploading files via fileParams.
        openai_file_input_optional_fields: openai_file_input_optional_fields_for_server(
            server,
            &tool_info.openai_file_input_optional_fields,
        ),
    })
}

async fn lookup_mcp_app_usage_metadata(
    manager: &McpConnectionManager,
    server: &str,
    tool_name: &str,
) -> Option<McpAppUsageMetadata> {
    let tool_info = manager.tool_info(server, tool_name).await?;
    Some(McpAppUsageMetadata {
        connector_id: tool_info.connector_id,
        app_name: tool_info.connector_name,
    })
}

async fn mcp_tool_approval_is_remembered(sess: &Session, key: &McpToolApprovalKey) -> bool {
    let store = sess.services.tool_approvals.lock().await;
    matches!(store.get(key), Some(ReviewDecision::ApprovedForSession))
}

async fn remember_mcp_tool_approval(sess: &Session, key: McpToolApprovalKey) {
    let mut store = sess.services.tool_approvals.lock().await;
    store.put(key, ReviewDecision::ApprovedForSession);
}

async fn apply_mcp_tool_approval_decision(
    sess: &Session,
    turn_context: &TurnContext,
    decision: &McpToolApprovalDecision,
    session_approval_key: Option<McpToolApprovalKey>,
    persistent_approval_key: Option<McpToolApprovalKey>,
) {
    match decision {
        McpToolApprovalDecision::AcceptForSession => {
            if let Some(key) = session_approval_key {
                remember_mcp_tool_approval(sess, key).await;
            }
        }
        McpToolApprovalDecision::AcceptAndRemember => {
            if let Some(key) = persistent_approval_key {
                maybe_persist_mcp_tool_approval(sess, turn_context, key).await;
            } else if let Some(key) = session_approval_key {
                remember_mcp_tool_approval(sess, key).await;
            }
        }
        McpToolApprovalDecision::Accept
        | McpToolApprovalDecision::Decline { .. }
        | McpToolApprovalDecision::Cancel => {}
    }
}

async fn maybe_persist_mcp_tool_approval(
    sess: &Session,
    turn_context: &TurnContext,
    key: McpToolApprovalKey,
) {
    let tool_name = key.tool_name.clone();

    let persist_result = if key.server == CODEX_APPS_MCP_SERVER_NAME {
        let Some(connector_id) = key.connector_id.clone() else {
            remember_mcp_tool_approval(sess, key).await;
            return;
        };
        persist_codex_app_tool_approval(&turn_context.config, &connector_id, &tool_name).await
    } else {
        persist_non_app_mcp_tool_approval(sess, &turn_context.config, &key.server, &tool_name).await
    };

    if let Err(err) = persist_result {
        error!(
            error = %err,
            server = key.server,
            tool_name,
            "failed to persist MCP tool approval"
        );
        remember_mcp_tool_approval(sess, key).await;
        return;
    }

    sess.reload_user_config_layer().await;
    remember_mcp_tool_approval(sess, key).await;
}

async fn persist_codex_app_tool_approval(
    config: &Config,
    connector_id: &str,
    tool_name: &str,
) -> anyhow::Result<()> {
    ConfigEditsBuilder::for_config(config)
        .with_edits([ConfigEdit::SetPath {
            segments: vec![
                "apps".to_string(),
                connector_id.to_string(),
                "tools".to_string(),
                tool_name.to_string(),
                "approval_mode".to_string(),
            ],
            value: value("approve"),
        }])
        .apply()
        .await
}

#[cfg(test)]
async fn persist_custom_mcp_tool_approval(
    config: &Config,
    server: &str,
    tool_name: &str,
) -> anyhow::Result<()> {
    let Some(config_edits_builder) = custom_mcp_tool_approval_config_builder(config, server)?
    else {
        anyhow::bail!("MCP server `{server}` is not configured in config.toml");
    };

    persist_custom_mcp_tool_approval_with(config_edits_builder, server, tool_name).await
}

async fn persist_non_app_mcp_tool_approval(
    sess: &Session,
    config: &Config,
    server: &str,
    tool_name: &str,
) -> anyhow::Result<()> {
    if let Some(config_edits_builder) = custom_mcp_tool_approval_config_builder(config, server)? {
        return persist_custom_mcp_tool_approval_with(config_edits_builder, server, tool_name)
            .await;
    }

    let plugin_config_name = sess
        .services
        .plugins_manager
        .plugins_for_config(&config.plugins_config_input())
        .await
        .plugins()
        .iter()
        .filter(|plugin| plugin.is_active())
        .find(|plugin| plugin.mcp_servers.contains_key(server))
        .map(|plugin| plugin.config_name.clone());

    if let Some(plugin_config_name) = plugin_config_name {
        return ConfigEditsBuilder::for_config(config)
            .with_edits([ConfigEdit::SetPath {
                segments: vec![
                    "plugins".to_string(),
                    plugin_config_name,
                    "mcp_servers".to_string(),
                    server.to_string(),
                    "tools".to_string(),
                    tool_name.to_string(),
                    "approval_mode".to_string(),
                ],
                value: value("approve"),
            }])
            .apply()
            .await;
    }

    anyhow::bail!("MCP server `{server}` is not configured in config.toml or an enabled plugin")
}

fn custom_mcp_tool_approval_config_builder(
    config: &Config,
    server: &str,
) -> anyhow::Result<Option<ConfigEditsBuilder>> {
    if let Some(project_config_folder) = project_mcp_tool_approval_config_folder(config, server) {
        return Ok(Some(ConfigEditsBuilder::new(&project_config_folder)));
    }

    Ok(user_mcp_server_is_configured(config, server)?
        .then(|| ConfigEditsBuilder::for_config(config)))
}

async fn persist_custom_mcp_tool_approval_with(
    config_edits_builder: ConfigEditsBuilder,
    server: &str,
    tool_name: &str,
) -> anyhow::Result<()> {
    config_edits_builder
        .with_edits([ConfigEdit::SetPath {
            segments: vec![
                "mcp_servers".to_string(),
                server.to_string(),
                "tools".to_string(),
                tool_name.to_string(),
                "approval_mode".to_string(),
            ],
            value: value("approve"),
        }])
        .apply()
        .await
}

fn user_mcp_server_is_configured(config: &Config, server: &str) -> anyhow::Result<bool> {
    let Some(mcp_servers_toml) = config
        .config_layer_stack
        .effective_user_config()
        .as_ref()
        .and_then(|user_config| user_config.get("mcp_servers"))
        .cloned()
    else {
        return Ok(false);
    };
    let servers =
        HashMap::<String, codex_config::types::McpServerConfig>::deserialize(mcp_servers_toml)?;
    Ok(servers.contains_key(server))
}

fn project_mcp_tool_approval_config_folder(
    config: &Config,
    server: &str,
) -> Option<AbsolutePathBuf> {
    config
        .config_layer_stack
        .layers_high_to_low()
        .into_iter()
        .find_map(|layer| {
            if !matches!(layer.name, ConfigLayerSource::Project { .. }) {
                return None;
            }

            let servers = layer
                .config
                .as_table()
                .and_then(|table| table.get("mcp_servers"))
                .cloned()
                .and_then(|value| {
                    HashMap::<String, codex_config::types::McpServerConfig>::deserialize(value).ok()
                })?;
            if servers.contains_key(server) {
                layer.config_folder()
            } else {
                None
            }
        })
}

async fn notify_mcp_tool_call_skip(
    sess: &Session,
    turn_context: &TurnContext,
    call_id: &str,
    invocation: McpInvocation,
    item_metadata: McpToolCallItemMetadata,
    message: String,
    already_started: bool,
) -> Result<CallToolResult, String> {
    if !already_started {
        notify_mcp_tool_call_started(
            sess,
            turn_context,
            call_id,
            invocation.clone(),
            item_metadata.clone(),
        )
        .await;
    }

    notify_mcp_tool_call_completed(
        sess,
        turn_context,
        call_id,
        invocation,
        item_metadata,
        Duration::ZERO,
        truncate_mcp_tool_result_for_event(
            &Err(message.clone()),
            MCP_TOOL_CALL_EVENT_RESULT_MAX_BYTES,
        ),
    )
    .await;
    Err(message)
}

#[cfg(test)]
#[path = "mcp_tool_call_tests.rs"]
mod tests;
