//! Durable provider-owned metadata attached to response items.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use ts_rs::TS;

/// Provider data required to replay a response item.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "provider", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderItemMetadata {
    /// Metadata required when replaying a Gemini tool call.
    Gemini {
        /// Opaque signature returned with the tool call.
        thought_signature: String,
    },
}

impl ProviderItemMetadata {
    /// Returns the Gemini thought signature when this metadata belongs to Gemini.
    pub fn gemini_thought_signature(&self) -> Option<&str> {
        match self {
            Self::Gemini { thought_signature } => Some(thought_signature),
        }
    }
}

impl fmt::Debug for ProviderItemMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gemini { .. } => formatter
                .debug_struct("Gemini")
                .field("thought_signature", &"[REDACTED]")
                .finish(),
        }
    }
}
