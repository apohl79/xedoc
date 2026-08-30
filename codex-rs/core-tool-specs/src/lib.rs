//! Model-visible tool schemas shared by Codex core tool runtimes.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod apply_patch_spec;
pub mod get_context_remaining_spec;
pub mod hosted_spec;
pub mod mcp_resource_spec;
pub mod multi_agents_spec;
pub mod new_context_window_spec;
pub mod plan_spec;
pub mod request_user_input_spec;
pub mod shell_spec;
pub mod test_sync_spec;
pub mod tool_search_spec;
pub mod view_image_spec;

use codex_protocol::openai_models::ModelPreset;
use codex_protocol::protocol::MultiAgentVersion;

#[doc(hidden)]
pub mod multi_agents_common {
    use super::*;

    pub const MAX_SPAWN_AGENT_MODEL_OVERRIDES: usize = 5;

    pub fn model_supports_multi_agent_backend(
        model: &ModelPreset,
        multi_agent_version: MultiAgentVersion,
    ) -> bool {
        multi_agent_version != MultiAgentVersion::V2
            || model.multi_agent_version == Some(multi_agent_version)
    }
}
