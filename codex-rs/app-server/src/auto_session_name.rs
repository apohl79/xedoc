use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::thread_state::MidTurnAutoSessionNameRequest;
use crate::thread_state::ThreadState;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadNameUpdateSource;
use codex_app_server_protocol::ThreadNameUpdatedNotification;
use codex_core::CodexThread;
use codex_core::ThreadManager;
use codex_protocol::ThreadId;
use codex_state::ThreadTitleSource;
use codex_thread_store::ThreadMetadataPatch;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;
use tracing::error;
use tracing::warn;

const GENERATED_SESSION_NAME_REFRESH_TURN_INTERVAL: u64 = 4;

pub(crate) enum AutoSessionNameUpdate {
    TurnCompleted {
        completed_turn_count: u64,
        refresh_after_mid_turn_initial_name: bool,
    },
    MidTurn(MidTurnAutoSessionNameRequest),
}

impl AutoSessionNameUpdate {
    pub(crate) fn turn_completed(
        completed_turn_count: u64,
        refresh_after_mid_turn_initial_name: bool,
    ) -> Self {
        Self::TurnCompleted {
            completed_turn_count,
            refresh_after_mid_turn_initial_name,
        }
    }

    pub(crate) fn mid_turn(request: MidTurnAutoSessionNameRequest) -> Self {
        Self::MidTurn(request)
    }

    fn completed_turn_count(&self) -> u64 {
        match self {
            Self::TurnCompleted {
                completed_turn_count,
                ..
            } => *completed_turn_count,
            Self::MidTurn(request) => request.completed_turn_count,
        }
    }

    fn partial_response(&self) -> Option<&str> {
        match self {
            Self::TurnCompleted { .. } => None,
            Self::MidTurn(request) => Some(request.partial_response.as_str()),
        }
    }

    fn has_required_turn_progress(&self) -> bool {
        match self {
            Self::TurnCompleted {
                completed_turn_count,
                ..
            } => *completed_turn_count > 0,
            Self::MidTurn(_) => true,
        }
    }

    fn should_update_session_name(&self, title_source: ThreadTitleSource) -> bool {
        match (self, title_source) {
            (_, ThreadTitleSource::Manual) => false,
            (Self::MidTurn(_), ThreadTitleSource::Derived) => true,
            (Self::MidTurn(_), ThreadTitleSource::Generated) => false,
            (Self::TurnCompleted { .. }, ThreadTitleSource::Derived) => true,
            (
                Self::TurnCompleted {
                    refresh_after_mid_turn_initial_name: true,
                    ..
                },
                ThreadTitleSource::Generated,
            ) => true,
            (
                Self::TurnCompleted {
                    completed_turn_count,
                    refresh_after_mid_turn_initial_name: false,
                },
                ThreadTitleSource::Generated,
            ) => completed_turn_count.is_multiple_of(GENERATED_SESSION_NAME_REFRESH_TURN_INTERVAL),
        }
    }

    fn still_applies_to(&self, state: &ThreadState) -> bool {
        match self {
            Self::TurnCompleted {
                completed_turn_count,
                ..
            } => state.completed_turn_count == *completed_turn_count,
            Self::MidTurn(request) => {
                state.completed_turn_count == request.completed_turn_count
                    && state.active_turn_id_if_explicit().as_deref()
                        == Some(request.turn_id.as_str())
            }
        }
    }
}

pub(crate) fn maybe_spawn_auto_session_name_update(
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    thread_manager: Arc<ThreadManager>,
    outgoing: ThreadScopedOutgoingMessageSender,
    thread_state: Arc<Mutex<ThreadState>>,
    thread_list_state_permit: Arc<Semaphore>,
    update: AutoSessionNameUpdate,
) {
    tokio::spawn(async move {
        if let Err(err) = maybe_update_auto_session_name(
            thread_id,
            thread,
            thread_manager,
            outgoing,
            thread_state,
            thread_list_state_permit,
            update,
        )
        .await
        {
            warn!("failed to update generated session name for thread {thread_id}: {err}");
        }
    });
}

async fn maybe_update_auto_session_name(
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    thread_manager: Arc<ThreadManager>,
    outgoing: ThreadScopedOutgoingMessageSender,
    thread_state: Arc<Mutex<ThreadState>>,
    thread_list_state_permit: Arc<Semaphore>,
    update: AutoSessionNameUpdate,
) -> anyhow::Result<()> {
    if !update.has_required_turn_progress() {
        return Ok(());
    }
    let config = thread.config().await;
    if !config.auto_session_name {
        return Ok(());
    }
    if thread.config_snapshot().await.ephemeral {
        return Ok(());
    }

    let Some((current_title, title_source)) = read_title_state(&thread, thread_id).await? else {
        return Ok(());
    };
    let completed_turn_count = update.completed_turn_count();
    if !update.should_update_session_name(title_source) {
        return Ok(());
    }

    let current_name = (!current_title.trim().is_empty()).then_some(current_title.as_str());
    let Some(generated_name) = thread
        .generate_session_name_with_partial_response(current_name, update.partial_response())
        .await?
    else {
        return Ok(());
    };

    let _permit = thread_list_state_permit
        .acquire_owned()
        .await
        .map_err(|err| anyhow::anyhow!("thread list state permit closed: {err}"))?;
    let config = thread.config().await;
    if !config.auto_session_name || thread.config_snapshot().await.ephemeral {
        return Ok(());
    }
    let latest_completed_turn_count = {
        let state = thread_state.lock().await;
        if !update.still_applies_to(&state) {
            return Ok(());
        }
        state.completed_turn_count
    };
    let Some((latest_title, latest_source)) = read_title_state(&thread, thread_id).await? else {
        return Ok(());
    };
    if !should_persist_generated_name(
        completed_turn_count,
        latest_completed_turn_count,
        current_title.as_str(),
        title_source,
        latest_title.as_str(),
        latest_source,
        generated_name.as_str(),
    ) {
        return Ok(());
    }

    thread_manager
        .update_thread_metadata(
            thread_id,
            ThreadMetadataPatch {
                name: Some(Some(generated_name.clone())),
                title_source: Some(ThreadTitleSource::Generated),
                ..Default::default()
            },
            /*include_archived*/ false,
        )
        .await
        .map_err(|err| anyhow::anyhow!("failed to persist generated name: {err}"))?;

    outgoing
        .send_server_notification(ServerNotification::ThreadNameUpdated(
            ThreadNameUpdatedNotification {
                thread_id: thread_id.to_string(),
                thread_name: Some(generated_name),
                source: ThreadNameUpdateSource::Generated,
            },
        ))
        .await;
    Ok(())
}

async fn read_title_state(
    thread: &CodexThread,
    thread_id: ThreadId,
) -> anyhow::Result<Option<(String, ThreadTitleSource)>> {
    let Some(state_db) = thread.state_db() else {
        return Ok(None);
    };
    let metadata = state_db.get_thread(thread_id).await.map_err(|err| {
        error!("failed to read thread metadata for generated session name: {err}");
        err
    })?;
    Ok(metadata.map(|metadata| (metadata.title, metadata.title_source)))
}

fn should_persist_generated_name(
    completed_turn_count: u64,
    latest_completed_turn_count: u64,
    initial_title: &str,
    initial_source: ThreadTitleSource,
    latest_title: &str,
    latest_source: ThreadTitleSource,
    generated_name: &str,
) -> bool {
    if completed_turn_count != latest_completed_turn_count {
        return false;
    }
    if latest_source == ThreadTitleSource::Manual {
        return false;
    }
    if latest_source != initial_source || latest_title.trim() != initial_title.trim() {
        return false;
    }
    latest_source != ThreadTitleSource::Generated || latest_title.trim() != generated_name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_trigger_respects_source_and_interval() {
        let mid_turn = AutoSessionNameUpdate::mid_turn(MidTurnAutoSessionNameRequest {
            turn_id: "turn-1".to_string(),
            completed_turn_count: 0,
            partial_response: "partial".to_string(),
        });
        let turn_completed = AutoSessionNameUpdate::turn_completed(
            1, /*refresh_after_mid_turn_initial_name*/ false,
        );
        let interval_turn = AutoSessionNameUpdate::turn_completed(
            4, /*refresh_after_mid_turn_initial_name*/ false,
        );
        let initial_final_turn = AutoSessionNameUpdate::turn_completed(
            1, /*refresh_after_mid_turn_initial_name*/ true,
        );

        assert!(mid_turn.should_update_session_name(ThreadTitleSource::Derived));
        assert!(!mid_turn.should_update_session_name(ThreadTitleSource::Generated));
        assert!(!turn_completed.should_update_session_name(ThreadTitleSource::Manual));
        assert!(!turn_completed.should_update_session_name(ThreadTitleSource::Generated));
        assert!(interval_turn.should_update_session_name(ThreadTitleSource::Generated));
        assert!(initial_final_turn.should_update_session_name(ThreadTitleSource::Generated));
    }

    #[test]
    fn should_persist_generated_name_rejects_stale_or_changed_state() {
        assert!(should_persist_generated_name(
            4,
            4,
            "Old title",
            ThreadTitleSource::Generated,
            "Old title",
            ThreadTitleSource::Generated,
            "New title",
        ));
        assert!(!should_persist_generated_name(
            4,
            5,
            "Old title",
            ThreadTitleSource::Generated,
            "Old title",
            ThreadTitleSource::Generated,
            "New title",
        ));
        assert!(!should_persist_generated_name(
            4,
            4,
            "Old title",
            ThreadTitleSource::Generated,
            "Manual title",
            ThreadTitleSource::Manual,
            "New title",
        ));
        assert!(!should_persist_generated_name(
            4,
            4,
            "Old title",
            ThreadTitleSource::Generated,
            "Other generated title",
            ThreadTitleSource::Generated,
            "New title",
        ));
    }
}
