use xedoc_protocol::models::PermissionProfile;
use xedoc_protocol::protocol::NetworkAccess;
use xedoc_protocol::protocol::SandboxPolicy;
use xedoc_utils_absolute_path::AbsolutePathBuf;

pub fn summarize_sandbox_policy(sandbox_policy: &SandboxPolicy) -> String {
    match sandbox_policy {
        SandboxPolicy::DangerFullAccess => "danger-full-access".to_string(),
        SandboxPolicy::ReadOnly { network_access, .. } => {
            let mut summary = "read-only".to_string();
            if *network_access {
                summary.push_str(" (network access enabled)");
            }
            summary
        }
        SandboxPolicy::ExternalSandbox { network_access } => {
            let mut summary = "external-sandbox".to_string();
            if matches!(network_access, NetworkAccess::Enabled) {
                summary.push_str(" (network access enabled)");
            }
            summary
        }
        SandboxPolicy::WorkspaceWrite {
            writable_roots,
            network_access,
            exclude_tmpdir_env_var,
            exclude_slash_tmp,
        } => {
            let mut summary = "workspace-write".to_string();

            let mut writable_entries = Vec::<String>::new();
            writable_entries.push("workdir".to_string());
            if !*exclude_slash_tmp {
                writable_entries.push("/tmp".to_string());
            }
            if !*exclude_tmpdir_env_var {
                writable_entries.push("$TMPDIR".to_string());
            }
            writable_entries.extend(
                writable_roots
                    .iter()
                    .map(|p| p.to_string_lossy().to_string()),
            );

            summary.push_str(&format!(" [{}]", writable_entries.join(", ")));
            if *network_access {
                summary.push_str(" (network access enabled)");
            }
            summary
        }
    }
}

pub fn summarize_permission_profile(
    permission_profile: &PermissionProfile,
    cwd: &AbsolutePathBuf,
    workspace_roots: &[AbsolutePathBuf],
) -> String {
    match permission_profile.to_legacy_sandbox_policy(cwd.as_path()) {
        Ok(SandboxPolicy::WorkspaceWrite {
            network_access,
            exclude_tmpdir_env_var,
            exclude_slash_tmp,
            ..
        }) => {
            let mut summary = "workspace-write".to_string();
            let mut writable_entries = vec!["workdir".to_string()];
            if !exclude_slash_tmp {
                writable_entries.push("/tmp".to_string());
            }
            if !exclude_tmpdir_env_var {
                writable_entries.push("$TMPDIR".to_string());
            }
            writable_entries.extend(
                workspace_roots
                    .iter()
                    .filter(|root| *root != cwd)
                    .map(|root| root.to_string_lossy().to_string()),
            );

            summary.push_str(&format!(" [{}]", writable_entries.join(", ")));
            if network_access {
                summary.push_str(" (network access enabled)");
            }
            summary
        }
        Ok(policy) => summarize_sandbox_policy(&policy),
        Err(_) => {
            if permission_profile.network_sandbox_policy().is_enabled() {
                "custom permissions (network access enabled)".to_string()
            } else {
                "custom permissions".to_string()
            }
        }
    }
}
