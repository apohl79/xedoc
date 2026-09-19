//! Host policy for persistent session-script connections.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::time::Duration;

const MIN_RESPONSE_TIMEOUT_MS: u64 = 100;
const MAX_RESPONSE_TIMEOUT_MS: u64 = 60_000;

/// One host-managed script that may attach to a loaded session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SessionScriptConfigToml {
    /// Stable host-owned script identifier.
    pub id: String,
    /// Direct-exec argv for the script. The host never evaluates this through a shell.
    pub command: Vec<String>,
    /// Capabilities that the host may grant when this script registers.
    #[serde(default)]
    pub capabilities: Vec<SessionScriptCapabilityToml>,
    /// Notifications that the host may deliver to this script.
    #[serde(default)]
    pub subscriptions: Vec<SessionScriptSubscriptionToml>,
    /// Maximum time the script may exclusively hold a prompt it can respond to.
    #[serde(default = "default_response_timeout_ms")]
    pub response_timeout_ms: u64,
}

/// A capability that the host may grant to a configured session script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SessionScriptCapabilityToml {
    #[serde(rename = "userInput.send")]
    UserInputSend,
    #[serde(rename = "prompt.requestUserInput.respond")]
    PromptRequestUserInputRespond,
    #[serde(rename = "prompt.approval.respond")]
    PromptApprovalRespond,
}

/// A notification class that the host may deliver to a configured session script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SessionScriptSubscriptionToml {
    #[serde(rename = "modelResponseDeltas")]
    ModelResponseDeltas,
    #[serde(rename = "modelResponseCompleted")]
    ModelResponseCompleted,
    #[serde(rename = "userMessages")]
    UserMessages,
    #[serde(rename = "turnCompleted")]
    TurnCompleted,
    #[serde(rename = "sessionUpdates")]
    SessionUpdates,
    #[serde(rename = "prompts.requestUserInput")]
    PromptRequestUserInput,
    #[serde(rename = "prompts.extensionInteraction")]
    PromptExtensionInteraction,
    #[serde(rename = "prompts.commandExecutionApproval")]
    PromptCommandExecutionApproval,
    #[serde(rename = "prompts.fileChangeApproval")]
    PromptFileChangeApproval,
    #[serde(rename = "prompts.permissionsApproval")]
    PromptPermissionsApproval,
    #[serde(rename = "prompts.mcpElicitation")]
    PromptMcpElicitation,
}

impl SessionScriptConfigToml {
    /// Returns the non-empty argv accepted for a host-managed script.
    #[must_use]
    pub fn command_argv(&self) -> Option<&[String]> {
        self.command
            .first()
            .is_some_and(|program| !program.trim().is_empty())
            .then_some(self.command.as_slice())
    }

    /// Returns the host-enforced prompt response deadline.
    #[must_use]
    pub fn response_timeout(&self) -> Duration {
        Duration::from_millis(
            self.response_timeout_ms
                .clamp(MIN_RESPONSE_TIMEOUT_MS, MAX_RESPONSE_TIMEOUT_MS),
        )
    }
}

const fn default_response_timeout_ms() -> u64 {
    10_000
}
