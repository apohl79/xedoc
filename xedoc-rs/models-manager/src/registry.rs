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
use xedoc_utils_path::write_atomically;

pub const MODEL_REGISTRY_SCHEMA_VERSION: u32 = 1;
pub const MODEL_REGISTRY_FILE: &str = "models.json";

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
    registry: Arc<RwLock<ModelRegistry>>,
}

impl PartialEq for SharedModelRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.xedoc_home == other.xedoc_home && self.snapshot() == other.snapshot()
    }
}

impl SharedModelRegistry {
    pub fn load_or_create(xedoc_home: &Path) -> io::Result<Self> {
        let registry = ModelRegistry::load_or_create(xedoc_home)?;
        Ok(Self {
            xedoc_home: xedoc_home.to_path_buf(),
            registry: Arc::new(RwLock::new(registry)),
        })
    }

    pub fn snapshot(&self) -> ModelRegistry {
        self.registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn update<T>(
        &self,
        edit: impl FnOnce(&mut ModelRegistry) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut registry = self
            .registry
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = registry.clone();
        let result = match edit(&mut registry) {
            Ok(result) => result,
            Err(error) => {
                *registry = previous;
                return Err(error);
            }
        };
        if let Err(error) = registry.save(&self.xedoc_home) {
            *registry = previous;
            return Err(error);
        }
        Ok(result)
    }

    pub fn xedoc_home(&self) -> &Path {
        &self.xedoc_home
    }
}

impl ModelRegistry {
    pub fn path(xedoc_home: &Path) -> PathBuf {
        xedoc_home.join(MODEL_REGISTRY_FILE)
    }

    /// Loads the registry, writing a complete starter file when it is absent.
    pub fn load_or_create(xedoc_home: &Path) -> io::Result<Self> {
        let path = Self::path(xedoc_home);
        if !path.exists() {
            let registry = Self::default_registry()?;
            registry.save(xedoc_home)?;
            return Ok(registry);
        }
        Self::load(xedoc_home)
    }

    pub fn load(xedoc_home: &Path) -> io::Result<Self> {
        let path = Self::path(xedoc_home);
        let bytes = std::fs::read(&path)?;
        let registry: Self = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to parse {}: {error}", path.display()),
            )
        })?;
        registry.validate()?;
        Ok(registry)
    }

    pub fn save(&self, xedoc_home: &Path) -> io::Result<()> {
        self.validate()?;
        let path = Self::path(xedoc_home);
        let contents = format!(
            "{}\n",
            serde_json::to_string_pretty(self).map_err(io::Error::other)?
        );
        write_atomically(&path, &contents)
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

    fn default_registry() -> io::Result<Self> {
        crate::registry_defaults::default_registry()
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
    if let Some(compact_limit) = model.info.auto_compact_token_limit
        && (compact_limit <= 0 || compact_limit > context_window)
    {
        return Err(invalid_registry(format!(
            "provider {provider_id} model `{model_id}` compaction limit must be between 1 and {context_window}"
        )));
    }
    if model.info.base_instructions.trim().is_empty() {
        return Err(invalid_registry(format!(
            "provider {provider_id} model `{model_id}` base instructions must not be empty"
        )));
    }
    if let Some(prices) = &model.prices
        && [
            Some(prices.input),
            prices.cached_input,
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

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
