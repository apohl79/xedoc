//! Compatibility projections from the canonical permission profile model into
//! legacy shapes still required by older or remote app-server APIs.

use std::path::Path;
use xedoc_protocol::models::PermissionProfile;
use xedoc_utils_absolute_path::AbsolutePathBuf;

pub(crate) fn legacy_compatible_permission_profile(
    permission_profile: &PermissionProfile,
    cwd: &Path,
) -> PermissionProfile {
    if permission_profile.to_legacy_sandbox_policy(cwd).is_ok() {
        return permission_profile.clone();
    }

    let file_system_policy = permission_profile.file_system_sandbox_policy();
    let network_policy = permission_profile.network_sandbox_policy();
    let cwd_abs = AbsolutePathBuf::from_absolute_path(cwd).ok();
    let writable_roots = file_system_policy
        .get_writable_roots_with_cwd(cwd)
        .into_iter()
        .map(|root| root.root)
        .filter(|root| cwd_abs.as_ref() != Some(root))
        .collect::<Vec<_>>();
    let tmpdir_writable = std::env::var_os("TMPDIR")
        .filter(|tmpdir| !tmpdir.is_empty())
        .and_then(|tmpdir| {
            AbsolutePathBuf::from_absolute_path(std::path::PathBuf::from(tmpdir)).ok()
        })
        .is_some_and(|tmpdir| file_system_policy.can_write_path_with_cwd(tmpdir.as_path(), cwd));
    let slash_tmp = Path::new("/tmp");
    let slash_tmp_writable = slash_tmp.is_absolute()
        && slash_tmp.is_dir()
        && file_system_policy.can_write_path_with_cwd(slash_tmp, cwd);

    PermissionProfile::workspace_write_with(
        &writable_roots,
        network_policy,
        !tmpdir_writable,
        !slash_tmp_writable,
    )
}
