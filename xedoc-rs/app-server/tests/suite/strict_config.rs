use std::process::Command;

use anyhow::Result;
use tempfile::TempDir;

#[test]
fn strict_config_rejects_unknown_config_fields_for_standalone_app_server() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    std::fs::write(
        xedoc_home.path().join("config.toml"),
        r#"
foo = "bar"
"#,
    )?;

    let output = Command::new(xedoc_utils_cargo_bin::cargo_bin("xedoc-app-server")?)
        .env("XEDOC_HOME", xedoc_home.path())
        .env(
            "XEDOC_APP_SERVER_MANAGED_CONFIG_PATH",
            xedoc_home.path().join("managed_config.toml"),
        )
        .args(["--strict-config", "--listen", "off"])
        .output()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("unknown configuration field `foo`"),
        "expected strict config error in stderr, got: {stderr}"
    );

    Ok(())
}
