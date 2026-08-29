//! Central approval policy-stage execution.

use crate::hook_runtime::run_permission_request_hooks;
use crate::tools::flat_tool_name;
use crate::tools::sandboxing::ApprovalCtx;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::ToolRuntime;
use codex_hooks::PermissionRequestDecision;
use codex_otel::ToolDecisionSource;
use codex_protocol::protocol::NetworkPolicyRuleAction;
use codex_protocol::protocol::ReviewDecision;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApprovalResolutionSource {
    Hook,
    User,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ApprovalResolution {
    decision: ReviewDecision,
    source: ApprovalResolutionSource,
}

impl ApprovalResolution {
    fn into_tool_result(self) -> Result<ReviewDecision, ToolError> {
        let source = self.source;
        match self.decision {
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } if network_policy_amendment.action == NetworkPolicyRuleAction::Deny => {
                let rejection = match source {
                    ApprovalResolutionSource::Hook => "rejected by configuration",
                    ApprovalResolutionSource::User => "rejected by user",
                };
                Err(ToolError::Rejected(rejection.to_string()))
            }
            ReviewDecision::Denied { rejection } => Err(ToolError::Rejected(rejection)),
            ReviewDecision::TimedOut => {
                Err(ToolError::Rejected("approval review timed out".to_string()))
            }
            ReviewDecision::Abort => {
                Err(ToolError::Rejected("approval request aborted".to_string()))
            }
            decision => Ok(decision),
        }
    }
}

pub(super) async fn resolve_tool_apporval<Rq, Out, T>(
    tool: &mut T,
    req: &Rq,
    permission_request_run_id: &str,
    ctx: ApprovalCtx<'_>,
    tool_ctx: &ToolCtx,
    otel: &codex_otel::SessionTelemetry,
) -> Result<ReviewDecision, ToolError>
where
    T: ToolRuntime<Rq, Out>,
{
    if let Some(permission_request) = tool.permission_request_payload(req) {
        match run_permission_request_hooks(
            ctx.session,
            ctx.turn,
            permission_request_run_id,
            permission_request,
        )
        .await
        {
            Some(PermissionRequestDecision::Allow) => {
                let resolution = ApprovalResolution {
                    decision: ReviewDecision::Approved,
                    source: ApprovalResolutionSource::Hook,
                };
                record_resolution(otel, tool_ctx, &resolution);
                return resolution.into_tool_result();
            }
            Some(PermissionRequestDecision::Deny { message }) => {
                let resolution = ApprovalResolution {
                    decision: ReviewDecision::denied(message),
                    source: ApprovalResolutionSource::Hook,
                };
                record_resolution(otel, tool_ctx, &resolution);
                return resolution.into_tool_result();
            }
            None => {}
        }
    }

    let resolution = ApprovalResolution {
        decision: tool.start_approval_async(req, ctx.clone()).await,
        source: ApprovalResolutionSource::User,
    };
    record_resolution(otel, tool_ctx, &resolution);
    resolution.into_tool_result()
}

fn record_resolution(
    otel: &codex_otel::SessionTelemetry,
    tool_ctx: &ToolCtx,
    resolution: &ApprovalResolution,
) {
    let source = match resolution.source {
        ApprovalResolutionSource::Hook => ToolDecisionSource::Config,
        ApprovalResolutionSource::User => ToolDecisionSource::User,
    };
    let tool_name = flat_tool_name(&tool_ctx.tool_name);
    otel.tool_decision(
        tool_name.as_ref(),
        &tool_ctx.call_id,
        &resolution.decision,
        source,
    );
}

#[cfg(all(test, unix))]
#[path = "approvals_tests.rs"]
mod tests;
