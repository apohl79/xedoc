use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
#[cfg(unix)]
use anyhow::anyhow;
#[cfg(unix)]
use sha2::Digest;
#[cfg(unix)]
use sha2::Sha256;
use tokio::fs;
#[cfg(unix)]
use tokio::process::Command;

pub(crate) fn managed_xedoc_bin(xedoc_home: &Path) -> PathBuf {
    xedoc_home
        .join("packages")
        .join("standalone")
        .join("current")
        .join(managed_xedoc_file_name())
}

pub(crate) async fn resolved_managed_xedoc_bin(xedoc_bin: &Path) -> Result<PathBuf> {
    fs::canonicalize(xedoc_bin).await.with_context(|| {
        format!(
            "failed to resolve managed Xedoc binary {}",
            xedoc_bin.display()
        )
    })
}

#[cfg(unix)]
pub(crate) async fn managed_xedoc_version(xedoc_bin: &Path) -> Result<String> {
    let output = Command::new(xedoc_bin)
        .arg("--version")
        .output()
        .await
        .with_context(|| {
            format!(
                "failed to invoke managed Xedoc binary {}",
                xedoc_bin.display()
            )
        })?;
    if !output.status.success() {
        return Err(anyhow!(
            "managed Xedoc binary {} exited with status {}",
            xedoc_bin.display(),
            output.status
        ));
    }

    let stdout = String::from_utf8(output.stdout).with_context(|| {
        format!(
            "managed Xedoc version was not utf-8: {}",
            xedoc_bin.display()
        )
    })?;
    parse_xedoc_version(&stdout)
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutableIdentity {
    digest: [u8; 32],
}

#[cfg(unix)]
pub(crate) async fn executable_identity(executable: &Path) -> Result<ExecutableIdentity> {
    let bytes = fs::read(executable)
        .await
        .with_context(|| format!("failed to read executable {}", executable.display()))?;
    Ok(executable_identity_from_bytes(&bytes))
}

#[cfg(unix)]
pub(crate) fn executable_identity_from_bytes(bytes: &[u8]) -> ExecutableIdentity {
    ExecutableIdentity {
        digest: Sha256::digest(bytes).into(),
    }
}

fn managed_xedoc_file_name() -> &'static str {
    if cfg!(windows) { "xedoc.exe" } else { "xedoc" }
}

#[cfg(unix)]
fn parse_xedoc_version(output: &str) -> Result<String> {
    let version = output
        .split_whitespace()
        .nth(1)
        .filter(|version| !version.is_empty())
        .ok_or_else(|| anyhow!("managed Xedoc version output was malformed"))?;
    Ok(version.to_string())
}

#[cfg(all(test, unix))]
#[path = "managed_install_tests.rs"]
mod tests;
