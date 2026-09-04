use std::fmt;
use std::sync::Arc;

use tokio::sync::TryLockError;
use xedoc_http_client::HttpClientFactory;
use xedoc_login::AuthManager;
use xedoc_protocol::config_types::CollaborationModeMask;
use xedoc_protocol::openai_models::ModelInfo;
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
