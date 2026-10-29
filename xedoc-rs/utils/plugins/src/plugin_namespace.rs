//! Resolve plugin namespace from skill file paths by walking ancestors for `plugin.json`.

use std::path::Path;
use std::path::PathBuf;
use xedoc_exec_server::ExecutorFileSystem;
use xedoc_exec_server_protocol::DISCOVERABLE_PLUGIN_MANIFEST_PATHS;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_path_uri::PathUri;

pub fn find_plugin_manifest_path(plugin_root: &Path) -> Option<PathBuf> {
    DISCOVERABLE_PLUGIN_MANIFEST_PATHS
        .iter()
        .map(|relative_path| plugin_root.join(relative_path))
        .find(|manifest_path| manifest_path.is_file())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPluginManifestName {
    #[serde(default)]
    name: String,
}

/// Returns the plugin manifest `name` defined directly below `plugin_root`.
pub async fn plugin_namespace_for_root_uri(
    fs: &dyn ExecutorFileSystem,
    plugin_root: &PathUri,
) -> Option<String> {
    let mut manifest_path = None;
    for relative_path in DISCOVERABLE_PLUGIN_MANIFEST_PATHS {
        let candidate = plugin_root.join(relative_path).ok()?;
        match fs.get_metadata(&candidate, /*sandbox*/ None).await {
            Ok(metadata) if metadata.is_file => {
                manifest_path = Some(candidate);
                break;
            }
            Ok(_) | Err(_) => {}
        }
    }
    let contents = fs
        .read_file_text(&manifest_path?, /*sandbox*/ None)
        .await
        .ok()?;
    let RawPluginManifestName { name: raw_name } = serde_json::from_str(&contents).ok()?;
    Some(
        plugin_root
            .basename()
            .filter(|_| raw_name.trim().is_empty())
            .unwrap_or(raw_name),
    )
}

/// Returns the plugin manifest `name` for the nearest ancestor of `path` that contains a valid
/// plugin manifest (same `name` rules as full manifest loading in xedoc-core).
pub async fn plugin_namespace_for_skill_path(
    fs: &dyn ExecutorFileSystem,
    path: &AbsolutePathBuf,
) -> Option<String> {
    plugin_namespace_for_skill_uri(fs, &PathUri::from_abs_path(path)).await
}

/// Returns the plugin manifest `name` for the nearest URI ancestor of `path`.
pub async fn plugin_namespace_for_skill_uri(
    fs: &dyn ExecutorFileSystem,
    path: &PathUri,
) -> Option<String> {
    let mut ancestor = Some(path.clone());
    while let Some(path) = ancestor {
        if let Some(name) = plugin_namespace_for_root_uri(fs, &path).await {
            return Some(name);
        }
        ancestor = path.parent();
    }
    None
}
