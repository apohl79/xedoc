use super::*;
use crate::agent::control::SpawnAgentForkMode;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::next_thread_spawn_depth;
use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::tools::handlers::multi_agents_spec::SpawnAgentToolOptions;
use crate::tools::handlers::multi_agents_spec::create_spawn_agent_tool_v2;
use crate::tools::handlers::multi_agents_v2::message_tool::message_content;
use xedoc_protocol::AgentPath;
use xedoc_protocol::models::PermissionProfile;
use xedoc_protocol::protocol::SandboxPolicy;
use xedoc_tools::ToolSpec;

#[derive(Default)]
pub(crate) struct Handler {
    options: SpawnAgentToolOptions,
}

impl Handler {
    pub(crate) fn new(options: SpawnAgentToolOptions) -> Self {
        Self { options }
    }
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("spawn_agent")
    }

    fn spec(&self) -> ToolSpec {
        create_spawn_agent_tool_v2(self.options.clone())
    }

    fn handle(&self, invocation: ToolInvocation) -> xedoc_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { handle_spawn_agent(invocation).await.map(boxed_tool_output) })
    }
}

async fn handle_spawn_agent(
    invocation: ToolInvocation,
) -> Result<SpawnAgentResult, FunctionCallError> {
    let ToolInvocation {
        session,
        turn,
        payload,
        call_id,
        ..
    } = invocation;
    let arguments = function_arguments(payload)?;
    let args: SpawnAgentArgs = parse_arguments(&arguments)?;
    let fork_mode = args.fork_mode()?;
    let role_name = args
        .agent_type
        .as_deref()
        .map(str::trim)
        .filter(|role| !role.is_empty());

    let message = message_content(args.message.clone())?;
    let session_source = turn.session_source.clone();
    let child_depth = next_thread_spawn_depth(&session_source);
    let max_depth = turn.config.agent_max_depth;
    if child_depth > max_depth {
        return Err(FunctionCallError::RespondToModel(format!(
            "Cannot spawn agent: maximum nesting depth of {max_depth} exceeded (would be {child_depth})"
        )));
    }
    let mut config =
        build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())?;
    if let Some(service_tier) = args.service_tier.as_ref() {
        config.service_tier = Some(service_tier.clone());
    }
    let is_full_history_fork = matches!(fork_mode, Some(SpawnAgentForkMode::FullHistory));
    if is_full_history_fork {
        reject_full_fork_agent_type_override(role_name)?;
    }
    apply_requested_spawn_agent_model_overrides(
        &session,
        turn.as_ref(),
        &mut config,
        args.model.as_deref(),
        args.reasoning_effort.clone(),
    )
    .await?;
    if !is_full_history_fork {
        apply_spawn_agent_role(&session, &mut config, role_name).await?;
    }
    apply_spawn_agent_service_tier(
        &session,
        &mut config,
        turn.config.service_tier.as_deref(),
        args.service_tier.as_deref(),
    )
    .await?;
    apply_spawn_agent_delegation_override(&mut config, args.allow_delegation);
    apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;
    let orchestrator_config = config.clone();
    let mut router_decision =
        if let Some(current_route) = crate::model_router::current_route(&config) {
            crate::model_router::ModelRouterService::decide_subagent(
                turn.config.as_ref(),
                &session.services.models_manager,
                &message,
                current_route,
                args.model.is_some()
                    || args.reasoning_effort.is_some()
                    || args.service_tier.is_some()
                    || role_name.is_some()
                    || turn.config.agent_default_subagent_model.is_some()
                    || turn
                        .config
                        .agent_default_subagent_reasoning_effort
                        .is_some(),
            )
            .await
        } else {
            None
        };
    if let Some(decision) = router_decision.as_mut()
        && decision.disposition == xedoc_model_router::RouteDisposition::Applied
    {
        let applied = match decision.effective_route.as_ref() {
            Some(route) => {
                crate::model_router::apply_route_to_config(
                    &mut config,
                    &session.services.models_manager,
                    route,
                )
                .await
            }
            None => false,
        };
        if !applied {
            crate::model_router::fallback_to_original_route(decision);
        }
    }

    if let Some(result) = try_spawn_ab_pair(
        &session,
        turn.as_ref(),
        &call_id,
        &args,
        &fork_mode,
        role_name,
        &message,
        &config,
        &orchestrator_config,
        router_decision.as_ref(),
    )
    .await?
    {
        return Ok(result);
    }

    let spawn_source = thread_spawn_source(
        session.thread_id,
        &turn.session_source,
        child_depth,
        role_name,
        Some(args.task_name.clone()),
    )?;
    let new_agent_path = spawn_source.get_agent_path().ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "spawned agent is missing a canonical task name".to_string(),
        )
    })?;
    let author = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let communication = communication_from_tool_message(
        author,
        new_agent_path.clone(),
        message,
        ToolMessageKind::NewTask,
    );
    let context = AgentCommunicationContext::new(AgentCommunicationKind::Spawn, session.thread_id);
    let spawned_agent = Box::pin(
        session
            .services
            .agent_control
            .spawn_agent_with_communication(
                config,
                communication,
                context,
                Some(spawn_source),
                SpawnAgentOptions {
                    fork_parent_spawn_call_id: fork_mode.as_ref().map(|_| call_id.clone()),
                    fork_mode,
                    parent_thread_id: Some(session.thread_id),
                    environments: Some(turn.environments.to_selections()),
                    ..Default::default()
                },
            ),
    )
    .await
    .map_err(collab_spawn_error)?;
    let new_thread_id = spawned_agent.thread_id;
    if let Some(router_decision) = router_decision {
        session
            .send_event(
                &turn,
                xedoc_protocol::protocol::EventMsg::ModelRouterDecision(
                    crate::model_router::decision_event(
                        router_decision,
                        session.thread_id.to_string(),
                        turn.sub_id.clone(),
                        xedoc_protocol::protocol::ModelRouterScope::Subagent,
                        now_unix_timestamp_ms() / 1_000,
                    ),
                ),
            )
            .await;
    }
    let agent_snapshot = session
        .services
        .agent_control
        .get_agent_config_snapshot(new_thread_id)
        .await;
    let nickname = agent_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.session_source.get_nickname())
        .or(spawned_agent.metadata.agent_nickname);
    session
        .send_event(
            &turn,
            SubAgentActivityEvent {
                event_id: call_id,
                occurred_at_ms: now_unix_timestamp_ms(),
                agent_thread_id: new_thread_id,
                agent_path: new_agent_path.clone(),
                model_provider: agent_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.model_provider_id.clone()),
                model: agent_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.model.clone()),
                reasoning_effort: agent_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.reasoning_effort.clone()),
                kind: SubAgentActivityKind::Started,
                current_activity: Some("Working".to_string()),
            }
            .into(),
        )
        .await;
    let role_tag = role_name.unwrap_or(DEFAULT_ROLE_NAME);
    turn.session_telemetry.counter(
        "xedoc.multi_agent.spawn",
        /*inc*/ 1,
        &[("role", role_tag), ("version", "v2")],
    );
    let task_name = String::from(new_agent_path);

    let hide_agent_metadata = turn.config.multi_agent_v2.hide_spawn_agent_metadata;
    if hide_agent_metadata {
        Ok(SpawnAgentResult::HiddenMetadata { task_name })
    } else {
        Ok(SpawnAgentResult::WithNickname {
            task_name,
            nickname,
        })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the pair path deliberately shares the complete validated spawn context"
)]
async fn try_spawn_ab_pair(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &crate::session::turn_context::TurnContext,
    call_id: &str,
    args: &SpawnAgentArgs,
    fork_mode: &Option<SpawnAgentForkMode>,
    role_name: Option<&str>,
    message: &str,
    routed_config: &crate::config::Config,
    orchestrator_config: &crate::config::Config,
    router_decision: Option<&xedoc_model_router::RouteDecision>,
) -> Result<Option<SpawnAgentResult>, FunctionCallError> {
    if turn.session_source.is_non_root_agent()
        || is_operations_or_deployment_task(message)
        || !matches!(router_decision, Some(decision) if decision.disposition == xedoc_model_router::RouteDisposition::Applied)
    {
        return Ok(None);
    }
    let active_pair = session.take_model_router_ab_pair_for_spawn().await;
    let Some(active_pair) = active_pair else {
        return Ok(None);
    };
    let mut routed_config = routed_config.clone();
    let mut baseline_config = orchestrator_config.clone();
    if !crate::model_router::apply_route_to_config(
        &mut baseline_config,
        &session.services.models_manager,
        &active_pair.orchestrator_route,
    )
    .await
    {
        return Ok(None);
    }
    if !matches!(turn.sandbox_policy(), SandboxPolicy::ReadOnly { .. }) {
        return Ok(None);
    }
    for config in [&mut routed_config, &mut baseline_config] {
        config
            .permissions
            .set_permission_profile(PermissionProfile::read_only())
            .map_err(|error| {
                FunctionCallError::RespondToModel(format!(
                    "failed to enforce read-only A/B child permissions: {error}"
                ))
            })?;
        apply_spawn_agent_delegation_override(config, Some(false));
    }

    let routed_task_name = format!("{}__ab_a", args.task_name);
    let orchestrator_task_name = format!("{}__ab_b", args.task_name);
    let child_depth = next_thread_spawn_depth(&turn.session_source);
    let routed_source = thread_spawn_source(
        session.thread_id,
        &turn.session_source,
        child_depth,
        role_name,
        Some(routed_task_name.clone()),
    )?;
    let baseline_source = thread_spawn_source(
        session.thread_id,
        &turn.session_source,
        child_depth,
        role_name,
        Some(orchestrator_task_name.clone()),
    )?;
    let author = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    // The distinct child paths are transport-only. Both branches receive this
    // exact same model-visible envelope so the classifier cannot infer branch
    // identity from the task name.
    let pair_recipient =
        AgentPath::try_from("/root/ab_pair").expect("the canonical A/B task path is valid");
    let pair_communication = communication_from_tool_message(
        author,
        pair_recipient,
        message.to_string(),
        ToolMessageKind::NewTask,
    );
    let reservations = session
        .services
        .agent_control
        .reserve_ab_pair_capacity(&routed_config, &turn.session_source)
        .await
        .map_err(collab_spawn_error)?;
    let (mut routed_options, mut baseline_options) = reservations.into_options();
    for options in [&mut routed_options, &mut baseline_options] {
        options.fork_parent_spawn_call_id = fork_mode.as_ref().map(|_| call_id.to_string());
        options.fork_mode = fork_mode.clone();
        options.parent_thread_id = Some(session.thread_id);
        options.environments = Some(turn.environments.to_selections());
    }
    let router_event = router_decision.map(|decision| {
        crate::model_router::decision_event(
            decision.clone(),
            session.thread_id.to_string(),
            turn.sub_id.clone(),
            xedoc_protocol::protocol::ModelRouterScope::Subagent,
            now_unix_timestamp_ms() / 1_000,
        )
    });
    let router_decision_id = router_event.as_ref().map(|event| event.decision_id.clone());
    let routed_context =
        AgentCommunicationContext::new(AgentCommunicationKind::Spawn, session.thread_id)
            .with_ab_pair(
                active_pair.pair_id.clone(),
                crate::agent_communication::AbPairBranch::Routed,
                router_decision_id.clone(),
            );
    let baseline_context =
        AgentCommunicationContext::new(AgentCommunicationKind::Spawn, session.thread_id)
            .with_ab_pair(
                active_pair.pair_id.clone(),
                crate::agent_communication::AbPairBranch::Orchestrator,
                router_decision_id,
            );
    let routed_spawn = session
        .services
        .agent_control
        .spawn_agent_with_deferred_communication(routed_config, Some(routed_source), routed_options)
        .await
        .map_err(collab_spawn_error)?;
    let baseline_spawn = match session
        .services
        .agent_control
        .spawn_agent_with_deferred_communication(
            baseline_config,
            Some(baseline_source),
            baseline_options,
        )
        .await
    {
        Ok(spawned) => spawned,
        Err(error) => {
            let _ = session
                .services
                .agent_control
                .shutdown_live_agent(routed_spawn.thread_id)
                .await;
            return Err(collab_spawn_error(error));
        }
    };
    let barrier_id = active_pair.pair_id.clone();
    if let Err(error) = session
        .services
        .agent_control
        .stage_deferred_agent_communication(
            routed_spawn.thread_id,
            pair_communication.clone(),
            routed_context,
            barrier_id.clone(),
        )
        .await
    {
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(routed_spawn.thread_id)
            .await;
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(baseline_spawn.thread_id)
            .await;
        return Err(collab_spawn_error(error));
    }
    if let Err(error) = session
        .services
        .agent_control
        .stage_deferred_agent_communication(
            baseline_spawn.thread_id,
            pair_communication,
            baseline_context,
            barrier_id.clone(),
        )
        .await
    {
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(routed_spawn.thread_id)
            .await;
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(baseline_spawn.thread_id)
            .await;
        return Err(collab_spawn_error(error));
    }
    if let Err(error) = session
        .services
        .agent_control
        .release_deferred_agent_communication(routed_spawn.thread_id, barrier_id.clone())
        .await
    {
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(routed_spawn.thread_id)
            .await;
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(baseline_spawn.thread_id)
            .await;
        return Err(collab_spawn_error(error));
    }
    if let Err(error) = session
        .services
        .agent_control
        .release_deferred_agent_communication(baseline_spawn.thread_id, barrier_id)
        .await
    {
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(routed_spawn.thread_id)
            .await;
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(baseline_spawn.thread_id)
            .await;
        return Err(collab_spawn_error(error));
    }
    if let Err(error) = session
        .services
        .agent_control
        .commit_deferred_agent_pair(routed_spawn.thread_id, baseline_spawn.thread_id)
        .await
    {
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(routed_spawn.thread_id)
            .await;
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(baseline_spawn.thread_id)
            .await;
        return Err(collab_spawn_error(error));
    }
    if let Some(router_event) = router_event {
        let router_decision = xedoc_state::ModelRouterDecisionRecord::from(&router_event);
        if let Some(state_db) = session.state_db()
            && let Err(error) = state_db
                .insert_model_router_decision(&router_decision)
                .await
        {
            tracing::warn!(%error, "failed to persist model-router A/B decision");
        }
        session
            .set_model_router_ab_decision_id(&active_pair.pair_id, router_event.decision_id.clone())
            .await;
        session
            .start_model_router_ab_outcome(
                &active_pair.pair_id,
                &turn.sub_id,
                Some(router_event.decision_id.clone()),
            )
            .await;
        session
            .send_event(
                turn,
                xedoc_protocol::protocol::EventMsg::ModelRouterDecision(router_event),
            )
            .await;
    }
    if let Err(error) = session
        .services
        .agent_control
        .start_deferred_agent_pair(routed_spawn.thread_id, baseline_spawn.thread_id)
        .await
    {
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(routed_spawn.thread_id)
            .await;
        let _ = session
            .services
            .agent_control
            .shutdown_live_agent(baseline_spawn.thread_id)
            .await;
        return Err(collab_spawn_error(error));
    }
    Ok(Some(SpawnAgentResult::AbPair {
        pair_id: active_pair.pair_id,
        routed_task_name,
        orchestrator_task_name,
    }))
}

fn is_operations_or_deployment_task(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["deploy", "deployment", "production operation", "runbook"]
        .into_iter()
        .any(|term| message.contains(term))
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    message: String,
    task_name: String,
    agent_type: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    service_tier: Option<String>,
    allow_delegation: Option<bool>,
    fork_turns: Option<String>,
    fork_context: Option<bool>,
}

impl SpawnAgentArgs {
    fn fork_mode(&self) -> Result<Option<SpawnAgentForkMode>, FunctionCallError> {
        if self.fork_context.is_some() {
            return Err(FunctionCallError::RespondToModel(
                "fork_context is not supported in MultiAgentV2; use fork_turns instead".to_string(),
            ));
        }

        let fork_turns = self
            .fork_turns
            .as_deref()
            .map(str::trim)
            .filter(|fork_turns| !fork_turns.is_empty())
            .unwrap_or("all");

        if fork_turns.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        if fork_turns.eq_ignore_ascii_case("all") {
            return Ok(Some(SpawnAgentForkMode::FullHistory));
        }

        let last_n_turns = fork_turns.parse::<usize>().map_err(|_| {
            FunctionCallError::RespondToModel(
                "fork_turns must be `none`, `all`, or a positive integer string".to_string(),
            )
        })?;
        if last_n_turns == 0 {
            return Err(FunctionCallError::RespondToModel(
                "fork_turns must be `none`, `all`, or a positive integer string".to_string(),
            ));
        }

        Ok(Some(SpawnAgentForkMode::LastNTurns(last_n_turns)))
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum SpawnAgentResult {
    WithNickname {
        task_name: String,
        nickname: Option<String>,
    },
    HiddenMetadata {
        task_name: String,
    },
    AbPair {
        pair_id: String,
        routed_task_name: String,
        orchestrator_task_name: String,
    },
}

impl ToolOutput for SpawnAgentResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "spawn_agent")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, Some(true), "spawn_agent")
    }
}
