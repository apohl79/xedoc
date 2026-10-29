use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use xedoc_config::HooksFile;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_path_uri::PathConvention;
use xedoc_utils_path_uri::PathUri;
use xedoc_utils_plugins::find_plugin_manifest_path;
const MAX_DEFAULT_PROMPT_COUNT: usize = 3;
const MAX_DEFAULT_PROMPT_LEN: usize = 128;
const MAX_THREAD_CONTEXT_ENTRIES: usize = 32;
const MAX_THREAD_CONTEXT_ENTRY_LEN: usize = 8_000;
const MAX_THREAD_CONTEXT_CONDITION_SHELL_LEN: usize = 8_000;
const MAX_THREAD_CONTEXT_TOTAL_LEN: usize = 32_000;

pub type PluginManifest = xedoc_plugin::manifest::PluginManifest<AbsolutePathBuf>;
pub type PluginManifestExtension = xedoc_plugin::manifest::PluginManifestExtension<AbsolutePathBuf>;
pub type PluginManifestExtensionCommand = xedoc_plugin::manifest::PluginManifestExtensionCommand;
pub type PluginManifestHooks = xedoc_plugin::manifest::PluginManifestHooks<AbsolutePathBuf>;
pub type PluginManifestInterface = xedoc_plugin::manifest::PluginManifestInterface<AbsolutePathBuf>;
pub type PluginManifestMcpServers =
    xedoc_plugin::manifest::PluginManifestMcpServers<AbsolutePathBuf>;
pub type PluginManifestPaths = xedoc_plugin::manifest::PluginManifestPaths<AbsolutePathBuf>;

pub type UriPluginManifest = xedoc_plugin::manifest::PluginManifest<PathUri>;
pub type UriPluginManifestExtension = xedoc_plugin::manifest::PluginManifestExtension<PathUri>;
const MAX_EXTENSION_ID_LEN: usize = 128;
const MAX_EXTENSION_COMMAND_NAME_LEN: usize = 128;
const MAX_EXTENSION_DESCRIPTION_LEN: usize = 512;
const MAX_EXTENSION_CAPABILITY_LEN: usize = 128;
const DEFAULT_EXTENSION_APPROVAL_RESPONSE_TIMEOUT_MS: u64 = 10_000;
const MIN_EXTENSION_APPROVAL_RESPONSE_TIMEOUT_MS: u64 = 1_000;
const MAX_EXTENSION_APPROVAL_RESPONSE_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPluginManifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    extensions: Option<JsonValue>,
    // Keep manifest paths as raw strings so we can validate the required `./...` syntax before
    // resolving them under the plugin root.
    #[serde(default)]
    skills: Option<RawPluginManifestPaths>,
    #[serde(default)]
    mcp_servers: Option<RawPluginManifestMcpServers>,
    #[serde(default)]
    apps: Option<String>,
    #[serde(default)]
    hooks: Option<RawPluginManifestHooks>,
    #[serde(default)]
    context: Option<RawPluginManifestContext>,
    #[serde(default)]
    interface: Option<RawPluginManifestInterface>,
}

#[derive(Deserialize)]
struct RawPluginCommandManifest {
    #[serde(default)]
    commands: Option<RawPluginManifestPaths>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPluginManifestContext {
    #[serde(default)]
    thread: Vec<RawPluginThreadContextEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPluginThreadContextEntry {
    slot: String,
    position: String,
    text: String,
    #[serde(default, rename = "condition_shell", alias = "conditionShell")]
    condition_shell: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPluginManifestInterface {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    short_description: Option<String>,
    #[serde(default)]
    long_description: Option<String>,
    #[serde(default)]
    developer_name: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    #[serde(alias = "websiteURL")]
    website_url: Option<String>,
    #[serde(default)]
    #[serde(alias = "privacyPolicyURL")]
    privacy_policy_url: Option<String>,
    #[serde(default)]
    #[serde(alias = "termsOfServiceURL")]
    terms_of_service_url: Option<String>,
    #[serde(default)]
    default_prompt: Option<RawPluginManifestDefaultPrompt>,
    #[serde(default)]
    brand_color: Option<String>,
    #[serde(default)]
    composer_icon: Option<String>,
    #[serde(default)]
    logo: Option<String>,
    #[serde(default)]
    logo_dark: Option<String>,
    #[serde(default)]
    screenshots: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawPluginManifestDefaultPrompt {
    String(String),
    List(Vec<RawPluginManifestDefaultPromptEntry>),
    Invalid(JsonValue),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawPluginManifestDefaultPromptEntry {
    String(String),
    Invalid(JsonValue),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawPluginManifestPaths {
    Path(String),
    Paths(Vec<String>),
    Invalid(JsonValue),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawPluginManifestMcpServers {
    Path(String),
    Object(std::collections::BTreeMap<String, JsonValue>),
    Invalid(JsonValue),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawPluginManifestHooks {
    Path(String),
    Paths(Vec<String>),
    Inline(Box<HooksFile>),
    InlineList(Vec<HooksFile>),
    Invalid(JsonValue),
}

/// Loads a plugin manifest from the local host filesystem.
pub fn load_plugin_manifest(plugin_root: &Path) -> Option<PluginManifest> {
    let manifest_path = find_plugin_manifest_path(plugin_root)?;
    let contents = fs::read_to_string(&manifest_path).ok()?;
    match parse_plugin_manifest(plugin_root, &manifest_path, &contents) {
        Ok(manifest) => Some(manifest),
        Err(err) => {
            tracing::warn!(
                path = %manifest_path.display(),
                "failed to parse plugin manifest: {err}"
            );
            None
        }
    }
}

pub(crate) fn load_plugin_command_paths(plugin_root: &Path) -> io::Result<Option<Vec<PathBuf>>> {
    let Some(manifest_path) = find_plugin_manifest_path(plugin_root) else {
        return Ok(None);
    };
    let manifest =
        serde_json::from_str::<RawPluginCommandManifest>(&fs::read_to_string(manifest_path)?)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let Some(commands) = manifest.commands else {
        return Ok(None);
    };
    let plugin_root = PathUri::from_host_native_path(plugin_root)?;
    resolve_manifest_paths(&plugin_root, "commands", Some(&commands))
        .into_iter()
        .map(|path| Ok(path.to_abs_path()?.into_path_buf()))
        .collect::<io::Result<Vec<_>>>()
        .map(Some)
}

pub(crate) fn parse_plugin_manifest(
    plugin_root: &Path,
    manifest_path: &Path,
    contents: &str,
) -> Result<PluginManifest, serde_json::Error> {
    let plugin_root_uri =
        PathUri::from_host_native_path(plugin_root).map_err(serde_json::Error::io)?;
    let manifest_path_uri =
        PathUri::from_host_native_path(manifest_path).map_err(serde_json::Error::io)?;
    parse_plugin_manifest_uri(&plugin_root_uri, &manifest_path_uri, contents)?
        .try_map_resources(|path| path.to_abs_path().map_err(serde_json::Error::io))
}

pub fn parse_plugin_manifest_uri(
    plugin_root: &PathUri,
    manifest_path: &PathUri,
    contents: &str,
) -> Result<UriPluginManifest, serde_json::Error> {
    let RawPluginManifest {
        name: raw_name,
        version,
        description,
        keywords,
        extensions,
        skills,
        mcp_servers,
        apps,
        hooks,
        context,
        interface,
    } = serde_json::from_str::<RawPluginManifest>(contents)?;
    let name = plugin_root
        .basename()
        .filter(|_| raw_name.trim().is_empty())
        .unwrap_or(raw_name);
    let context = context.map(|c| {
        let mut total_len = 0;
        let mut thread = Vec::new();
        for raw in c.thread.into_iter().take(MAX_THREAD_CONTEXT_ENTRIES) {
            let Some(entry) = convert_thread_context_entry(raw) else {
                continue;
            };
            let entry_len = entry.text.chars().count();
            if total_len + entry_len > MAX_THREAD_CONTEXT_TOTAL_LEN {
                tracing::warn!(
                    "plugin thread context exceeds the maximum aggregate length; skipping remaining entries"
                );
                break;
            }
            total_len += entry_len;
            thread.push(entry);
        }
        xedoc_plugin::manifest::PluginManifestContext { thread }
    });
    let manifest_path_for_warning = manifest_path.to_string();
    let version = version.and_then(|version| {
        let version = version.trim();
        (!version.is_empty()).then(|| version.to_string())
    });
    let extensions = resolve_manifest_extensions(plugin_root, extensions);
    let interface = interface.and_then(|interface| {
        let RawPluginManifestInterface {
            display_name,
            short_description,
            long_description,
            developer_name,
            category,
            capabilities,
            website_url,
            privacy_policy_url,
            terms_of_service_url,
            default_prompt,
            brand_color,
            composer_icon,
            logo,
            logo_dark,
            screenshots,
        } = interface;

        let interface = xedoc_plugin::manifest::PluginManifestInterface {
            display_name,
            short_description,
            long_description,
            developer_name,
            category,
            capabilities,
            website_url,
            privacy_policy_url,
            terms_of_service_url,
            default_prompt: resolve_default_prompts(
                &manifest_path_for_warning,
                default_prompt.as_ref(),
            ),
            brand_color,
            composer_icon: resolve_interface_asset_path(
                plugin_root,
                "interface.composerIcon",
                composer_icon.as_deref(),
            ),
            logo: resolve_interface_asset_path(plugin_root, "interface.logo", logo.as_deref()),
            logo_dark: resolve_interface_asset_path(
                plugin_root,
                "interface.logoDark",
                logo_dark.as_deref(),
            ),
            screenshots: screenshots
                .iter()
                .filter_map(|screenshot| {
                    resolve_interface_asset_path(
                        plugin_root,
                        "interface.screenshots",
                        Some(screenshot),
                    )
                })
                .collect(),
        };

        let has_fields = interface.display_name.is_some()
            || interface.short_description.is_some()
            || interface.long_description.is_some()
            || interface.developer_name.is_some()
            || interface.category.is_some()
            || !interface.capabilities.is_empty()
            || interface.website_url.is_some()
            || interface.privacy_policy_url.is_some()
            || interface.terms_of_service_url.is_some()
            || interface.default_prompt.is_some()
            || interface.brand_color.is_some()
            || interface.composer_icon.is_some()
            || interface.logo.is_some()
            || interface.logo_dark.is_some()
            || !interface.screenshots.is_empty();

        has_fields.then_some(interface)
    });
    Ok(xedoc_plugin::manifest::PluginManifest {
        name,
        version,
        description,
        keywords,
        extensions,
        paths: xedoc_plugin::manifest::PluginManifestPaths {
            skills: resolve_manifest_paths(plugin_root, "skills", skills.as_ref()),
            mcp_servers: resolve_manifest_mcp_servers(plugin_root, mcp_servers),
            apps: resolve_manifest_path(plugin_root, "apps", apps.as_deref()),
            hooks: resolve_manifest_hooks(plugin_root, hooks),
            context,
        },
        interface,
    })
}

fn resolve_manifest_extensions(
    plugin_root: &PathUri,
    extensions: Option<JsonValue>,
) -> Vec<xedoc_plugin::manifest::PluginManifestExtension<PathUri>> {
    let Some(extensions) = extensions else {
        return Vec::new();
    };
    let Some(extensions) = extensions.as_array() else {
        tracing::warn!("ignoring extensions: expected an array");
        return Vec::new();
    };

    let mut seen_ids = HashSet::new();
    extensions
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let field = format!("extensions[{index}]");
            let Some(extension) = value.as_object() else {
                tracing::warn!("ignoring {field}: extension must be an object");
                return None;
            };
            let id = extension.get("id").and_then(JsonValue::as_str)?.trim();
            if !is_valid_extension_identifier(id, MAX_EXTENSION_ID_LEN) {
                tracing::warn!("ignoring {field}: id must be a non-empty stable identifier");
                return None;
            }
            if seen_ids.contains(id) {
                tracing::warn!("ignoring {field}: id must be unique");
                return None;
            }
            let entrypoint = extension
                .get("entrypoint")
                .and_then(JsonValue::as_str)
                .map(str::trim);
            let Some(entrypoint) = entrypoint else {
                tracing::warn!("ignoring {field}: entrypoint must be a string");
                return None;
            };
            let Some(entrypoint) =
                resolve_manifest_path(plugin_root, "extensions[].entrypoint", Some(entrypoint))
            else {
                return None;
            };
            let Some(commands) = parse_manifest_extension_commands(extension, &field) else {
                return None;
            };
            let Some(requested_capabilities) =
                parse_manifest_extension_capabilities(extension, &field)
            else {
                return None;
            };
            let Some(approval_response_timeout_ms) =
                parse_manifest_extension_approval_response_timeout(extension, &field)
            else {
                return None;
            };
            seen_ids.insert(id.to_string());
            Some(xedoc_plugin::manifest::PluginManifestExtension {
                id: id.to_string(),
                entrypoint,
                commands,
                requested_capabilities,
                approval_response_timeout_ms,
            })
        })
        .collect()
}

fn parse_manifest_extension_commands(
    extension: &serde_json::Map<String, JsonValue>,
    field: &str,
) -> Option<Vec<xedoc_plugin::manifest::PluginManifestExtensionCommand>> {
    let Some(value) = extension.get("commands") else {
        return Some(Vec::new());
    };
    let Some(commands) = value.as_array() else {
        tracing::warn!("ignoring {field}: commands must be an array");
        return None;
    };
    commands
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let command_field = format!("{field}.commands[{index}]");
            let Some(command) = value.as_object() else {
                tracing::warn!("ignoring {command_field}: command must be an object");
                return None;
            };
            let Some(name) = command.get("name").and_then(JsonValue::as_str).map(str::trim)
            else {
                tracing::warn!("ignoring {command_field}: command name must be a string");
                return None;
            };
            if !is_valid_extension_identifier(name, MAX_EXTENSION_COMMAND_NAME_LEN) {
                tracing::warn!(
                    "ignoring {command_field}: command name must be a non-empty slash-command identifier"
                );
                return None;
            }
            let Some(description) = command
                .get("description")
                .and_then(JsonValue::as_str)
                .map(str::trim)
            else {
                tracing::warn!("ignoring {command_field}: command description must be a string");
                return None;
            };
            if description.is_empty()
                || description.chars().count() > MAX_EXTENSION_DESCRIPTION_LEN
            {
                tracing::warn!(
                    "ignoring {command_field}: command description must be non-empty and at most {} characters",
                    MAX_EXTENSION_DESCRIPTION_LEN
                );
                return None;
            }
            Some(xedoc_plugin::manifest::PluginManifestExtensionCommand {
                name: name.to_string(),
                description: description.to_string(),
            })
        })
        .collect()
}

fn parse_manifest_extension_capabilities(
    extension: &serde_json::Map<String, JsonValue>,
    field: &str,
) -> Option<Vec<String>> {
    let Some(value) = extension.get("requestedCapabilities") else {
        return Some(Vec::new());
    };
    let Some(capabilities) = value.as_array() else {
        tracing::warn!("ignoring {field}: requestedCapabilities must be an array");
        return None;
    };
    capabilities
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let Some(capability) = value.as_str().map(str::trim) else {
                tracing::warn!(
                    "ignoring {field}: requestedCapabilities[{index}] must be a string"
                );
                return None;
            };
            if capability.is_empty()
                || capability.chars().count() > MAX_EXTENSION_CAPABILITY_LEN
                || capability
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control())
            {
                tracing::warn!(
                    "ignoring {field}: requestedCapabilities[{index}] must be a non-empty identifier"
                );
                return None;
            }
            Some(capability.to_string())
        })
        .collect()
}

fn parse_manifest_extension_approval_response_timeout(
    extension: &serde_json::Map<String, JsonValue>,
    field: &str,
) -> Option<u64> {
    let Some(value) = extension.get("approvalResponseTimeoutMs") else {
        return Some(DEFAULT_EXTENSION_APPROVAL_RESPONSE_TIMEOUT_MS);
    };
    let Some(timeout_ms) = value.as_u64() else {
        tracing::warn!("ignoring {field}: approvalResponseTimeoutMs must be an integer");
        return None;
    };
    if !(MIN_EXTENSION_APPROVAL_RESPONSE_TIMEOUT_MS..=MAX_EXTENSION_APPROVAL_RESPONSE_TIMEOUT_MS)
        .contains(&timeout_ms)
    {
        tracing::warn!(
            "ignoring {field}: approvalResponseTimeoutMs must be between {} and {}",
            MIN_EXTENSION_APPROVAL_RESPONSE_TIMEOUT_MS,
            MAX_EXTENSION_APPROVAL_RESPONSE_TIMEOUT_MS
        );
        return None;
    }
    Some(timeout_ms)
}

fn is_valid_extension_identifier(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.chars().count() <= max_len
        && value.chars().enumerate().all(|(index, character)| {
            character.is_ascii_alphanumeric() || (index > 0 && matches!(character, '-' | '_'))
        })
}

fn convert_thread_context_entry(
    raw: RawPluginThreadContextEntry,
) -> Option<xedoc_plugin::manifest::PluginThreadContextEntry> {
    let slot = match raw.slot.as_str() {
        "developer_policy" => xedoc_plugin::manifest::PluginContextSlot::DeveloperPolicy,
        "developer_capabilities" => {
            xedoc_plugin::manifest::PluginContextSlot::DeveloperCapabilities
        }
        "contextual_user" => xedoc_plugin::manifest::PluginContextSlot::ContextualUser,
        "separate_developer" => xedoc_plugin::manifest::PluginContextSlot::SeparateDeveloper,
        _ => {
            tracing::warn!(slot = %raw.slot, "unknown plugin context slot; skipping entry");
            return None;
        }
    };
    let position = match raw.position.as_str() {
        "preamble" => xedoc_plugin::manifest::PluginContextPosition::Preamble,
        "supplement" => xedoc_plugin::manifest::PluginContextPosition::Supplement,
        _ => {
            tracing::warn!(position = %raw.position, "unknown plugin context position; skipping entry");
            return None;
        }
    };
    let text = raw.text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() > MAX_THREAD_CONTEXT_ENTRY_LEN {
        tracing::warn!(
            length = text.chars().count(),
            max_length = MAX_THREAD_CONTEXT_ENTRY_LEN,
            "plugin thread context entry is too long; skipping entry"
        );
        return None;
    }
    let condition_shell = raw
        .condition_shell
        .map(|condition| condition.trim().to_string());
    if condition_shell.as_ref().is_some_and(String::is_empty) {
        tracing::warn!("plugin thread context condition is empty; skipping entry");
        return None;
    }
    if condition_shell
        .as_ref()
        .is_some_and(|condition| condition.chars().count() > MAX_THREAD_CONTEXT_CONDITION_SHELL_LEN)
    {
        tracing::warn!(
            max_length = MAX_THREAD_CONTEXT_CONDITION_SHELL_LEN,
            "plugin thread context condition is too long; skipping entry"
        );
        return None;
    }
    Some(xedoc_plugin::manifest::PluginThreadContextEntry {
        slot,
        position,
        text,
        condition_shell,
    })
}

fn resolve_manifest_hooks(
    plugin_root: &PathUri,
    hooks: Option<RawPluginManifestHooks>,
) -> Option<xedoc_plugin::manifest::PluginManifestHooks<PathUri>> {
    match hooks? {
        RawPluginManifestHooks::Path(path) => {
            resolve_manifest_path(plugin_root, "hooks", Some(&path))
                .map(|path| xedoc_plugin::manifest::PluginManifestHooks::Paths(vec![path]))
        }
        RawPluginManifestHooks::Paths(paths) => {
            let hooks = paths
                .iter()
                .filter_map(|path| resolve_manifest_path(plugin_root, "hooks", Some(path)))
                .collect::<Vec<_>>();
            (!hooks.is_empty()).then_some(xedoc_plugin::manifest::PluginManifestHooks::Paths(hooks))
        }
        RawPluginManifestHooks::Inline(hooks) => {
            Some(xedoc_plugin::manifest::PluginManifestHooks::Inline(vec![
                *hooks,
            ]))
        }
        RawPluginManifestHooks::InlineList(hooks) => (!hooks.is_empty())
            .then_some(xedoc_plugin::manifest::PluginManifestHooks::Inline(hooks)),
        RawPluginManifestHooks::Invalid(value) => {
            tracing::warn!(
                "ignoring hooks: expected a string, string array, object, or object array; found {}",
                json_value_type(&value)
            );
            None
        }
    }
}

fn resolve_manifest_mcp_servers(
    plugin_root: &PathUri,
    mcp_servers: Option<RawPluginManifestMcpServers>,
) -> Option<xedoc_plugin::manifest::PluginManifestMcpServers<PathUri>> {
    match mcp_servers? {
        RawPluginManifestMcpServers::Path(path) => {
            resolve_manifest_path(plugin_root, "mcpServers", Some(&path))
                .map(xedoc_plugin::manifest::PluginManifestMcpServers::Path)
        }
        RawPluginManifestMcpServers::Object(servers) => match serde_json::to_string(&servers) {
            Ok(servers) => Some(xedoc_plugin::manifest::PluginManifestMcpServers::Object(
                servers,
            )),
            Err(err) => {
                tracing::warn!("ignoring mcpServers: failed to serialize object: {err}");
                None
            }
        },
        RawPluginManifestMcpServers::Invalid(value) => {
            tracing::warn!(
                "ignoring mcpServers: expected a string or object; found {}",
                json_value_type(&value)
            );
            None
        }
    }
}

fn resolve_interface_asset_path(
    plugin_root: &PathUri,
    field: &'static str,
    path: Option<&str>,
) -> Option<PathUri> {
    resolve_manifest_path(plugin_root, field, path)
}

fn resolve_default_prompts(
    manifest_path: &str,
    value: Option<&RawPluginManifestDefaultPrompt>,
) -> Option<Vec<String>> {
    match value? {
        RawPluginManifestDefaultPrompt::String(prompt) => {
            resolve_default_prompt_str(manifest_path, "interface.defaultPrompt", prompt)
                .map(|prompt| vec![prompt])
        }
        RawPluginManifestDefaultPrompt::List(values) => {
            let mut prompts = Vec::new();
            for (index, item) in values.iter().enumerate() {
                if prompts.len() >= MAX_DEFAULT_PROMPT_COUNT {
                    warn_invalid_default_prompt(
                        manifest_path,
                        "interface.defaultPrompt",
                        &format!("maximum of {MAX_DEFAULT_PROMPT_COUNT} prompts is supported"),
                    );
                    break;
                }

                match item {
                    RawPluginManifestDefaultPromptEntry::String(prompt) => {
                        let field = format!("interface.defaultPrompt[{index}]");
                        if let Some(prompt) =
                            resolve_default_prompt_str(manifest_path, &field, prompt)
                        {
                            prompts.push(prompt);
                        }
                    }
                    RawPluginManifestDefaultPromptEntry::Invalid(value) => {
                        let field = format!("interface.defaultPrompt[{index}]");
                        warn_invalid_default_prompt(
                            manifest_path,
                            &field,
                            &format!("expected a string, found {}", json_value_type(value)),
                        );
                    }
                }
            }

            (!prompts.is_empty()).then_some(prompts)
        }
        RawPluginManifestDefaultPrompt::Invalid(value) => {
            warn_invalid_default_prompt(
                manifest_path,
                "interface.defaultPrompt",
                &format!(
                    "expected a string or array of strings, found {}",
                    json_value_type(value)
                ),
            );
            None
        }
    }
}

fn resolve_default_prompt_str(manifest_path: &str, field: &str, prompt: &str) -> Option<String> {
    let prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if prompt.is_empty() {
        warn_invalid_default_prompt(manifest_path, field, "prompt must not be empty");
        return None;
    }
    if prompt.chars().count() > MAX_DEFAULT_PROMPT_LEN {
        warn_invalid_default_prompt(
            manifest_path,
            field,
            &format!("prompt must be at most {MAX_DEFAULT_PROMPT_LEN} characters"),
        );
        return None;
    }
    Some(prompt)
}

fn warn_invalid_default_prompt(manifest_path: &str, field: &str, message: &str) {
    tracing::warn!(path = %manifest_path, "ignoring {field}: {message}");
}

fn json_value_type(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

fn resolve_manifest_paths(
    plugin_root: &PathUri,
    field: &'static str,
    paths: Option<&RawPluginManifestPaths>,
) -> Vec<PathUri> {
    match paths {
        Some(RawPluginManifestPaths::Path(path)) => {
            resolve_manifest_path(plugin_root, field, Some(path))
                .map(|path| vec![path])
                .unwrap_or_default()
        }
        Some(RawPluginManifestPaths::Paths(paths)) => paths
            .iter()
            .filter_map(|path| resolve_manifest_path(plugin_root, field, Some(path)))
            .collect(),
        Some(RawPluginManifestPaths::Invalid(value)) => {
            tracing::warn!(
                "ignoring {field}: expected a string or string array; found {}",
                json_value_type(value)
            );
            Vec::new()
        }
        None => Vec::new(),
    }
}

fn resolve_manifest_path(
    plugin_root: &PathUri,
    field: &'static str,
    path: Option<&str>,
) -> Option<PathUri> {
    let path = path?;
    if path.is_empty() {
        return None;
    }
    let Some(relative_path) = path.strip_prefix("./") else {
        tracing::warn!("ignoring {field}: path must start with `./` relative to plugin root");
        return None;
    };
    if relative_path.is_empty() {
        tracing::warn!("ignoring {field}: path must not be `./`");
        return None;
    }

    let convention = plugin_root.infer_path_convention();
    let has_parent_component = match convention {
        Some(PathConvention::Windows) => relative_path
            .split(['/', '\\'])
            .any(|component| component == ".."),
        Some(PathConvention::Posix) | None => {
            relative_path.split('/').any(|component| component == "..")
        }
    };
    if has_parent_component {
        tracing::warn!("ignoring {field}: path must not contain '..'");
        return None;
    }

    let has_windows_root = convention == Some(PathConvention::Windows)
        && (relative_path.starts_with('\\')
            || matches!(relative_path.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic()));
    if relative_path.starts_with('/') || has_windows_root {
        tracing::warn!("ignoring {field}: path must stay within the plugin root");
        return None;
    }

    let resolved = match plugin_root.join(relative_path) {
        Ok(resolved) => resolved,
        Err(err) => {
            tracing::warn!("ignoring {field}: path must resolve under plugin root: {err}");
            return None;
        }
    };
    if !resolved.starts_with(plugin_root) {
        tracing::warn!("ignoring {field}: path must stay within the plugin root");
        return None;
    }
    Some(resolved)
}
