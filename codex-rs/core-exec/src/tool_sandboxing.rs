//! Tool sandbox policy selection and command materialization.

use crate::approval::ExecApprovalRequirement;
use crate::sandboxing::ExecOptions;
use crate::sandboxing::ExecRequest;
use crate::sandboxing::SandboxPermissions;
use codex_file_system::FileSystemSandboxContext;
use codex_network_proxy::NetworkProxy;
use codex_protocol::error::CodexErr;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxTransformRequest;
use codex_sandboxing::SandboxType;
use codex_sandboxing::SandboxablePreference;
use codex_sandboxing::policy_transforms::effective_permission_profile;
use codex_utils_path_uri::PathUri;
use tokio_util::sync::CancellationToken;

/// Resolves the default approval policy for executable tool calls.
#[doc(hidden)]
pub fn default_exec_approval_requirement(
    policy: AskForApproval,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
) -> ExecApprovalRequirement {
    let needs_approval = match policy {
        AskForApproval::Never => false,
        AskForApproval::OnRequest | AskForApproval::Granular(_) => {
            matches!(
                file_system_sandbox_policy.kind,
                FileSystemSandboxKind::Restricted
            )
        }
        AskForApproval::UnlessTrusted => true,
    };

    if needs_approval
        && matches!(
            policy,
            AskForApproval::Granular(granular_config)
                if !granular_config.allows_sandbox_approval()
        )
    {
        ExecApprovalRequirement::Forbidden {
            reason: "approval policy disallowed sandbox approval prompt".to_string(),
        }
    } else if needs_approval {
        ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        }
    } else {
        ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        }
    }
}

/// Overrides the default first sandbox attempt for an executable tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum SandboxOverride {
    /// Keep the tool runtime's default sandbox choice.
    NoOverride,
    /// Bypass the sandbox for the initial execution attempt.
    BypassSandboxFirstAttempt,
}

/// Selects whether an executable tool should bypass its first sandbox attempt.
#[doc(hidden)]
pub fn sandbox_override_for_first_attempt(
    sandbox_permissions: SandboxPermissions,
    exec_approval_requirement: &ExecApprovalRequirement,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
) -> SandboxOverride {
    if !unsandboxed_execution_allowed(file_system_sandbox_policy) {
        return SandboxOverride::NoOverride;
    }

    if matches!(
        exec_approval_requirement,
        ExecApprovalRequirement::Skip {
            bypass_sandbox: true,
            ..
        }
    ) {
        return SandboxOverride::BypassSandboxFirstAttempt;
    }

    if sandbox_permissions.requires_escalated_permissions() {
        SandboxOverride::BypassSandboxFirstAttempt
    } else {
        SandboxOverride::NoOverride
    }
}

/// Reports whether the active filesystem policy permits an unsandboxed execution.
#[doc(hidden)]
pub fn unsandboxed_execution_allowed(file_system_sandbox_policy: &FileSystemSandboxPolicy) -> bool {
    !file_system_sandbox_policy.has_denied_read_restrictions()
}

/// Retains deny-read restrictions when an execution request is escalated.
#[doc(hidden)]
pub fn sandbox_permissions_preserving_denied_reads(
    sandbox_permissions: SandboxPermissions,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
) -> SandboxPermissions {
    if sandbox_permissions.requires_escalated_permissions()
        && !unsandboxed_execution_allowed(file_system_sandbox_policy)
    {
        SandboxPermissions::UseDefault
    } else {
        sandbox_permissions
    }
}

/// Suppresses managed network proxying for escalated execution.
#[doc(hidden)]
pub fn managed_network_for_sandbox_permissions(
    network: Option<&NetworkProxy>,
    sandbox_permissions: SandboxPermissions,
) -> Option<&NetworkProxy> {
    if sandbox_permissions.requires_escalated_permissions() {
        None
    } else {
        network
    }
}

/// Declares how a tool runtime interacts with the sandbox.
#[doc(hidden)]
pub trait Sandboxable {
    /// Chooses the sandbox implementation preferred by the tool runtime.
    fn sandbox_preference(&self) -> SandboxablePreference;

    /// Indicates whether the runtime may retry after a sandbox failure.
    fn escalate_on_failure(&self) -> bool {
        true
    }
}

/// Describes a tool execution failure.
#[derive(Debug)]
#[doc(hidden)]
pub enum ToolError {
    /// The request was rejected before execution.
    Rejected(String),
    /// Codex failed while preparing or running the request.
    Codex(CodexErr),
}

/// Materialized sandbox state for one execution attempt.
#[doc(hidden)]
pub struct SandboxAttempt<'a> {
    /// Selected sandbox implementation.
    pub sandbox: SandboxType,
    /// Whether policy requested sandboxing, independent of the host wrapper.
    pub sandbox_requested: bool,
    /// Permission profile materialized for the host execution environment.
    pub permissions: &'a codex_protocol::models::PermissionProfile,
    /// Canonical permissions before this host materializes workspace roots.
    pub exec_server_permissions: &'a codex_protocol::models::PermissionProfile,
    /// Whether the managed-network policy must be enforced.
    pub enforce_managed_network: bool,
    /// Sandbox transformer for the active execution environment.
    pub manager: &'a SandboxManager,
    /// Current sandbox working directory.
    pub sandbox_cwd: &'a PathUri,
    /// Workspace roots available to the sandbox.
    pub workspace_roots: &'a [PathUri],
    /// Optional Linux sandbox executable.
    pub codex_linux_sandbox_exe: Option<&'a std::path::PathBuf>,
    /// Whether legacy Landlock behavior is enabled.
    pub use_legacy_landlock: bool,
    /// Cancellation token for managed-network denials.
    pub network_denial_cancellation_token: Option<CancellationToken>,
    /// Managed network proxy configured for this sandbox attempt.
    pub network_proxy: Option<&'a NetworkProxy>,
}

impl<'a> SandboxAttempt<'a> {
    /// Selects the attempt-specific proxy or the supplied fallback.
    #[doc(hidden)]
    pub fn network_proxy<'b>(
        &'b self,
        fallback: Option<&'b NetworkProxy>,
    ) -> Option<&'b NetworkProxy> {
        fallback.map(|fallback| self.network_proxy.unwrap_or(fallback))
    }

    /// Builds a local execution request from a sandbox command.
    #[doc(hidden)]
    pub fn env_for(
        &self,
        command: SandboxCommand,
        options: ExecOptions,
        network: Option<&NetworkProxy>,
        environment_id: Option<&str>,
    ) -> Result<ExecRequest, CodexErr> {
        let network = self.network_proxy(network);
        let request = self
            .manager
            .transform(SandboxTransformRequest {
                command,
                permissions: self.permissions,
                sandbox: self.sandbox,
                enforce_managed_network: self.enforce_managed_network,
                environment_id,
                network,
                sandbox_policy_cwd: self.sandbox_cwd,
                codex_linux_sandbox_exe: self
                    .codex_linux_sandbox_exe
                    .map(std::path::PathBuf::as_path),
                use_legacy_landlock: self.use_legacy_landlock,
            })
            .map_err(CodexErr::from)?;
        Ok(ExecRequest::from_sandbox_exec_request(request, options))
    }

    /// Builds a remote execution request from a sandbox command.
    #[doc(hidden)]
    pub fn env_for_exec_server(
        &self,
        command: SandboxCommand,
        options: ExecOptions,
    ) -> Result<ExecRequest, CodexErr> {
        let managed_network = command.managed_network.clone();
        let exec_server_permissions = effective_permission_profile(
            self.exec_server_permissions,
            command.additional_permissions.as_ref(),
        );
        let request = self
            .manager
            .transform(SandboxTransformRequest {
                command,
                permissions: self.permissions,
                sandbox: SandboxType::None,
                enforce_managed_network: self.enforce_managed_network,
                environment_id: None,
                network: None,
                sandbox_policy_cwd: self.sandbox_cwd,
                codex_linux_sandbox_exe: None,
                use_legacy_landlock: self.use_legacy_landlock,
            })
            .map_err(CodexErr::from)?;
        let mut exec_request = ExecRequest::from_sandbox_exec_request(request, options);
        exec_request.exec_server_managed_network = managed_network;
        if self.sandbox_requested {
            exec_request.exec_server_sandbox = Some(FileSystemSandboxContext {
                permissions: exec_server_permissions.into(),
                cwd: Some(exec_request.sandbox_policy_cwd.clone()),
                workspace_roots: self.workspace_roots.to_vec(),
                use_legacy_landlock: self.use_legacy_landlock,
            });
            exec_request.exec_server_enforce_managed_network = self.enforce_managed_network;
        }
        Ok(exec_request)
    }
}

#[cfg(test)]
#[path = "tool_sandboxing_tests.rs"]
mod tests;
