use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn test_user_config_path(temp_dir: &TempDir, file_name: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(temp_dir.path().join(file_name))
        .expect("test user config path should be absolute")
}

#[test]
fn origins_use_canonical_key_aliases() {
    let layer = ConfigLayerEntry::new(
        ConfigLayerSource::SessionFlags,
        toml::from_str(
            r#"
[agents]
max_threads = 4
"#,
        )
        .expect("config TOML should parse"),
    );
    let metadata = layer.metadata();
    let stack = ConfigLayerStack::new(
        vec![layer],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("single layer stack should be valid");

    let origins = stack.origins();

    assert_eq!(
        origins.get("agents.max_concurrent_threads_per_session"),
        Some(&metadata)
    );
    assert!(
        !origins.contains_key("agents.max_threads"),
        "legacy key should be canonicalized before origin recording"
    );
}

#[test]
fn active_user_layer_is_highest_precedence_user_layer() {
    let temp_dir = TempDir::new().expect("tempdir");
    let base_file = test_user_config_path(&temp_dir, "config.toml");
    let profile_file = test_user_config_path(&temp_dir, "work.config.toml");
    let base_layer = ConfigLayerEntry::new(
        ConfigLayerSource::User {
            file: base_file,
            profile: None,
        },
        toml::from_str(
            r#"
model = "base"
approval_policy = "on-request"
"#,
        )
        .expect("base config"),
    );
    let profile_layer = ConfigLayerEntry::new(
        ConfigLayerSource::User {
            file: profile_file.clone(),
            profile: Some("work".to_string()),
        },
        toml::from_str(r#"model = "profile""#).expect("profile config"),
    );
    let stack = ConfigLayerStack::new(
        vec![base_layer, profile_layer],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("multiple user layers should be valid");

    assert_eq!(stack.get_user_config_file(), Some(&profile_file));
    assert_eq!(
        stack
            .effective_user_config()
            .expect("merged user config")
            .get("model")
            .and_then(toml::Value::as_str),
        Some("profile")
    );
    assert_eq!(
        stack
            .effective_user_config()
            .expect("merged user config")
            .get("approval_policy")
            .and_then(toml::Value::as_str),
        Some("on-request")
    );
}

#[test]
fn with_user_config_updates_matching_user_layer_without_replacing_active_profile() {
    let temp_dir = TempDir::new().expect("tempdir");
    let base_file = test_user_config_path(&temp_dir, "config.toml");
    let profile_file = test_user_config_path(&temp_dir, "work.config.toml");
    let base_layer = ConfigLayerEntry::new(
        ConfigLayerSource::User {
            file: base_file.clone(),
            profile: None,
        },
        toml::from_str(r#"model = "base""#).expect("base config"),
    );
    let profile_layer = ConfigLayerEntry::new(
        ConfigLayerSource::User {
            file: profile_file.clone(),
            profile: Some("work".to_string()),
        },
        toml::from_str(r#"approval_policy = "on-request""#).expect("profile config"),
    );
    let stack = ConfigLayerStack::new(
        vec![base_layer, profile_layer],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("multiple user layers should be valid");

    let updated = stack.with_user_config(
        &base_file,
        toml::from_str(r#"model = "updated-base""#).expect("updated base config"),
    );

    assert_eq!(updated.get_user_config_file(), Some(&profile_file));
    assert_eq!(
        updated
            .effective_user_config()
            .expect("merged user config")
            .get("model")
            .and_then(toml::Value::as_str),
        Some("updated-base")
    );
    assert_eq!(
        updated
            .effective_user_config()
            .expect("merged user config")
            .get("approval_policy")
            .and_then(toml::Value::as_str),
        Some("on-request")
    );
}
