use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use xedoc_http_client::ClientRouteClass;
use xedoc_http_client::HttpClientFactory;
use xedoc_protocol::openai_models::InputModality;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::openai_models::ReasoningEffortPreset;
use xedoc_utils_path::write_atomically;

use crate::registry::ManagedModel;
use crate::registry::ModelRegistry;
use crate::registry::ModelTokenPrices;
use crate::registry::SharedModelRegistry;

pub(crate) const CACHE_FILE: &str = "model-catalog.json";
const SOURCE: &str =
    "https://raw.githubusercontent.com/apohl79/xedoc/main/xedoc-rs/models-manager/catalog.json";
const MAX_BYTES: usize = 131_072;
const MAX_ROUTING_ROLES_BYTES: usize = 900;
const REFRESH_INTERVAL: Duration = Duration::from_secs(3600);
static LAST_REFRESH: Mutex<Option<(PathBuf, Instant)>> = Mutex::new(None);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Catalog {
    schema_version: u32,
    pub(crate) revision: u64,
    pub(crate) providers: BTreeMap<String, ProviderPreferences>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderPreferences {
    default_model: String,
    fast_model: String,
    default_reasoning_effort: Option<ReasoningEffort>,
    models: Vec<String>,
    #[serde(default)]
    pub(crate) roles: BTreeMap<String, String>,
    #[serde(default)]
    prices: BTreeMap<String, ModelTokenPrices>,
    #[serde(default)]
    capabilities: BTreeMap<String, ModelCapabilities>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelCapabilities {
    context_window: i64,
    max_context_window: i64,
    auto_compact_token_limit: Option<i64>,
    effective_context_window_percent: Option<i64>,
    default_reasoning_effort: ReasoningEffort,
    reasoning_efforts: Vec<ReasoningEffort>,
    input_modalities: Vec<InputModality>,
    #[serde(default)]
    supports_reasoning_summary_parameter: bool,
    #[serde(default)]
    supports_parallel_tool_calls: bool,
}

impl Catalog {
    pub(crate) fn parse(contents: &str) -> io::Result<Self> {
        if contents.len() > MAX_BYTES {
            return Err(io::Error::other("Managed model catalog exceeds size limit"));
        }
        let catalog: Self = serde_json::from_str(contents).map_err(io::Error::other)?;
        if catalog.schema_version != 1 || catalog.revision == 0 || catalog.providers.len() != 4 {
            return Err(io::Error::other("Invalid managed model catalog version"));
        }
        for (id, provider) in &catalog.providers {
            let ids = provider.models.iter().collect::<BTreeSet<_>>();
            if !matches!(id.as_str(), "openai" | "anthropic" | "google" | "deepseek")
                || ids.is_empty()
                || ids.len() > 64
                || ids.len() != provider.models.len()
                || !ids.contains(&provider.default_model)
                || !ids.contains(&provider.fast_model)
                || matches!(id.as_str(), "openai" | "anthropic") && provider.roles.len() != 4
                || ids.iter().any(|id| {
                    id.is_empty()
                        || id.len() > 128
                        || id.contains("..")
                        || !id.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric()
                                || matches!(byte, b'-' | b'_' | b'.' | b':')
                        })
                })
                || provider.roles.iter().any(|(role, model)| {
                    !matches!(role.as_str(), "fast" | "balanced" | "strong" | "flagship")
                        || !ids.contains(model)
                })
                || provider.prices.keys().any(|model| !ids.contains(model))
                || provider.capabilities.iter().any(|(model, capabilities)| {
                    !ids.contains(model)
                        || capabilities.reasoning_efforts.is_empty()
                        || capabilities.reasoning_efforts.len() > 16
                        || capabilities
                            .reasoning_efforts
                            .iter()
                            .collect::<HashSet<_>>()
                            .len()
                            != capabilities.reasoning_efforts.len()
                        || !capabilities
                            .reasoning_efforts
                            .contains(&capabilities.default_reasoning_effort)
                        || capabilities.reasoning_efforts.iter().any(|effort| {
                            let effort = effort.as_str();
                            effort.is_empty()
                                || effort.len() > 32
                                || !effort.bytes().all(|byte| {
                                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
                                })
                        })
                        || capabilities.input_modalities.len() > 3
                        || !capabilities.input_modalities.contains(&InputModality::Text)
                        || capabilities
                            .effective_context_window_percent
                            .is_some_and(|percent| !(1..=100).contains(&percent))
                })
            {
                return Err(io::Error::other("Invalid managed provider preferences"));
            }
        }
        if serde_json::to_vec(&catalog.routing_roles())
            .map_err(io::Error::other)?
            .len()
            > MAX_ROUTING_ROLES_BYTES
        {
            return Err(io::Error::other(
                "Managed routing roles exceed context limit",
            ));
        }
        Ok(catalog)
    }

    pub(crate) fn routing_roles(&self) -> BTreeMap<String, BTreeMap<String, String>> {
        self.providers
            .iter()
            .filter(|(provider, _)| matches!(provider.as_str(), "openai" | "anthropic"))
            .map(|(provider, preferences)| (provider.clone(), preferences.roles.clone()))
            .collect()
    }

    pub(crate) fn bundled() -> io::Result<Self> {
        Self::parse(include_str!("../catalog.json"))
    }

    pub(crate) fn load(home: &Path) -> io::Result<Self> {
        let bundled = Self::bundled()?;
        let path = home.join(CACHE_FILE);
        if let Ok(metadata) = path.metadata()
            && metadata.len() <= MAX_BYTES as u64
            && let Ok(contents) = std::fs::read_to_string(path)
            && let Ok(catalog) = Self::parse(&contents)
            && catalog.revision >= bundled.revision
            && catalog
                .apply(crate::registry_defaults::legacy_defaults()?)
                .is_ok()
        {
            return Ok(catalog);
        }
        Ok(bundled)
    }

    pub(crate) fn apply(&self, mut registry: ModelRegistry) -> io::Result<ModelRegistry> {
        for (id, preferences) in &self.providers {
            let provider = registry
                .providers
                .get_mut(id)
                .ok_or_else(|| io::Error::other("Unknown managed provider"))?;
            let mut models = BTreeMap::new();
            for (index, id) in preferences.models.iter().enumerate() {
                let mut model = provider.models.get(id).cloned().unwrap_or_else(|| {
                    let mut info = provider.template.info.clone();
                    info.slug.clone_from(id);
                    info.display_name.clone_from(id);
                    info.used_fallback_model_metadata = true;
                    ManagedModel { info, prices: None }
                });
                model.info.priority = i32::try_from(index).unwrap_or(i32::MAX);
                if let Some(prices) = preferences.prices.get(id) {
                    model.prices = Some(prices.clone());
                }
                if let Some(capabilities) = preferences.capabilities.get(id) {
                    model.info.context_window = Some(capabilities.context_window);
                    model.info.max_context_window = Some(capabilities.max_context_window);
                    model.info.auto_compact_token_limit = capabilities.auto_compact_token_limit;
                    if let Some(percent) = capabilities.effective_context_window_percent {
                        model.info.effective_context_window_percent = percent;
                    }
                    model.info.default_reasoning_level =
                        Some(capabilities.default_reasoning_effort.clone());
                    model.info.supported_reasoning_levels = capabilities
                        .reasoning_efforts
                        .iter()
                        .map(|effort| ReasoningEffortPreset {
                            effort: effort.clone(),
                            description: effort.as_str().to_string(),
                        })
                        .collect();
                    model
                        .info
                        .input_modalities
                        .clone_from(&capabilities.input_modalities);
                    model.info.supports_reasoning_summary_parameter =
                        capabilities.supports_reasoning_summary_parameter;
                    model.info.supports_parallel_tool_calls =
                        capabilities.supports_parallel_tool_calls;
                    model.info.used_fallback_model_metadata = false;
                }
                models.insert(id.clone(), model);
            }
            provider.models = models;
            provider
                .default_model
                .clone_from(&preferences.default_model);
            provider.fast_model.clone_from(&preferences.fast_model);
            if let Some(effort) = &preferences.default_reasoning_effort {
                provider.default_reasoning_effort.clone_from(effort);
            }
        }
        registry.validate()?;
        Ok(registry)
    }
}

pub(crate) fn install(home: &Path, contents: &str) -> io::Result<()> {
    let catalog = Catalog::parse(contents)?;
    if catalog.revision < Catalog::load(home)?.revision {
        return Err(io::Error::other("Refusing managed model catalog rollback"));
    }
    catalog.apply(crate::registry_defaults::legacy_defaults()?)?;
    write_atomically(&home.join(CACHE_FILE), contents)
}

pub(crate) fn schedule_refresh(registry: SharedModelRegistry, factory: HttpClientFactory) {
    let home = registry.xedoc_home().to_path_buf();
    let Ok(mut last) = LAST_REFRESH.lock() else {
        return;
    };
    if last
        .as_ref()
        .is_some_and(|(path, time)| path == &home && time.elapsed() < REFRESH_INTERVAL)
    {
        return;
    }
    *last = Some((home, Instant::now()));
    drop(last);
    tokio::spawn(async move {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let client = factory
                .build_client(SOURCE, ClientRouteClass::Other)
                .map_err(io::Error::other)?;
            let mut response = client
                .get(SOURCE)
                .send()
                .await
                .map_err(io::Error::other)?
                .error_for_status()
                .map_err(io::Error::other)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(io::Error::other)? {
                if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
                    return Err(io::Error::other("Managed model catalog exceeds size limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
            let contents = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
            registry.install_managed_catalog(contents)
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::debug!("Managed model catalog refresh unavailable; retaining current catalog");
        }
    });
}
