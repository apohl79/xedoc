use std::collections::BTreeMap;
use std::io;
use std::io::Write;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use xedoc_protocol::openai_models::ModelInfo;
use xedoc_utils_path::write_atomically;

use super::MODEL_REGISTRY_SCHEMA_VERSION;
use super::ManagedModel;
use super::ModelRegistry;
use super::ProviderModelConfig;
use super::invalid_registry;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RegistryFile {
    schema_version: u32,
    catalog_revision: String,
    overrides: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_schema_version: Option<u32>,
    // Only consulted when a managed provider/model disappears, never as an override.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    fallback_providers: BTreeMap<String, ProviderModelConfig>,
}

#[derive(Clone, Debug)]
pub(super) struct RegistryState {
    pub(super) resolved: ModelRegistry,
    pub(super) routing_roles: BTreeMap<String, BTreeMap<String, String>>,
    pub(super) routing_aliases: BTreeMap<String, BTreeMap<String, String>>,
    baseline: ModelRegistry,
    catalog_revision: String,
    file: RegistryFile,
}

impl RegistryState {
    pub(super) fn default_model_selection(
        &self,
        provider_id: &str,
    ) -> super::DefaultModelSelection {
        if let Some(model) = self.file.overrides["providers"][provider_id]["default_model"].as_str()
        {
            return super::DefaultModelSelection::Pinned(model.to_string());
        }
        let mut models = self
            .baseline
            .provider(provider_id)
            .map(|provider| provider.models.values().collect::<Vec<_>>())
            .unwrap_or_default();
        models.sort_by_key(|model| model.info.priority);
        let mut candidates = self
            .baseline
            .provider(provider_id)
            .map(|provider| vec![provider.default_model.clone()])
            .unwrap_or_default();
        for model in models {
            if !candidates.contains(&model.info.slug) {
                candidates.push(model.info.slug.clone());
            }
        }
        super::DefaultModelSelection::Preferred(candidates)
    }

    pub(super) fn load_or_create(home: &Path) -> io::Result<Self> {
        match std::fs::symlink_metadata(ModelRegistry::path(home)) {
            Ok(_) => Self::load(home),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let managed = crate::registry_defaults::managed_registry(home)?;
                let catalog_revision = managed.revision;
                let baseline = managed.registry;
                let state = Self {
                    routing_roles: managed.routing_roles,
                    routing_aliases: managed.routing_aliases,
                    resolved: baseline.clone(),
                    baseline,
                    file: RegistryFile {
                        schema_version: MODEL_REGISTRY_SCHEMA_VERSION,
                        catalog_revision: catalog_revision.clone(),
                        overrides: json!({}),
                        legacy_schema_version: None,
                        fallback_providers: BTreeMap::new(),
                    },
                    catalog_revision,
                };
                state.write(home)?;
                Ok(state)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn load(home: &Path) -> io::Result<Self> {
        let path = ModelRegistry::path(home);
        let bytes = std::fs::read(&path)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            invalid_registry(format!("failed to parse {}: {error}", path.display()))
        })?;
        let schema = value["schema_version"]
            .as_u64()
            .ok_or_else(|| invalid_registry("schema_version must be an unsigned integer"))?;
        if schema > u64::from(MODEL_REGISTRY_SCHEMA_VERSION) {
            return Err(invalid_registry(format!(
                "unsupported schema_version {schema}"
            )));
        }
        let managed = crate::registry_defaults::managed_registry(home)?;
        let catalog_revision = managed.revision;
        let baseline = managed.registry;
        let migrating = schema < u64::from(MODEL_REGISTRY_SCHEMA_VERSION);
        let mut migration_source = None;
        let file = if migrating {
            let mut legacy: ModelRegistry =
                serde_json::from_value(value.clone()).map_err(io::Error::other)?;
            legacy.schema_version = MODEL_REGISTRY_SCHEMA_VERSION;
            legacy.validate()?;
            let overrides = if schema == 10 {
                // Freeze the old defaults: later catalog or price changes must not
                // reclassify copied defaults as user overrides.
                let historical: ModelRegistry =
                    serde_json::from_str(include_str!("../schema-10-defaults.json"))
                        .map_err(io::Error::other)?;
                difference(
                    &json!({ "providers": &historical.providers }),
                    &json!({ "providers": &legacy.providers }),
                )
                .unwrap_or_else(|| json!({}))
            } else {
                // Older files have no known baseline, so retain their values conservatively.
                let mut overrides = json!({ "providers": value["providers"].clone() });
                for (provider_id, provider) in &legacy.providers {
                    if let Some(base) = baseline.providers.get(provider_id) {
                        let provider_patch = &mut overrides["providers"][provider_id];
                        for (model_id, model) in &provider.models {
                            if let Some(base_model) = base.models.get(model_id) {
                                preserve_absent_fields(
                                    &mut provider_patch["models"][model_id],
                                    base_model,
                                    model,
                                )?;
                            }
                        }
                        preserve_absent_fields(
                            &mut provider_patch["template"],
                            &base.template,
                            &provider.template,
                        )?;
                    }
                }
                overrides
            };
            migration_source = Some(legacy);
            RegistryFile {
                schema_version: MODEL_REGISTRY_SCHEMA_VERSION,
                catalog_revision: catalog_revision.clone(),
                overrides,
                legacy_schema_version: Some(u32::try_from(schema).map_err(io::Error::other)?),
                fallback_providers: BTreeMap::new(),
            }
        } else {
            serde_json::from_value(value).map_err(io::Error::other)?
        };
        if !file.overrides.is_object()
            || file
                .overrides
                .as_object()
                .is_some_and(|patch| patch.keys().any(|key| key != "providers"))
        {
            return Err(invalid_registry(
                "overrides may only contain provider settings",
            ));
        }
        let mut state = Self {
            routing_roles: managed.routing_roles,
            routing_aliases: managed.routing_aliases,
            resolved: migration_source.unwrap_or_else(|| baseline.clone()),
            baseline,
            catalog_revision,
            file,
        };
        if migrating {
            state.capture_fallbacks();
        }
        state.resolved = state.resolve()?;
        if migrating {
            let backup = home.join(format!("models.schema-{schema}.backup.json"));
            match std::fs::OpenOptions::new()
                .write(/*write*/ true)
                .create_new(/*create_new*/ true)
                .open(&backup)
            {
                Ok(mut output) => {
                    output.write_all(&bytes)?;
                    output.sync_all()?;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if std::fs::read(&backup)? != bytes {
                        return Err(invalid_registry(format!(
                            "migration backup {} already contains a different registry",
                            backup.display()
                        )));
                    }
                }
                Err(error) => return Err(error),
            }
            state.write(home)?;
        }
        Ok(state)
    }

    pub(super) fn save_edit(&mut self, home: &Path, edited: ModelRegistry) -> io::Result<()> {
        self.save(home, edited, /*pins*/ None)
    }

    pub(super) fn configured_discovered_model(
        &self,
        provider_id: &str,
        discovered: &ModelInfo,
    ) -> io::Result<ModelInfo> {
        let configured = self
            .resolved
            .provider(provider_id)
            .and_then(|provider| provider.models.get(&discovered.slug));
        let curated = self
            .baseline
            .provider(provider_id)
            .and_then(|provider| provider.models.get(&discovered.slug))
            .is_some_and(|model| !model.info.used_fallback_model_metadata);
        let mut info = if discovered.used_fallback_model_metadata && curated {
            configured
                .map(|model| model.info.clone())
                .unwrap_or_else(|| discovered.clone())
        } else {
            discovered.clone()
        };
        if let Some(configured) = configured {
            info.priority = configured.info.priority;
            info.visibility = configured.info.visibility;
        }
        let provider_patch = &self.file.overrides["providers"][provider_id];
        let patch = if configured.is_some() {
            &provider_patch["models"][&discovered.slug]["info"]
        } else {
            &provider_patch["template"]["info"]
        };
        if patch.is_object() {
            let fallback_metadata = info.used_fallback_model_metadata;
            let mut patch = patch.clone();
            let fields = patch.as_object_mut().expect("object checked");
            fields.remove("slug");
            if configured.is_none() {
                fields.remove("display_name");
            }
            let mut value = serde_json::to_value(&info).map_err(io::Error::other)?;
            merge(&mut value, &patch);
            info = serde_json::from_value(value).map_err(io::Error::other)?;
            info.used_fallback_model_metadata = fallback_metadata;
        }
        info.slug.clone_from(&discovered.slug);
        super::validate_managed_model(
            provider_id,
            &discovered.slug,
            &ManagedModel {
                info: info.clone(),
                prices: None,
            },
        )?;
        Ok(info)
    }

    pub(super) fn pin_provider_defaults(
        &mut self,
        home: &Path,
        provider_id: &str,
        defaults: ProviderModelConfig,
    ) -> io::Result<()> {
        let pins = json!({
            "providers": {
                (provider_id): {
                    "default_model": defaults.default_model,
                    "fast_model": defaults.fast_model,
                    "default_reasoning_effort": defaults.default_reasoning_effort,
                }
            }
        });
        let mut edited = self.resolved.clone();
        let provider = edited
            .providers
            .entry(provider_id.to_string())
            .or_insert_with(|| defaults.clone());
        provider.default_model = defaults.default_model;
        provider.fast_model = defaults.fast_model;
        provider.default_reasoning_effort = defaults.default_reasoning_effort;
        self.save(home, edited, Some(pins))
    }

    pub(super) fn pin_model_context(
        &mut self,
        home: &Path,
        provider_id: &str,
        model_id: &str,
        settings: ProviderModelConfig,
    ) -> io::Result<()> {
        let requested = settings
            .models
            .get(model_id)
            .ok_or_else(|| invalid_registry("Cannot pin context for an unknown model"))?;
        let pins = json!({
            "providers": {
                (provider_id): {
                    "models": {
                        (model_id): {
                            "info": {
                                "context_window": requested.info.context_window,
                                "max_context_window": requested.info.max_context_window,
                                "auto_compact_token_limit": requested.info.auto_compact_token_limit,
                            }
                        }
                    }
                }
            }
        });
        let mut edited = self.resolved.clone();
        let provider = edited
            .providers
            .entry(provider_id.to_string())
            .or_insert_with(|| settings.clone());
        let model = provider
            .models
            .entry(model_id.to_string())
            .or_insert_with(|| requested.clone());
        model.info.context_window = requested.info.context_window;
        model.info.max_context_window = requested.info.max_context_window;
        model.info.auto_compact_token_limit = requested.info.auto_compact_token_limit;
        self.save(home, edited, Some(pins))
    }

    fn save(&mut self, home: &Path, edited: ModelRegistry, pins: Option<Value>) -> io::Result<()> {
        edited.validate()?;
        let mut next = self.clone();
        if let Some(delta) = difference(
            &serde_json::to_value(&self.resolved).map_err(io::Error::other)?,
            &serde_json::to_value(&edited).map_err(io::Error::other)?,
        ) {
            compose(&mut next.file.overrides, delta);
        }
        if let Some(pins) = pins {
            compose(&mut next.file.overrides, pins);
        }
        next.resolved = edited;
        next.file
            .catalog_revision
            .clone_from(&self.catalog_revision);
        next.capture_fallbacks();
        next.resolved = next.resolve()?;
        next.write(home)?;
        *self = next;
        Ok(())
    }

    fn resolve(&self) -> io::Result<ModelRegistry> {
        let mut base = self.baseline.clone();
        for (provider_id, fallback) in &self.file.fallback_providers {
            let patch = &self.file.overrides["providers"][provider_id];
            if !patch.is_object() {
                continue;
            }
            let provider = base
                .providers
                .entry(provider_id.clone())
                .or_insert_with(|| fallback.clone());
            for (model_id, model) in &fallback.models {
                if patch["models"].get(model_id).is_some_and(Value::is_object)
                    || patch["default_model"].as_str() == Some(model_id.as_str())
                    || patch["fast_model"].as_str() == Some(model_id.as_str())
                {
                    provider
                        .models
                        .entry(model_id.clone())
                        .or_insert_with(|| model.clone());
                }
            }
        }
        let mut value = serde_json::to_value(base).map_err(io::Error::other)?;
        merge(&mut value, &self.file.overrides);
        let resolved: ModelRegistry = serde_json::from_value(value).map_err(io::Error::other)?;
        resolved.validate()?;
        Ok(resolved)
    }

    fn capture_fallbacks(&mut self) {
        let Some(providers) = self.file.overrides["providers"].as_object() else {
            return;
        };
        self.file
            .fallback_providers
            .retain(|id, _| providers.get(id).is_some_and(Value::is_object));
        for (id, patch) in providers {
            let Some(provider) = self
                .resolved
                .providers
                .get(id)
                .filter(|_| patch.is_object())
            else {
                continue;
            };
            let mut fallback = provider.clone();
            fallback.models.retain(|model_id, _| {
                patch["default_model"].as_str() == Some(model_id.as_str())
                    || patch["fast_model"].as_str() == Some(model_id.as_str())
                    || patch["models"].get(model_id).is_some_and(Value::is_object)
            });
            self.file.fallback_providers.insert(id.clone(), fallback);
        }
    }

    fn write(&self, home: &Path) -> io::Result<()> {
        let contents = format!(
            "{}\n",
            serde_json::to_string_pretty(&self.file).map_err(io::Error::other)?
        );
        write_atomically(&ModelRegistry::path(home), &contents)
    }
}

fn preserve_absent_fields(
    patch: &mut Value,
    base: &impl Serialize,
    target: &impl Serialize,
) -> io::Result<()> {
    if let Some(delta) = difference(
        &serde_json::to_value(base).map_err(io::Error::other)?,
        &serde_json::to_value(target).map_err(io::Error::other)?,
    ) {
        compose(patch, delta);
    }
    Ok(())
}

fn difference(before: &Value, after: &Value) -> Option<Value> {
    if before == after {
        return None;
    }
    if let (Some(before), Some(after)) = (before.as_object(), after.as_object()) {
        let mut delta = serde_json::Map::new();
        for (key, value) in after {
            match before.get(key) {
                Some(previous) => {
                    if let Some(changed) = difference(previous, value) {
                        delta.insert(key.clone(), changed);
                    }
                }
                None => {
                    delta.insert(key.clone(), value.clone());
                }
            }
        }
        for key in before.keys().filter(|key| !after.contains_key(*key)) {
            delta.insert(key.clone(), Value::Null);
        }
        Some(Value::Object(delta))
    } else {
        Some(after.clone())
    }
}

// Patch composition retains null tombstones; applying a patch removes those keys.
fn compose(target: &mut Value, patch: Value) {
    if let Value::Object(patch) = patch {
        if !target.is_object() {
            *target = json!({});
        }
        for (key, value) in patch {
            compose(&mut target[&key], value);
        }
    } else {
        *target = patch;
    }
}

fn merge(target: &mut Value, patch: &Value) {
    if let Value::Object(patch) = patch {
        if !target.is_object() {
            *target = json!({});
        }
        for (key, value) in patch {
            if value.is_null() {
                target
                    .as_object_mut()
                    .expect("object initialized")
                    .remove(key);
            } else {
                merge(&mut target[key], value);
            }
        }
    } else {
        target.clone_from(patch);
    }
}
