use codex_config::config_toml::ProjectConfig;

pub(crate) use codex_core_config_runtime::permissions::BUILT_IN_DANGER_FULL_ACCESS_PROFILE;
pub(crate) use codex_core_config_runtime::permissions::BUILT_IN_READ_ONLY_PROFILE;
pub(crate) use codex_core_config_runtime::permissions::BUILT_IN_WORKSPACE_PROFILE;
pub(crate) use codex_core_config_runtime::permissions::apply_network_proxy_feature_config;
pub(crate) use codex_core_config_runtime::permissions::builtin_permission_profile;
pub(crate) use codex_core_config_runtime::permissions::compile_permission_profile_selection;
pub(crate) use codex_core_config_runtime::permissions::compile_permission_profile_workspace_roots;
pub(crate) use codex_core_config_runtime::permissions::get_readable_roots_required_for_codex_runtime;
pub(crate) use codex_core_config_runtime::permissions::is_builtin_permission_profile_name;
pub(crate) use codex_core_config_runtime::permissions::network_proxy_config_for_profile_selection;
pub(crate) use codex_core_config_runtime::permissions::validate_user_permission_profile_names;

pub(crate) fn default_builtin_permission_profile_name(
    active_project: &ProjectConfig,
) -> &'static str {
    if (active_project.is_trusted() || active_project.is_untrusted())
        && !cfg!(target_os = "windows")
    {
        BUILT_IN_WORKSPACE_PROFILE
    } else {
        BUILT_IN_READ_ONLY_PROFILE
    }
}

#[cfg(test)]
#[path = "permissions_config_tests.rs"]
mod tests;
