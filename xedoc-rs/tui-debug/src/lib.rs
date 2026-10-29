//! Rendering for the TUI's configuration diagnostics.

#![deny(clippy::print_stdout, clippy::print_stderr)]

use ratatui::style::Stylize;
use ratatui::text::Line;
use toml::Value as TomlValue;
use xedoc_config::CONFIG_TOML_FILE;
use xedoc_config::ConfigLayerEntry;
use xedoc_config::ConfigLayerSource;
use xedoc_config::ConfigLayerStack;
use xedoc_config::ConfigLayerStackOrdering;
use xedoc_config::ManagedHooksRequirementsToml;
use xedoc_config::NetworkConstraints;
use xedoc_config::NetworkDomainPermissionToml;
use xedoc_config::NetworkUnixSocketPermissionToml;
use xedoc_config::RequirementSource;
use xedoc_config::ResidencyRequirement;
use xedoc_config::SandboxModeRequirement;
use xedoc_config::WebSearchModeRequirement;
use xedoc_config::format_config_layer_source;
use xedoc_core_config::config::Config;
use xedoc_core_config::config::Permissions;
use xedoc_protocol::models::PermissionProfile;
use xedoc_protocol::permissions::NetworkSandboxPolicy;
use xedoc_tui_render::city_lights::CityLightsStylize;
use xedoc_tui_transcript::history_cell::PlainHistoryCell;
use xedoc_tui_transcript::session_state::SessionNetworkProxyRuntime;

pub fn new_debug_config_output(
    config: &Config,
    session_network_proxy: Option<&SessionNetworkProxyRuntime>,
) -> PlainHistoryCell {
    let mut lines = render_debug_config_lines(&config.config_layer_stack, |mode| {
        sandbox_mode_is_allowed_by_permissions(&config.permissions, mode)
    });
    lines.extend(render_agents_config_lines(config));

    if let Some(proxy) = session_network_proxy {
        lines.push("".into());
        lines.push("Session runtime:".bold().into());
        lines.push("  - network_proxy".into());
        let SessionNetworkProxyRuntime {
            http_addr,
            socks_addr,
        } = proxy;
        let all_proxy = session_all_proxy_url(
            http_addr,
            socks_addr,
            config
                .permissions
                .network
                .as_ref()
                .is_some_and(xedoc_core_config::config::NetworkProxySpec::socks_enabled),
        );
        lines.push(format!("    - HTTP_PROXY  = http://{http_addr}").into());
        lines.push(format!("    - ALL_PROXY   = {all_proxy}").into());
    }

    PlainHistoryCell::new(lines)
}

fn render_agents_config_lines(config: &Config) -> Vec<Line<'static>> {
    vec![
        "".into(),
        "[agents]:".bold().into(),
        format!("  - enabled = {}", config.agents_enabled).into(),
        format!(
            "  - max_concurrent_threads_per_session = {}",
            format_optional(config.agent_max_threads)
        )
        .into(),
        format!(
            "  - max_depth = {} (V1 only; ignored by V2)",
            config.agent_max_depth
        )
        .into(),
        format!(
            "  - default_subagent_model = {}",
            format_optional(config.agent_default_subagent_model.as_deref())
        )
        .into(),
        format!(
            "  - default_subagent_reasoning_effort = {}",
            format_optional(config.agent_default_subagent_reasoning_effort.as_ref())
        )
        .into(),
        format!(
            "  - interrupt_message = {}",
            config.agent_interrupt_message_enabled
        )
        .into(),
    ]
}

fn format_optional(value: Option<impl std::fmt::Display>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "<unset>".to_string())
}

fn sandbox_mode_is_allowed_by_permissions(
    permissions: &Permissions,
    mode: SandboxModeRequirement,
) -> bool {
    let permission_profile = match mode {
        SandboxModeRequirement::ReadOnly => PermissionProfile::read_only(),
        SandboxModeRequirement::WorkspaceWrite => PermissionProfile::workspace_write(),
        SandboxModeRequirement::DangerFullAccess => PermissionProfile::Disabled,
        SandboxModeRequirement::ExternalSandbox => PermissionProfile::External {
            network: NetworkSandboxPolicy::Restricted,
        },
    };

    permissions
        .can_set_permission_profile(&permission_profile)
        .is_ok()
}

fn session_all_proxy_url(http_addr: &str, socks_addr: &str, socks_enabled: bool) -> String {
    if socks_enabled {
        format!("socks5h://{socks_addr}")
    } else {
        format!("http://{http_addr}")
    }
}

fn render_debug_config_lines(
    stack: &ConfigLayerStack,
    sandbox_mode_is_effectively_allowed: impl Fn(SandboxModeRequirement) -> bool,
) -> Vec<Line<'static>> {
    let mut lines = vec!["/debug-config".cl_magenta().into(), "".into()];

    lines.push(
        "Config layer stack (lowest precedence first):"
            .bold()
            .into(),
    );
    let layers = stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ true,
    );
    if layers.is_empty() {
        lines.push("  <none>".dim().into());
    } else {
        for (index, layer) in layers.iter().enumerate() {
            let source = format_config_layer_source(&layer.name, CONFIG_TOML_FILE);
            let status = if layer.is_disabled() {
                "disabled"
            } else {
                "enabled"
            };
            lines.push(format!("  {}. {source} ({status})", index + 1).into());
            lines.extend(render_non_file_layer_details(layer));
            if let Some(reason) = &layer.disabled_reason {
                lines.push(format!("     reason: {reason}").dim().into());
            }
        }
    }

    let requirements = stack.requirements();
    let requirements_toml = stack.requirements_toml();

    lines.push("".into());
    lines.push("Requirements:".bold().into());
    let mut requirement_lines = Vec::new();

    if let Some(policies) = requirements_toml.allowed_approval_policies.as_ref() {
        let value = join_or_empty(policies.iter().map(ToString::to_string).collect::<Vec<_>>());
        requirement_lines.push(requirement_line(
            "allowed_approval_policies",
            value,
            requirements.approval_policy.source.as_ref(),
        ));
    }

    if let Some(modes) = requirements_toml.allowed_sandbox_modes.as_ref() {
        let value = join_or_empty(
            modes
                .iter()
                .copied()
                .filter(|mode| sandbox_mode_is_effectively_allowed(*mode))
                .map(format_sandbox_mode_requirement)
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "allowed_sandbox_modes",
            value,
            requirements.permission_profile.source.as_ref(),
        ));
    }

    if let Some(modes) = requirements_toml.allowed_web_search_modes.as_ref() {
        let normalized = normalize_allowed_web_search_modes(modes);
        let value = join_or_empty(
            normalized
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "allowed_web_search_modes",
            value,
            requirements.web_search_mode.source.as_ref(),
        ));
    }

    if let Some(allow_managed_hooks_only) = requirements_toml.allow_managed_hooks_only {
        requirement_lines.push(requirement_line(
            "allow_managed_hooks_only",
            allow_managed_hooks_only.to_string(),
            requirements
                .allow_managed_hooks_only
                .as_ref()
                .map(|sourced| &sourced.source),
        ));
    }

    if let Some(allow_appshots) = requirements_toml.allow_appshots {
        requirement_lines.push(requirement_line(
            "allow_appshots",
            allow_appshots.to_string(),
            requirements
                .allow_appshots
                .as_ref()
                .map(|sourced| &sourced.source),
        ));
    }

    if let Some(feature_requirements) = requirements.feature_requirements.as_ref() {
        let value = join_or_empty(
            feature_requirements
                .value
                .entries
                .iter()
                .map(|(feature, enabled)| format!("{feature}={enabled}"))
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "features",
            value,
            Some(&feature_requirements.source),
        ));
    }

    if let Some(hooks) = requirements_toml.hooks.as_ref() {
        requirement_lines.push(requirement_line(
            "hooks",
            format_managed_hooks_requirements(hooks),
            requirements
                .managed_hooks
                .as_ref()
                .and_then(|managed_hooks| managed_hooks.source.as_ref()),
        ));
    }

    if let Some(servers) = requirements_toml.mcp_servers.as_ref() {
        let value = join_or_empty(servers.keys().cloned().collect::<Vec<_>>());
        requirement_lines.push(requirement_line(
            "mcp_servers",
            value,
            requirements
                .mcp_servers
                .as_ref()
                .map(|sourced| &sourced.source),
        ));
    }

    // TODO(gt): Expand this debug output with detailed skills and rules display.
    if requirements_toml.rules.is_some() {
        requirement_lines.push(requirement_line(
            "rules",
            "configured".to_string(),
            requirements.exec_policy_source(),
        ));
    }

    if let Some(residency) = requirements_toml.enforce_residency {
        requirement_lines.push(requirement_line(
            "enforce_residency",
            format_residency_requirement(residency),
            requirements.enforce_residency.source.as_ref(),
        ));
    }

    if let Some(network) = requirements.network.as_ref() {
        requirement_lines.push(requirement_line(
            "experimental_network",
            format_network_constraints(&network.value),
            Some(&network.source),
        ));
    }

    if let Some(filesystem) = requirements.filesystem.as_ref() {
        let deny_read = join_or_empty(
            filesystem
                .value
                .deny_read
                .iter()
                .map(|pattern| pattern.as_str().to_string())
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "permissions.filesystem.deny_read",
            deny_read,
            Some(&filesystem.source),
        ));
    }

    if requirement_lines.is_empty() {
        lines.push("  <none>".dim().into());
    } else {
        lines.extend(requirement_lines);
    }

    lines
}

fn render_non_file_layer_details(layer: &ConfigLayerEntry) -> Vec<Line<'static>> {
    match &layer.name {
        ConfigLayerSource::SessionFlags => render_session_flag_details(&layer.config),
        ConfigLayerSource::Mdm { .. }
        | ConfigLayerSource::EnterpriseManaged { .. }
        | ConfigLayerSource::LegacyManagedConfigTomlFromMdm => render_non_file_layer_value(layer),
        ConfigLayerSource::System { .. }
        | ConfigLayerSource::User { .. }
        | ConfigLayerSource::Project { .. }
        | ConfigLayerSource::LegacyManagedConfigTomlFromFile { .. } => Vec::new(),
    }
}

fn render_session_flag_details(config: &TomlValue) -> Vec<Line<'static>> {
    let mut pairs = Vec::new();
    flatten_toml_key_values(config, /*prefix*/ None, &mut pairs);

    if pairs.is_empty() {
        return vec!["     - <none>".dim().into()];
    }

    pairs
        .into_iter()
        .map(|(key, value)| format!("     - {key} = {value}").into())
        .collect()
}

fn format_managed_hooks_requirements(hooks: &ManagedHooksRequirementsToml) -> String {
    let mut parts = Vec::new();

    if let Some(managed_dir) = hooks.managed_dir.as_ref() {
        parts.push(format!("managed_dir={}", managed_dir.display()));
    }
    if let Some(windows_managed_dir) = hooks.windows_managed_dir.as_ref() {
        parts.push(format!(
            "windows_managed_dir={}",
            windows_managed_dir.display()
        ));
    }
    parts.push(format!("handlers={}", hooks.handler_count()));

    join_or_empty(parts)
}

fn render_non_file_layer_value(layer: &ConfigLayerEntry) -> Vec<Line<'static>> {
    let label = non_file_layer_value_label(&layer.name);
    let value = layer
        .raw_toml()
        .map(ToString::to_string)
        .unwrap_or_else(|| format_toml_value(&layer.config));
    if value.is_empty() {
        return vec![format!("     {label}: <empty>").dim().into()];
    }

    if value.contains('\n') {
        let mut lines = vec![format!("     {label}:").into()];
        lines.extend(value.lines().map(|line| format!("       {line}").into()));
        lines
    } else {
        vec![format!("     {label}: {value}").into()]
    }
}

fn non_file_layer_value_label(source: &ConfigLayerSource) -> &'static str {
    match source {
        ConfigLayerSource::Mdm { .. } | ConfigLayerSource::LegacyManagedConfigTomlFromMdm => {
            "MDM value"
        }
        ConfigLayerSource::EnterpriseManaged { .. } => "Enterprise-managed config value",
        ConfigLayerSource::SessionFlags
        | ConfigLayerSource::System { .. }
        | ConfigLayerSource::User { .. }
        | ConfigLayerSource::Project { .. }
        | ConfigLayerSource::LegacyManagedConfigTomlFromFile { .. } => "Layer value",
    }
}

fn flatten_toml_key_values(
    value: &TomlValue,
    prefix: Option<&str>,
    out: &mut Vec<(String, String)>,
) {
    match value {
        TomlValue::Table(table) => {
            let mut entries = table.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| key.as_str());
            for (key, child) in entries {
                let next_prefix = if let Some(prefix) = prefix {
                    format!("{prefix}.{key}")
                } else {
                    key.to_string()
                };
                flatten_toml_key_values(child, Some(&next_prefix), out);
            }
        }
        _ => {
            let key = prefix.unwrap_or("<value>").to_string();
            out.push((key, format_toml_value(value)));
        }
    }
}

fn format_toml_value(value: &TomlValue) -> String {
    value.to_string()
}

fn requirement_line(
    name: &str,
    value: String,
    source: Option<&RequirementSource>,
) -> Line<'static> {
    let source = source
        .map(ToString::to_string)
        .unwrap_or_else(|| "<unspecified>".to_string());
    format!("  - {name}: {value} (source: {source})").into()
}

fn join_or_empty(values: Vec<String>) -> String {
    if values.is_empty() {
        "<empty>".to_string()
    } else {
        values.join(", ")
    }
}

fn normalize_allowed_web_search_modes(
    modes: &[WebSearchModeRequirement],
) -> Vec<WebSearchModeRequirement> {
    if modes.is_empty() {
        return vec![WebSearchModeRequirement::Disabled];
    }

    let mut normalized = modes.to_vec();
    if !normalized.contains(&WebSearchModeRequirement::Disabled) {
        normalized.push(WebSearchModeRequirement::Disabled);
    }
    normalized
}

fn format_sandbox_mode_requirement(mode: SandboxModeRequirement) -> String {
    match mode {
        SandboxModeRequirement::ReadOnly => "read-only".to_string(),
        SandboxModeRequirement::WorkspaceWrite => "workspace-write".to_string(),
        SandboxModeRequirement::DangerFullAccess => "danger-full-access".to_string(),
        SandboxModeRequirement::ExternalSandbox => "external-sandbox".to_string(),
    }
}

fn format_residency_requirement(requirement: ResidencyRequirement) -> String {
    match requirement {
        ResidencyRequirement::Us => "us".to_string(),
    }
}

fn format_network_constraints(network: &NetworkConstraints) -> String {
    let mut parts = Vec::new();

    let NetworkConstraints {
        enabled,
        http_port,
        socks_port,
        allow_upstream_proxy,
        dangerously_allow_non_loopback_proxy,
        dangerously_allow_all_unix_sockets,
        domains,
        managed_allowed_domains_only,
        unix_sockets,
        allow_local_binding,
    } = network;

    if let Some(enabled) = enabled {
        parts.push(format!("enabled={enabled}"));
    }
    if let Some(http_port) = http_port {
        parts.push(format!("http_port={http_port}"));
    }
    if let Some(socks_port) = socks_port {
        parts.push(format!("socks_port={socks_port}"));
    }
    if let Some(allow_upstream_proxy) = allow_upstream_proxy {
        parts.push(format!("allow_upstream_proxy={allow_upstream_proxy}"));
    }
    if let Some(dangerously_allow_non_loopback_proxy) = dangerously_allow_non_loopback_proxy {
        parts.push(format!(
            "dangerously_allow_non_loopback_proxy={dangerously_allow_non_loopback_proxy}"
        ));
    }
    if let Some(dangerously_allow_all_unix_sockets) = dangerously_allow_all_unix_sockets {
        parts.push(format!(
            "dangerously_allow_all_unix_sockets={dangerously_allow_all_unix_sockets}"
        ));
    }
    if let Some(domains) = domains {
        parts.push(format!(
            "domains={}",
            format_network_permission_entries(&domains.entries, format_network_domain_permission)
        ));
    }
    if let Some(managed_allowed_domains_only) = managed_allowed_domains_only {
        parts.push(format!(
            "managed_allowed_domains_only={managed_allowed_domains_only}"
        ));
    }
    if let Some(unix_sockets) = unix_sockets {
        parts.push(format!(
            "unix_sockets={}",
            format_network_permission_entries(
                &unix_sockets.entries,
                format_network_unix_socket_permission,
            )
        ));
    }
    if let Some(allow_local_binding) = allow_local_binding {
        parts.push(format!("allow_local_binding={allow_local_binding}"));
    }

    join_or_empty(parts)
}

fn format_network_permission_entries<T: Copy>(
    entries: &std::collections::BTreeMap<String, T>,
    format_value: impl Fn(T) -> &'static str,
) -> String {
    let parts = entries
        .iter()
        .map(|(key, value)| format!("{key}={}", format_value(*value)))
        .collect::<Vec<_>>();
    format!("{{{}}}", parts.join(", "))
}

fn format_network_domain_permission(permission: NetworkDomainPermissionToml) -> &'static str {
    match permission {
        NetworkDomainPermissionToml::Allow => "allow",
        NetworkDomainPermissionToml::Deny => "deny",
    }
}

fn format_network_unix_socket_permission(
    permission: NetworkUnixSocketPermissionToml,
) -> &'static str {
    match permission {
        NetworkUnixSocketPermissionToml::Allow => "allow",
        NetworkUnixSocketPermissionToml::Deny => "deny",
    }
}
