#![cfg(not(debug_assertions))]

use crate::legacy_core::config::Config;
use crate::update_versions::extract_version_from_latest_tag;
use crate::update_versions::is_newer;
use crate::update_versions::is_source_build_version;
use crate::updates_cache::VersionInfo;
use crate::updates_cache::read_version_info;
use crate::updates_cache::version_filepath;
use chrono::Duration;
use chrono::Utc;
use serde::Deserialize;
use std::path::Path;
use xedoc_login::default_client::create_client;

use crate::version::xedoc_cli_version;

pub(crate) use crate::updates_cache::dismiss_version;

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/apohl79/codex/releases/latest";

pub fn get_upgrade_version(config: &Config) -> Option<String> {
    if !config.check_for_update_on_startup || is_source_build_version(xedoc_cli_version()) {
        return None;
    }

    let version_file = version_filepath(config);
    let info = read_version_info(&version_file).ok();

    if match &info {
        None => true,
        Some(info) => info.last_checked_at < Utc::now() - Duration::hours(20),
    } {
        // Refresh the cached latest version in the background so TUI startup
        // isn’t blocked by a network call. The UI reads the previously cached
        // value (if any) for this run; the next run shows the banner if needed.
        tokio::spawn(async move {
            check_for_update(&version_file)
                .await
                .inspect_err(|e| tracing::error!("Failed to update version: {e}"))
        });
    }

    info.and_then(|info| {
        if is_newer(&info.latest_version, xedoc_cli_version()).unwrap_or(false) {
            Some(info.latest_version)
        } else {
            None
        }
    })
}

#[derive(Deserialize, Debug, Clone)]
struct ReleaseInfo {
    tag_name: String,
}

async fn check_for_update(version_file: &Path) -> anyhow::Result<()> {
    let latest_version = fetch_latest_github_release_version().await?;

    // Preserve any previously dismissed version if present.
    let prev_info = read_version_info(version_file).ok();
    let info = VersionInfo {
        latest_version,
        last_checked_at: Utc::now(),
        dismissed_version: prev_info.and_then(|p| p.dismissed_version),
    };
    write_version_info(version_file, &info).await
}

async fn write_version_info(version_file: &Path, info: &VersionInfo) -> anyhow::Result<()> {
    let json_line = format!("{}\n", serde_json::to_string(info)?);
    if let Some(parent) = version_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(version_file, json_line).await?;
    Ok(())
}

async fn fetch_latest_github_release_version() -> anyhow::Result<String> {
    let ReleaseInfo {
        tag_name: latest_tag_name,
    } = create_client()
        .get(LATEST_RELEASE_URL)
        .send()
        .await?
        .error_for_status()?
        .json::<ReleaseInfo>()
        .await?;
    extract_version_from_latest_tag(&latest_tag_name)
}

/// Returns the latest version to show in a popup, if it should be shown.
/// Fetches the latest release synchronously so a fresh release is offered on
/// the next start, and respects the user's dismissal choice for that version.
pub async fn get_upgrade_version_for_popup(config: &Config) -> Option<String> {
    if !config.check_for_update_on_startup || is_source_build_version(xedoc_cli_version()) {
        return None;
    }

    let latest_version = fetch_latest_github_release_version().await.ok()?;
    let version_file = version_filepath(config);
    let dismissed_version = read_version_info(&version_file)
        .ok()
        .and_then(|info| info.dismissed_version);
    let is_dismissed = dismissed_version.as_deref() == Some(latest_version.as_str());
    let info = VersionInfo {
        latest_version: latest_version.clone(),
        last_checked_at: Utc::now(),
        dismissed_version,
    };
    if let Err(e) = write_version_info(&version_file, &info).await {
        tracing::error!("Failed to update version: {e}");
    }
    if is_dismissed || !is_newer(&latest_version, xedoc_cli_version()).unwrap_or(false) {
        return None;
    }
    Some(latest_version)
}
