use super::*;
use color_eyre::eyre::WrapErr;
use pretty_assertions::assert_eq;
use std::path::Path;

#[test]
fn trusted_project_edit_targets_project_trust_level() {
    assert_eq!(
        trusted_project_edit(Path::new("/workspace/team.project")),
        ConfigEdit {
            key_path: "projects.\"/workspace/team.project\".trust_level".to_string(),
            value: serde_json::json!("trusted"),
            merge_strategy: MergeStrategy::Replace,
        }
    );
}

#[test]
fn format_config_error_preserves_server_validation_message() {
    let err = Err::<(), _>(color_eyre::eyre::eyre!(
        "config/batchWrite failed: Invalid configuration: features.fast_mode=true violates \
         managed requirements; allowed set [fast_mode=false]"
    ))
    .wrap_err("config/batchWrite failed in TUI")
    .unwrap_err();

    assert_eq!(
        format_config_error(&err),
        "config/batchWrite failed in TUI: config/batchWrite failed: Invalid configuration: \
         features.fast_mode=true violates managed requirements; allowed set [fast_mode=false]"
    );
}

#[test]
fn build_auto_session_name_edits_targets_top_level_setting() {
    assert_eq!(
        build_auto_session_name_edits(false),
        vec![ConfigEdit {
            key_path: "auto_session_name".to_string(),
            value: serde_json::json!(false),
            merge_strategy: MergeStrategy::Replace,
        }]
    );
}

#[test]
fn build_model_selection_edits_persists_provider_with_reasoning_effort() {
    assert_eq!(
        build_model_selection_edits("deepseek-v4-pro", "deepseek", Some("high")),
        vec![
            ConfigEdit {
                key_path: "model".to_string(),
                value: serde_json::json!("deepseek-v4-pro"),
                merge_strategy: MergeStrategy::Replace,
            },
            ConfigEdit {
                key_path: "model_provider".to_string(),
                value: serde_json::json!("deepseek"),
                merge_strategy: MergeStrategy::Replace,
            },
            ConfigEdit {
                key_path: "model_reasoning_effort".to_string(),
                value: serde_json::json!("high"),
                merge_strategy: MergeStrategy::Replace,
            },
        ]
    );
}

#[test]
fn build_model_selection_edits_persists_provider_when_clearing_reasoning_effort() {
    assert_eq!(
        build_model_selection_edits("claude-opus-5", "anthropic", None::<String>),
        vec![
            ConfigEdit {
                key_path: "model".to_string(),
                value: serde_json::json!("claude-opus-5"),
                merge_strategy: MergeStrategy::Replace,
            },
            ConfigEdit {
                key_path: "model_provider".to_string(),
                value: serde_json::json!("anthropic"),
                merge_strategy: MergeStrategy::Replace,
            },
            ConfigEdit {
                key_path: "model_reasoning_effort".to_string(),
                value: serde_json::Value::Null,
                merge_strategy: MergeStrategy::Replace,
            },
        ]
    );
}
