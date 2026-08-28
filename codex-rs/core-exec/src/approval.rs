use codex_protocol::approvals::ExecPolicyAmendment;

/// Specifies what tool orchestration should do with an executable tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecApprovalRequirement {
    /// No approval is required for this tool call.
    Skip {
        /// The first attempt should skip sandboxing when policy explicitly allows it.
        bypass_sandbox: bool,
        /// An amendment that can skip future approval for similar commands.
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    },
    /// Approval is required for this tool call.
    NeedsApproval {
        reason: Option<String>,
        /// An amendment that can skip future approval for similar commands.
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    },
    /// Execution is forbidden.
    Forbidden { reason: String },
}

impl ExecApprovalRequirement {
    pub fn proposed_execpolicy_amendment(&self) -> Option<&ExecPolicyAmendment> {
        match self {
            Self::NeedsApproval {
                proposed_execpolicy_amendment: Some(prefix),
                ..
            }
            | Self::Skip {
                proposed_execpolicy_amendment: Some(prefix),
                ..
            } => Some(prefix),
            _ => None,
        }
    }
}
