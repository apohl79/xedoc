//! Plugin marketplace storage, manifests, and source-management operations.

#![deny(clippy::print_stdout, clippy::print_stderr)]

#[doc(hidden)]
pub mod command_migration;
#[doc(hidden)]
pub mod curated_paths;
pub mod installed_marketplaces;
pub mod manifest;
pub mod marketplace;
pub mod marketplace_add;
#[doc(hidden)]
pub mod marketplace_policy;
pub mod marketplace_remove;
pub mod marketplace_upgrade;
#[doc(hidden)]
pub mod npm_source;
#[doc(hidden)]
pub mod plugin_bundle_archive;
pub mod store;

pub const OPENAI_CURATED_MARKETPLACE_NAME: &str = "openai-curated";
pub const OPENAI_API_CURATED_MARKETPLACE_NAME: &str = "openai-api-curated";
pub const OPENAI_BUNDLED_MARKETPLACE_NAME: &str = "openai-bundled";
pub const OPENAI_BUNDLED_ALPHA_MARKETPLACE_NAME: &str = "openai-bundled-alpha";
pub const OPENAI_PRIMARY_RUNTIME_MARKETPLACE_NAME: &str = "openai-primary-runtime";

pub fn is_openai_curated_marketplace_name(marketplace_name: &str) -> bool {
    marketplace_name == OPENAI_CURATED_MARKETPLACE_NAME
        || marketplace_name == OPENAI_API_CURATED_MARKETPLACE_NAME
}
