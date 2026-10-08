//! Periodic model-generated activity summaries for running remote sessions.

use std::time::Duration;

use xedoc_protocol::ThreadId;

use super::thread_processor::ThreadRequestProcessor;
use crate::remote_session_registry::RemoteSessionProjectionUpdate;

const ACTIVITY_TICK: Duration = Duration::from_secs(12);

impl ThreadRequestProcessor {
    /// Starts the root thread's activity timer unless one is already running.
    ///
    /// Like local sub-agents, every tick summarizes sessions that produced new output with the
    /// fast model and publishes the result as the session's activity text.
    pub(super) async fn ensure_remote_activity_timer(&self, root_thread_id: ThreadId) {
        if !self
            .remote_session_registry
            .claim_activity_timer(root_thread_id)
            .await
        {
            return;
        }
        let processor = self.clone();
        self.background_tasks.spawn(async move {
            processor.run_remote_activity_timer(root_thread_id).await;
        });
    }

    async fn run_remote_activity_timer(&self, root_thread_id: ThreadId) {
        loop {
            tokio::time::sleep(ACTIVITY_TICK).await;
            let Some(inputs) = self
                .remote_session_registry
                .take_activity_inputs(root_thread_id)
                .await
            else {
                return;
            };
            let Ok(thread) = self.thread_manager.get_thread(root_thread_id).await else {
                self.remote_session_registry
                    .release_activity_timer(root_thread_id)
                    .await;
                return;
            };
            for (remote_session_id, recent_output) in inputs {
                let summary = thread
                    .generate_remote_session_activity_summary(&recent_output)
                    .await;
                let update: Option<RemoteSessionProjectionUpdate> = match summary {
                    Ok(Some(summary)) => {
                        self.remote_session_registry
                            .set_generated_activity(root_thread_id, &remote_session_id, summary)
                            .await
                    }
                    Ok(None) | Err(_) => {
                        self.remote_session_registry
                            .retry_activity(&remote_session_id)
                            .await;
                        None
                    }
                };
                if let Some(update) = update {
                    self.send_remote_session_updated(root_thread_id, update)
                        .await;
                }
            }
        }
    }
}
