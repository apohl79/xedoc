use codex_core_context::is_contextual_user_fragment;
use codex_protocol::models::ContentItem;
use codex_protocol::protocol::COLLABORATION_MODE_OPEN_TAG;
use codex_protocol::protocol::CONTEXT_WINDOW_GUIDANCE_OPEN_TAG;
use codex_protocol::protocol::CONTEXT_WINDOW_OPEN_TAG;
use codex_protocol::protocol::ENVIRONMENTS_INSTRUCTIONS_OPEN_TAG;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_protocol::protocol::PLUGINS_INSTRUCTIONS_OPEN_TAG;
use codex_protocol::protocol::SKILLS_INSTRUCTIONS_OPEN_TAG;

const CONTEXTUAL_DEVELOPER_PREFIXES: &[&str] = &[
    "<permissions instructions>",
    "<model_switch>",
    COLLABORATION_MODE_OPEN_TAG,
    MULTI_AGENT_MODE_OPEN_TAG,
    ENVIRONMENTS_INSTRUCTIONS_OPEN_TAG,
    PLUGINS_INSTRUCTIONS_OPEN_TAG,
    SKILLS_INSTRUCTIONS_OPEN_TAG,
    "<personality_spec>",
    "<token_budget>",
    CONTEXT_WINDOW_OPEN_TAG,
    CONTEXT_WINDOW_GUIDANCE_OPEN_TAG,
    "<rollout_budget>",
];

/// Returns whether a user message is fully model-contextual.
pub fn is_contextual_user_message_content(message: &[ContentItem]) -> bool {
    message.iter().any(is_contextual_user_fragment)
}

/// Returns whether a developer message contains contextual fragments.
pub fn is_contextual_dev_message_content(message: &[ContentItem]) -> bool {
    message.iter().any(is_contextual_dev_fragment)
}

/// Returns whether a developer message contains persistent non-contextual text.
pub fn has_non_contextual_dev_message_content(message: &[ContentItem]) -> bool {
    message
        .iter()
        .any(|content_item| !is_contextual_dev_fragment(content_item))
}

fn is_contextual_dev_fragment(content_item: &ContentItem) -> bool {
    let ContentItem::InputText { text } = content_item else {
        return false;
    };

    let trimmed = text.trim_start();
    CONTEXTUAL_DEVELOPER_PREFIXES.iter().any(|prefix| {
        trimmed
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
    })
}
