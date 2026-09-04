use std::collections::HashSet;
use std::fmt;

use tokio::sync::TryLockError;
use xedoc_http_client::HttpClientFactory;
use xedoc_login::AuthManager;
use xedoc_protocol::config_types::CollaborationModeMask;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ModelsResponse;

use crate::config::ModelsManagerConfig;
use crate::manager::ModelsManager;
use crate::manager::ModelsManagerFuture;
use crate::manager::RefreshStrategy;
use crate::manager::SharedModelsManager;
use crate::model_info::model_info_from_provider_catalog_slug;
use crate::model_info::with_config_overrides;
use crate::registry::ModelRegistry;
use crate::registry::SharedModelRegistry;

/// Applies the user-managed provider registry over a discovered model catalog.
pub struct RegistryModelsManager {
    provider_id: String,
    registry: SharedModelRegistry,
    inner: SharedModelsManager,
}

impl RegistryModelsManager {
    pub fn new(
        provider_id: String,
        registry: SharedModelRegistry,
        inner: SharedModelsManager,
    ) -> Self {
        Self {
            provider_id,
            registry,
            inner,
        }
    }

    fn configured_models(&self, discovered: Vec<ModelInfo>) -> Vec<ModelInfo> {
        let registry = self.registry.snapshot();
        let mut models = configured_models(&registry, &self.provider_id, discovered);
        for model in &mut models {
            if let Some(prompt) =
                load_prompt_override(self.registry.xedoc_home(), &self.provider_id, &model.slug)
            {
                model.base_instructions = prompt;
                model.model_messages = None;
            }
        }
        models
    }
}

impl fmt::Debug for RegistryModelsManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegistryModelsManager")
            .field("provider_id", &self.provider_id)
            .field(
                "registry_path",
                &ModelRegistry::path(self.registry.xedoc_home()),
            )
            .field("inner", &self.inner)
            .finish()
    }
}

impl ModelsManager for RegistryModelsManager {
    fn raw_model_catalog(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ModelsResponse> {
        Box::pin(async move {
            let discovered = self
                .inner
                .raw_model_catalog(refresh_strategy, http_client_factory)
                .await
                .models;
            ModelsResponse {
                models: self.configured_models(discovered),
            }
        })
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        Box::pin(async move {
            let discovered = self.inner.get_remote_models().await;
            self.configured_models(discovered)
        })
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        self.inner
            .try_get_remote_models()
            .map(|discovered| self.configured_models(discovered))
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        self.inner.auth_manager()
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        self.inner.list_collaboration_modes()
    }

    fn get_default_model<'a>(
        &'a self,
        model: &'a Option<String>,
        allow_provider_model_fallback: bool,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'a, String> {
        if model.is_some() {
            return self.inner.get_default_model(
                model,
                allow_provider_model_fallback,
                refresh_strategy,
                http_client_factory,
            );
        }
        if let Some(default_model) = self
            .registry
            .snapshot()
            .provider(&self.provider_id)
            .map(|provider| provider.default_model.clone())
        {
            return Box::pin(async move { default_model });
        }
        self.inner.get_default_model(
            model,
            allow_provider_model_fallback,
            refresh_strategy,
            http_client_factory,
        )
    }

    fn get_model_info<'a>(
        &'a self,
        model: &'a str,
        config: &'a ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, ModelInfo> {
        Box::pin(async move {
            let discovered = self
                .inner
                .get_model_info(model, &ModelsManagerConfig::default())
                .await;
            let configured = self
                .registry
                .snapshot()
                .configured_model(&self.provider_id, &discovered)
                .unwrap_or(discovered);
            let mut configured = with_config_overrides(configured, config);
            if config.base_instructions.is_none()
                && let Some(prompt) =
                    load_prompt_override(self.registry.xedoc_home(), &self.provider_id, model)
            {
                configured.base_instructions = prompt;
                configured.model_messages = None;
            }
            configured
        })
    }

    fn refresh_if_new_etag(
        &self,
        etag: String,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        self.inner.refresh_if_new_etag(etag, http_client_factory)
    }
}

fn configured_models(
    registry: &ModelRegistry,
    provider_id: &str,
    discovered: Vec<ModelInfo>,
) -> Vec<ModelInfo> {
    let mut seen = HashSet::new();
    let mut configured = discovered
        .into_iter()
        .filter_map(|model| {
            seen.insert(model.slug.clone());
            Some(
                registry
                    .configured_model(provider_id, &model)
                    .unwrap_or_else(|| {
                        model_info_from_provider_catalog_slug(&model.slug, provider_id)
                    }),
            )
        })
        .collect::<Vec<_>>();
    configured.extend(
        registry
            .configured_models(provider_id)
            .into_iter()
            .filter(|model| seen.insert(model.slug.clone())),
    );
    configured.sort_by_key(|model| model.priority);
    configured
}

fn load_prompt_override(
    xedoc_home: &std::path::Path,
    provider_id: &str,
    model_id: &str,
) -> Option<String> {
    [
        xedoc_home
            .join("prompts")
            .join(provider_id)
            .join(format!("{model_id}.md")),
        xedoc_home.join("prompts").join(format!("{model_id}.md")),
    ]
    .into_iter()
    .find_map(|path| {
        std::fs::read_to_string(path)
            .ok()
            .filter(|contents| !contents.trim().is_empty())
    })
}

#[cfg(test)]
#[path = "registry_manager_tests.rs"]
mod tests;
