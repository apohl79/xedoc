use super::session::Session;
use super::step_context::StepContext;
use crate::context::ApprovalPromptContext;
use crate::context::FileSystemContext;
use crate::context::NetworkContext;
use crate::context::world_state::AgentsMdState;
use crate::context::world_state::CollaborationModeState;
use crate::context::world_state::EnvironmentState;
use crate::context::world_state::EnvironmentsInstructionsState;
use crate::context::world_state::EnvironmentsState;
use crate::context::world_state::PermissionsState;
use crate::context::world_state::PluginsInstructionsState;
use crate::context::world_state::RealtimeState;
use crate::context::world_state::WorldState;
use codex_extension_api::WorldStateContributionInput;
use codex_features::Feature;
use std::collections::BTreeMap;

impl Session {
    #[tracing::instrument(name = "world_state.build", level = "info", skip_all)]
    pub(crate) async fn build_world_state_for_step(
        &self,
        step_context: &StepContext,
    ) -> WorldState {
        let turn_context = step_context.turn.as_ref();
        tracing::trace!(
            selected_capability_root_count = step_context.selected_capability_roots.len(),
            "building step world state"
        );
        let environment_subagents = if turn_context.config.include_environment_context {
            self.services
                .agent_control
                .format_environment_context_subagents(self.thread_id)
                .await
        } else {
            String::new()
        };
        let mut world_state = WorldState::default();
        world_state.add_section(RealtimeState::new(
            turn_context.realtime_active,
            turn_context
                .config
                .experimental_realtime_start_instructions
                .as_deref(),
        ));
        world_state.add_section(AgentsMdState::new(
            step_context
                .loaded_agents_md
                .as_deref()
                .map(|loaded| loaded.contextual_user_fragment()),
        ));
        if turn_context.config.include_permissions_instructions {
            let permission_profile = turn_context.permission_profile();
            let model_messages = turn_context.model_info.model_messages.as_ref();
            let exec_policy = self.services.exec_policy.current();
            world_state.add_section(PermissionsState::new(
                &permission_profile,
                turn_context.approval_policy.value(),
                ApprovalPromptContext::new(
                    model_messages.and_then(|messages| messages.approvals.as_ref()),
                    model_messages.and_then(|messages| messages.permissions.as_ref()),
                ),
                exec_policy.as_ref(),
                #[allow(deprecated)]
                &turn_context.cwd,
                turn_context
                    .config
                    .features
                    .enabled(Feature::ExecPermissionApprovals),
                turn_context
                    .config
                    .features
                    .enabled(Feature::RequestPermissionsTool),
            ));
        }
        if turn_context.config.include_collaboration_mode_instructions
            && let Some(collaboration_mode) =
                CollaborationModeState::from_collaboration_mode(&turn_context.collaboration_mode())
        {
            world_state.add_section(collaboration_mode);
        }
        if turn_context.config.include_environment_context {
            let mut environment_contexts = step_context
                .environments
                .turn_environments()
                .map(|environment| {
                    (
                        environment.environment_id.clone(),
                        EnvironmentState::available(
                            environment.cwd().clone(),
                            environment
                                .shell
                                .as_ref()
                                .map(|shell| shell.name().to_string()),
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            for environment in step_context.environments.starting() {
                environment_contexts
                    .entry(environment.selection.environment_id.clone())
                    .or_insert_with(|| {
                        EnvironmentState::starting(environment.selection.cwd.clone())
                    });
            }
            let workspace_roots = step_context
                .environments
                .primary()
                .map(|environment| environment.workspace_roots())
                .unwrap_or_default();
            let requirements = turn_context.config.config_layer_stack.requirements();
            let network = requirements.network.as_ref().map(|network| {
                NetworkContext::new(
                    network
                        .domains
                        .as_ref()
                        .and_then(codex_config::NetworkDomainPermissionsToml::allowed_domains)
                        .unwrap_or_default(),
                    network
                        .domains
                        .as_ref()
                        .and_then(codex_config::NetworkDomainPermissionsToml::denied_domains)
                        .unwrap_or_default(),
                )
            });
            world_state.add_section(
                EnvironmentsState {
                    environments: environment_contexts,
                    current_date: turn_context.current_date.clone(),
                    timezone: turn_context.timezone.clone(),
                    network,
                    filesystem: Some(FileSystemContext::from_permission_profile(
                        turn_context.config.permissions.permission_profile(),
                        workspace_roots,
                    )),
                    subagents: None,
                }
                .with_subagents(environment_subagents),
            );
        }
        world_state.add_section(EnvironmentsInstructionsState::new(
            turn_context.config.include_environment_context
                && turn_context
                    .config
                    .features
                    .enabled(Feature::DeferredExecutor),
        ));
        world_state.add_section(PluginsInstructionsState::new(
            step_context.mcp.plugins_available(),
        ));
        let environments = step_context.environments.to_selections();
        let ready_selected_capability_roots = step_context
            .selected_capability_roots
            .iter()
            .map(|root| root.selected_root().clone())
            .collect::<Vec<_>>();
        for contributor in self.services.extensions.context_contributors() {
            for section in contributor
                .contribute_world_state(WorldStateContributionInput {
                    thread_id: self.thread_id(),
                    turn_id: turn_context.sub_id.as_str(),
                    environments: &environments,
                    ready_selected_capability_roots: &ready_selected_capability_roots,
                    executor_capability_discovery: step_context
                        .executor_capability_discovery
                        .as_deref(),
                    session_store: &self.services.session_extension_data,
                    thread_store: &self.services.thread_extension_data,
                    turn_store: turn_context.extension_data.as_ref(),
                })
                .await
            {
                world_state.add_extension_section(section);
            }
        }
        world_state
    }
}
