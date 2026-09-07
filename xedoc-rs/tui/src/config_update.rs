//! App-server-backed config update helpers for the TUI.
//!
//! This module centralizes the small typed update helpers the TUI uses
//! when a config mutation must be owned by the app server rather than written
//! to the local `config.toml` directly.

use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use serde_json::Value as JsonValue;
use std::fmt::Display;
use std::path::Path;
use uuid::Uuid;
use xedoc_app_server_client::AppServerRequestHandle;
use xedoc_app_server_protocol::ClientRequest;
use xedoc_app_server_protocol::ConfigBatchWriteParams;
use xedoc_app_server_protocol::ConfigEdit;
use xedoc_app_server_protocol::ConfigReadParams;
use xedoc_app_server_protocol::ConfigReadResponse;
use xedoc_app_server_protocol::ConfigWriteResponse;
use xedoc_app_server_protocol::MergeStrategy;
use xedoc_app_server_protocol::RequestId;
use xedoc_app_server_protocol::SkillsConfigWriteParams;
use xedoc_app_server_protocol::SkillsConfigWriteResponse;
use xedoc_app_server_protocol::TokenUsageOptimizerReadResponse;
use xedoc_config::loader::project_trust_key;
use xedoc_features::FEATURES;
use xedoc_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use xedoc_protocol::config_types::TrustLevel;
use xedoc_utils_absolute_path::AbsolutePathBuf;

pub(crate) fn replace_config_value(key_path: impl Into<String>, value: JsonValue) -> ConfigEdit {
    ConfigEdit {
        key_path: key_path.into(),
        value,
        merge_strategy: MergeStrategy::Replace,
    }
}

pub(crate) fn clear_config_value(key_path: impl Into<String>) -> ConfigEdit {
    replace_config_value(key_path, JsonValue::Null)
}

pub(crate) fn format_config_error(err: &impl Display) -> String {
    format!("{err:#}")
}

fn trusted_project_edit(project_path: &Path) -> ConfigEdit {
    let project_key = project_trust_key(project_path)
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    replace_config_value(
        format!("projects.\"{project_key}\".trust_level"),
        serde_json::json!(TrustLevel::Trusted.to_string()),
    )
}

pub(crate) fn build_model_selection_edits(
    model: &str,
    model_provider: &str,
    effort: Option<impl ToString>,
) -> Vec<ConfigEdit> {
    let effort_edit = effort.map_or_else(
        || clear_config_value("model_reasoning_effort"),
        |effort| {
            replace_config_value(
                "model_reasoning_effort",
                serde_json::json!(effort.to_string()),
            )
        },
    );
    vec![
        replace_config_value("model", serde_json::json!(model)),
        replace_config_value("model_provider", serde_json::json!(model_provider)),
        effort_edit,
    ]
}

pub(crate) fn build_service_tier_selection_edits(service_tier: Option<&str>) -> Vec<ConfigEdit> {
    let service_tier_edit = service_tier.map_or_else(
        || clear_config_value("service_tier"),
        |service_tier| {
            let config_value = if service_tier == SERVICE_TIER_DEFAULT_REQUEST_VALUE {
                SERVICE_TIER_DEFAULT_REQUEST_VALUE
            } else {
                match xedoc_protocol::config_types::ServiceTier::from_request_value(service_tier) {
                    Some(xedoc_protocol::config_types::ServiceTier::Fast) => "fast",
                    Some(xedoc_protocol::config_types::ServiceTier::Flex) => "flex",
                    None => service_tier,
                }
            };
            replace_config_value("service_tier", serde_json::json!(config_value))
        },
    );
    vec![service_tier_edit]
}

pub(crate) fn build_feature_enabled_edit(feature_key: &str, enabled: bool) -> ConfigEdit {
    if feature_key == "token_usage_optimizer" {
        return replace_config_value("token_usage_optimizer.enabled", serde_json::json!(enabled));
    }
    let key_path = format!("features.{feature_key}");
    let is_default_false_feature = FEATURES
        .iter()
        .find(|spec| spec.key == feature_key)
        .is_some_and(|spec| !spec.default_enabled);
    if enabled || !is_default_false_feature {
        replace_config_value(key_path, serde_json::json!(enabled))
    } else {
        clear_config_value(key_path)
    }
}

pub(crate) fn build_auto_session_name_edits(enabled: bool) -> Vec<ConfigEdit> {
    vec![replace_config_value(
        "auto_session_name",
        serde_json::json!(enabled),
    )]
}

pub(crate) fn build_oss_provider_edit(provider: &str) -> ConfigEdit {
    replace_config_value("oss_provider", serde_json::json!(provider))
}

pub(crate) async fn write_config_batch(
    request_handle: AppServerRequestHandle,
    edits: Vec<ConfigEdit>,
) -> Result<ConfigWriteResponse> {
    let request_id = RequestId::String(format!("tui-config-write-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::ConfigBatchWrite {
            request_id,
            params: ConfigBatchWriteParams {
                edits,
                file_path: None,
                expected_version: None,
                reload_user_config: true,
            },
        })
        .await
        .wrap_err("config/batchWrite failed in TUI")
}

pub(crate) async fn write_trusted_project(
    request_handle: AppServerRequestHandle,
    project_path: &Path,
) -> Result<ConfigWriteResponse> {
    write_config_batch(request_handle, vec![trusted_project_edit(project_path)]).await
}

pub(crate) async fn read_effective_config(
    request_handle: AppServerRequestHandle,
    cwd: String,
) -> Result<ConfigReadResponse> {
    let request_id = RequestId::String(format!("tui-config-read-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::ConfigRead {
            request_id,
            params: ConfigReadParams {
                include_layers: false,
                cwd: Some(cwd),
            },
        })
        .await
        .wrap_err("config/read failed in TUI")
}

pub(crate) async fn read_token_usage_optimizer(
    request_handle: AppServerRequestHandle,
) -> Result<TokenUsageOptimizerReadResponse> {
    let request_id =
        RequestId::String(format!("tui-token-usage-optimizer-read-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::TokenUsageOptimizerRead {
            request_id,
            params: None,
        })
        .await
        .wrap_err("tokenUsageOptimizer/read failed in TUI")
}

pub(crate) async fn read_token_usage_optimizer_report(
    request_handle: AppServerRequestHandle,
    days: Option<u32>,
) -> Result<xedoc_app_server_protocol::TokenUsageOptimizerReportResponse> {
    let request_id = RequestId::String(format!(
        "tui-token-usage-optimizer-report-{}",
        Uuid::new_v4()
    ));
    let until_day = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    let since_day = until_day - i64::from(days.unwrap_or(90).clamp(1, 366) - 1) * 86400;
    request_handle
        .request_typed(ClientRequest::TokenUsageOptimizerReport {
            request_id,
            params: xedoc_app_server_protocol::TokenUsageOptimizerReportParams {
                since_day: Some(since_day),
                until_day: Some(until_day),
                model: None,
            },
        })
        .await
        .wrap_err("tokenUsageOptimizer/report failed in TUI")
}

pub(crate) async fn reset_token_usage_optimizer_stats(
    request_handle: AppServerRequestHandle,
) -> Result<()> {
    let request_id = RequestId::String(format!(
        "tui-token-usage-optimizer-reset-{}",
        Uuid::new_v4()
    ));
    request_handle
        .request_typed(ClientRequest::TokenUsageOptimizerWrite {
            request_id,
            params: xedoc_app_server_protocol::TokenUsageOptimizerWriteParams {
                enabled: None,
                level: None,
                expected_version: None,
                reset_stats: true,
            },
        })
        .await
        .map(|_: xedoc_app_server_protocol::TokenUsageOptimizerWriteResponse| ())
        .wrap_err("tokenUsageOptimizer/write reset failed in TUI")
}

pub(crate) async fn write_skill_enabled(
    request_handle: AppServerRequestHandle,
    path: AbsolutePathBuf,
    enabled: bool,
) -> Result<()> {
    let request_id = RequestId::String(format!("tui-skill-config-write-{}", Uuid::new_v4()));
    let _: SkillsConfigWriteResponse = request_handle
        .request_typed(ClientRequest::SkillsConfigWrite {
            request_id,
            params: SkillsConfigWriteParams {
                path: Some(path),
                name: None,
                enabled,
            },
        })
        .await
        .wrap_err("skills/config/write failed in TUI")?;
    Ok(())
}

#[cfg(test)]
#[path = "config_update_tests.rs"]
mod tests;
