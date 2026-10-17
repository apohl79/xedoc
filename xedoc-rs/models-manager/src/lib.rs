pub mod availability;
pub(crate) mod cache;
pub mod collaboration_mode_presets;
pub(crate) mod config;
pub mod instructions;
pub mod manager;
pub mod model_info;
pub mod model_presets;
pub mod registry;
mod registry_defaults;
pub mod registry_manager;
pub mod test_support;

pub use config::ModelsManagerConfig;
pub use xedoc_protocol::auth::AuthMode;

/// Load the bundled model catalog shipped with `xedoc-models-manager`.
pub fn bundled_models_response()
-> std::result::Result<xedoc_protocol::openai_models::ModelsResponse, serde_json::Error> {
    let mut catalog: xedoc_protocol::openai_models::ModelsResponse =
        serde_json::from_str(include_str!("../models.json"))?;
    if let Some(mut model) = catalog
        .models
        .iter()
        .find(|model| model.slug == "gpt-6-sol")
        .cloned()
    {
        model.slug = "gpt-6.1-sol".to_string();
        model.display_name = "GPT-6.1-Sol".to_string();
        model.context_window = Some(1_050_000);
        model.max_context_window = Some(1_050_000);
        model.auto_compact_token_limit = Some(945_000);
        model
            .supported_reasoning_levels
            .retain(|level| level.effort != xedoc_protocol::openai_models::ReasoningEffort::Ultra);
        catalog.models.push(model);
    }
    Ok(catalog)
}

/// Convert the client version string to a whole version string (e.g. "1.2.3-alpha.4" -> "1.2.3").
pub fn client_version_to_whole() -> String {
    format!(
        "{}.{}.{}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH")
    )
}
