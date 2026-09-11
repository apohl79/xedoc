use xedoc_protocol::ThreadId;
use xedoc_protocol::protocol::InterAgentCommunication;

const AGENT_COMMUNICATION_TARGET: &str = "xedoc_otel.agent_communication";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentCommunicationKind {
    Spawn,
    Message,
    Followup,
    Result,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbPairBranch {
    Routed,
    Orchestrator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AbPairTransportMetadata {
    pub(crate) pair_id: String,
    pub(crate) branch: AbPairBranch,
    pub(crate) router_decision_id: Option<String>,
}

impl AgentCommunicationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Message => "message",
            Self::Followup => "followup",
            Self::Result => "result",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentCommunicationContext {
    kind: AgentCommunicationKind,
    sender_thread_id: ThreadId,
    ab_pair: Option<AbPairTransportMetadata>,
}

impl AgentCommunicationContext {
    pub(crate) fn new(kind: AgentCommunicationKind, sender_thread_id: ThreadId) -> Self {
        Self {
            kind,
            sender_thread_id,
            ab_pair: None,
        }
    }

    pub(crate) fn with_ab_pair(
        mut self,
        pair_id: String,
        branch: AbPairBranch,
        router_decision_id: Option<String>,
    ) -> Self {
        self.ab_pair = Some(AbPairTransportMetadata {
            pair_id,
            branch,
            router_decision_id,
        });
        self
    }

    pub(crate) fn ab_pair(&self) -> Option<&AbPairTransportMetadata> {
        self.ab_pair.as_ref()
    }
}

pub(crate) fn logging_enabled() -> bool {
    tracing::enabled!(target: AGENT_COMMUNICATION_TARGET, tracing::Level::INFO)
}

pub(crate) fn emit_agent_communication_send(
    communication_id: &str,
    context: &AgentCommunicationContext,
    communication: &InterAgentCommunication,
    receiver_thread_id: ThreadId,
) {
    tracing::info!(
        target: AGENT_COMMUNICATION_TARGET,
        {
            event.name = "xedoc.agent_communication",
            communication_id,
            kind = context.kind.as_str(),
            state = "send",
            sender_thread_id = %context.sender_thread_id,
            receiver_thread_id = %receiver_thread_id,
            content = if communication.content.is_empty() {
                communication.encrypted_content.as_deref().unwrap_or_default()
            } else {
                communication.content.as_str()
            },
        },
        "agent communication"
    );
}

pub(crate) fn emit_agent_communication_receive(communication_id: &str) {
    tracing::info!(
        target: AGENT_COMMUNICATION_TARGET,
        {
            event.name = "xedoc.agent_communication",
            communication_id,
            state = "receive",
        },
        "agent communication"
    );
}
