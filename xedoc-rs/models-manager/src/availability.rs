use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use tokio::sync::TryLockError;
use xedoc_http_client::HttpClientFactory;
use xedoc_login::AuthManager;
use xedoc_protocol::config_types::CollaborationModeMask;
use xedoc_protocol::openai_models::ModelAvailability;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ModelPreset;
use xedoc_protocol::openai_models::ModelsResponse;

use crate::ModelsManagerConfig;
use crate::manager::ModelsManager;
use crate::manager::ModelsManagerFuture;
use crate::manager::RefreshStrategy;
use crate::manager::SharedModelsManager;

/// Hides an inactive provider's catalog until its runtime availability predicate succeeds.
pub struct AvailabilityGatedModelsManager {
    inner: SharedModelsManager,
    is_available: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl AvailabilityGatedModelsManager {
    pub fn new(
        inner: SharedModelsManager,
        is_available: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            is_available: Arc::new(is_available),
        }
    }
}

impl fmt::Debug for AvailabilityGatedModelsManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AvailabilityGatedModelsManager")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl ModelsManager for AvailabilityGatedModelsManager {
    fn unlisted_model_availability_for_provider(&self, provider_id: &str) -> ModelAvailability {
        if (self.is_available)() {
            self.inner
                .unlisted_model_availability_for_provider(provider_id)
        } else {
            ModelAvailability::Unavailable
        }
    }

    fn routing_model_aliases(&self) -> BTreeMap<String, BTreeMap<String, String>> {
        self.inner.routing_model_aliases()
    }

    fn routing_model_roles(&self) -> BTreeMap<String, BTreeMap<String, String>> {
        self.inner.routing_model_roles()
    }

    fn build_available_models(&self, models: Vec<ModelInfo>) -> Vec<ModelPreset> {
        self.inner.build_available_models(models)
    }

    fn model_availability(&self, model: &str) -> ModelAvailability {
        if (self.is_available)() {
            self.inner.model_availability(model)
        } else {
            ModelAvailability::Unavailable
        }
    }

    fn raw_model_catalog(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ModelsResponse> {
        if !(self.is_available)() {
            return Box::pin(async { ModelsResponse { models: Vec::new() } });
        }
        self.inner
            .raw_model_catalog(refresh_strategy, http_client_factory)
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        if !(self.is_available)() {
            return Box::pin(async { Vec::new() });
        }
        self.inner.get_remote_models()
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        if !(self.is_available)() {
            return Ok(Vec::new());
        }
        self.inner.try_get_remote_models()
    }

    fn try_get_raw_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        if !(self.is_available)() {
            return Ok(Vec::new());
        }
        self.inner.try_get_raw_remote_models()
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        self.inner.auth_manager()
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        self.inner.list_collaboration_modes()
    }

    fn get_model_info<'a>(
        &'a self,
        model: &'a str,
        config: &'a ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, ModelInfo> {
        self.inner.get_model_info(model, config)
    }

    fn get_model_info_for_provider<'a>(
        &'a self,
        model: &'a str,
        provider_id: &'a str,
        config: &'a ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, ModelInfo> {
        self.inner
            .get_model_info_for_provider(model, provider_id, config)
    }

    fn prioritize_provider(&self, provider_id: &str) {
        self.inner.prioritize_provider(provider_id);
    }

    fn refresh_if_new_etag(
        &self,
        etag: String,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        if !(self.is_available)() {
            return Box::pin(async {});
        }
        self.inner.refresh_if_new_etag(etag, http_client_factory)
    }
}
