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
    /// Opaque continuity data issued by an Anthropic Messages API model.
    Anthropic {
        /// Configured provider identifier that issued the item.
        provider_id: String,
        /// Model that issued the thinking signature.
        model: String,
        /// Opaque signature returned with the thinking block.
        thinking_signature: Option<String>,
    },
    /// Metadata required when replaying a Gemini tool call.
    Gemini {
        /// Configured provider identifier that issued the item.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        provider_id: String,
        /// Opaque signature returned with the tool call.
        thought_signature: String,
    },
    /// Opaque continuity data issued by a Responses-compatible provider.
    Responses {
        /// Configured provider identifier that issued the item.
        provider_id: String,
    },
}

impl ProviderItemMetadata {
    /// Returns the Anthropic thinking signature when this metadata belongs to the target model.
    pub fn anthropic_thinking_signature(
        &self,
        provider_id: &str,
        model: &str,
    ) -> Option<Option<&str>> {
        match self {
            Self::Anthropic {
                provider_id: item_provider_id,
                model: item_model,
                thinking_signature,
            } if item_provider_id == provider_id && item_model == model => {
                Some(thinking_signature.as_deref())
            }
            Self::Anthropic { .. } | Self::Gemini { .. } | Self::Responses { .. } => None,
        }
    }

    /// Returns whether this metadata belongs to the configured Responses provider.
    pub fn belongs_to_responses_provider(&self, provider_id: &str) -> bool {
        matches!(
            self,
            Self::Responses {
                provider_id: item_provider_id
            } if item_provider_id == provider_id
        )
    }

    /// Returns whether this metadata belongs to the configured Gemini provider.
    ///
    /// Metadata written before provider provenance was tracked is accepted for
    /// backwards compatibility.
    pub fn belongs_to_gemini_provider(&self, provider_id: &str) -> bool {
        match self {
            Self::Gemini {
                provider_id: item_provider_id,
                ..
            } => item_provider_id.is_empty() || item_provider_id == provider_id,
            Self::Anthropic { .. } | Self::Responses { .. } => false,
        }
    }

    /// Returns the Gemini thought signature when this metadata belongs to Gemini.
    pub fn gemini_thought_signature(&self) -> Option<&str> {
        match self {
            Self::Gemini {
                thought_signature, ..
            } => Some(thought_signature),
            Self::Anthropic { .. } | Self::Responses { .. } => None,
        }
    }
}

impl fmt::Debug for ProviderItemMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anthropic { .. } => formatter
                .debug_struct("Anthropic")
                .field("thinking_signature", &"[REDACTED]")
                .finish(),
            Self::Gemini { .. } => formatter
                .debug_struct("Gemini")
                .field("thought_signature", &"[REDACTED]")
                .finish(),
            Self::Responses { .. } => formatter.debug_struct("Responses").finish(),
        }
    }
}
