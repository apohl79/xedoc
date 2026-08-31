use crate::agents_md::LoadedAgentsMd;
use crate::agents_md::load_project_instructions;
use std::sync::Arc;
use tokio::sync::Mutex;
use xedoc_core_config::config::Config;
use xedoc_core_environment::TurnEnvironmentSnapshot;
use xedoc_extension_api::UserInstructions;
use xedoc_extension_api::UserInstructionsProvider;
use xedoc_protocol::protocol::TurnEnvironmentSelection;

/// Owns the inputs and cached result of AGENTS.md discovery for a session.
pub struct AgentsMdManager {
    user_instructions_provider: Option<Arc<dyn UserInstructionsProvider>>,
    cache: Mutex<AgentsMdCache>,
}

#[derive(Default)]
struct AgentsMdCache {
    selections: Option<Vec<TurnEnvironmentSelection>>,
    user_instructions: Option<UserInstructions>,
    loaded: Option<Arc<LoadedAgentsMd>>,
}

impl AgentsMdManager {
    pub fn new(user_instructions: Option<UserInstructions>) -> Self {
        Self::new_with_user_instructions_provider(user_instructions, None)
    }

    /// Creates a manager that refreshes host-provided global instructions.
    pub fn new_with_user_instructions_provider(
        user_instructions: Option<UserInstructions>,
        user_instructions_provider: Option<Arc<dyn UserInstructionsProvider>>,
    ) -> Self {
        Self {
            user_instructions_provider,
            cache: Mutex::new(AgentsMdCache {
                user_instructions: user_instructions
                    .filter(|instructions| !instructions.text.trim().is_empty()),
                ..Default::default()
            }),
        }
    }

    #[tracing::instrument(name = "agents_md.refresh", skip_all)]
    pub async fn refresh(&self, config: &Config, environments: &TurnEnvironmentSnapshot) {
        let user_instructions = self.current_user_instructions().await;
        let selections = environments.to_selections();
        let mut cache = self.cache.lock().await;
        if cache.selections.as_ref() == Some(&selections) {
            if cache.user_instructions == user_instructions {
                return;
            }
            cache.loaded = cache
                .loaded
                .as_ref()
                .and_then(|loaded| loaded.with_user_instructions(user_instructions.clone()))
                .map(Arc::new)
                .or_else(|| {
                    user_instructions.clone().map(|instructions| {
                        Arc::new(LoadedAgentsMd::new_user(
                            instructions.text,
                            instructions.source,
                        ))
                    })
                });
            cache.user_instructions = user_instructions;
            return;
        }
        drop(cache);

        let loaded = load_project_instructions(config, user_instructions.clone(), environments)
            .await
            .map(Arc::new);
        let mut cache = self.cache.lock().await;
        cache.selections = Some(selections);
        cache.user_instructions = user_instructions;
        cache.loaded = loaded;
    }

    pub async fn get_loaded(&self) -> Option<Arc<LoadedAgentsMd>> {
        self.cache.lock().await.loaded.clone()
    }

    pub async fn user_instructions(&self) -> Option<UserInstructions> {
        self.cache.lock().await.user_instructions.clone()
    }

    async fn current_user_instructions(&self) -> Option<UserInstructions> {
        let Some(provider) = &self.user_instructions_provider else {
            return self.cache.lock().await.user_instructions.clone();
        };
        provider
            .load_user_instructions()
            .await
            .instructions
            .filter(|instructions| !instructions.text.trim().is_empty())
    }
}
