//! Shared approvals and sandboxing traits used by tool runtimes.
//!
//! Consolidates the approval flow primitives (`ApprovalDecision`, `ApprovalStore`,
//! `ApprovalCtx`, `Approvable`) together with the sandbox orchestration traits
//! and helpers (`Sandboxable`, `ToolRuntime`, `SandboxAttempt`, etc.).

use crate::sandboxing::SandboxPermissions;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::SessionServices;
use crate::tools::hook_names::HookToolName;
use crate::tools::network_approval::NetworkApprovalSpec;
use codex_protocol::approvals::NetworkApprovalContext;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::ReviewDecision;
use codex_tools::ToolName;
use codex_utils_path_uri::PathUri;
use futures::Future;
use futures::future::BoxFuture;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

#[derive(Clone, Default, Debug)]
pub(crate) struct ApprovalStore {
    // Store serialized keys for generic caching across requests.
    map: HashMap<String, ReviewDecision>,
}

impl ApprovalStore {
    pub fn get<K>(&self, key: &K) -> Option<ReviewDecision>
    where
        K: Serialize,
    {
        let s = serde_json::to_string(key).ok()?;
        self.map.get(&s).cloned()
    }

    pub fn put<K>(&mut self, key: K, value: ReviewDecision)
    where
        K: Serialize,
    {
        if let Ok(s) = serde_json::to_string(&key) {
            self.map.insert(s, value);
        }
    }
}

/// Takes a vector of approval keys and returns a ReviewDecision.
/// There will be one key in most cases, but apply_patch can modify multiple files at once.
///
/// - If all keys are already approved for session, we skip prompting.
/// - If the user approves for session, we store the decision for each key individually
///   so future requests touching any subset can also skip prompting.
pub(crate) async fn with_cached_approval<K, F, Fut>(
    services: &SessionServices,
    // Name of the tool, used for metrics collection.
    tool_name: &str,
    keys: Vec<K>,
    fetch: F,
) -> ReviewDecision
where
    K: Serialize,
    F: FnOnce() -> Fut,
    Fut: Future<Output = ReviewDecision>,
{
    // To be defensive here, don't bother with checking the cache if keys are empty.
    if keys.is_empty() {
        return fetch().await;
    }

    let already_approved = {
        let store = services.tool_approvals.lock().await;
        keys.iter()
            .all(|key| matches!(store.get(key), Some(ReviewDecision::ApprovedForSession)))
    };

    if already_approved {
        return ReviewDecision::ApprovedForSession;
    }

    let decision = fetch().await;

    services.session_telemetry.counter(
        "codex.approval.requested",
        /*inc*/ 1,
        &[
            ("tool", tool_name),
            ("approved", decision.to_opaque_string()),
        ],
    );

    if matches!(decision, ReviewDecision::ApprovedForSession) {
        let mut store = services.tool_approvals.lock().await;
        for key in keys {
            store.put(key, ReviewDecision::ApprovedForSession);
        }
    }

    decision
}

pub(crate) struct ApprovalCtx<'a, S = Session> {
    pub session: &'a Arc<S>,
    pub turn: &'a Arc<TurnContext>,
    pub call_id: &'a str,
    pub retry_reason: Option<String>,
    pub network_approval_context: Option<NetworkApprovalContext>,
}

impl<S> Clone for ApprovalCtx<'_, S> {
    fn clone(&self) -> Self {
        Self {
            session: self.session,
            turn: self.turn,
            call_id: self.call_id,
            retry_reason: self.retry_reason.clone(),
            network_approval_context: self.network_approval_context.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PermissionRequestPayload {
    pub tool_name: HookToolName,
    pub tool_input: serde_json::Value,
}

impl PermissionRequestPayload {
    pub(crate) fn bash(command: String, description: Option<String>) -> Self {
        let mut tool_input = serde_json::Map::new();
        tool_input.insert("command".to_string(), serde_json::Value::String(command));
        if let Some(description) = description {
            tool_input.insert(
                "description".to_string(),
                serde_json::Value::String(description),
            );
        }

        Self {
            tool_name: HookToolName::bash(),
            tool_input: serde_json::Value::Object(tool_input),
        }
    }
}

pub(crate) use codex_core_exec::approval::ExecApprovalRequirement;
pub(crate) use codex_core_exec::tool_sandboxing::SandboxAttempt;
pub(crate) use codex_core_exec::tool_sandboxing::SandboxOverride;
pub(crate) use codex_core_exec::tool_sandboxing::Sandboxable;
pub(crate) use codex_core_exec::tool_sandboxing::ToolError;
pub(crate) use codex_core_exec::tool_sandboxing::default_exec_approval_requirement;
pub(crate) use codex_core_exec::tool_sandboxing::managed_network_for_sandbox_permissions;
pub(crate) use codex_core_exec::tool_sandboxing::sandbox_override_for_first_attempt;
pub(crate) use codex_core_exec::tool_sandboxing::sandbox_permissions_preserving_denied_reads;
pub(crate) use codex_core_exec::tool_sandboxing::unsandboxed_execution_allowed;

pub(crate) trait Approvable<Req, S = Session> {
    type ApprovalKey: Hash + Eq + Clone + Debug + Serialize;

    // In most cases (shell, unified_exec), a request will have a single approval key.
    //
    // However, apply_patch needs session "Allow, don't ask again" semantics that
    // apply to multiple atomic targets (e.g., apply_patch approves per file path). Returning
    // a list of keys lets the runtime treat the request as approved-for-session only if
    // *all* keys are already approved, while still caching approvals per-key so future
    // requests touching a subset can be auto-approved.
    fn approval_keys(&self, req: &Req) -> Vec<Self::ApprovalKey>;

    /// Return per-request sandbox permissions for first-attempt sandbox
    /// selection. Most tools use the ambient sandbox policy unchanged.
    fn sandbox_permissions(&self, _req: &Req) -> SandboxPermissions {
        SandboxPermissions::UseDefault
    }

    fn should_bypass_approval(&self, policy: AskForApproval, already_approved: bool) -> bool {
        if already_approved {
            // We do not ask one more time
            return true;
        }
        matches!(policy, AskForApproval::Never)
    }

    /// Return `Some(_)` to specify a custom exec approval requirement, or `None`
    /// to fall back to policy-based default.
    fn exec_approval_requirement(&self, _req: &Req) -> Option<ExecApprovalRequirement> {
        None
    }

    /// Return hook input for approval-time policy hooks when this runtime wants
    /// hook evaluation to run before user approval.
    fn permission_request_payload(&self, _req: &Req) -> Option<PermissionRequestPayload> {
        None
    }

    /// Decide we can request an approval for no-sandbox execution.
    fn wants_no_sandbox_approval(&self, policy: AskForApproval) -> bool {
        match policy {
            AskForApproval::UnlessTrusted => true,
            AskForApproval::Never => false,
            AskForApproval::OnRequest => false,
            AskForApproval::Granular(granular_config) => granular_config.sandbox_approval,
        }
    }

    fn start_approval_async<'a>(
        &'a mut self,
        req: &'a Req,
        ctx: ApprovalCtx<'a, S>,
    ) -> BoxFuture<'a, ReviewDecision>;
}

pub(crate) struct ToolCtx<S = Session> {
    pub session: Arc<S>,
    pub turn: Arc<TurnContext>,
    pub call_id: String,
    pub tool_name: ToolName,
}

pub(crate) trait ToolRuntime<Req, Out, S = Session>:
    Approvable<Req, S> + Sandboxable
{
    fn workspace_roots<'a>(&self, req: &'a Req) -> &'a [PathUri];

    fn network_approval_spec(&self, _req: &Req, _ctx: &ToolCtx<S>) -> Option<NetworkApprovalSpec> {
        None
    }

    fn sandbox_cwd<'a>(&self, _req: &'a Req) -> Option<&'a PathUri> {
        None
    }

    async fn run(
        &mut self,
        req: &Req,
        attempt: &SandboxAttempt<'_>,
        ctx: &ToolCtx<S>,
    ) -> Result<Out, ToolError>;
}

#[cfg(test)]
#[path = "sandboxing_tests.rs"]
mod tests;
