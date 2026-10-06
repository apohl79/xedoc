//! Unix installed remote-agent package verification.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value as JsonValue;
use sha2::Digest;
use sha2::Sha256;
use xedoc_install_context::InstallContext;

const REMOTE_AGENT_DIR: &str = "remote-agent";
const MANIFEST_NAME: &str = "remote-agent-manifest.json";
const MANIFEST_VERSION: u64 = 1;
const TOOLS_SCHEMA_VERSION: u64 = 1;
const CONTRACT_VERSION: &str = "xedoc.remote-agent/v1";
const PAYLOAD_VERSION: &str = "0.1.0";
const PYTHON_VERSION: &str = "3.12.14";
const DEPENDENCY_LOCK_SHA256: &str =
    "5e43635c8d2377fc3c1861c280df36908683d372c91c0cb54bbf2ce644d3de5a";
const EXPECTED_DEPENDENCIES: &[&str] = &["cffi==2.0.0", "cryptography==46.0.5", "pycparser==2.23"];
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_TOOLS_BYTES: u64 = 64 * 1024;
const MAX_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RUNTIME_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_RUNTIME_ENTRIES: usize = 32 * 1024;

const EXPECTED_TOOL_NAMES: &[&str] = &[
    "remote_hosts_list",
    "remote_hosts_discover",
    "remote_host_pair",
    "remote_pairing_requests",
    "remote_pairing_approve",
    "remote_pairing_reject",
    "remote_host_grants_list",
    "remote_host_grant_set",
    "remote_host_suspend",
    "remote_host_revoke",
    "remote_host_remove",
    "remote_host_rotate",
    "remote_workspaces_list",
    "remote_sessions_list",
    "remote_sessions_search",
    "remote_session_start",
    "remote_session_resume",
    "remote_session_attach",
    "remote_session_read",
    "remote_session_send",
    "remote_session_steer",
    "remote_session_message",
    "remote_session_status",
    "remote_session_wait",
    "remote_session_cancel",
    "remote_session_detach",
    "remote_request_review",
    "remote_request_approve",
    "remote_request_reject",
];

static PACKAGE: OnceLock<Result<Arc<RemoteAgentPackage>, String>> = OnceLock::new();

pub(crate) struct RemoteAgentPackage {
    pub(crate) payload_version: String,
    pub(crate) payload_hash: String,
    pub(crate) tools_schema_version: u64,
    pub(crate) tools_hash: String,
    pub(crate) package_version: String,
    pub(crate) runtime_hash: String,
    pub(crate) tools: Arc<[RemoteAgentTool]>,
    interpreter: PathBuf,
    payload: PathBuf,
    _snapshot: tempfile::TempDir,
}

#[derive(Clone)]
pub(crate) struct RemoteAgentTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: JsonValue,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageManifest {
    manifest_version: u64,
    contract_version: String,
    package_version: String,
    target: String,
    payload: HashedPayload,
    tools: HashedTools,
    runtime: RuntimeManifest,
}

#[derive(Deserialize)]
struct InstalledPackageMetadata {
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HashedPayload {
    path: String,
    version: String,
    sha256: String,
    bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HashedTools {
    path: String,
    schema_version: u64,
    sha256: String,
    bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeManifest {
    path: String,
    target: String,
    runtime_id: String,
    python_version: String,
    sha256: String,
    interpreter: String,
    site_packages: String,
    dependency_lock_sha256: String,
    dependencies: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolsManifest {
    schema_version: u64,
    contract_version: String,
    namespace: String,
    tools: Vec<ToolManifest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolManifest {
    name: String,
    description: String,
    input_schema: JsonValue,
}

impl RemoteAgentPackage {
    pub(crate) fn command_argv(&self) -> Vec<OsString> {
        vec![
            self.interpreter.as_os_str().to_owned(),
            OsString::from("-B"),
            OsString::from("-I"),
            self.payload.as_os_str().to_owned(),
            OsString::from("extension"),
        ]
    }
}

pub(crate) fn installed_remote_agent_package() -> Result<Arc<RemoteAgentPackage>, String> {
    PACKAGE
        .get_or_init(load_installed_package)
        .as_ref()
        .map(Arc::clone)
        .map_err(Clone::clone)
}

fn load_installed_package() -> Result<Arc<RemoteAgentPackage>, String> {
    let manifest_path = InstallContext::current()
        .bundled_resource(Path::new(REMOTE_AGENT_DIR).join(MANIFEST_NAME))
        .ok_or_else(|| "built-in remote-agent package manifest is unavailable".to_string())?
        .into_path_buf();
    let manifest_dir = canonical_parent(&manifest_path)?;
    let manifest: PackageManifest =
        read_json(&manifest_path, MAX_MANIFEST_BYTES, "package manifest")?;
    validate_manifest_identity(&manifest, &manifest_dir)?;

    let source_payload = verified_file(
        &manifest_dir,
        &manifest.payload.path,
        "remote-agent.pyz",
        manifest.payload.bytes,
        MAX_PAYLOAD_BYTES,
        &manifest.payload.sha256,
    )?;
    let source_tools = verified_file(
        &manifest_dir,
        &manifest.tools.path,
        "remote-tools.json",
        manifest.tools.bytes,
        MAX_TOOLS_BYTES,
        &manifest.tools.sha256,
    )?;
    let source_runtime = verified_runtime_root(&manifest_dir, &manifest.runtime)?;
    let snapshot = create_snapshot(&source_payload, &source_tools, &source_runtime)?;
    let snapshot_dir = snapshot
        .path()
        .join(REMOTE_AGENT_DIR)
        .canonicalize()
        .map_err(|_| "remote-agent snapshot is unavailable".to_string())?;
    let payload = verified_file(
        &snapshot_dir,
        &manifest.payload.path,
        "remote-agent.pyz",
        manifest.payload.bytes,
        MAX_PAYLOAD_BYTES,
        &manifest.payload.sha256,
    )?;
    let tools_path = verified_file(
        &snapshot_dir,
        &manifest.tools.path,
        "remote-tools.json",
        manifest.tools.bytes,
        MAX_TOOLS_BYTES,
        &manifest.tools.sha256,
    )?;
    let tools_manifest: ToolsManifest = read_json(&tools_path, MAX_TOOLS_BYTES, "tool schema")?;
    let tools = validate_tools(tools_manifest)?;
    let runtime_root = verified_runtime_root(&snapshot_dir, &manifest.runtime)?;
    verified_runtime_dependencies(&runtime_root, &manifest.runtime)?;
    let interpreter = verified_runtime_interpreter(&runtime_root, &manifest.runtime.interpreter)?;
    let runtime_hash = hash_runtime(&runtime_root)?;
    if runtime_hash != manifest.runtime.sha256 {
        return Err("remote-agent runtime hash does not match its manifest".to_string());
    }

    Ok(Arc::new(RemoteAgentPackage {
        payload_version: manifest.payload.version,
        payload_hash: manifest.payload.sha256,
        tools_schema_version: manifest.tools.schema_version,
        tools_hash: manifest.tools.sha256,
        package_version: manifest.package_version,
        runtime_hash,
        tools: tools.into(),
        interpreter,
        payload,
        _snapshot: snapshot,
    }))
}

fn create_snapshot(
    payload: &Path,
    tools: &Path,
    runtime: &Path,
) -> Result<tempfile::TempDir, String> {
    let snapshot = tempfile::Builder::new()
        .prefix("xedoc-remote-agent-")
        .tempdir()
        .map_err(|_| "remote-agent snapshot could not be created".to_string())?;
    make_snapshot_private(snapshot.path())?;
    let root = snapshot.path().join(REMOTE_AGENT_DIR);
    fs::create_dir(&root).map_err(|_| "remote-agent snapshot could not be created".to_string())?;
    copy_bounded_file(payload, &root.join("remote-agent.pyz"), MAX_PAYLOAD_BYTES)?;
    copy_bounded_file(tools, &root.join("remote-tools.json"), MAX_TOOLS_BYTES)?;
    let runtime_destination = root.join("runtime/python");
    fs::create_dir_all(&runtime_destination)
        .map_err(|_| "remote-agent runtime snapshot could not be created".to_string())?;
    let mut copied_bytes = 0_u64;
    let mut copied_entries = 0_usize;
    copy_runtime_tree(
        runtime,
        &runtime_destination,
        runtime,
        &mut copied_bytes,
        &mut copied_entries,
    )?;
    Ok(snapshot)
}

#[cfg(unix)]
fn make_snapshot_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| "remote-agent snapshot permissions could not be secured".to_string())
}

#[cfg(not(unix))]
fn make_snapshot_private(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn copy_bounded_file(source: &Path, destination: &Path, max_bytes: u64) -> Result<u64, String> {
    let mut input = fs::File::open(source)
        .map_err(|_| "remote-agent snapshot source is unavailable".to_string())?;
    let mut output = fs::File::create(destination)
        .map_err(|_| "remote-agent snapshot destination is unavailable".to_string())?;
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|_| "remote-agent snapshot source is unreadable".to_string())?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(
            read.try_into()
                .map_err(|_| "remote-agent snapshot exceeds its size bound".to_string())?,
        );
        if total > max_bytes {
            return Err("remote-agent snapshot exceeds its size bound".to_string());
        }
        output
            .write_all(&buffer[..read])
            .map_err(|_| "remote-agent snapshot destination is unwritable".to_string())?;
    }
    output
        .sync_all()
        .map_err(|_| "remote-agent snapshot destination is unwritable".to_string())?;
    let permissions = fs::metadata(source)
        .map_err(|_| "remote-agent snapshot source is unavailable".to_string())?
        .permissions();
    fs::set_permissions(destination, permissions)
        .map_err(|_| "remote-agent snapshot permissions could not be copied".to_string())?;
    Ok(total)
}

fn copy_runtime_tree(
    source_root: &Path,
    destination_root: &Path,
    current: &Path,
    copied_bytes: &mut u64,
    copied_entries: &mut usize,
) -> Result<(), String> {
    for entry in fs::read_dir(current)
        .map_err(|_| "remote-agent runtime snapshot is unreadable".to_string())?
    {
        *copied_entries = copied_entries.saturating_add(1);
        if *copied_entries > MAX_RUNTIME_ENTRIES {
            return Err("remote-agent runtime snapshot has too many entries".to_string());
        }
        let entry = entry.map_err(|_| "remote-agent runtime snapshot is unreadable".to_string())?;
        let source = entry.path();
        let relative = source
            .strip_prefix(source_root)
            .map_err(|_| "remote-agent runtime snapshot path is invalid".to_string())?;
        let destination = destination_root.join(relative);
        let metadata = fs::symlink_metadata(&source)
            .map_err(|_| "remote-agent runtime snapshot entry is unreadable".to_string())?;
        if metadata.is_dir() {
            fs::create_dir(&destination)
                .map_err(|_| "remote-agent runtime snapshot directory is unwritable".to_string())?;
            copy_runtime_tree(
                source_root,
                destination_root,
                &source,
                copied_bytes,
                copied_entries,
            )?;
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&source)
                .map_err(|_| "remote-agent runtime snapshot symlink is unreadable".to_string())?;
            copy_symlink(&target, &destination, &source)?;
        } else if metadata.is_file() {
            let remaining = MAX_RUNTIME_BYTES.saturating_sub(*copied_bytes);
            let bytes = copy_bounded_file(&source, &destination, remaining)?;
            *copied_bytes = copied_bytes.saturating_add(bytes);
            if *copied_bytes > MAX_RUNTIME_BYTES {
                return Err("remote-agent runtime snapshot exceeds its size bound".to_string());
            }
        } else {
            return Err("remote-agent runtime snapshot has an unsupported entry".to_string());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(target: &Path, destination: &Path, _source: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, destination)
        .map_err(|_| "remote-agent runtime snapshot symlink is unwritable".to_string())
}

#[cfg(windows)]
fn copy_symlink(target: &Path, destination: &Path, source: &Path) -> Result<(), String> {
    if source
        .metadata()
        .map_err(|_| "remote-agent runtime snapshot symlink is unreadable".to_string())?
        .is_dir()
    {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    }
    .map_err(|_| "remote-agent runtime snapshot symlink is unwritable".to_string())
}

fn validate_manifest_identity(
    manifest: &PackageManifest,
    manifest_dir: &Path,
) -> Result<(), String> {
    let package_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "remote-agent package root is invalid".to_string())?;
    let package_metadata: InstalledPackageMetadata = read_json(
        &package_root.join("xedoc-package.json"),
        MAX_MANIFEST_BYTES,
        "installed package metadata",
    )?;

    if manifest.manifest_version != MANIFEST_VERSION
        || manifest.contract_version != CONTRACT_VERSION
        || manifest.package_version != package_metadata.version
        || manifest.target != package_target()
        || manifest.payload.version != PAYLOAD_VERSION
        || manifest.tools.schema_version != TOOLS_SCHEMA_VERSION
        || manifest.runtime.target != package_target()
        || manifest.runtime.path != "remote-agent/runtime"
        || !manifest
            .runtime
            .runtime_id
            .starts_with("remote-agent-python-r2-")
        || manifest.runtime.runtime_id.len() != "remote-agent-python-r2-".len() + 64
        || manifest.runtime.python_version != PYTHON_VERSION
        || !is_sha256(&manifest.runtime.sha256)
        || manifest.runtime.dependency_lock_sha256 != DEPENDENCY_LOCK_SHA256
        || manifest.runtime.dependencies.len() != EXPECTED_DEPENDENCIES.len()
        || !manifest
            .runtime
            .dependencies
            .iter()
            .map(String::as_str)
            .eq(EXPECTED_DEPENDENCIES.iter().copied())
        || manifest.runtime.site_packages != runtime_site_packages_path()
    {
        return Err("remote-agent package manifest identity is invalid".to_string());
    }
    Ok(())
}

fn validate_tools(manifest: ToolsManifest) -> Result<Vec<RemoteAgentTool>, String> {
    if manifest.schema_version != TOOLS_SCHEMA_VERSION
        || manifest.contract_version != CONTRACT_VERSION
        || manifest.namespace != "remote"
        || manifest.tools.len() != EXPECTED_TOOL_NAMES.len()
    {
        return Err("remote-agent tool schema identity is invalid".to_string());
    }
    manifest
        .tools
        .into_iter()
        .zip(EXPECTED_TOOL_NAMES)
        .map(|(tool, expected_name)| {
            let schema = tool
                .input_schema
                .as_object()
                .filter(|schema| {
                    schema.get("type").and_then(JsonValue::as_str) == Some("object")
                        && schema.get("properties").is_some_and(JsonValue::is_object)
                        && schema.get("required").is_some_and(JsonValue::is_array)
                        && schema
                            .get("additionalProperties")
                            .and_then(JsonValue::as_bool)
                            == Some(false)
                })
                .ok_or_else(|| "remote-agent tool input schema is invalid".to_string())?;
            if tool.name != *expected_name
                || tool.description.is_empty()
                || tool.description.len() > 1024
                || serde_json::to_vec(schema).map_or(true, |bytes| bytes.len() > 16 * 1024)
            {
                return Err("remote-agent tool declaration is invalid".to_string());
            }
            Ok(RemoteAgentTool {
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
            })
        })
        .collect()
}

fn verified_file(
    manifest_dir: &Path,
    manifest_path: &str,
    expected_name: &str,
    expected_bytes: u64,
    max_bytes: u64,
    expected_hash: &str,
) -> Result<PathBuf, String> {
    let expected_manifest_path = format!("{REMOTE_AGENT_DIR}/{expected_name}");
    if manifest_path != expected_manifest_path || expected_bytes == 0 || expected_bytes > max_bytes
    {
        return Err(format!(
            "remote-agent {expected_name} manifest entry is invalid"
        ));
    }
    let path = manifest_dir.join(expected_name);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| format!("remote-agent {expected_name} is unavailable"))?;
    if !metadata.file_type().is_file() || metadata.len() != expected_bytes {
        return Err(format!("remote-agent {expected_name} identity is invalid"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| format!("remote-agent {expected_name} is unavailable"))?;
    let (actual_hash, actual_bytes) = sha256_file(&canonical, max_bytes)?;
    if canonical.parent() != Some(manifest_dir)
        || actual_bytes != expected_bytes
        || actual_hash != expected_hash
    {
        return Err(format!("remote-agent {expected_name} hash is invalid"));
    }
    Ok(canonical)
}

fn verified_runtime_root(
    manifest_dir: &Path,
    runtime: &RuntimeManifest,
) -> Result<PathBuf, String> {
    let root = manifest_dir.join("runtime/python");
    let canonical = root
        .canonicalize()
        .map_err(|_| "remote-agent runtime is unavailable".to_string())?;
    if !canonical.is_dir() || !canonical.starts_with(manifest_dir) {
        return Err("remote-agent runtime path is invalid".to_string());
    }
    if runtime.interpreter != runtime_interpreter_path() {
        return Err("remote-agent runtime interpreter identity is invalid".to_string());
    }
    Ok(canonical)
}

fn verified_runtime_interpreter(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = relative
        .strip_prefix("runtime/python/")
        .ok_or_else(|| "remote-agent runtime interpreter path is invalid".to_string())?;
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| "remote-agent runtime interpreter is unavailable".to_string())?;
    let canonical = path
        .canonicalize()
        .map_err(|_| "remote-agent runtime interpreter is unavailable".to_string())?;
    if (!metadata.file_type().is_file() && !metadata.file_type().is_symlink())
        || !canonical.starts_with(root)
    {
        return Err("remote-agent runtime interpreter identity is invalid".to_string());
    }
    Ok(canonical)
}

fn verified_runtime_dependencies(root: &Path, runtime: &RuntimeManifest) -> Result<(), String> {
    let relative = runtime
        .site_packages
        .strip_prefix("runtime/python/")
        .ok_or_else(|| "remote-agent runtime site-packages path is invalid".to_string())?;
    let site_packages = root.join(relative);
    let canonical = site_packages
        .canonicalize()
        .map_err(|_| "remote-agent runtime site-packages are unavailable".to_string())?;
    if !canonical.is_dir() || !canonical.starts_with(root) {
        return Err("remote-agent runtime site-packages path is invalid".to_string());
    }
    let expected_metadata = EXPECTED_DEPENDENCIES
        .iter()
        .map(|dependency| {
            dependency
                .split_once("==")
                .map(|(name, version)| format!("{}-{version}", name.replace('-', "_")))
                .ok_or_else(|| "remote-agent runtime dependency identity is invalid".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut discovered_metadata = Vec::new();
    for entry in fs::read_dir(&canonical)
        .map_err(|_| "remote-agent runtime site-packages are unreadable".to_string())?
    {
        let entry =
            entry.map_err(|_| "remote-agent runtime site-packages are unreadable".to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".dist-info") {
            let metadata = fs::symlink_metadata(&path).map_err(|_| {
                "remote-agent runtime dependency metadata is unreadable".to_string()
            })?;
            if !metadata.file_type().is_dir() {
                return Err("remote-agent runtime dependency metadata is invalid".to_string());
            }
            discovered_metadata.push(name);
        }
    }
    discovered_metadata.sort();
    let mut expected_metadata = expected_metadata
        .into_iter()
        .map(|name| format!("{name}.dist-info"))
        .collect::<Vec<_>>();
    expected_metadata.sort();
    if discovered_metadata != expected_metadata {
        return Err("remote-agent runtime dependency metadata is invalid".to_string());
    }
    for dependency in EXPECTED_DEPENDENCIES {
        verified_dependency_metadata(&canonical, dependency)?;
    }
    Ok(())
}

fn verified_dependency_metadata(site_packages: &Path, dependency: &str) -> Result<(), String> {
    let (name, version) = dependency
        .split_once("==")
        .ok_or_else(|| "remote-agent runtime dependency identity is invalid".to_string())?;
    let normalized_name = name.replace('-', "_");
    let path = site_packages
        .join(format!("{normalized_name}-{version}.dist-info"))
        .join("METADATA");
    let bytes = fs::read(&path)
        .map_err(|_| "remote-agent runtime dependency metadata is unavailable".to_string())?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 {
        return Err("remote-agent runtime dependency metadata is invalid".to_string());
    }
    let metadata = std::str::from_utf8(&bytes)
        .map_err(|_| "remote-agent runtime dependency metadata is invalid".to_string())?;
    let metadata_name = metadata
        .lines()
        .find_map(|line| line.strip_prefix("Name: "))
        .map(|value| value.replace('_', "-").to_lowercase());
    let metadata_version = metadata
        .lines()
        .find_map(|line| line.strip_prefix("Version: "));
    if metadata_name.as_deref() != Some(name) || metadata_version != Some(version) {
        return Err("remote-agent runtime dependency identity is invalid".to_string());
    }
    Ok(())
}

fn canonical_parent(path: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "remote-agent package manifest is unavailable".to_string())?;
    if !metadata.file_type().is_file() {
        return Err("remote-agent package manifest identity is invalid".to_string());
    }
    path.canonicalize()
        .map_err(|_| "remote-agent package manifest is unavailable".to_string())?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "remote-agent package manifest path is invalid".to_string())
}

fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    max_bytes: u64,
    label: &str,
) -> Result<T, String> {
    let file = fs::File::open(path).map_err(|_| format!("remote-agent {label} is unavailable"))?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| format!("remote-agent {label} is unavailable"))?;
    if bytes.is_empty() || u64::try_from(bytes.len()).map_or(true, |length| length > max_bytes) {
        return Err(format!("remote-agent {label} exceeds its size bound"));
    }
    serde_json::from_slice(&bytes).map_err(|_| format!("remote-agent {label} is invalid"))
}

fn sha256_file(path: &Path, max_bytes: u64) -> Result<(String, u64), String> {
    let mut file =
        fs::File::open(path).map_err(|_| "remote-agent resource is unavailable".to_string())?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "remote-agent resource is unreadable".to_string())?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(
            read.try_into()
                .map_err(|_| "remote-agent resource is too large".to_string())?,
        );
        if total > max_bytes {
            return Err("remote-agent resource exceeds its size bound".to_string());
        }
        digest.update(&buffer[..read]);
    }
    Ok((format!("{:x}", digest.finalize()), total))
}

fn hash_runtime(root: &Path) -> Result<String, String> {
    let mut entries = Vec::new();
    collect_runtime_entries(root, root, &mut entries)?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    for (relative, path, link_target) in entries {
        digest.update(relative.as_bytes());
        digest.update([0]);
        if let Some(link_target) = link_target {
            digest.update(b"symlink\0");
            digest.update(link_target.as_bytes());
        } else {
            digest.update(b"file\0");
            let mut file = fs::File::open(path)
                .map_err(|_| "remote-agent runtime entry is unreadable".to_string())?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|_| "remote-agent runtime entry is unreadable".to_string())?;
                if read == 0 {
                    break;
                }
                total = total.saturating_add(
                    read.try_into()
                        .map_err(|_| "remote-agent runtime exceeds its size bound".to_string())?,
                );
                if total > MAX_RUNTIME_BYTES {
                    return Err("remote-agent runtime exceeds its size bound".to_string());
                }
                digest.update(&buffer[..read]);
            }
        }
        digest.update([0]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn collect_runtime_entries(
    root: &Path,
    current: &Path,
    entries: &mut Vec<(String, PathBuf, Option<String>)>,
) -> Result<(), String> {
    for entry in
        fs::read_dir(current).map_err(|_| "remote-agent runtime is unreadable".to_string())?
    {
        if entries.len() >= MAX_RUNTIME_ENTRIES {
            return Err("remote-agent runtime has too many entries".to_string());
        }
        let entry = entry.map_err(|_| "remote-agent runtime is unreadable".to_string())?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| "remote-agent runtime entry is unreadable".to_string())?;
        if metadata.is_dir() {
            collect_runtime_entries(root, &path, entries)?;
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|_| "remote-agent runtime symlink is unreadable".to_string())?;
            let canonical = path
                .canonicalize()
                .map_err(|_| "remote-agent runtime symlink is invalid".to_string())?;
            if !canonical.starts_with(root) {
                return Err("remote-agent runtime symlink escapes its root".to_string());
            }
            entries.push((
                relative_runtime_path(root, &path)?,
                path,
                Some(target.to_string_lossy().replace('\\', "/")),
            ));
        } else if metadata.is_file() {
            entries.push((relative_runtime_path(root, &path)?, path, None));
        } else {
            return Err("remote-agent runtime has an unsupported entry".to_string());
        }
    }
    Ok(())
}

fn relative_runtime_path(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|_| "remote-agent runtime entry path is invalid".to_string())
}

fn runtime_interpreter_path() -> &'static str {
    if cfg!(windows) {
        "runtime/python/python.exe"
    } else {
        "runtime/python/bin/python3"
    }
}

fn runtime_site_packages_path() -> &'static str {
    if cfg!(windows) {
        "runtime/python/Lib/site-packages"
    } else {
        "runtime/python/lib/python3.12/site-packages"
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte <= b'f'))
}

fn package_target() -> &'static str {
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "aarch64", target_os = "windows")) {
        "aarch64-pc-windows-msvc"
    } else if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported"
    }
}
