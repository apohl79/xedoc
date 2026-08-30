use std::path::Path;

use anyhow::Result;
use app_test_support::app_server_json_shutdown_event;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

fn xedoc_command(xedoc_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(xedoc_utils_cargo_bin::cargo_bin("xedoc")?);
    cmd.env("XEDOC_HOME", xedoc_home);
    Ok(cmd)
}

#[test]
fn strict_config_rejects_unknown_config_fields_for_app_server() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    std::fs::write(
        xedoc_home.path().join("config.toml"),
        r#"
foo = "bar"
"#,
    )?;

    let mut cmd = xedoc_command(xedoc_home.path())?;
    cmd.args(["app-server", "--strict-config", "--listen", "off"])
        .assert()
        .failure()
        .stderr(contains("unknown configuration field"));

    Ok(())
}

#[test]
fn profile_is_loaded_for_direct_app_server() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    std::fs::write(
        xedoc_home.path().join("config.toml"),
        "model = \"gpt-base\"\n",
    )?;
    std::fs::write(
        xedoc_home.path().join("work.config.toml"),
        "profile_only_field = true\n",
    )?;

    let mut cmd = xedoc_command(xedoc_home.path())?;
    cmd.args([
        "--profile",
        "work",
        "app-server",
        "--strict-config",
        "--listen",
        "off",
    ])
    .assert()
    .failure()
    .stderr(contains("unknown configuration field"));

    Ok(())
}

#[test]
fn profile_is_rejected_for_app_server_daemon_tooling() -> Result<()> {
    let xedoc_home = TempDir::new()?;

    let mut cmd = xedoc_command(xedoc_home.path())?;
    cmd.args(["--profile", "work", "app-server", "daemon", "version"])
        .assert()
        .failure()
        .stderr(contains("--profile only applies"));

    Ok(())
}

#[test]
fn app_server_emits_json_info_events() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    let event = app_server_json_shutdown_event("xedoc", &["app-server"], xedoc_home.path())?;

    assert_eq!(
        event,
        json!({
            "level": "INFO",
            "fields": {
                "message": "processor task exited",
                "exit_reason": "last_connection_closed",
                "remaining_connection_count": 0,
                "shutdown_forced": false,
            },
            "target": "xedoc_app_server",
        })
    );

    Ok(())
}
