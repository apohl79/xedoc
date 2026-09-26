use std::collections::HashSet;
use std::fmt;

use tokio::sync::TryLockError;
use xedoc_http_client::HttpClientFactory;
use xedoc_login::AuthManager;
use xedoc_protocol::config_types::CollaborationModeMask;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ModelsResponse;

use crate::config::ModelsManagerConfig;
use crate::instructions::load_prompt_override;
use crate::instructions::resolve_instructions;
use crate::manager::ModelsManager;
use crate::manager::ModelsManagerFuture;
use crate::manager::RefreshStrategy;
use crate::manager::SharedModelsManager;
use crate::model_info::model_info_from_provider_catalog_slug;
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
        let provider_display_name = registry
            .provider(&self.provider_id)
            .map(|provider| provider.display_name.as_str())
            .unwrap_or(self.provider_id.as_str());
        let config = ModelsManagerConfig {
            personality_enabled: true,
            ..Default::default()
        };
        let mut seen = HashSet::new();
        let mut models = discovered
            .into_iter()
            .map(|catalog_model| {
                seen.insert(catalog_model.slug.clone());
                let model = registry
                    .configured_model(&self.provider_id, &catalog_model)
                    .unwrap_or_else(|| {
                        model_info_from_provider_catalog_slug(
                            &catalog_model.slug,
                            provider_display_name,
                        )
                    });
                let prompt_override = load_prompt_override(
                    self.registry.xedoc_home(),
                    &self.provider_id,
                    &model.slug,
                );
                resolve_instructions(
                    model,
                    Some(&catalog_model),
                    &config,
                    Some(provider_display_name),
                    prompt_override.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        models.extend(
            registry
                .configured_models(&self.provider_id)
                .into_iter()
                .filter(|model| seen.insert(model.slug.clone()))
                .map(|model| {
                    let prompt_override = load_prompt_override(
                        self.registry.xedoc_home(),
                        &self.provider_id,
                        &model.slug,
                    );
                    resolve_instructions(
                        model,
                        None,
                        &config,
                        Some(provider_display_name),
                        prompt_override.as_deref(),
                    )
                }),
        );
        models.sort_by_key(|model| model.priority);
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

    fn try_get_raw_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        self.inner.try_get_raw_remote_models()
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
            let catalog_model = self
                .inner
                .get_model_info(model, &ModelsManagerConfig::default())
                .await;
            let registry = self.registry.snapshot();
            let provider_display_name = registry
                .provider(&self.provider_id)
                .map(|provider| provider.display_name.as_str())
                .unwrap_or(self.provider_id.as_str());
            let configured = registry
                .configured_model(&self.provider_id, &catalog_model)
                .unwrap_or(catalog_model.clone());
            let prompt_override =
                load_prompt_override(self.registry.xedoc_home(), &self.provider_id, model);
            resolve_instructions(
                configured,
                Some(&catalog_model),
                config,
                Some(provider_display_name),
                prompt_override.as_deref(),
            )
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

#[cfg(test)]
#[path = "registry_manager_tests.rs"]
mod tests;
