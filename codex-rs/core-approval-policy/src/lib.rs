//! Approval-policy decisions shared by Codex core subsystems.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod command_canonicalization;
mod mcp_tool_approval;
mod mcp_tool_approval_templates;
mod network_policy_decision;
mod safety;

pub use command_canonicalization::canonicalize_command_for_approval;
pub use mcp_tool_approval::MCP_TOOL_APPROVAL_ACCEPT;
pub use mcp_tool_approval::MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER;
pub use mcp_tool_approval::MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION;
pub use mcp_tool_approval::MCP_TOOL_APPROVAL_CANCEL;
pub use mcp_tool_approval::MCP_TOOL_APPROVAL_DECLINE_SYNTHETIC;
pub use mcp_tool_approval::MCP_TOOL_APPROVAL_QUESTION_ID_PREFIX;
pub use mcp_tool_approval::McpToolApprovalDecision;
pub use mcp_tool_approval::McpToolApprovalElicitationRequest;
pub use mcp_tool_approval::McpToolApprovalKey;
pub use mcp_tool_approval::McpToolApprovalMetadata;
pub use mcp_tool_approval::McpToolApprovalPromptOptions;
pub use mcp_tool_approval::build_mcp_tool_approval_display_params;
pub use mcp_tool_approval::build_mcp_tool_approval_elicitation_meta;
pub use mcp_tool_approval::build_mcp_tool_approval_elicitation_request;
pub use mcp_tool_approval::build_mcp_tool_approval_question;
pub use mcp_tool_approval::is_mcp_tool_approval_question_id;
pub use mcp_tool_approval::mcp_tool_approval_prompt_options;
pub use mcp_tool_approval::normalize_approval_decision_for_mode;
pub use mcp_tool_approval::parse_mcp_tool_approval_elicitation_response;
pub use mcp_tool_approval::parse_mcp_tool_approval_response;
pub use mcp_tool_approval::persistent_mcp_tool_approval_key;
pub use mcp_tool_approval::request_user_input_response_from_elicitation_content;
pub use mcp_tool_approval::requires_mcp_tool_approval;
pub use mcp_tool_approval::requires_mcp_tool_approval_for_mode;
pub use mcp_tool_approval::session_mcp_tool_approval_key;
pub use mcp_tool_approval_templates::RenderedMcpToolApprovalParam;
pub use mcp_tool_approval_templates::RenderedMcpToolApprovalTemplate;
pub use mcp_tool_approval_templates::render_mcp_tool_approval_template;
pub use network_policy_decision::ExecPolicyNetworkRuleAmendment;
pub use network_policy_decision::denied_network_policy_message;
pub use network_policy_decision::execpolicy_network_rule_amendment;
pub use network_policy_decision::network_approval_context_from_payload;
pub use safety::SafetyCheck;
pub use safety::assess_patch_safety;
