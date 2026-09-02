use std::fs;

use pretty_assertions::assert_eq;
use tempfile::TempDir;
use xedoc_protocol::openai_models::ReasoningEffort;

use super::MODEL_REGISTRY_FILE;
use super::ModelRegistry;
use crate::registry_defaults::DEFAULT_COMPACT_LIMIT;
use crate::registry_defaults::DEFAULT_CONTEXT_WINDOW;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn missing_registry_is_seeded_with_all_managed_providers() -> TestResult {
    let home = TempDir::new()?;

    let registry = ModelRegistry::load_or_create(home.path())?;

    assert_eq!(
        registry.providers.keys().cloned().collect::<Vec<_>>(),
        vec!["anthropic", "deepseek", "google", "openai"]
    );
    assert!(home.path().join(MODEL_REGISTRY_FILE).is_file());
    Ok(())
}

#[test]
fn seeded_provider_defaults_match_reference_limits() -> TestResult {
    let home = TempDir::new()?;
    let registry = ModelRegistry::load_or_create(home.path())?;

    let anthropic = registry
        .provider("anthropic")
        .expect("Anthropic defaults should exist");

    assert_eq!(anthropic.default_model, "claude-fable-5-1");
    assert_eq!(anthropic.fast_model, "claude-haiku-4-5-20251001");
    assert_eq!(anthropic.default_reasoning_effort, ReasoningEffort::Medium);
    assert_eq!(
        anthropic.template.info.context_window,
        Some(DEFAULT_CONTEXT_WINDOW)
    );
    assert_eq!(
        anthropic.template.info.auto_compact_token_limit,
        Some(DEFAULT_COMPACT_LIMIT)
    );
    Ok(())
}

#[test]
fn existing_registry_is_never_overwritten() -> TestResult {
    let home = TempDir::new()?;
    let mut registry = ModelRegistry::load_or_create(home.path())?;
    registry
        .provider_mut("google")
        .expect("Google defaults should exist")
        .fast_model = "gemini-3.5-flash".to_string();
    registry.save(home.path())?;

    let loaded = ModelRegistry::load_or_create(home.path())?;

    assert_eq!(
        loaded
            .provider("google")
            .expect("Google defaults should exist")
            .fast_model,
        "gemini-3.5-flash"
    );
    Ok(())
}

#[test]
fn invalid_compaction_limit_is_rejected() -> TestResult {
    let home = TempDir::new()?;
    let mut registry = ModelRegistry::load_or_create(home.path())?;
    registry
        .provider_mut("deepseek")
        .expect("DeepSeek defaults should exist")
        .template
        .info
        .auto_compact_token_limit = Some(DEFAULT_COMPACT_LIMIT + 1);
    fs::write(
        home.path().join(MODEL_REGISTRY_FILE),
        serde_json::to_vec_pretty(&registry)?,
    )?;

    let error = ModelRegistry::load(home.path()).expect_err("invalid limit should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("compaction limit must be between")
    );
    Ok(())
}

#[test]
fn maximum_context_window_below_context_window_is_rejected() -> TestResult {
    let home = TempDir::new()?;
    let mut registry = ModelRegistry::load_or_create(home.path())?;
    let model = &mut registry
        .provider_mut("deepseek")
        .expect("DeepSeek defaults should exist")
        .template
        .info;
    model.max_context_window = Some(DEFAULT_CONTEXT_WINDOW - 1);
    fs::write(
        home.path().join(MODEL_REGISTRY_FILE),
        serde_json::to_vec_pretty(&registry)?,
    )?;

    let error = ModelRegistry::load(home.path()).expect_err("invalid maximum should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("maximum context window must be at least")
    );
    Ok(())
}

#[test]
fn discovered_model_uses_provider_template() -> TestResult {
    let home = TempDir::new()?;
    let registry = ModelRegistry::load_or_create(home.path())?;
    let mut discovered =
        crate::model_info::model_info_from_provider_catalog_slug("gemini-new", "Google");
    discovered.context_window = Some(123);
    discovered.priority = 42;

    let configured = registry
        .configured_model("google", &discovered)
        .expect("Google provider should exist");

    assert_eq!(configured.slug, "gemini-new");
    assert_eq!(configured.priority, 42);
    assert_eq!(configured.context_window, Some(DEFAULT_CONTEXT_WINDOW));
    assert!(!configured.base_instructions.contains("the * model"));
    Ok(())
}

#[test]
fn failed_edit_restores_in_memory_registry() -> TestResult {
    let home = TempDir::new()?;
    let registry = super::SharedModelRegistry::load_or_create(home.path())?;
    let expected = registry.snapshot();

    let error = registry
        .update(|configured| {
            configured.providers.clear();
            Err::<(), _>(std::io::Error::other("edit failed"))
        })
        .expect_err("failed edit should be returned");

    assert_eq!(error.to_string(), "edit failed");
    assert_eq!(registry.snapshot(), expected);
    Ok(())
}
