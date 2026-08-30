use std::path::Path;
use std::path::PathBuf;

const CURATED_PLUGINS_RELATIVE_DIR: &str = ".tmp/plugins";

pub fn curated_plugins_repo_path(xedoc_home: &Path) -> PathBuf {
    xedoc_home.join(CURATED_PLUGINS_RELATIVE_DIR)
}

pub fn curated_plugins_api_marketplace_path(xedoc_home: &Path) -> PathBuf {
    curated_plugins_repo_path(xedoc_home).join(".agents/plugins/api_marketplace.json")
}
