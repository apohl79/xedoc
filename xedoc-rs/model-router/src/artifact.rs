//! Offline-only model artifact verification.

use std::collections::BTreeMap;
use std::fs;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use sha2::Digest;
use sha2::Sha256;

#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("artifact path must be a local directory")]
    RemoteOrNotDirectory,
    #[error("artifact manifest is missing or invalid")]
    InvalidManifest(#[source] serde_json::Error),
    #[error("artifact file is missing")]
    MissingFile,
    #[error("artifact path resolves outside its selected root")]
    OutsideRoot,
    #[error("artifact hash does not match manifest")]
    HashMismatch,
    #[error("artifact dimensions or revision are invalid")]
    InvalidIdentity,
    #[error("could not read artifact: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDescriptor {
    pub revision: String,
    pub sha256: String,
    pub bundle_sha256: String,
    pub dimensions: usize,
    pub file: String,
}

#[derive(Debug, Clone)]
pub struct LocalArtifact {
    pub root: PathBuf,
    pub descriptor: ArtifactDescriptor,
}

impl LocalArtifact {
    pub fn load(path: &Path) -> Result<Self, ArtifactError> {
        if path.to_string_lossy().contains("://") || !path.is_dir() {
            return Err(ArtifactError::RemoteOrNotDirectory);
        }
        let root = path.canonicalize()?;
        let manifest_path = root.join("manifest.json").canonicalize().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ArtifactError::MissingFile
            } else {
                ArtifactError::Io(error)
            }
        })?;
        if !manifest_path.starts_with(&root) {
            return Err(ArtifactError::OutsideRoot);
        }
        let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ArtifactError::MissingFile
            } else {
                ArtifactError::Io(error)
            }
        })?;
        let descriptor: ArtifactDescriptor =
            serde_json::from_slice(&manifest_bytes).map_err(ArtifactError::InvalidManifest)?;
        if descriptor.revision.is_empty()
            || descriptor.dimensions == 0
            || !is_sha256(&descriptor.sha256)
            || Path::new(&descriptor.file).is_absolute()
            || Path::new(&descriptor.file)
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(ArtifactError::InvalidIdentity);
        }
        let artifact_path = root
            .join(&descriptor.file)
            .canonicalize()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    ArtifactError::MissingFile
                } else {
                    ArtifactError::Io(error)
                }
            })?;
        if !artifact_path.starts_with(&root) {
            return Err(ArtifactError::OutsideRoot);
        }
        let bytes = fs::read(artifact_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ArtifactError::MissingFile
            } else {
                ArtifactError::Io(error)
            }
        })?;
        if format!("{:x}", Sha256::digest(bytes)) != descriptor.sha256 {
            return Err(ArtifactError::HashMismatch);
        }
        let bundle_sha256 = verify_bundle(&root)?;
        if descriptor.bundle_sha256 != bundle_sha256 {
            return Err(ArtifactError::HashMismatch);
        }
        Ok(Self { root, descriptor })
    }

    pub fn native_runtime_library(&self) -> Result<PathBuf, ArtifactError> {
        let runtime_root = self.root.join("runtime").join(runtime_target());
        let descriptor_path = runtime_root.join("manifest.json");
        let descriptor: RuntimeDescriptor =
            serde_json::from_slice(&fs::read(&descriptor_path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    ArtifactError::MissingFile
                } else {
                    ArtifactError::Io(error)
                }
            })?)
            .map_err(ArtifactError::InvalidManifest)?;
        if !is_sha256(&descriptor.sha256)
            || Path::new(&descriptor.file).is_absolute()
            || Path::new(&descriptor.file)
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(ArtifactError::InvalidIdentity);
        }
        let library_path = runtime_root
            .join(descriptor.file)
            .canonicalize()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    ArtifactError::MissingFile
                } else {
                    ArtifactError::Io(error)
                }
            })?;
        if !library_path.starts_with(&runtime_root)
            || format!("{:x}", Sha256::digest(fs::read(&library_path)?)) != descriptor.sha256
        {
            return Err(ArtifactError::HashMismatch);
        }
        Ok(library_path)
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeDescriptor {
    sha256: String,
    file: String,
}

fn verify_bundle(root: &Path) -> Result<String, ArtifactError> {
    let checksum_manifest = root.join("bundle.sha256");
    let mut entries = BTreeMap::<String, String>::new();
    let contents = fs::read_to_string(checksum_manifest)?;
    for line in contents.lines() {
        let Some((expected_hash, relative_path)) = line.split_once("  ") else {
            return Err(ArtifactError::InvalidIdentity);
        };
        let relative_path = Path::new(relative_path);
        let path = root.join(relative_path).canonicalize()?;
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            || !path.starts_with(root)
            || !is_sha256(expected_hash)
            || format!("{:x}", Sha256::digest(fs::read(path)?)) != expected_hash
            || entries
                .insert(
                    relative_path.to_string_lossy().into_owned(),
                    expected_hash.to_owned(),
                )
                .is_some()
        {
            return Err(ArtifactError::HashMismatch);
        }
    }
    const REQUIRED_FILES: [&str; 5] = [
        "onnx/model.onnx",
        "tokenizer.json",
        "config.json",
        "special_tokens_map.json",
        "tokenizer_config.json",
    ];
    if REQUIRED_FILES
        .iter()
        .any(|required| !entries.contains_key(*required))
    {
        return Err(ArtifactError::InvalidIdentity);
    }
    let canonical = entries
        .iter()
        .map(|(path, hash)| format!("{hash}  {path}\n"))
        .collect::<String>();
    Ok(format!("{:x}", Sha256::digest(canonical.as_bytes())))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const fn runtime_target() -> &'static str {
    "aarch64-apple-darwin"
}

#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
const fn runtime_target() -> &'static str {
    "x86_64-apple-darwin"
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const fn runtime_target() -> &'static str {
    "x86_64-unknown-linux-gnu"
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
const fn runtime_target() -> &'static str {
    "aarch64-unknown-linux-gnu"
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
const fn runtime_target() -> &'static str {
    "x86_64-pc-windows-msvc"
}

#[cfg(all(target_arch = "aarch64", target_os = "windows"))]
const fn runtime_target() -> &'static str {
    "aarch64-pc-windows-msvc"
}
