pub(crate) mod apply_patch;
pub(crate) use xedoc_core_tool_specs::apply_patch_spec;
mod current_time;
mod dynamic;
pub(crate) mod extension_tools;
mod get_context_remaining;
mod get_model_router_state;
mod mcp;
mod mcp_resource;
mod model_lookup;
pub(crate) use xedoc_core_tool_specs::mcp_resource_spec;
pub(crate) mod multi_agents;
pub(crate) mod multi_agents_common;
pub(crate) use xedoc_core_tool_specs::multi_agents_spec;
pub(crate) mod multi_agents_v2;
mod new_context_window;
mod plan;
mod request_permissions;
mod request_user_input;
mod shell;
pub(crate) use xedoc_core_tool_specs::shell_spec;
mod sleep;
mod test_sync;
mod tool_search;
pub(crate) mod unified_exec;
mod view_image;
pub(crate) use xedoc_core_tool_specs::view_image_spec;
mod wait_for_environment;

use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use std::path::Path;
use xedoc_sandboxing::policy_transforms::intersect_permission_profiles;
use xedoc_sandboxing::policy_transforms::merge_permission_profiles;
use xedoc_sandboxing::policy_transforms::normalize_additional_permissions;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_absolute_path::AbsolutePathBufGuard;

use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::function_tool::FunctionCallError;
use crate::sandboxing::SandboxPermissions;
use crate::session::session::Session;
use crate::session::turn_context::TurnEnvironment;
pub use apply_patch::ApplyPatchHandler;
pub use current_time::CurrentTimeHandler;
pub use dynamic::DynamicToolHandler;
pub use get_context_remaining::GetContextRemainingHandler;
pub use get_model_router_state::Handler as GetModelRouterStateHandler;
pub use mcp::McpHandler;
pub use mcp_resource::ListMcpResourceTemplatesHandler;
pub use mcp_resource::ListMcpResourcesHandler;
pub use mcp_resource::ReadMcpResourceHandler;
pub use model_lookup::ModelLookupHandler;
pub use new_context_window::NewContextWindowHandler;
pub use plan::PlanHandler;
pub use request_permissions::RequestPermissionsHandler;
pub use request_user_input::RequestUserInputHandler;
pub use shell::ShellCommandHandler;
pub(crate) use shell::ShellCommandHandlerOptions;
pub use sleep::SleepHandler;
pub use test_sync::TestSyncHandler;
pub(crate) use tool_search::ToolSearchHandlerCache;
pub use unified_exec::ExecCommandHandler;
pub(crate) use unified_exec::ExecCommandHandlerOptions;
pub use unified_exec::WriteStdinHandler;
pub use view_image::ViewImageHandler;
pub(crate) use wait_for_environment::WaitForEnvironmentHandler;
use xedoc_protocol::models::AdditionalPermissionProfile;
use xedoc_protocol::protocol::AskForApproval;

pub(crate) fn parse_arguments<T>(arguments: &str) -> Result<T, FunctionCallError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
    })
}

fn updated_hook_command(updated_input: &Value) -> Result<&str, FunctionCallError> {
    updated_input
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "hook returned updatedInput without string field `command`".to_string(),
            )
        })
}

fn rewrite_function_arguments(
    arguments: &str,
    tool_name: &str,
    rewrite: impl FnOnce(&mut Map<String, Value>),
) -> Result<String, FunctionCallError> {
    let mut arguments: Value = parse_arguments(arguments)?;
    let Value::Object(arguments) = &mut arguments else {
        return Err(FunctionCallError::RespondToModel(format!(
            "{tool_name} arguments must be an object"
        )));
    };
    rewrite(arguments);
    serde_json::to_string(&arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to serialize rewritten {tool_name} arguments: {err}"
        ))
    })
}

fn rewrite_function_string_argument(
    arguments: &str,
    tool_name: &str,
    field_name: &str,
    value: &str,
) -> Result<String, FunctionCallError> {
    rewrite_function_arguments(arguments, tool_name, |arguments| {
        arguments.insert(field_name.to_string(), Value::String(value.to_string()));
    })
}

fn parse_arguments_with_base_path<T>(
    arguments: &str,
    base_path: &AbsolutePathBuf,
) -> Result<T, FunctionCallError>
where
    T: for<'de> Deserialize<'de>,
{
    let _guard = AbsolutePathBufGuard::new(base_path);
    parse_arguments(arguments)
}

fn resolve_workdir_base_path(
    arguments: &str,
    default_cwd: &AbsolutePathBuf,
) -> Result<AbsolutePathBuf, FunctionCallError> {
    let arguments: Value = parse_arguments(arguments)?;
    Ok(arguments
        .get("workdir")
        .and_then(Value::as_str)
        .filter(|workdir| !workdir.is_empty())
        .map_or_else(|| default_cwd.clone(), |workdir| default_cwd.join(workdir)))
}

fn resolve_tool_environment<'a>(
    environments: &'a TurnEnvironmentSnapshot,
    environment_id: Option<&str>,
) -> Result<Option<&'a TurnEnvironment>, FunctionCallError> {
    environment_id.map_or_else(
        || Ok(environments.primary()),
        |environment_id| {
            environments
                .turn_environments()
                .find(|environment| environment.environment_id == environment_id)
                .map(Some)
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(format!(
                        "unknown turn environment id `{environment_id}`"
                    ))
                })
        },
    )
}

/// Validates feature/policy constraints for `with_additional_permissions` and
/// normalizes any path-based permissions. Errors if the request is invalid.
pub(crate) fn normalize_and_validate_additional_permissions(
    additional_permissions_allowed: bool,
    approval_policy: AskForApproval,
    sandbox_permissions: SandboxPermissions,
    additional_permissions: Option<AdditionalPermissionProfile>,
    permissions_preapproved: bool,
    _cwd: &Path,
) -> Result<Option<AdditionalPermissionProfile>, String> {
    let uses_additional_permissions = matches!(
        sandbox_permissions,
        SandboxPermissions::WithAdditionalPermissions
    );

    if !permissions_preapproved
        && !additional_permissions_allowed
        && (uses_additional_permissions || additional_permissions.is_some())
    {
        return Err(
            "additional permissions are disabled; enable `features.exec_permission_approvals` before using `with_additional_permissions`"
                .to_string(),
        );
    }

    if uses_additional_permissions {
        if !permissions_preapproved && !matches!(approval_policy, AskForApproval::OnRequest) {
            return Err(format!(
                "approval policy is {approval_policy:?}; reject command — you cannot request additional permissions unless the approval policy is OnRequest"
            ));
        }
        let Some(additional_permissions) = additional_permissions else {
            return Err(
                "missing `additional_permissions`; provide at least one of `network` or `file_system` when using `with_additional_permissions`"
                    .to_string(),
            );
        };
        let normalized = normalize_additional_permissions(additional_permissions)?;
        if normalized.is_empty() {
            return Err(
                "`additional_permissions` must include at least one requested permission in `network` or `file_system`"
                    .to_string(),
            );
        }
        return Ok(Some(normalized));
    }

    if additional_permissions.is_some() {
        Err(
            "`additional_permissions` requires `sandbox_permissions` set to `with_additional_permissions`"
                .to_string(),
        )
    } else {
        Ok(None)
    }
}

pub(super) struct EffectiveAdditionalPermissions {
    pub sandbox_permissions: SandboxPermissions,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
    pub permissions_preapproved: bool,
}

pub(super) fn implicit_granted_permissions(
    sandbox_permissions: SandboxPermissions,
    additional_permissions: Option<&AdditionalPermissionProfile>,
    effective_additional_permissions: &EffectiveAdditionalPermissions,
) -> Option<AdditionalPermissionProfile> {
    if !sandbox_permissions.uses_additional_permissions()
        && !matches!(sandbox_permissions, SandboxPermissions::RequireEscalated)
        && additional_permissions.is_none()
    {
        effective_additional_permissions
            .additional_permissions
            .clone()
    } else {
        None
    }
}

pub(super) async fn apply_granted_turn_permissions(
    session: &Session,
    environment_id: &str,
    cwd: &Path,
    sandbox_permissions: SandboxPermissions,
    additional_permissions: Option<AdditionalPermissionProfile>,
) -> EffectiveAdditionalPermissions {
    if matches!(sandbox_permissions, SandboxPermissions::RequireEscalated) {
        return EffectiveAdditionalPermissions {
            sandbox_permissions,
            additional_permissions,
            permissions_preapproved: false,
        };
    }

    let granted_session_permissions = session.granted_session_permissions(environment_id).await;
    let granted_turn_permissions = session.granted_turn_permissions(environment_id).await;
    let granted_permissions = merge_permission_profiles(
        granted_session_permissions.as_ref(),
        granted_turn_permissions.as_ref(),
    );
    let effective_permissions = merge_permission_profiles(
        additional_permissions.as_ref(),
        granted_permissions.as_ref(),
    );
    let permissions_preapproved = match (effective_permissions.as_ref(), granted_permissions) {
        (Some(effective_permissions), Some(granted_permissions)) => {
            permissions_are_preapproved(effective_permissions, granted_permissions, cwd)
        }
        _ => false,
    };

    let sandbox_permissions =
        if effective_permissions.is_some() && !sandbox_permissions.uses_additional_permissions() {
            SandboxPermissions::WithAdditionalPermissions
        } else {
            sandbox_permissions
        };

    EffectiveAdditionalPermissions {
        sandbox_permissions,
        additional_permissions: effective_permissions,
        permissions_preapproved,
    }
}

fn permissions_are_preapproved(
    effective_permissions: &AdditionalPermissionProfile,
    granted_permissions: AdditionalPermissionProfile,
    cwd: &Path,
) -> bool {
    let materialized_effective_permissions = intersect_permission_profiles(
        effective_permissions.clone(),
        effective_permissions.clone(),
        cwd,
    );
    intersect_permission_profiles(effective_permissions.clone(), granted_permissions, cwd)
        == materialized_effective_permissions
}
