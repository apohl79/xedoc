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

#[cfg(test)]
mod tests {
    use super::find_plugin_manifest_path;
    use super::plugin_namespace_for_skill_path;
    use std::fs;
    use tempfile::tempdir;
    use xedoc_exec_server::LOCAL_FS;
    use xedoc_utils_absolute_path::test_support::PathBufExt;

    const ALTERNATE_PLUGIN_CLA_MANIFEST_RELATIVE_PATH: &str = ".claude-plugin/plugin.json";
    const ALTERNATE_PLUGIN_CUR_MANIFEST_RELATIVE_PATH: &str = ".cursor-plugin/plugin.json";
    const LEGACY_CODEX_PLUGIN_MANIFEST_RELATIVE_PATH: &str = ".codex-plugin/plugin.json";

    #[tokio::test]
    async fn uses_manifest_name() {
        let tmp = tempdir().expect("tempdir");
        let plugin_root = tmp.path().join("plugins/sample");
        let skill_path = plugin_root.join("skills/search/SKILL.md");

        fs::create_dir_all(skill_path.parent().expect("parent")).expect("mkdir");
        fs::create_dir_all(plugin_root.join(".xedoc-plugin")).expect("mkdir manifest");
        fs::write(
            plugin_root.join(".xedoc-plugin/plugin.json"),
            r#"{"name":"sample"}"#,
        )
        .expect("write manifest");
        fs::write(&skill_path, "---\ndescription: search\n---\n").expect("write skill");

        assert_eq!(
            plugin_namespace_for_skill_path(LOCAL_FS.as_ref(), &skill_path.abs()).await,
            Some("sample".to_string())
        );
    }

    #[tokio::test]
    async fn uses_name_from_alternate_discoverable_manifest_path() {
        let tmp = tempdir().expect("tempdir");
        let plugin_root = tmp.path().join("plugins/sample");
        let skill_path = plugin_root.join("skills/search/SKILL.md");
        let manifest_path = plugin_root.join(ALTERNATE_PLUGIN_CLA_MANIFEST_RELATIVE_PATH);

        fs::create_dir_all(skill_path.parent().expect("parent")).expect("mkdir");
        fs::create_dir_all(manifest_path.parent().expect("manifest parent"))
            .expect("mkdir manifest");
        fs::write(&manifest_path, r#"{"name":"sample"}"#).expect("write manifest");
        fs::write(&skill_path, "---\ndescription: search\n---\n").expect("write skill");

        assert_eq!(
            plugin_namespace_for_skill_path(LOCAL_FS.as_ref(), &skill_path.abs()).await,
            Some("sample".to_string())
        );
        assert_eq!(find_plugin_manifest_path(&plugin_root), Some(manifest_path));
    }

    #[tokio::test]
    async fn uses_name_from_cur_plugin_manifest_path() {
        let tmp = tempdir().expect("tempdir");
        let plugin_root = tmp.path().join("plugins/sample");
        let skill_path = plugin_root.join("skills/search/SKILL.md");
        let manifest_path = plugin_root.join(ALTERNATE_PLUGIN_CUR_MANIFEST_RELATIVE_PATH);

        fs::create_dir_all(skill_path.parent().expect("parent")).expect("mkdir");
        fs::create_dir_all(manifest_path.parent().expect("manifest parent"))
            .expect("mkdir manifest");
        fs::write(&manifest_path, r#"{"name":"sample"}"#).expect("write manifest");
        fs::write(&skill_path, "---\ndescription: search\n---\n").expect("write skill");

        assert_eq!(
            plugin_namespace_for_skill_path(LOCAL_FS.as_ref(), &skill_path.abs()).await,
            Some("sample".to_string())
        );
        assert_eq!(find_plugin_manifest_path(&plugin_root), Some(manifest_path));
    }

    #[tokio::test]
    async fn uses_name_from_legacy_codex_plugin_manifest_path() {
        let tmp = tempdir().expect("tempdir");
        let plugin_root = tmp.path().join("plugins/sample");
        let skill_path = plugin_root.join("skills/search/SKILL.md");
        let manifest_path = plugin_root.join(LEGACY_CODEX_PLUGIN_MANIFEST_RELATIVE_PATH);

        fs::create_dir_all(skill_path.parent().expect("parent")).expect("mkdir");
        fs::create_dir_all(manifest_path.parent().expect("manifest parent"))
            .expect("mkdir manifest");
        fs::write(&manifest_path, r#"{"name":"sample"}"#).expect("write manifest");
        fs::write(&skill_path, "---\ndescription: search\n---\n").expect("write skill");

        assert_eq!(
            plugin_namespace_for_skill_path(LOCAL_FS.as_ref(), &skill_path.abs()).await,
            Some("sample".to_string())
        );
        assert_eq!(find_plugin_manifest_path(&plugin_root), Some(manifest_path));
    }

    #[test]
    fn prefers_xedoc_plugin_manifest_over_legacy_codex_manifest() {
        let tmp = tempdir().expect("tempdir");
        let plugin_root = tmp.path().join("plugins/sample");
        let xedoc_manifest = plugin_root.join(".xedoc-plugin/plugin.json");
        let codex_manifest = plugin_root.join(LEGACY_CODEX_PLUGIN_MANIFEST_RELATIVE_PATH);

        for manifest_path in [&xedoc_manifest, &codex_manifest] {
            fs::create_dir_all(manifest_path.parent().expect("manifest parent"))
                .expect("mkdir manifest");
            fs::write(manifest_path, r#"{"name":"sample"}"#).expect("write manifest");
        }

        assert_eq!(
            find_plugin_manifest_path(&plugin_root),
            Some(xedoc_manifest)
        );
    }
}
