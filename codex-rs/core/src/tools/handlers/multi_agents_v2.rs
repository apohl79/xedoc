//! Implements the MultiAgentV2 collaboration tool surface.

use crate::agent::AgentStatus;
use crate::agent::agent_resolver::resolve_agent_target;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::multi_agents_common::*;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::AgentPath;
use codex_protocol::items::CollabAgentTool;
use codex_protocol::items::CollabAgentToolCallItem;
use codex_protocol::items::CollabAgentToolCallStatus;
use codex_protocol::items::SubAgentActivityItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::SubAgentActivityKind;
use codex_tools::ToolName;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

pub(crate) use followup_task::Handler as FollowupTaskHandler;
pub(crate) use interrupt_agent::Handler as InterruptAgentHandler;
pub(crate) use list_agents::Handler as ListAgentsHandler;
pub(crate) use send_message::Handler as SendMessageHandler;
pub(crate) use spawn::Handler as SpawnAgentHandler;
pub(crate) use wait::Handler as WaitAgentHandler;

mod followup_task;
mod interrupt_agent;
mod list_agents;
mod message_tool;
mod send_message;
mod spawn;
pub(crate) mod wait;

pub(crate) async fn emit_sub_agent_activity(
    session: &crate::session::session::Session,
    turn: &crate::session::turn_context::TurnContext,
    item: SubAgentActivityItem,
) {
    session
        .emit_turn_item_completed(turn, TurnItem::SubAgentActivity(item))
        .await;
}

#[derive(Clone, Copy)]
pub(super) enum ToolMessageKind {
    NewTask,
    Message,
}

impl ToolMessageKind {
    fn label(self) -> &'static str {
        match self {
            Self::NewTask => "NEW_TASK",
            Self::Message => "MESSAGE",
        }
    }

    fn trigger_turn(self) -> bool {
        matches!(self, Self::NewTask)
    }
}

pub(super) fn communication_from_tool_message(
    author: AgentPath,
    recipient: AgentPath,
    message: String,
    message_kind: ToolMessageKind,
) -> InterAgentCommunication {
    let message_type = message_kind.label();
    // `message` is model-controlled and may contain lines that look like
    // envelope headers (e.g. `Sender:`). It is deliberately not escaped:
    // the recipient is a model, not a parser, so no escaping is robust
    // against reframing, and mangling the payload would corrupt legitimate
    // messages. Sender/recipient identity is authoritative via the
    // structured `AgentMessage.author`/`.recipient` fields set in
    // `to_model_input_item`, not this advisory text header.
    let content = format!(
        "Message Type: {message_type}\nTask name: {recipient}\nSender: {author}\nPayload:\n{message}"
    );
    InterAgentCommunication::new(
        author,
        recipient,
        Vec::new(),
        content,
        message_kind.trigger_turn(),
    )
}
