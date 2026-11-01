use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use serde::Deserialize;
use serde::Serialize;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ReasoningEffort;

#[path = "registry_storage.rs"]
mod storage;

pub const MODEL_REGISTRY_SCHEMA_VERSION: u32 = 11;
pub const MODEL_REGISTRY_FILE: &str = "models.json";

/// Exact user pins do not participate in rolling managed-default selection.
pub(crate) enum DefaultModelSelection {
    Pinned(String),
    Preferred(Vec<String>),
}

/// User-managed model settings stored under `$XEDOC_HOME`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelRegistry {
    pub schema_version: u32,
    pub providers: BTreeMap<String, ProviderModelConfig>,
}

/// Defaults and explicit model entries for one provider.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderModelConfig {
    pub display_name: String,
    pub default_model: String,
    pub fast_model: String,
    pub default_reasoning_effort: ReasoningEffort,
    pub template: ManagedModel,
    pub models: BTreeMap<String, ManagedModel>,
}

/// Complete runtime metadata plus optional token pricing for one model.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ManagedModel {
    pub info: ModelInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prices: Option<ModelTokenPrices>,
}

/// Token prices in USD per one million tokens.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelTokenPrices {
    pub input: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context_cache_write_input: Option<f64>,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context_input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context_cached_input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context_output: Option<f64>,
}

/// Shared mutable registry used by configuration, model discovery, and the TUI.
#[derive(Clone, Debug)]
pub struct SharedModelRegistry {
    xedoc_home: PathBuf,
    registry: Arc<RwLock<storage::RegistryState>>,
}

impl PartialEq for SharedModelRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.xedoc_home == other.xedoc_home && self.snapshot() == other.snapshot()
    }
}

impl SharedModelRegistry {
    pub(crate) fn routing_model_aliases(&self) -> BTreeMap<String, BTreeMap<String, String>> {
        self.registry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .routing_aliases
            .clone()
    }

    pub(crate) fn routing_model_roles(&self) -> BTreeMap<String, BTreeMap<String, String>> {
        self.registry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .routing_roles
            .clone()
    }

    pub(crate) fn default_model_selection(&self, provider_id: &str) -> DefaultModelSelection {
        self.registry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .default_model_selection(provider_id)
    }

    pub fn load_or_create(xedoc_home: &Path) -> io::Result<Self> {
        let registry = storage::RegistryState::load_or_create(xedoc_home)?;
        Ok(Self {
            xedoc_home: xedoc_home.to_path_buf(),
            registry: Arc::new(RwLock::new(registry)),
        })
    }

    pub fn snapshot(&self) -> ModelRegistry {
        self.registry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resolved
            .clone()
    }

    /// Overlays explicit user metadata on backend capabilities, not managed defaults.
    pub fn configured_discovered_model(
        &self,
        provider_id: &str,
        discovered: &ModelInfo,
    ) -> io::Result<ModelInfo> {
        self.registry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .configured_discovered_model(provider_id, discovered)
    }

    pub fn update<T>(
        &self,
        edit: impl FnOnce(&mut ModelRegistry) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut registry = self
            .registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut edited = registry.resolved.clone();
        let result = edit(&mut edited)?;
        registry.save_edit(&self.xedoc_home, edited)?;
        Ok(result)
    }

    /// Pins explicit provider choices, including values equal to today's defaults.
    pub fn pin_provider_defaults(
        &self,
        provider_id: &str,
        provider: ProviderModelConfig,
    ) -> io::Result<()> {
        self.registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pin_provider_defaults(&self.xedoc_home, provider_id, provider)
    }

    /// Pins explicitly saved context limits, including unchanged managed defaults.
    pub fn pin_model_context(
        &self,
        provider_id: &str,
        model_id: &str,
        provider: ProviderModelConfig,
    ) -> io::Result<()> {
        self.registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pin_model_context(&self.xedoc_home, provider_id, model_id, provider)
    }

    /// Resolves the current managed catalog while retaining persisted user overrides.
    pub fn reload_catalog(&self) -> io::Result<()> {
        let mut registry = self
            .registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *registry = storage::RegistryState::load_or_create(&self.xedoc_home)?;
        Ok(())
    }

    /// Installs validated managed data without losing concurrent user edits.
    pub fn install_managed_catalog(&self, contents: &str) -> io::Result<()> {
        let mut registry = self
            .registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.xedoc_home.join("model-catalog.json");
        let previous = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        crate::registry_defaults::install_catalog(&self.xedoc_home, contents)?;
        match storage::RegistryState::load(&self.xedoc_home) {
            Ok(next) => {
                *registry = next;
                Ok(())
            }
            Err(error) => {
                let rollback = match previous {
                    Some(bytes) => String::from_utf8(bytes)
                        .map_err(io::Error::other)
                        .and_then(|contents| xedoc_utils_path::write_atomically(&path, &contents)),
                    None => std::fs::remove_file(&path),
                };
                if let Err(rollback_error) = rollback {
                    return Err(io::Error::other(format!(
                        "failed to resolve managed catalog: {error}; failed to restore previous catalog: {rollback_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    pub fn xedoc_home(&self) -> &Path {
        &self.xedoc_home
    }
}

impl ModelRegistry {
    pub fn path(xedoc_home: &Path) -> PathBuf {
        xedoc_home.join(MODEL_REGISTRY_FILE)
    }

    /// Resolves the managed catalog, creating an empty user override file if absent.
    pub fn load_or_create(xedoc_home: &Path) -> io::Result<Self> {
        Ok(storage::RegistryState::load_or_create(xedoc_home)?.resolved)
    }

    pub fn load(xedoc_home: &Path) -> io::Result<Self> {
        Ok(storage::RegistryState::load(xedoc_home)?.resolved)
    }

    pub fn save(&self, xedoc_home: &Path) -> io::Result<()> {
        storage::RegistryState::load_or_create(xedoc_home)?.save_edit(xedoc_home, self.clone())
    }

    pub fn provider(&self, provider_id: &str) -> Option<&ProviderModelConfig> {
        self.providers.get(provider_id)
    }

    pub fn provider_mut(&mut self, provider_id: &str) -> Option<&mut ProviderModelConfig> {
        self.providers.get_mut(provider_id)
    }

    pub fn configured_model(&self, provider_id: &str, discovered: &ModelInfo) -> Option<ModelInfo> {
        let provider = self.provider(provider_id)?;
        let configured = provider
            .models
            .get(&discovered.slug)
            .unwrap_or(&provider.template);
        let mut info = configured.info.clone();
        if configured.info.slug == "*" {
            info.slug.clone_from(&discovered.slug);
            info.display_name.clone_from(&discovered.display_name);
            info.description.clone_from(&discovered.description);
            info.priority = discovered.priority;
            info.visibility = discovered.visibility;
            info.supported_in_api = discovered.supported_in_api;
        }
        Some(info)
    }

    pub fn configured_models(&self, provider_id: &str) -> Vec<ModelInfo> {
        self.provider(provider_id)
            .map(|provider| {
                provider
                    .models
                    .values()
                    .map(|model| model.info.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn model_prices(&self, provider_id: &str) -> BTreeMap<String, ModelTokenPrices> {
        self.provider(provider_id)
            .map(|provider| {
                provider
                    .models
                    .iter()
                    .filter_map(|(model, configured)| {
                        configured
                            .prices
                            .clone()
                            .map(|prices| (model.clone(), prices))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema_version != MODEL_REGISTRY_SCHEMA_VERSION {
            return Err(invalid_registry(format!(
                "unsupported schema_version {}; expected {MODEL_REGISTRY_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        for (provider_id, provider) in &self.providers {
            if provider_id.trim().is_empty() {
                return Err(invalid_registry("provider IDs must not be empty"));
            }
            if !provider.models.contains_key(&provider.default_model) {
                return Err(invalid_registry(format!(
                    "provider {provider_id} default_model `{}` is not configured",
                    provider.default_model
                )));
            }
            if !provider.models.contains_key(&provider.fast_model) {
                return Err(invalid_registry(format!(
                    "provider {provider_id} fast_model `{}` is not configured",
                    provider.fast_model
                )));
            }
            validate_managed_model(provider_id, "*", &provider.template)?;
            for (model_id, model) in &provider.models {
                if model.info.slug != *model_id {
                    return Err(invalid_registry(format!(
                        "provider {provider_id} model key `{model_id}` does not match slug `{}`",
                        model.info.slug
                    )));
                }
                validate_managed_model(provider_id, model_id, model)?;
            }
            let default_model = &provider.models[&provider.default_model].info;
            if !default_model
                .supported_reasoning_levels
                .iter()
                .any(|preset| preset.effort == provider.default_reasoning_effort)
            {
                return Err(invalid_registry(format!(
                    "provider {provider_id} default reasoning effort `{}` is not supported by `{}`",
                    provider.default_reasoning_effort, provider.default_model
                )));
            }
        }
        Ok(())
    }
}

fn validate_managed_model(
    provider_id: &str,
    model_id: &str,
    model: &ManagedModel,
) -> io::Result<()> {
    let context_window = model.info.resolved_context_window().ok_or_else(|| {
        invalid_registry(format!(
            "provider {provider_id} model `{model_id}` must define a context window"
        ))
    })?;
    if context_window <= 0 {
        return Err(invalid_registry(format!(
            "provider {provider_id} model `{model_id}` context window must be positive"
        )));
    }
    let max_context_window = model.info.max_context_window.unwrap_or(context_window);
    if max_context_window < context_window {
        return Err(invalid_registry(format!(
            "provider {provider_id} model `{model_id}` maximum context window must be at least {context_window}"
        )));
    }
    let max_compact_limit = context_window.saturating_mul(9) / 10;
    if let Some(compact_limit) = model.info.auto_compact_token_limit
        && (compact_limit <= 0 || compact_limit > max_compact_limit)
    {
        return Err(invalid_registry(format!(
            "provider {provider_id} model `{model_id}` compaction limit must be between 1 and {max_compact_limit}"
        )));
    }
    if let Some(prices) = &model.prices
        && [
            Some(prices.input),
            prices.cached_input,
            prices.cache_write_input,
            prices.long_context_cache_write_input,
            Some(prices.output),
            prices.long_context_input,
            prices.long_context_cached_input,
            prices.long_context_output,
        ]
        .into_iter()
        .flatten()
        .any(|price| !price.is_finite() || price < 0.0)
    {
        return Err(invalid_registry(format!(
            "provider {provider_id} model `{model_id}` prices must be finite and non-negative"
        )));
    }
    Ok(())
}

fn invalid_registry(message: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid model registry: {}", message.into()),
    )
}
