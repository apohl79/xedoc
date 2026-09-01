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
use crate::turn_timing::now_unix_timestamp_ms;
use serde::Deserialize;
use serde::Serialize;
use xedoc_protocol::AgentPath;
use xedoc_protocol::items::CollabAgentTool;
use xedoc_protocol::items::CollabAgentToolCallItem;
use xedoc_protocol::items::CollabAgentToolCallStatus;
use xedoc_protocol::items::TurnItem;
use xedoc_protocol::models::ResponseInputItem;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::protocol::InterAgentCommunication;
use xedoc_protocol::protocol::SubAgentActivityEvent;
use xedoc_protocol::protocol::SubAgentActivityKind;
use xedoc_tools::ToolName;

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
