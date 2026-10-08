use xedoc_protocol::error::Result as XedocResult;

use crate::session::session::Session;
use crate::session::turn::generate_sub_agent_activity_summary;
use crate::session::turn_context::TurnMultiAgentRuntime;

const MAX_RECENT_ACTIVITY_WORDS: usize = 100;

impl Session {
    /// Summarizes the recent output of a remote session with the fast activity-summary model.
    pub(crate) async fn generate_remote_session_activity_summary(
        &self,
        recent_output: &str,
    ) -> XedocResult<Option<String>> {
        let words = recent_output.split_whitespace().collect::<Vec<_>>();
        let tail = words[words.len().saturating_sub(MAX_RECENT_ACTIVITY_WORDS)..].join(" ");
        let session_configuration = {
            let state = self.state.lock().await;
            state.session_configuration.clone()
        };
        let turn_context = self
            .new_turn_context_from_configuration(
                "remote-activity".to_string(),
                session_configuration,
                /*final_output_json_schema*/ None,
                TurnMultiAgentRuntime::Preview,
            )
            .await;
        generate_sub_agent_activity_summary(self, turn_context.as_ref(), Some(&tail)).await
    }
}
