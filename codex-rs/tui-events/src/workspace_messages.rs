pub use crate::WorkspaceHeadlineFetchResult;
use codex_app_server_protocol::GetWorkspaceMessagesResponse;
use codex_app_server_protocol::WorkspaceMessageType;
use std::time::Duration;

pub const WORKSPACE_HEADLINE_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub fn workspace_headline_from_response(
    response: GetWorkspaceMessagesResponse,
) -> WorkspaceHeadlineFetchResult {
    if !response.feature_enabled {
        return WorkspaceHeadlineFetchResult::FeatureDisabled;
    }

    WorkspaceHeadlineFetchResult::Available(response.messages.into_iter().find_map(|message| {
        (message.message_type == WorkspaceMessageType::Headline)
            .then(|| message.message_body.trim().to_string())
            .filter(|headline| !headline.is_empty())
    }))
}

#[cfg(test)]
#[path = "workspace_messages_tests.rs"]
mod tests;
