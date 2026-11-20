use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
#[cfg(unix)]
use sha2::Digest;
#[cfg(unix)]
use sha2::Sha256;
use tokio::fs;
use tokio::process::Command;

pub(crate) fn managed_xedoc_bin(xedoc_home: &Path) -> PathBuf {
    standalone_xedoc_bin(xedoc_home, "current")
}

pub(crate) fn experimental_managed_xedoc_bin(xedoc_home: &Path) -> PathBuf {
    standalone_xedoc_bin(xedoc_home, "experimental")
}

fn standalone_xedoc_bin(xedoc_home: &Path, release_link: &str) -> PathBuf {
    let release = xedoc_home
        .join("packages")
        .join("standalone")
        .join(release_link);
    // Windows has no top-level symlink to bin\xedoc.exe; the binary must run from
    // bin\ so it finds its packaged resources.
    if cfg!(windows) {
        release.join("bin").join("xedoc.exe")
    } else {
        release.join("xedoc")
    }
}

pub(crate) async fn resolved_managed_xedoc_bin(xedoc_bin: &Path) -> Result<PathBuf> {
    fs::canonicalize(xedoc_bin).await.with_context(|| {
        format!(
            "failed to resolve managed Xedoc binary {}",
            xedoc_bin.display()
        )
    })
}

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

fn parse_xedoc_version(output: &str) -> Result<String> {
    let version = output
        .split_whitespace()
        .nth(1)
        .filter(|version| !version.is_empty())
        .ok_or_else(|| anyhow!("managed Xedoc version output was malformed"))?;
    Ok(version.to_string())
}
