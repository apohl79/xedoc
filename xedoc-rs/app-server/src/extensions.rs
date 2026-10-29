use std::sync::Arc;
use std::sync::Weak;

use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ThreadGoal;
use xedoc_app_server_protocol::ThreadGoalUpdatedNotification;
use xedoc_core::ThreadManager;
use xedoc_core::config::Config;
use xedoc_exec_server::EnvironmentManager;
use xedoc_extension_api::ExtensionEventSink;
use xedoc_extension_api::ExtensionRegistry;
use xedoc_extension_api::ExtensionRegistryBuilder;
use xedoc_goal_extension::GoalService;
use xedoc_login::AuthManager;
use xedoc_protocol::protocol::Event;
use xedoc_protocol::protocol::EventMsg;
use xedoc_rollout::state_db::StateDbHandle;
use xedoc_thread_store::ThreadStore;

use crate::outgoing_message::OutgoingMessageSender;
use crate::thread_state::ThreadListenerCommand;
use crate::thread_state::ThreadStateManager;

pub(crate) struct ThreadExtensionDependencies {
    pub(crate) event_sink: Arc<dyn ExtensionEventSink>,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) state_db: Option<StateDbHandle>,
    pub(crate) thread_manager: Weak<ThreadManager>,
    pub(crate) goal_service: Arc<GoalService>,
    pub(crate) environment_manager: Arc<EnvironmentManager>,
    pub(crate) executor_skill_provider: Arc<dyn xedoc_skills_extension::SkillProvider>,
    /// Process-scoped persistence backend for extensions that need stored thread history.
    pub(crate) thread_store: Arc<dyn ThreadStore>,
}

pub(crate) fn thread_extensions(
    dependencies: ThreadExtensionDependencies,
) -> Arc<ExtensionRegistry<Config>> {
    let ThreadExtensionDependencies {
        event_sink,
        auth_manager,
        state_db,
        thread_manager,
        goal_service,
        environment_manager,
        executor_skill_provider,
        thread_store: _thread_store,
    } = dependencies;
    let mut builder = ExtensionRegistryBuilder::<Config>::with_event_sink(event_sink);
    if let Some(state_db) = state_db {
        xedoc_goal_extension::install_with_backend(
            &mut builder,
            state_db,
            xedoc_otel::global(),
            thread_manager,
            goal_service,
            |config: &Config| config.features.enabled(xedoc_features::Feature::Goals),
        );
    }
    xedoc_mcp_extension::install_executor_plugins(&mut builder, environment_manager);
    xedoc_web_search_extension::install(&mut builder, auth_manager.clone());
    xedoc_image_generation_extension::install(&mut builder, auth_manager, |config: &Config| {
        Some(config.xedoc_home.clone())
    });
    let skill_providers = xedoc_skills_extension::SkillProviders::new()
        .with_executor_provider(executor_skill_provider)
        .with_host_provider(Arc::new(xedoc_skills_extension::HostSkillProvider::new()));
    xedoc_skills_extension::install_with_providers_and_metrics(
        &mut builder,
        skill_providers,
        xedoc_otel::global(),
        |config: &Config| xedoc_skills_extension::SkillsExtensionConfig {
            include_instructions: config.include_skill_instructions,
            bundled_skills_enabled: config.bundled_skills_enabled(),
            orchestrator_skills_enabled: config.orchestrator_skills_enabled,
            shadow_selection_enabled: config
                .features
                .enabled(xedoc_features::Feature::SkillSearch),
        },
    );
    Arc::new(builder.build())
}

pub(crate) fn app_server_extension_event_sink(
    outgoing: Arc<OutgoingMessageSender>,
    thread_state_manager: ThreadStateManager,
) -> Arc<dyn ExtensionEventSink> {
    Arc::new(AppServerExtensionEventSink {
        outgoing,
        thread_state_manager,
    })
}

struct AppServerExtensionEventSink {
    outgoing: Arc<OutgoingMessageSender>,
    thread_state_manager: ThreadStateManager,
}

impl ExtensionEventSink for AppServerExtensionEventSink {
    fn emit(&self, event: Event) {
        match event.msg {
            EventMsg::ThreadGoalUpdated(thread_goal_event) => {
                let thread_id = thread_goal_event.thread_id;
                let turn_id = thread_goal_event.turn_id;
                let goal: ThreadGoal = thread_goal_event.goal.into();
                if let Some(listener_command_tx) = self
                    .thread_state_manager
                    .current_listener_command_tx(thread_id)
                {
                    let command = ThreadListenerCommand::EmitThreadGoalUpdated {
                        turn_id: turn_id.clone(),
                        goal: goal.clone(),
                    };
                    if listener_command_tx.send(command).is_ok() {
                        return;
                    }
                    tracing::warn!(
                        "failed to enqueue extension goal update for {thread_id}: listener command channel is closed"
                    );
                }
                let outgoing = Arc::clone(&self.outgoing);
                tokio::spawn(async move {
                    outgoing
                        .send_server_notification(ServerNotification::ThreadGoalUpdated(
                            ThreadGoalUpdatedNotification {
                                thread_id: thread_id.to_string(),
                                turn_id,
                                goal,
                            },
                        ))
                        .await;
                });
            }
            msg => {
                tracing::debug!(event_id = %event.id, ?msg, "dropping unsupported extension event");
            }
        }
    }
}
