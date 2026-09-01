use anyhow::Result;
use predicates::str::contains;
use std::path::Path;
use tempfile::TempDir;

fn xedoc_command(xedoc_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(xedoc_utils_cargo_bin::cargo_bin("xedoc")?);
    cmd.env("XEDOC_HOME", xedoc_home);
    Ok(cmd)
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn update_does_not_start_interactive_prompt() -> Result<()> {
    let xedoc_home = TempDir::new()?;

    xedoc_command(xedoc_home.path())?
        .arg("update")
        .assert()
        .failure()
        .stderr(contains("`xedoc update` is not available in debug builds"));

    Ok(())
}
