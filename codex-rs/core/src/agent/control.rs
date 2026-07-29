use crate::agent::AgentStatus;
use crate::agent::registry::AgentMetadata;
use crate::agent::registry::AgentRegistry;
use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::role::resolve_role_config;
use crate::agent::status::is_final;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::codex_thread::ThreadConfigSnapshot;
use crate::config::Config;
use crate::config::RolloutBudgetConfig;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::rollout_budget::RolloutBudget;
use crate::session::emit_subagent_session_started;
use crate::session::session::Session;
use crate::session::turn::generate_sub_agent_activity_summary;
use crate::session::turn::sub_agent_activity::RecentSubAgentActivity;
use crate::session::turn_context::TurnContext;
use crate::session_prefix::format_inter_agent_completion_message;
use crate::session_prefix::format_subagent_context_line;
use crate::session_prefix::format_subagent_notification_message;
use crate::thread_manager::ResumeThreadWithHistoryOptions;
use crate::thread_manager::ThreadManagerState;
use crate::thread_rollout_truncation::truncate_rollout_to_last_n_fork_turns;
use codex_protocol::AgentPath;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ResumedHistory;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_protocol::user_input::UserInput;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ReadThreadParams;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::watch;
use tracing::warn;

pub(crate) use self::execution::AgentExecutionGuard;
use self::execution::AgentExecutionLimiter;
use self::residency::V2Residency;

const ROOT_LAST_TASK_MESSAGE: &str = "Main thread";

mod execution;
mod legacy;
mod residency;
mod spawn;

#[derive(Default)]
struct SubAgentActivityRegistry {
    parents: HashMap<ThreadId, ParentSubAgentActivity>,
}

#[derive(Default)]
struct ParentSubAgentActivity {
    children: HashMap<ThreadId, ChildSubAgentActivity>,
    timer_running: bool,
}

struct ChildSubAgentActivity {
    agent_path: AgentPath,
    recent_activity: RecentSubAgentActivity,
}

struct PendingSubAgentActivity {
    agent_thread_id: ThreadId,
    agent_path: AgentPath,
    recent_activity: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpawnAgentForkMode {
    FullHistory,
    LastNTurns(usize),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SpawnAgentOptions {
    pub(crate) fork_parent_spawn_call_id: Option<String>,
    pub(crate) fork_mode: Option<SpawnAgentForkMode>,
    pub(crate) parent_thread_id: Option<ThreadId>,
    pub(crate) environments: Option<Vec<TurnEnvironmentSelection>>,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveAgent {
    pub(crate) thread_id: ThreadId,
    pub(crate) metadata: AgentMetadata,
    pub(crate) status: AgentStatus,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ListedAgent {
    pub(crate) agent_name: String,
    pub(crate) agent_status: AgentStatus,
    pub(crate) last_task_message: Option<String>,
}

/// Control-plane handle for multi-agent operations.
/// `AgentControl` is held by each session (via `SessionServices`). It provides capability to
/// spawn new agents and the inter-agent communication layer.
/// An `AgentControl` instance is intended to be created at most once per root thread/session
/// tree. That same `AgentControl` is then shared with every sub-agent spawned from that root,
/// which keeps the registry scoped to that root thread rather than the entire `ThreadManager`.
#[derive(Clone, Default)]
pub(crate) struct AgentControl {
    /// ID shared by the whole agent control session. This means every sub-agents from a common
    /// root share the same session ID.
    session_id: SessionId,
    /// Weak handle back to the global thread registry/state.
    /// This is `Weak` to avoid reference cycles and shadow persistence of the form
    /// `ThreadManagerState -> CodexThread -> Session -> SessionServices -> ThreadManagerState`.
    manager: Weak<ThreadManagerState>,
    state: Arc<AgentRegistry>,
    v2_residency: Arc<V2Residency>,
    agent_execution_limiter: Arc<AgentExecutionLimiter>,
    /// Session-scoped state shared by the root thread and every cloned sub-agent control handle.
    rollout_budget: Arc<RolloutBudget>,
    /// Bounded raw activity retained for direct children until their parent summarizes it.
    sub_agent_activity: Arc<Mutex<SubAgentActivityRegistry>>,
}

impl AgentControl {
    /// Construct a new `AgentControl` that can spawn/message agents via the given manager state.
    pub(crate) fn new(
        manager: Weak<ThreadManagerState>,
        rollout_budget: Option<RolloutBudgetConfig>,
    ) -> Self {
        let control = Self {
            manager,
            ..Default::default()
        };
        if let Some(rollout_budget) = rollout_budget {
            control.rollout_budget.configure(rollout_budget);
        }
        control
    }

    pub(crate) fn with_session_id(mut self, session_id: SessionId, max_threads: usize) -> Self {
        self.session_id = session_id;
        self.agent_execution_limiter.initialize(max_threads);
        self
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(crate) fn rollout_budget(&self) -> &RolloutBudget {
        self.rollout_budget.as_ref()
    }

    /// Forward a sub-agent activity event to the target thread.
    pub(crate) async fn forward_sub_agent_activity_event(
        &self,
        agent_id: ThreadId,
        activity: codex_protocol::protocol::SubAgentActivityEvent,
    ) {
        let Ok(state) = self.upgrade() else {
            return;
        };
        let Ok(thread) = state.get_thread(agent_id).await else {
            return;
        };
        thread
            .emit_event(codex_protocol::protocol::EventMsg::SubAgentActivity(
                activity,
            ))
            .await;
    }

    /// Starts parent-owned activity summarization for a newly spawned V2 child.
    pub(crate) async fn start_sub_agent_activity_tracking(
        &self,
        parent_thread_id: ThreadId,
        child_thread_id: ThreadId,
        agent_path: AgentPath,
    ) {
        let Ok(state) = self.upgrade() else {
            return;
        };
        let Ok(parent_thread) = state.get_thread(parent_thread_id).await else {
            return;
        };
        let parent_session = Arc::clone(&parent_thread.session);
        let Some((turn_context, _)) = parent_session
            .active_turn_context_and_cancellation_token()
            .await
        else {
            tracing::warn!(
                %parent_thread_id,
                %child_thread_id,
                "Skipping parent-owned sub-agent activity tracking without an active parent turn"
            );
            return;
        };

        let should_start_timer = {
            let mut registry = self
                .sub_agent_activity
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let parent = registry.parents.entry(parent_thread_id).or_default();
            parent.children.insert(
                child_thread_id,
                ChildSubAgentActivity {
                    agent_path,
                    recent_activity: RecentSubAgentActivity::default(),
                },
            );
            if parent.timer_running {
                false
            } else {
                parent.timer_running = true;
                true
            }
        };
        if should_start_timer {
            tracing::info!(
                %parent_thread_id,
                %child_thread_id,
                "Starting parent-owned sub-agent activity timer"
            );
            let control = self.clone();
            tokio::spawn(async move {
                control
                    .run_sub_agent_activity_timer(parent_thread_id, parent_session, turn_context)
                    .await;
            });
        }
    }

    pub(crate) fn record_sub_agent_activity(
        &self,
        parent_thread_id: ThreadId,
        child_thread_id: ThreadId,
        agent_path: AgentPath,
        item: &ResponseItem,
    ) {
        let mut registry = self
            .sub_agent_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let parent = registry.parents.entry(parent_thread_id).or_default();
        let child =
            parent
                .children
                .entry(child_thread_id)
                .or_insert_with(|| ChildSubAgentActivity {
                    agent_path,
                    recent_activity: RecentSubAgentActivity::default(),
                });
        child.recent_activity.record_response_item(item);
    }

    pub(crate) fn stop_sub_agent_activity_tracking(
        &self,
        parent_thread_id: ThreadId,
        child_thread_id: ThreadId,
    ) {
        let mut registry = self
            .sub_agent_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(parent) = registry.parents.get_mut(&parent_thread_id) else {
            return;
        };
        parent.children.remove(&child_thread_id);
    }

    async fn run_sub_agent_activity_timer(
        self,
        parent_thread_id: ThreadId,
        parent_session: Arc<Session>,
        turn_context: Arc<TurnContext>,
    ) {
        let mut tick = 0_u32;
        loop {
            tokio::time::sleep(Duration::from_secs(12)).await;
            tick += 1;
            let (pending_activity, has_children) =
                self.take_pending_sub_agent_activity(parent_thread_id);
            if !has_children {
                tracing::info!(
                    %parent_thread_id,
                    activity_timer_ticks = tick,
                    "Stopping parent-owned sub-agent activity timer"
                );
                return;
            }
            tracing::info!(
                %parent_thread_id,
                activity_timer_tick = tick,
                pending_children = pending_activity.len(),
                "Parent-owned sub-agent activity timer tick"
            );
            for pending in pending_activity {
                let summary_started_at = Instant::now();
                tracing::info!(
                    %parent_thread_id,
                    agent_thread_id = %pending.agent_thread_id,
                    activity_timer_tick = tick,
                    recent_activity_chars = pending.recent_activity.chars().count(),
                    "Starting parent-owned sub-agent activity summary request"
                );
                match generate_sub_agent_activity_summary(
                    parent_session.as_ref(),
                    turn_context.as_ref(),
                    Some(&pending.recent_activity),
                )
                .await
                {
                    Ok(Some(summary)) => {
                        tracing::info!(
                            %parent_thread_id,
                            agent_thread_id = %pending.agent_thread_id,
                            activity_timer_tick = tick,
                            elapsed_ms = summary_started_at.elapsed().as_millis(),
                            summary_chars = summary.chars().count(),
                            "Parent-owned sub-agent activity summary request completed"
                        );
                        parent_session
                            .send_event(
                                turn_context.as_ref(),
                                codex_protocol::protocol::SubAgentActivityEvent {
                                    event_id: format!(
                                        "parent-activity-{parent_thread_id}-{tick}-{}",
                                        pending.agent_thread_id
                                    ),
                                    occurred_at_ms: crate::turn_timing::now_unix_timestamp_ms(),
                                    agent_thread_id: pending.agent_thread_id,
                                    agent_path: pending.agent_path,
                                    model_provider: None,
                                    model: None,
                                    kind:
                                        codex_protocol::protocol::SubAgentActivityKind::Interacted,
                                    current_activity: Some(summary),
                                }
                                .into(),
                            )
                            .await;
                    }
                    Ok(None) => {
                        tracing::warn!(
                            %parent_thread_id,
                            agent_thread_id = %pending.agent_thread_id,
                            activity_timer_tick = tick,
                            elapsed_ms = summary_started_at.elapsed().as_millis(),
                            "Parent-owned sub-agent activity summary request returned empty"
                        );
                        self.retry_sub_agent_activity(parent_thread_id, pending.agent_thread_id);
                    }
                    Err(err) => {
                        tracing::info!(
                            %parent_thread_id,
                            agent_thread_id = %pending.agent_thread_id,
                            activity_timer_tick = tick,
                            elapsed_ms = summary_started_at.elapsed().as_millis(),
                            error = %err,
                            "Parent-owned sub-agent activity summary request failed"
                        );
                        self.retry_sub_agent_activity(parent_thread_id, pending.agent_thread_id);
                    }
                }
            }
        }
    }

    fn take_pending_sub_agent_activity(
        &self,
        parent_thread_id: ThreadId,
    ) -> (Vec<PendingSubAgentActivity>, bool) {
        let mut registry = self
            .sub_agent_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if registry
            .parents
            .get(&parent_thread_id)
            .is_none_or(|parent| parent.children.is_empty())
        {
            registry.parents.remove(&parent_thread_id);
            return (Vec::new(), false);
        }
        let Some(parent) = registry.parents.get_mut(&parent_thread_id) else {
            return (Vec::new(), false);
        };
        let pending_activity = parent
            .children
            .iter_mut()
            .filter_map(|(agent_thread_id, child)| {
                child
                    .recent_activity
                    .snapshot_if_changed()
                    .map(|recent_activity| PendingSubAgentActivity {
                        agent_thread_id: *agent_thread_id,
                        agent_path: child.agent_path.clone(),
                        recent_activity,
                    })
            })
            .collect();
        (pending_activity, !parent.children.is_empty())
    }

    fn retry_sub_agent_activity(&self, parent_thread_id: ThreadId, child_thread_id: ThreadId) {
        let mut registry = self
            .sub_agent_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(child) = registry
            .parents
            .get_mut(&parent_thread_id)
            .and_then(|parent| parent.children.get_mut(&child_thread_id))
        {
            child.recent_activity.retry();
        }
    }

    /// Send rich user input items to an existing agent thread.
    pub(crate) async fn send_input(
        &self,
        agent_id: ThreadId,
        input: Vec<UserInput>,
    ) -> CodexResult<String> {
        let state = self.upgrade()?;
        self.ensure_execution_capacity_for_turn_start(agent_id, /*starts_turn*/ true)
            .await?;
        self.send_input_after_capacity_check(agent_id, &state, input)
            .await
    }

    async fn send_input_after_capacity_check(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        input: Vec<UserInput>,
    ) -> CodexResult<String> {
        let last_task_message = non_empty_task_message(render_input_preview(&input));
        let result = self
            .handle_thread_request_result(
                agent_id,
                state,
                state.send_op(agent_id, input.into()).await,
            )
            .await;
        if result.is_ok() {
            match last_task_message {
                Some(last_task_message) => self
                    .state
                    .update_last_task_message(agent_id, last_task_message),
                None => self.state.clear_last_task_message(agent_id),
            }
        }
        result
    }

    pub(crate) async fn send_inter_agent_communication(
        &self,
        agent_id: ThreadId,
        communication: InterAgentCommunication,
        agent_communication_context: AgentCommunicationContext,
    ) -> CodexResult<String> {
        let state = self.upgrade()?;
        self.ensure_execution_capacity_for_turn_start(agent_id, communication.trigger_turn)
            .await?;
        self.send_inter_agent_communication_after_capacity_check(
            agent_id,
            &state,
            communication,
            agent_communication_context,
        )
        .await
    }

    async fn send_inter_agent_communication_after_capacity_check(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        communication: InterAgentCommunication,
        context: AgentCommunicationContext,
    ) -> CodexResult<String> {
        self.submit_inter_agent_communication(agent_id, state, communication, context)
            .await
    }

    async fn submit_inter_agent_communication(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        communication: InterAgentCommunication,
        context: AgentCommunicationContext,
    ) -> CodexResult<String> {
        let last_task_message = last_task_message_from_communication(&communication);
        let communication_for_log =
            crate::agent_communication::logging_enabled().then(|| communication.clone());
        let result = self
            .handle_thread_request_result(
                agent_id,
                state,
                state
                    .send_op(agent_id, Op::InterAgentCommunication { communication })
                    .await,
            )
            .await;
        if let (Some(communication), Ok(communication_id)) =
            (communication_for_log, result.as_ref())
        {
            crate::agent_communication::emit_agent_communication_send(
                communication_id,
                &context,
                &communication,
                agent_id,
            );
        }
        if result.is_ok() {
            match last_task_message {
                Some(last_task_message) => self
                    .state
                    .update_last_task_message(agent_id, last_task_message),
                None => self.state.clear_last_task_message(agent_id),
            }
        }
        result
    }

    /// Interrupt the current task for an existing agent thread.
    pub(crate) async fn interrupt_agent(&self, agent_id: ThreadId) -> CodexResult<String> {
        let state = self.upgrade()?;
        self.handle_thread_request_result(
            agent_id,
            &state,
            state.send_op(agent_id, Op::Interrupt).await,
        )
        .await
    }

    async fn handle_thread_request_result(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        result: CodexResult<String>,
    ) -> CodexResult<String> {
        if matches!(result, Err(CodexErr::InternalAgentDied)) {
            let _ = state.remove_thread(&agent_id).await;
            self.forget_v2_residency(agent_id);
            self.state.release_spawned_thread(agent_id);
        }
        result
    }

    /// Fetch the last known status for `agent_id`, returning `NotFound` when unavailable.
    pub(crate) async fn get_status(&self, agent_id: ThreadId) -> AgentStatus {
        let Ok(state) = self.upgrade() else {
            // No agent available if upgrade fails.
            return AgentStatus::NotFound;
        };
        let Ok(thread) = state.get_thread(agent_id).await else {
            return AgentStatus::NotFound;
        };
        thread.agent_status().await
    }

    pub(crate) fn register_session_root(
        &self,
        current_thread_id: ThreadId,
        current_parent_thread_id: Option<ThreadId>,
    ) {
        if current_parent_thread_id.is_none() {
            self.state.register_root_thread(current_thread_id);
        }
    }

    pub(crate) fn get_agent_metadata(&self, agent_id: ThreadId) -> Option<AgentMetadata> {
        self.state.agent_metadata_for_thread(agent_id)
    }

    pub(crate) fn ensure_agent_known(&self, agent_id: ThreadId) -> CodexResult<AgentMetadata> {
        self.state
            .agent_metadata_for_thread(agent_id)
            .ok_or(CodexErr::ThreadNotFound(agent_id))
    }

    pub(crate) async fn list_live_agent_subtree_thread_ids(
        &self,
        agent_id: ThreadId,
    ) -> CodexResult<Vec<ThreadId>> {
        let mut thread_ids = vec![agent_id];
        thread_ids.extend(self.live_thread_spawn_descendants(agent_id).await?);
        Ok(thread_ids)
    }

    pub(crate) async fn get_agent_config_snapshot(
        &self,
        agent_id: ThreadId,
    ) -> Option<ThreadConfigSnapshot> {
        let Ok(state) = self.upgrade() else {
            return None;
        };
        let Ok(thread) = state.get_thread(agent_id).await else {
            return None;
        };
        Some(thread.config_snapshot().await)
    }

    pub(crate) async fn resolve_agent_reference(
        &self,
        _current_thread_id: ThreadId,
        current_session_source: &SessionSource,
        agent_reference: &str,
    ) -> CodexResult<ThreadId> {
        let current_agent_path = current_session_source
            .get_agent_path()
            .unwrap_or_else(AgentPath::root);
        let agent_path = current_agent_path
            .resolve(agent_reference)
            .map_err(CodexErr::UnsupportedOperation)?;
        if let Some(thread_id) = self.state.agent_id_for_path(&agent_path) {
            return Ok(thread_id);
        }
        Err(CodexErr::UnsupportedOperation(format!(
            "live agent path `{}` not found",
            agent_path.as_str()
        )))
    }

    /// Subscribe to status updates for `agent_id`, yielding the latest value and changes.
    pub(crate) async fn subscribe_status(
        &self,
        agent_id: ThreadId,
    ) -> CodexResult<watch::Receiver<AgentStatus>> {
        let state = self.upgrade()?;
        let thread = state.get_thread(agent_id).await?;
        Ok(thread.subscribe_status())
    }

    pub(crate) async fn format_environment_context_subagents(
        &self,
        parent_thread_id: ThreadId,
    ) -> String {
        let Ok(agents) = self.open_thread_spawn_children(parent_thread_id).await else {
            return String::new();
        };

        agents
            .into_iter()
            .map(|(thread_id, metadata)| {
                let reference = metadata
                    .agent_path
                    .as_ref()
                    .map(|agent_path| agent_path.name().to_string())
                    .unwrap_or_else(|| thread_id.to_string());
                format_subagent_context_line(reference.as_str(), metadata.agent_nickname.as_deref())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) async fn list_agents(
        &self,
        current_session_source: &SessionSource,
        path_prefix: Option<&str>,
    ) -> CodexResult<Vec<ListedAgent>> {
        let state = self.upgrade()?;
        let resolved_prefix = path_prefix
            .map(|prefix| {
                current_session_source
                    .get_agent_path()
                    .unwrap_or_else(AgentPath::root)
                    .resolve(prefix)
                    .map_err(CodexErr::UnsupportedOperation)
            })
            .transpose()?;

        let mut live_agents = self.state.live_agents();
        live_agents.sort_by(|left, right| {
            left.agent_path
                .as_deref()
                .unwrap_or_default()
                .cmp(right.agent_path.as_deref().unwrap_or_default())
                .then_with(|| {
                    left.agent_id
                        .map(|id| id.to_string())
                        .unwrap_or_default()
                        .cmp(&right.agent_id.map(|id| id.to_string()).unwrap_or_default())
                })
        });

        let root_path = AgentPath::root();
        let mut agents = Vec::with_capacity(live_agents.len().saturating_add(1));
        if resolved_prefix
            .as_ref()
            .is_none_or(|prefix| agent_matches_prefix(Some(&root_path), prefix))
            && let Some(root_thread_id) = self.state.agent_id_for_path(&root_path)
            && let Ok(root_thread) = state.get_thread(root_thread_id).await
        {
            agents.push(ListedAgent {
                agent_name: root_path.to_string(),
                agent_status: root_thread.agent_status().await,
                last_task_message: Some(ROOT_LAST_TASK_MESSAGE.to_string()),
            });
        }

        for metadata in live_agents {
            let Some(thread_id) = metadata.agent_id else {
                continue;
            };
            if resolved_prefix
                .as_ref()
                .is_some_and(|prefix| !agent_matches_prefix(metadata.agent_path.as_ref(), prefix))
            {
                continue;
            }

            let Ok(thread) = state.get_thread(thread_id).await else {
                continue;
            };
            let agent_name = metadata
                .agent_path
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| thread_id.to_string());
            let last_task_message = metadata.last_task_message.clone();
            agents.push(ListedAgent {
                agent_name,
                agent_status: thread.agent_status().await,
                last_task_message,
            });
        }

        Ok(agents)
    }

    /// Starts a detached watcher for sub-agents spawned from another thread.
    ///
    /// This is only enabled for `SubAgentSource::ThreadSpawn`, where a parent thread exists and
    /// can receive completion notifications.
    fn maybe_start_completion_watcher(
        &self,
        child_thread_id: ThreadId,
        session_source: Option<SessionSource>,
        child_reference: String,
        child_agent_path: Option<AgentPath>,
    ) {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return;
        };
        let control = self.clone();
        tokio::spawn(async move {
            let status = match control.subscribe_status(child_thread_id).await {
                Ok(mut status_rx) => {
                    let mut status = status_rx.borrow().clone();
                    while !is_final(&status) {
                        if status_rx.changed().await.is_err() {
                            status = control.get_status(child_thread_id).await;
                            break;
                        }
                        status = status_rx.borrow().clone();
                    }
                    status
                }
                Err(_) => control.get_status(child_thread_id).await,
            };
            if !is_final(&status) {
                return;
            }

            let Ok(state) = control.upgrade() else {
                return;
            };
            let child_thread = state.get_thread(child_thread_id).await.ok();
            let child_uses_multi_agent_v2 = match child_thread.as_ref() {
                Some(child_thread) => {
                    child_thread.multi_agent_version() == Some(MultiAgentVersion::V2)
                }
                None => true,
            };
            if child_agent_path.is_some() && child_uses_multi_agent_v2 {
                let Some(child_agent_path) = child_agent_path.clone() else {
                    return;
                };
                let Some(parent_agent_path) = child_agent_path
                    .as_str()
                    .rsplit_once('/')
                    .and_then(|(parent, _)| AgentPath::try_from(parent).ok())
                else {
                    return;
                };
                let Some(message) = format_inter_agent_completion_message(
                    parent_agent_path.clone(),
                    child_agent_path.clone(),
                    &status,
                ) else {
                    return;
                };
                let communication = InterAgentCommunication::new(
                    child_agent_path,
                    parent_agent_path,
                    Vec::new(),
                    message,
                    /*trigger_turn*/ false,
                );
                let context =
                    AgentCommunicationContext::new(AgentCommunicationKind::Result, child_thread_id);
                let _ = control
                    .send_inter_agent_communication(parent_thread_id, communication, context)
                    .await;
                return;
            }
            let message = format_subagent_notification_message(child_reference.as_str(), &status);
            let Ok(parent_thread) = state.get_thread(parent_thread_id).await else {
                return;
            };
            parent_thread
                .inject_user_message_without_turn(message)
                .await;
        });
    }

    fn prepare_agent_metadata(
        &self,
        reservation: &mut crate::agent::registry::SpawnReservation,
        config: &Config,
        agent_path: Option<AgentPath>,
        agent_role: Option<String>,
        preferred_agent_nickname: Option<String>,
    ) -> CodexResult<AgentMetadata> {
        if let Some(agent_path) = agent_path.as_ref() {
            reservation.reserve_agent_path(agent_path)?;
        }
        let candidate_names = spawn::agent_nickname_candidates(config, agent_role.as_deref());
        let candidate_name_refs: Vec<&str> = candidate_names.iter().map(String::as_str).collect();
        let agent_nickname = Some(reservation.reserve_agent_nickname_with_preference(
            &candidate_name_refs,
            preferred_agent_nickname.as_deref(),
        )?);
        Ok(AgentMetadata {
            agent_id: None,
            agent_path,
            agent_nickname,
            agent_role,
            last_task_message: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_thread_spawn(
        &self,
        reservation: &mut crate::agent::registry::SpawnReservation,
        config: &Config,
        parent_thread_id: ThreadId,
        depth: i32,
        agent_path: Option<AgentPath>,
        agent_role: Option<String>,
        preferred_agent_nickname: Option<String>,
    ) -> CodexResult<(SessionSource, AgentMetadata)> {
        if depth == 1 {
            self.state.register_root_thread(parent_thread_id);
        }
        let agent_metadata = self.prepare_agent_metadata(
            reservation,
            config,
            agent_path,
            agent_role,
            preferred_agent_nickname,
        )?;
        let session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth,
            agent_path: agent_metadata.agent_path.clone(),
            agent_nickname: agent_metadata.agent_nickname.clone(),
            agent_role: agent_metadata.agent_role.clone(),
        });
        Ok((session_source, agent_metadata))
    }

    fn upgrade(&self) -> CodexResult<Arc<ThreadManagerState>> {
        self.manager
            .upgrade()
            .ok_or_else(|| CodexErr::UnsupportedOperation("thread manager dropped".to_string()))
    }

    async fn inherited_environments_for_source(
        &self,
        state: &Arc<ThreadManagerState>,
        session_source: Option<&SessionSource>,
    ) -> Option<TurnEnvironmentSnapshot> {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return None;
        };

        let parent_thread = state.get_thread(*parent_thread_id).await.ok()?;
        Some(
            parent_thread
                .session
                .services
                .turn_environments
                .snapshot()
                .await,
        )
    }

    async fn inherited_exec_policy_for_source(
        &self,
        state: &Arc<ThreadManagerState>,
        session_source: Option<&SessionSource>,
        child_config: &Config,
    ) -> Option<Arc<crate::exec_policy::ExecPolicyManager>> {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return None;
        };

        let parent_thread = state.get_thread(*parent_thread_id).await.ok()?;
        let parent_config = parent_thread.session.get_config().await;
        if !crate::exec_policy::child_uses_parent_exec_policy(&parent_config, child_config) {
            return None;
        }

        Some(Arc::clone(&parent_thread.session.services.exec_policy))
    }

    async fn open_thread_spawn_children(
        &self,
        parent_thread_id: ThreadId,
    ) -> CodexResult<Vec<(ThreadId, AgentMetadata)>> {
        let mut children_by_parent = self.live_thread_spawn_children().await?;
        Ok(children_by_parent
            .remove(&parent_thread_id)
            .unwrap_or_default())
    }

    async fn live_thread_spawn_children(
        &self,
    ) -> CodexResult<HashMap<ThreadId, Vec<(ThreadId, AgentMetadata)>>> {
        let state = self.upgrade()?;
        let mut children_by_parent = HashMap::<ThreadId, Vec<(ThreadId, AgentMetadata)>>::new();

        for (parent_thread_id, child_thread_id) in state.list_live_thread_spawn_edges().await {
            children_by_parent
                .entry(parent_thread_id)
                .or_default()
                .push((
                    child_thread_id,
                    self.state
                        .agent_metadata_for_thread(child_thread_id)
                        .unwrap_or(AgentMetadata {
                            agent_id: Some(child_thread_id),
                            ..Default::default()
                        }),
                ));
        }

        for children in children_by_parent.values_mut() {
            children.sort_by(|left, right| {
                left.1
                    .agent_path
                    .as_deref()
                    .unwrap_or_default()
                    .cmp(right.1.agent_path.as_deref().unwrap_or_default())
                    .then_with(|| left.0.to_string().cmp(&right.0.to_string()))
            });
        }

        Ok(children_by_parent)
    }

    async fn persist_thread_spawn_edge_for_source(
        &self,
        child_thread: &crate::CodexThread,
        child_thread_id: ThreadId,
        session_source: Option<&SessionSource>,
    ) {
        let Some(parent_thread_id) = session_source.and_then(SessionSource::parent_thread_id)
        else {
            return;
        };
        if child_thread.config_snapshot().await.ephemeral {
            return;
        }
        let Ok(state) = self.upgrade() else {
            return;
        };
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return;
        };
        if let Err(err) = agent_graph_store
            .upsert_thread_spawn_edge(
                parent_thread_id,
                child_thread_id,
                codex_agent_graph_store::ThreadSpawnEdgeStatus::Open,
            )
            .await
        {
            warn!("failed to persist thread-spawn edge: {err}");
        }
    }

    async fn live_thread_spawn_descendants(
        &self,
        root_thread_id: ThreadId,
    ) -> CodexResult<Vec<ThreadId>> {
        let mut children_by_parent = self.live_thread_spawn_children().await?;
        let mut descendants = Vec::new();
        let mut stack = children_by_parent
            .remove(&root_thread_id)
            .unwrap_or_default()
            .into_iter()
            .map(|(child_thread_id, _)| child_thread_id)
            .rev()
            .collect::<Vec<_>>();

        while let Some(thread_id) = stack.pop() {
            descendants.push(thread_id);
            if let Some(children) = children_by_parent.remove(&thread_id) {
                for (child_thread_id, _) in children.into_iter().rev() {
                    stack.push(child_thread_id);
                }
            }
        }

        Ok(descendants)
    }
}

fn agent_matches_prefix(agent_path: Option<&AgentPath>, prefix: &AgentPath) -> bool {
    if prefix.is_root() {
        return true;
    }

    agent_path.is_some_and(|agent_path| {
        agent_path == prefix
            || agent_path
                .as_str()
                .strip_prefix(prefix.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

pub(crate) fn render_input_preview(input: &[UserInput]) -> String {
    input
        .iter()
        .map(|item| match item {
            UserInput::Text { text, .. } => text.clone(),
            UserInput::Image { .. } => "[image]".to_string(),
            UserInput::LocalImage { path, .. } => {
                format!("[local_image:{}]", path.display())
            }
            UserInput::Audio { .. } => "[audio]".to_string(),
            UserInput::LocalAudio { path } => {
                format!("[local_audio:{}]", path.display())
            }
            UserInput::Skill { name, path, .. } => {
                format!("[skill:${name}]({})", path.display())
            }
            UserInput::Mention { name, path, .. } => format!("[mention:${name}]({path})"),
            _ => "[input]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn last_task_message_from_communication(communication: &InterAgentCommunication) -> Option<String> {
    if communication.encrypted_content.is_some() {
        return None;
    }
    non_empty_task_message(
        communication
            .content
            .split_once("\nPayload:\n")
            .map_or(communication.content.as_str(), |(_, payload)| payload)
            .to_string(),
    )
}

fn non_empty_task_message(message: String) -> Option<String> {
    (!message.is_empty()).then_some(message)
}

fn thread_spawn_depth(session_source: &SessionSource) -> Option<i32> {
    match session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { depth, .. }) => Some(*depth),
        _ => None,
    }
}
#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
