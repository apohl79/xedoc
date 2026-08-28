use super::*;

use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use codex_network_proxy::ManagedNetworkSandboxContext;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::protocol::GranularApprovalConfig;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxManager;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::collections::HashMap;

#[test]
fn external_sandbox_skips_exec_approval_on_request() {
    assert_eq!(
        default_exec_approval_requirement(
            AskForApproval::OnRequest,
            &FileSystemSandboxPolicy::external_sandbox(),
        ),
        ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        }
    );
}

#[test]
fn restricted_sandbox_requires_exec_approval_on_request() {
    assert_eq!(
        default_exec_approval_requirement(
            AskForApproval::OnRequest,
            &FileSystemSandboxPolicy::default(),
        ),
        ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        }
    );
}

#[test]
fn granular_policy_without_sandbox_approval_is_forbidden() {
    let policy = AskForApproval::Granular(GranularApprovalConfig {
        sandbox_approval: false,
        rules: true,
        skill_approval: true,
        request_permissions: true,
        mcp_elicitations: true,
    });

    assert_eq!(
        default_exec_approval_requirement(policy, &FileSystemSandboxPolicy::default()),
        ExecApprovalRequirement::Forbidden {
            reason: "approval policy disallowed sandbox approval prompt".to_string(),
        }
    );
}

#[test]
fn granular_policy_with_sandbox_approval_requires_approval() {
    let policy = AskForApproval::Granular(GranularApprovalConfig {
        sandbox_approval: true,
        rules: false,
        skill_approval: true,
        request_permissions: true,
        mcp_elicitations: false,
    });

    assert_eq!(
        default_exec_approval_requirement(policy, &FileSystemSandboxPolicy::default()),
        ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        }
    );
}

#[test]
fn execpolicy_skip_bypasses_the_initial_sandbox_attempt() {
    assert_eq!(
        sandbox_override_for_first_attempt(
            SandboxPermissions::WithAdditionalPermissions,
            &ExecApprovalRequirement::Skip {
                bypass_sandbox: true,
                proposed_execpolicy_amendment: None,
            },
            &FileSystemSandboxPolicy::default(),
        ),
        SandboxOverride::BypassSandboxFirstAttempt
    );
}

#[test]
fn explicit_escalation_bypasses_the_initial_sandbox_attempt() {
    assert_eq!(
        sandbox_override_for_first_attempt(
            SandboxPermissions::RequireEscalated,
            &ExecApprovalRequirement::Skip {
                bypass_sandbox: false,
                proposed_execpolicy_amendment: None,
            },
            &FileSystemSandboxPolicy::default(),
        ),
        SandboxOverride::BypassSandboxFirstAttempt
    );
}

#[test]
fn deny_read_restrictions_preserve_the_sandbox_for_escalated_requests() {
    let file_system_policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::GlobPattern {
            pattern: "**/*.env".to_string(),
        },
        access: FileSystemAccessMode::Deny,
    }]);

    assert_eq!(
        (
            sandbox_override_for_first_attempt(
                SandboxPermissions::RequireEscalated,
                &ExecApprovalRequirement::Skip {
                    bypass_sandbox: false,
                    proposed_execpolicy_amendment: None,
                },
                &file_system_policy,
            ),
            unsandboxed_execution_allowed(&file_system_policy),
            sandbox_permissions_preserving_denied_reads(
                SandboxPermissions::RequireEscalated,
                &file_system_policy,
            ),
            sandbox_permissions_preserving_denied_reads(
                SandboxPermissions::WithAdditionalPermissions,
                &file_system_policy,
            ),
            sandbox_permissions_preserving_denied_reads(
                SandboxPermissions::RequireEscalated,
                &FileSystemSandboxPolicy::default(),
            ),
            sandbox_override_for_first_attempt(
                SandboxPermissions::WithAdditionalPermissions,
                &ExecApprovalRequirement::Skip {
                    bypass_sandbox: true,
                    proposed_execpolicy_amendment: None,
                },
                &file_system_policy,
            ),
        ),
        (
            SandboxOverride::NoOverride,
            false,
            SandboxPermissions::UseDefault,
            SandboxPermissions::WithAdditionalPermissions,
            SandboxPermissions::RequireEscalated,
            SandboxOverride::NoOverride,
        )
    );
}

#[test]
fn exec_server_materialization_keeps_command_native_and_carries_sandbox_context() {
    let cwd: AbsolutePathBuf = std::env::current_dir()
        .expect("current dir")
        .try_into()
        .expect("absolute cwd");
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let exec_server_permissions = codex_protocol::models::PermissionProfile::workspace_write();
    let permissions = exec_server_permissions
        .clone()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
    let manager = SandboxManager::new();
    let mut attempt = SandboxAttempt {
        sandbox: SandboxType::None,
        sandbox_requested: true,
        permissions: &permissions,
        exec_server_permissions: &exec_server_permissions,
        enforce_managed_network: true,
        manager: &manager,
        sandbox_cwd: &cwd_uri,
        workspace_roots: std::slice::from_ref(&cwd_uri),
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        windows_sandbox_level: codex_protocol::config_types::WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };
    let managed_network = ManagedNetworkSandboxContext {
        loopback_ports: vec![43123],
        allow_local_binding: false,
    };
    let command = || SandboxCommand {
        program: "/bin/bash".into(),
        args: vec!["-lc".to_string(), "pwd".to_string()],
        cwd: cwd_uri.clone(),
        env: HashMap::new(),
        managed_network: Some(managed_network.clone()),
        additional_permissions: None,
    };
    let options = || ExecOptions {
        expiration: ExecExpiration::DefaultTimeout,
        capture_policy: ExecCapturePolicy::ShellTool,
    };
    let sandboxed = attempt
        .env_for_exec_server(command(), options())
        .expect("prepare sandboxed remote exec request");
    attempt.sandbox_requested = false;
    let unsandboxed = attempt
        .env_for_exec_server(command(), options())
        .expect("prepare unsandboxed remote exec request");

    assert_eq!(
        (
            sandboxed.command,
            sandboxed.arg0,
            sandboxed.sandbox,
            sandboxed.exec_server_sandbox,
            sandboxed.exec_server_enforce_managed_network,
            sandboxed.exec_server_managed_network,
            unsandboxed.exec_server_sandbox,
            unsandboxed.exec_server_enforce_managed_network,
            unsandboxed.exec_server_managed_network,
        ),
        (
            vec![
                "/bin/bash".to_string(),
                "-lc".to_string(),
                "pwd".to_string()
            ],
            None,
            SandboxType::None,
            Some(FileSystemSandboxContext {
                permissions: exec_server_permissions.into(),
                cwd: Some(cwd_uri),
                workspace_roots: vec![PathUri::from_abs_path(&cwd)],
                windows_sandbox_level: codex_protocol::config_types::WindowsSandboxLevel::Disabled,
                windows_sandbox_private_desktop: false,
                use_legacy_landlock: false,
            }),
            true,
            Some(managed_network.clone()),
            None,
            false,
            Some(managed_network),
        )
    );
}
