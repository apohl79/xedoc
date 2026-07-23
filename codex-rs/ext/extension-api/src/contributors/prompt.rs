// All this file should be replaced by the existing fragment implementation ofc

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PromptSlot {
    DeveloperPolicy,
    DeveloperCapabilities,
    ContextualUser,
    SeparateDeveloper,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptFragment {
    slot: PromptSlot,
    text: String,
    /// Optional position relative to the world-state (AGENTS.md) user message
    /// within its slot. `None` means default ordering (appended after preamble
    /// entries and before supplement entries).
    pub position: Option<PluginContextPosition>,
}

impl PromptFragment {
    /// Creates a prompt fragment for the given slot.
    pub fn new(slot: PromptSlot, text: impl Into<String>) -> Self {
        Self {
            slot,
            text: text.into(),
            position: None,
        }
    }

    /// Sets the position of this fragment relative to AGENTS.md.
    pub fn with_position(mut self, position: PluginContextPosition) -> Self {
        self.position = Some(position);
        self
    }

    /// Creates a developer-policy prompt fragment.
    pub fn developer_policy(text: impl Into<String>) -> Self {
        Self::new(PromptSlot::DeveloperPolicy, text)
    }

    /// Creates a developer-capabilities prompt fragment.
    pub fn developer_capability(text: impl Into<String>) -> Self {
        Self::new(PromptSlot::DeveloperCapabilities, text)
    }

    /// Creates a separate top-level developer prompt fragment.
    pub fn separate_developer(text: impl Into<String>) -> Self {
        Self::new(PromptSlot::SeparateDeveloper, text)
    }

    /// Returns the target prompt slot.
    pub fn slot(&self) -> PromptSlot {
        self.slot
    }

    /// Returns the model-visible text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Position of a plugin context entry relative to AGENTS.md.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginContextPosition {
    /// Inserted before AGENTS.md — foundational instructions.
    Preamble,
    /// Inserted after AGENTS.md — supplementary instructions.
    Supplement,
}
