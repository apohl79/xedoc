use std::path::PathBuf;
use xedoc_protocol::models::PermissionProfile;

/// Returns a warning describing why `--add-dir` entries will be ignored for the
/// resolved permission profile. The caller is responsible for presenting the
/// warning to the user (for example, printing to stderr).
pub fn add_dir_warning_message(
    additional_dirs: &[PathBuf],
    permission_profile: &PermissionProfile,
    cwd: &std::path::Path,
) -> Option<String> {
    if additional_dirs.is_empty() {
        return None;
    }

    if matches!(
        permission_profile,
        PermissionProfile::Disabled | PermissionProfile::External { .. }
    ) {
        return None;
    }

    let file_system_policy = permission_profile.file_system_sandbox_policy();
    if file_system_policy.has_full_disk_write_access() {
        return None;
    }

    if file_system_policy.can_write_path_with_cwd(cwd, cwd) {
        return None;
    }

    Some(format_warning(additional_dirs))
}

fn format_warning(additional_dirs: &[PathBuf]) -> String {
    let joined_paths = additional_dirs
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Ignoring --add-dir ({joined_paths}) because the effective permissions do not allow additional writable roots. Switch to workspace-write or danger-full-access to allow them."
    )
}
