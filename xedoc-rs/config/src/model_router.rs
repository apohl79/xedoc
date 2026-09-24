//! Host configuration for the scripted model router.

use std::collections::BTreeMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Bounded host controls in `[model_router]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRouterConfigToml {
    /// Direct-exec argv for the sole model-router script.
    pub script: Option<Vec<String>>,
    /// Maximum wall-clock duration for one route decision.
    #[serde(default = "default_decision_timeout_ms")]
    pub decision_timeout_ms: u64,
    /// Maximum wall-clock duration for one settings interaction.
    #[serde(default = "default_interaction_timeout_ms")]
    pub interaction_timeout_ms: u64,
    /// Bounded conversation context supplied to the script.
    #[serde(default)]
    pub context: ModelRouterContextToml,
    /// Forward-compatible fields preserved only long enough to report a startup warning.
    #[serde(default, flatten, skip_serializing)]
    #[schemars(skip)]
    pub ignored_fields: BTreeMap<String, IgnoredModelRouterField>,
}

impl Default for ModelRouterConfigToml {
    fn default() -> Self {
        Self {
            script: None,
            decision_timeout_ms: default_decision_timeout_ms(),
            interaction_timeout_ms: default_interaction_timeout_ms(),
            context: ModelRouterContextToml::default(),
            ignored_fields: BTreeMap::new(),
        }
    }
}

/// Bounded conversation context supplied to a model-router script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRouterContextToml {
    /// Maximum number of recent user and final-assistant messages.
    #[serde(default = "default_recent_messages")]
    pub recent_messages: usize,
    /// Maximum UTF-8 byte length retained for each recent message.
    #[serde(default = "default_max_recent_message_bytes")]
    pub max_recent_message_bytes: usize,
}

impl Default for ModelRouterContextToml {
    fn default() -> Self {
        Self {
            recent_messages: default_recent_messages(),
            max_recent_message_bytes: default_max_recent_message_bytes(),
        }
    }
}

impl ModelRouterContextToml {
    /// Returns the host-enforced maximum number of recent messages.
    #[must_use]
    pub fn bounded_recent_messages(&self) -> usize {
        self.recent_messages.min(MAX_RECENT_MESSAGES)
    }

    /// Returns the host-enforced maximum byte length for one recent message.
    #[must_use]
    pub fn bounded_max_recent_message_bytes(&self) -> usize {
        self.max_recent_message_bytes.min(MAX_RECENT_MESSAGE_BYTES)
    }
}

const MAX_RECENT_MESSAGES: usize = 32;
const MAX_RECENT_MESSAGE_BYTES: usize = 65_536;

const fn default_recent_messages() -> usize {
    8
}

const fn default_max_recent_message_bytes() -> usize {
    24_576
}

const fn default_decision_timeout_ms() -> u64 {
    3_000
}

const fn default_interaction_timeout_ms() -> u64 {
    10_000
}

/// Placeholder used to retain an ignored model-router field name without its value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IgnoredModelRouterField;

impl<'de> Deserialize<'de> for IgnoredModelRouterField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::de::IgnoredAny::deserialize(deserializer).map(|_| Self)
    }
}

impl ModelRouterConfigToml {
    /// Returns a non-empty direct-exec argv for the configured router script.
    #[must_use]
    pub fn script_argv(&self) -> Option<Vec<String>> {
        self.script
            .as_ref()
            .filter(|argv| argv.first().is_some_and(|program| !program.is_empty()))
            .cloned()
    }

    /// Returns the host-enforced route-decision timeout.
    #[must_use]
    pub fn decision_timeout(&self) -> Duration {
        Duration::from_millis(
            self.decision_timeout_ms
                .clamp(MIN_SCRIPT_TIMEOUT_MS, MAX_SCRIPT_TIMEOUT_MS),
        )
    }

    /// Returns the host-enforced interaction timeout.
    #[must_use]
    pub fn interaction_timeout(&self) -> Duration {
        Duration::from_millis(
            self.interaction_timeout_ms
                .clamp(MIN_SCRIPT_TIMEOUT_MS, MAX_SCRIPT_TIMEOUT_MS),
        )
    }
}

const MIN_SCRIPT_TIMEOUT_MS: u64 = 100;
const MAX_SCRIPT_TIMEOUT_MS: u64 = 60_000;
