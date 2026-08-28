//! MCP tool approval prompts and decision rules.

use std::collections::HashMap;

use codex_config::types::AppToolApproval;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
use codex_protocol::approvals::ElicitationRequest;
use codex_protocol::mcp_approval_meta::APPROVAL_KIND_KEY as MCP_TOOL_APPROVAL_KIND_KEY;
use codex_protocol::mcp_approval_meta::APPROVAL_KIND_MCP_TOOL_CALL as MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL;
use codex_protocol::mcp_approval_meta::CONNECTOR_DESCRIPTION_KEY as MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY;
use codex_protocol::mcp_approval_meta::CONNECTOR_ID_KEY as MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY;
use codex_protocol::mcp_approval_meta::CONNECTOR_NAME_KEY as MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY;
use codex_protocol::mcp_approval_meta::PERSIST_ALWAYS as MCP_TOOL_APPROVAL_PERSIST_ALWAYS;
use codex_protocol::mcp_approval_meta::PERSIST_KEY as MCP_TOOL_APPROVAL_PERSIST_KEY;
use codex_protocol::mcp_approval_meta::PERSIST_SESSION as MCP_TOOL_APPROVAL_PERSIST_SESSION;
use codex_protocol::mcp_approval_meta::SOURCE_CONNECTOR as MCP_TOOL_APPROVAL_SOURCE_CONNECTOR;
use codex_protocol::mcp_approval_meta::SOURCE_KEY as MCP_TOOL_APPROVAL_SOURCE_KEY;
use codex_protocol::mcp_approval_meta::TOOL_DESCRIPTION_KEY as MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY;
use codex_protocol::mcp_approval_meta::TOOL_PARAMS_DISPLAY_KEY as MCP_TOOL_APPROVAL_TOOL_PARAMS_DISPLAY_KEY;
use codex_protocol::mcp_approval_meta::TOOL_PARAMS_KEY as MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY;
use codex_protocol::mcp_approval_meta::TOOL_TITLE_KEY as MCP_TOOL_APPROVAL_TOOL_TITLE_KEY;
use codex_protocol::protocol::McpInvocation;
use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputQuestion;
use codex_protocol::request_user_input::RequestUserInputQuestionOption;
use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_rmcp_client::ElicitationAction;
use codex_rmcp_client::ElicitationResponse;
use rmcp::model::ToolAnnotations;
use serde::Serialize;
use serde_json::Value;

use crate::RenderedMcpToolApprovalParam;

/// The outcome of an MCP tool approval request.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpToolApprovalDecision {
    /// Execute the current tool call.
    Accept,
    /// Execute and remember the decision for this session.
    AcceptForSession,
    /// Execute and persist the decision for future sessions.
    AcceptAndRemember,
    /// Do not execute the tool call.
    Decline {
        /// An optional explanation supplied by the approver.
        message: Option<String>,
    },
    /// Cancel the approval request without an explicit denial.
    Cancel,
}

/// Runtime metadata used to form an MCP tool approval request.
#[doc(hidden)]
#[derive(Clone)]
pub struct McpToolApprovalMetadata {
    /// MCP annotations that determine whether approval is required.
    pub annotations: Option<ToolAnnotations>,
    /// Stable Codex App connector identifier.
    pub connector_id: Option<String>,
    /// Connector link identifier.
    pub link_id: Option<String>,
    /// Human-readable connector name.
    pub connector_name: Option<String>,
    /// Human-readable connector description.
    pub connector_description: Option<String>,
    /// Account identity associated with the connector.
    pub connected_account_email: Option<String>,
    /// Owning plugin identifier.
    pub plugin_id: Option<String>,
    /// Human-readable MCP tool title.
    pub tool_title: Option<String>,
    /// Human-readable MCP tool description.
    pub tool_description: Option<String>,
    /// Resource URI rendered by a Codex App tool.
    pub mcp_app_resource_uri: Option<String>,
    /// Codex App request metadata supplied by the MCP server.
    pub codex_apps_meta: Option<serde_json::Map<String, Value>>,
    /// Optional file-input fields accepted by an OpenAI MCP tool.
    pub openai_file_input_optional_fields: Option<HashMap<String, Vec<String>>>,
}

/// Options controlling MCP tool approval prompt persistence choices.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct McpToolApprovalPromptOptions {
    /// Whether the prompt offers a session-scoped approval.
    pub allow_session_remember: bool,
    /// Whether the prompt offers a persistent approval.
    pub allow_persistent_approval: bool,
}

/// Identifies one remembered MCP tool approval.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct McpToolApprovalKey {
    /// MCP server that owns the tool.
    pub server: String,
    /// Stable Codex App connector identifier, when one applies.
    pub connector_id: Option<String>,
    /// MCP tool name.
    pub tool_name: String,
}

/// Determines which remembered-approval choices an MCP prompt can offer.
#[doc(hidden)]
pub fn mcp_tool_approval_prompt_options(
    session_approval_key: Option<&McpToolApprovalKey>,
    persistent_approval_key: Option<&McpToolApprovalKey>,
    tool_call_mcp_elicitation_enabled: bool,
) -> McpToolApprovalPromptOptions {
    McpToolApprovalPromptOptions {
        allow_session_remember: session_approval_key.is_some(),
        allow_persistent_approval: tool_call_mcp_elicitation_enabled
            && persistent_approval_key.is_some(),
    }
}

/// Builds the session-scoped approval key for an eligible MCP invocation.
#[doc(hidden)]
pub fn session_mcp_tool_approval_key(
    invocation: &McpInvocation,
    metadata: Option<&McpToolApprovalMetadata>,
    approval_mode: AppToolApproval,
) -> Option<McpToolApprovalKey> {
    if approval_mode != AppToolApproval::Auto {
        return None;
    }

    let connector_id = metadata.and_then(|metadata| metadata.connector_id.clone());
    if invocation.server == CODEX_APPS_MCP_SERVER_NAME && connector_id.is_none() {
        return None;
    }

    Some(McpToolApprovalKey {
        server: invocation.server.clone(),
        connector_id,
        tool_name: invocation.tool.clone(),
    })
}

/// Builds the persistent approval key for an eligible MCP invocation.
#[doc(hidden)]
pub fn persistent_mcp_tool_approval_key(
    invocation: &McpInvocation,
    metadata: Option<&McpToolApprovalMetadata>,
    approval_mode: AppToolApproval,
) -> Option<McpToolApprovalKey> {
    session_mcp_tool_approval_key(invocation, metadata, approval_mode)
}

/// Input used to create an MCP elicitation approval request.
#[doc(hidden)]
pub struct McpToolApprovalElicitationRequest<'a> {
    /// MCP server hosting the tool.
    pub server: &'a str,
    /// Tool metadata used in the approval UI.
    pub metadata: Option<&'a McpToolApprovalMetadata>,
    /// Original or template-rendered tool arguments.
    pub tool_params: Option<&'a Value>,
    /// Display-ready tool argument descriptions.
    pub tool_params_display: Option<&'a [RenderedMcpToolApprovalParam]>,
    /// Compatibility question presented to request-user-input clients.
    pub question: RequestUserInputQuestion,
    /// Optional MCP-native elicitation message.
    pub message_override: Option<&'a str>,
    /// Persistence choices offered by the request.
    pub prompt_options: McpToolApprovalPromptOptions,
}

/// Prefix used to correlate request-user-input approval questions.
#[doc(hidden)]
pub const MCP_TOOL_APPROVAL_QUESTION_ID_PREFIX: &str = "mcp_tool_call_approval";
/// Label approving one MCP tool call.
#[doc(hidden)]
pub const MCP_TOOL_APPROVAL_ACCEPT: &str = "Allow";
/// Label approving MCP tool calls for the active session.
#[doc(hidden)]
pub const MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION: &str = "Allow for this session";
/// Synthetic marker for a guardian denial in the legacy prompt path.
#[doc(hidden)]
pub const MCP_TOOL_APPROVAL_DECLINE_SYNTHETIC: &str = "__codex_mcp_decline__";

/// Label approving MCP tool calls in future sessions.
#[doc(hidden)]
pub const MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER: &str = "Allow and don't ask me again";
/// Label cancelling an MCP tool approval request.
#[doc(hidden)]
pub const MCP_TOOL_APPROVAL_CANCEL: &str = "Cancel";

/// Reports whether an ID identifies an MCP approval question.
#[doc(hidden)]
pub fn is_mcp_tool_approval_question_id(question_id: &str) -> bool {
    question_id
        .strip_prefix(MCP_TOOL_APPROVAL_QUESTION_ID_PREFIX)
        .is_some_and(|suffix| suffix.starts_with('_'))
}

/// Builds the compatibility approval question for an MCP tool call.
#[doc(hidden)]
pub fn build_mcp_tool_approval_question(
    question_id: String,
    server: &str,
    tool_name: &str,
    connector_name: Option<&str>,
    prompt_options: McpToolApprovalPromptOptions,
    question_override: Option<&str>,
) -> RequestUserInputQuestion {
    let question = question_override
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            build_mcp_tool_approval_fallback_message(server, tool_name, connector_name)
        });
    let question = format!("{}?", question.trim_end_matches('?'));

    let mut options = vec![RequestUserInputQuestionOption {
        label: MCP_TOOL_APPROVAL_ACCEPT.to_string(),
        description: "Run the tool and continue.".to_string(),
    }];
    if prompt_options.allow_session_remember {
        options.push(RequestUserInputQuestionOption {
            label: MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION.to_string(),
            description: "Run the tool and remember this choice for this session.".to_string(),
        });
    }
    if prompt_options.allow_persistent_approval {
        options.push(RequestUserInputQuestionOption {
            label: MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER.to_string(),
            description: "Run the tool and remember this choice for future tool calls.".to_string(),
        });
    }
    options.push(RequestUserInputQuestionOption {
        label: MCP_TOOL_APPROVAL_CANCEL.to_string(),
        description: "Cancel this tool call.".to_string(),
    });

    RequestUserInputQuestion {
        id: question_id,
        header: "Approve app tool call?".to_string(),
        question,
        is_other: false,
        is_secret: false,
        options: Some(options),
    }
}

fn build_mcp_tool_approval_fallback_message(
    server: &str,
    tool_name: &str,
    connector_name: Option<&str>,
) -> String {
    let actor = connector_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            if server == CODEX_APPS_MCP_SERVER_NAME {
                "this app".to_string()
            } else {
                format!("the {server} MCP server")
            }
        });
    format!("Allow {actor} to run tool \"{tool_name}\"?")
}

/// Builds an MCP-native elicitation request for a tool approval.
#[doc(hidden)]
pub fn build_mcp_tool_approval_elicitation_request(
    request: McpToolApprovalElicitationRequest<'_>,
) -> ElicitationRequest {
    let message = request
        .message_override
        .map(ToString::to_string)
        .unwrap_or_else(|| request.question.question.clone());

    ElicitationRequest::Form {
        meta: build_mcp_tool_approval_elicitation_meta(
            request.server,
            request.metadata,
            request.tool_params,
            request.tool_params_display,
            request.prompt_options,
        ),
        message,
        requested_schema: serde_json::json!({
            "type": "object",
            "properties": {},
        }),
    }
}

/// Builds the metadata attached to an MCP-native approval elicitation.
#[doc(hidden)]
pub fn build_mcp_tool_approval_elicitation_meta(
    server: &str,
    metadata: Option<&McpToolApprovalMetadata>,
    tool_params: Option<&Value>,
    tool_params_display: Option<&[RenderedMcpToolApprovalParam]>,
    prompt_options: McpToolApprovalPromptOptions,
) -> Option<Value> {
    let mut meta = serde_json::Map::new();
    meta.insert(
        MCP_TOOL_APPROVAL_KIND_KEY.to_string(),
        Value::String(MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL.to_string()),
    );
    match (
        prompt_options.allow_session_remember,
        prompt_options.allow_persistent_approval,
    ) {
        (true, true) => {
            meta.insert(
                MCP_TOOL_APPROVAL_PERSIST_KEY.to_string(),
                serde_json::json!([
                    MCP_TOOL_APPROVAL_PERSIST_SESSION,
                    MCP_TOOL_APPROVAL_PERSIST_ALWAYS,
                ]),
            );
        }
        (true, false) => {
            meta.insert(
                MCP_TOOL_APPROVAL_PERSIST_KEY.to_string(),
                Value::String(MCP_TOOL_APPROVAL_PERSIST_SESSION.to_string()),
            );
        }
        (false, true) => {
            meta.insert(
                MCP_TOOL_APPROVAL_PERSIST_KEY.to_string(),
                Value::String(MCP_TOOL_APPROVAL_PERSIST_ALWAYS.to_string()),
            );
        }
        (false, false) => {}
    }
    if let Some(metadata) = metadata {
        if let Some(tool_title) = metadata.tool_title.as_ref() {
            meta.insert(
                MCP_TOOL_APPROVAL_TOOL_TITLE_KEY.to_string(),
                Value::String(tool_title.clone()),
            );
        }
        if let Some(tool_description) = metadata.tool_description.as_ref() {
            meta.insert(
                MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY.to_string(),
                Value::String(tool_description.clone()),
            );
        }
        if server == CODEX_APPS_MCP_SERVER_NAME
            && (metadata.connector_id.is_some()
                || metadata.connector_name.is_some()
                || metadata.connector_description.is_some())
        {
            meta.insert(
                MCP_TOOL_APPROVAL_SOURCE_KEY.to_string(),
                Value::String(MCP_TOOL_APPROVAL_SOURCE_CONNECTOR.to_string()),
            );
            if let Some(connector_id) = metadata.connector_id.as_deref() {
                meta.insert(
                    MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY.to_string(),
                    Value::String(connector_id.to_string()),
                );
            }
            if let Some(connector_name) = metadata.connector_name.as_ref() {
                meta.insert(
                    MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY.to_string(),
                    Value::String(connector_name.clone()),
                );
            }
            if let Some(connector_description) = metadata.connector_description.as_ref() {
                meta.insert(
                    MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY.to_string(),
                    Value::String(connector_description.clone()),
                );
            }
        }
    }
    if let Some(tool_params) = tool_params {
        meta.insert(
            MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY.to_string(),
            tool_params.clone(),
        );
    }
    if let Some(tool_params_display) = tool_params_display
        && let Ok(tool_params_display) = serde_json::to_value(tool_params_display)
    {
        meta.insert(
            MCP_TOOL_APPROVAL_TOOL_PARAMS_DISPLAY_KEY.to_string(),
            tool_params_display,
        );
    }
    (!meta.is_empty()).then_some(Value::Object(meta))
}

/// Converts tool arguments into a stable approval display order.
#[doc(hidden)]
pub fn build_mcp_tool_approval_display_params(
    tool_params: Option<&Value>,
) -> Option<Vec<RenderedMcpToolApprovalParam>> {
    let tool_params = tool_params?.as_object()?;
    let mut display_params = tool_params
        .iter()
        .map(|(name, value)| RenderedMcpToolApprovalParam {
            name: name.clone(),
            value: value.clone(),
            display_name: name.clone(),
        })
        .collect::<Vec<_>>();
    display_params.sort_by(|left, right| left.name.cmp(&right.name));
    Some(display_params)
}

/// Converts an MCP elicitation response into an approval decision.
#[doc(hidden)]
pub fn parse_mcp_tool_approval_elicitation_response(
    response: Option<ElicitationResponse>,
    question_id: &str,
) -> McpToolApprovalDecision {
    let Some(response) = response else {
        return McpToolApprovalDecision::Cancel;
    };
    match response.action {
        ElicitationAction::Accept => {
            match response
                .meta
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|meta| meta.get(MCP_TOOL_APPROVAL_PERSIST_KEY))
                .and_then(Value::as_str)
            {
                Some(MCP_TOOL_APPROVAL_PERSIST_SESSION) => {
                    return McpToolApprovalDecision::AcceptForSession;
                }
                Some(MCP_TOOL_APPROVAL_PERSIST_ALWAYS) => {
                    return McpToolApprovalDecision::AcceptAndRemember;
                }
                _ => {}
            }

            match parse_mcp_tool_approval_response(
                request_user_input_response_from_elicitation_content(response.content),
                question_id,
            ) {
                McpToolApprovalDecision::Cancel => McpToolApprovalDecision::Accept,
                decision => decision,
            }
        }
        ElicitationAction::Decline => McpToolApprovalDecision::Decline { message: None },
        ElicitationAction::Cancel => McpToolApprovalDecision::Cancel,
    }
}

/// Converts MCP elicitation content to a compatibility prompt response.
#[doc(hidden)]
pub fn request_user_input_response_from_elicitation_content(
    content: Option<Value>,
) -> Option<RequestUserInputResponse> {
    let Some(content) = content else {
        return Some(RequestUserInputResponse {
            answers: HashMap::new(),
        });
    };
    let content = content.as_object()?;
    let answers = content
        .iter()
        .filter_map(|(question_id, value)| {
            let answers = match value {
                Value::String(answer) => vec![answer.clone()],
                Value::Array(values) => values
                    .iter()
                    .filter_map(|value| value.as_str().map(ToString::to_string))
                    .collect(),
                _ => return None,
            };
            Some((question_id.clone(), RequestUserInputAnswer { answers }))
        })
        .collect();

    Some(RequestUserInputResponse { answers })
}

/// Converts a compatibility prompt response into an approval decision.
#[doc(hidden)]
pub fn parse_mcp_tool_approval_response(
    response: Option<RequestUserInputResponse>,
    question_id: &str,
) -> McpToolApprovalDecision {
    let Some(response) = response else {
        return McpToolApprovalDecision::Cancel;
    };
    let answers = response
        .answers
        .get(question_id)
        .map(|answer| answer.answers.as_slice());
    let Some(answers) = answers else {
        return McpToolApprovalDecision::Cancel;
    };
    if answers
        .iter()
        .any(|answer| answer == MCP_TOOL_APPROVAL_DECLINE_SYNTHETIC)
    {
        McpToolApprovalDecision::Decline { message: None }
    } else if answers
        .iter()
        .any(|answer| answer == MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION)
    {
        McpToolApprovalDecision::AcceptForSession
    } else if answers
        .iter()
        .any(|answer| answer == MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER)
    {
        McpToolApprovalDecision::AcceptAndRemember
    } else if answers
        .iter()
        .any(|answer| answer == MCP_TOOL_APPROVAL_ACCEPT)
    {
        McpToolApprovalDecision::Accept
    } else {
        McpToolApprovalDecision::Cancel
    }
}

/// Restricts remembered approvals in modes that always prompt.
#[doc(hidden)]
pub fn normalize_approval_decision_for_mode(
    decision: McpToolApprovalDecision,
    approval_mode: AppToolApproval,
) -> McpToolApprovalDecision {
    if matches!(
        approval_mode,
        AppToolApproval::Prompt | AppToolApproval::Writes
    ) && matches!(
        decision,
        McpToolApprovalDecision::AcceptForSession | McpToolApprovalDecision::AcceptAndRemember
    ) {
        McpToolApprovalDecision::Accept
    } else {
        decision
    }
}

/// Determines whether a tool requires approval for the configured mode.
#[doc(hidden)]
pub fn requires_mcp_tool_approval_for_mode(
    annotations: Option<&ToolAnnotations>,
    approval_mode: AppToolApproval,
) -> bool {
    match approval_mode {
        AppToolApproval::Auto => requires_mcp_tool_approval(annotations),
        AppToolApproval::Prompt => true,
        AppToolApproval::Writes => !annotations
            .and_then(|annotations| annotations.read_only_hint)
            .unwrap_or(false),
        AppToolApproval::Approve => false,
    }
}

/// Determines whether tool annotations require approval in automatic mode.
#[doc(hidden)]
pub fn requires_mcp_tool_approval(annotations: Option<&ToolAnnotations>) -> bool {
    let destructive_hint = annotations.and_then(|annotations| annotations.destructive_hint);
    if destructive_hint == Some(true) {
        return true;
    }

    let read_only_hint = annotations
        .and_then(|annotations| annotations.read_only_hint)
        .unwrap_or(false);
    if read_only_hint {
        return false;
    }

    destructive_hint.unwrap_or(true)
        || annotations
            .and_then(|annotations| annotations.open_world_hint)
            .unwrap_or(true)
}

#[cfg(test)]
#[path = "mcp_tool_approval_tests.rs"]
mod tests;
