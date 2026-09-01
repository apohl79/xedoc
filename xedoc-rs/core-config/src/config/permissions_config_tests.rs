use crate::config::Config;
use crate::config::ConfigOverrides;
use std::collections::BTreeMap;
use tempfile::TempDir;
use xedoc_config::config_toml::ConfigToml;
use xedoc_config::permissions_toml::FilesystemPermissionsToml;
use xedoc_config::permissions_toml::PermissionProfileToml;
use xedoc_config::permissions_toml::PermissionsToml;
use xedoc_utils_absolute_path::AbsolutePathBuf;

#[tokio::test]
async fn restricted_read_implicitly_allows_helper_executables() -> std::io::Result<()> {
    let temp_dir = TempDir::new()?;
    let cwd = temp_dir.path().join("workspace");
    let xedoc_home = temp_dir.path().join(".xedoc");
    let zsh_path = temp_dir.path().join("runtime").join("zsh");
    let arg0_root = xedoc_home.join("tmp").join("arg0");
    let allowed_arg0_dir = arg0_root.join("xedoc-arg0-session");
    let sibling_arg0_dir = arg0_root.join("xedoc-arg0-other-session");
    let execve_wrapper = allowed_arg0_dir.join("xedoc-execve-wrapper");
    std::fs::create_dir_all(&cwd)?;
    std::fs::create_dir_all(zsh_path.parent().expect("zsh path should have parent"))?;
    std::fs::create_dir_all(&allowed_arg0_dir)?;
    std::fs::create_dir_all(&sibling_arg0_dir)?;
    std::fs::write(&zsh_path, "")?;
    std::fs::write(&execve_wrapper, "")?;

    let config = Config::load_from_base_config_with_overrides(
        ConfigToml {
            default_permissions: Some("workspace".to_string()),
            permissions: Some(PermissionsToml {
                entries: BTreeMap::from([(
                    "workspace".to_string(),
                    PermissionProfileToml {
                        description: None,
                        extends: None,
                        workspace_roots: None,
                        filesystem: Some(FilesystemPermissionsToml {
                            glob_scan_max_depth: None,
                            entries: BTreeMap::new(),
                        }),
                        network: None,
                    },
                )]),
            }),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(cwd.clone()),
            default_zsh_path: Some(AbsolutePathBuf::try_from(zsh_path.clone())?),
            main_execve_wrapper_exe: Some(execve_wrapper),
            ..Default::default()
        },
        AbsolutePathBuf::from_absolute_path(&xedoc_home)?,
    )
    .await?;

    let expected_zsh = AbsolutePathBuf::try_from(zsh_path)?;
    let expected_allowed_arg0_dir = AbsolutePathBuf::try_from(allowed_arg0_dir)?;
    let expected_sibling_arg0_dir = AbsolutePathBuf::try_from(sibling_arg0_dir)?;
    let policy = config.permissions.file_system_sandbox_policy();

    assert!(
        policy.can_read_path_with_cwd(expected_zsh.as_path(), &cwd),
        "expected zsh helper path to be readable, policy: {policy:?}"
    );
    assert!(
        policy.can_read_path_with_cwd(expected_allowed_arg0_dir.as_path(), &cwd),
        "expected active arg0 helper dir to be readable, policy: {policy:?}"
    );
    assert!(
        !policy.can_read_path_with_cwd(expected_sibling_arg0_dir.as_path(), &cwd),
        "expected sibling arg0 helper dir to remain unreadable, policy: {policy:?}"
    );

    Ok(())
}
