use std::sync::Arc;

use pretty_assertions::assert_eq;
use tempfile::TempDir;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;
use xedoc_protocol::openai_models::ModelPreset;
use xedoc_protocol::openai_models::ModelsResponse;

use super::RegistryModelsManager;
use crate::ModelsManagerConfig;
use crate::manager::ModelsManager;
use crate::manager::RefreshStrategy;
use crate::manager::StaticModelsManager;
use crate::model_info::model_info_from_provider_catalog_slug;
use crate::registry::SharedModelRegistry;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn registry_overrides_discovered_model_and_adds_configured_models() -> TestResult {
    let home = TempDir::new()?;
    let registry = SharedModelRegistry::load_or_create(home.path())?;
    let mut discovered = model_info_from_provider_catalog_slug("deepseek-v4-pro", "DeepSeek");
    discovered.context_window = Some(17);
    let inner = Arc::new(StaticModelsManager::new(
        /*auth_manager*/ None,
        ModelsResponse {
            models: vec![discovered],
        },
    ));
    let manager = RegistryModelsManager::new("deepseek".to_string(), registry, inner);

    let models = manager.get_remote_models().await;

    assert_eq!(models.len(), 2);
    assert_eq!(models[0].slug, "deepseek-v4-pro");
    assert_eq!(models[0].context_window, Some(400_000));
    assert_eq!(models[1].slug, "deepseek-v4-flash");
    Ok(())
}

#[tokio::test]
async fn unregistered_discovered_model_remains_visible_in_picker() -> TestResult {
    let home = TempDir::new()?;
    let registry = SharedModelRegistry::load_or_create(home.path())?;
    let model = model_info_from_provider_catalog_slug("gemma4:e2b", "ollama");
    let inner = Arc::new(StaticModelsManager::new(
        /*auth_manager*/ None,
        ModelsResponse {
            models: vec![model],
        },
    ));
    let manager = RegistryModelsManager::new("ollama".to_string(), registry, inner);

    let models = manager.get_remote_models().await;

    assert_eq!(models.len(), 1);
    assert!(ModelPreset::from(models[0].clone()).show_in_picker);
    Ok(())
}

#[tokio::test]
async fn registry_default_model_is_provider_scoped() -> TestResult {
    let home = TempDir::new()?;
    let registry = SharedModelRegistry::load_or_create(home.path())?;
    let inner = Arc::new(StaticModelsManager::new(
        /*auth_manager*/ None,
        ModelsResponse::default(),
    ));
    let manager = RegistryModelsManager::new("google".to_string(), registry, inner);

    let model = manager
        .get_default_model(
            &None,
            /*allow_provider_model_fallback*/ true,
            RefreshStrategy::Offline,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
        .await;

    assert_eq!(model, "gemini-3.1-pro-preview");
    Ok(())
}

#[tokio::test]
async fn runtime_config_override_has_precedence_over_registry() -> TestResult {
    let home = TempDir::new()?;
    let registry = SharedModelRegistry::load_or_create(home.path())?;
    let model = model_info_from_provider_catalog_slug("deepseek-v4-pro", "DeepSeek");
    let inner = Arc::new(StaticModelsManager::new(
        /*auth_manager*/ None,
        ModelsResponse {
            models: vec![model],
        },
    ));
    let manager = RegistryModelsManager::new("deepseek".to_string(), registry, inner);

    let configured = manager
        .get_model_info(
            "deepseek-v4-pro",
            &ModelsManagerConfig {
                model_context_window: Some(200_000),
                ..Default::default()
            },
        )
        .await;

    assert_eq!(configured.context_window, Some(200_000));
    Ok(())
}
