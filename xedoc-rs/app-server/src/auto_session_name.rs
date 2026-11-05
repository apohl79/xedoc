use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::session_script_registry::SessionScriptRegistry;
use crate::thread_state::MidTurnAutoSessionNameRequest;
use crate::thread_state::ThreadState;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;
use tracing::debug;
use tracing::error;
use tracing::warn;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ThreadNameUpdateSource;
use xedoc_app_server_protocol::ThreadNameUpdatedNotification;
use xedoc_app_server_protocol::TurnStatus;
use xedoc_core::ThreadManager;
use xedoc_core::XedocThread;
use xedoc_protocol::ThreadId;
use xedoc_state::ThreadTitleSource;
use xedoc_thread_store::ThreadMetadataPatch;

const GENERATED_SESSION_NAME_REFRESH_TURN_INTERVAL: u64 = 4;
const GENERATED_SESSION_NAME_MAX_ATTEMPTS: u64 = 3;
const GENERATED_SESSION_NAME_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

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

    fn kind(&self) -> &'static str {
        match self {
            Self::TurnCompleted { .. } => "turn_completed",
            Self::MidTurn(_) => "mid_turn",
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
                    && state.active_turn_snapshot().is_some_and(|turn| {
                        turn.id == request.turn_id && turn.status == TurnStatus::InProgress
                    })
            }
        }
    }
}

pub(crate) fn maybe_spawn_auto_session_name_update(
    thread_id: ThreadId,
    thread: Arc<XedocThread>,
    thread_manager: Arc<ThreadManager>,
    outgoing: ThreadScopedOutgoingMessageSender,
    session_script_registry: SessionScriptRegistry,
    session_script_outgoing: Arc<OutgoingMessageSender>,
    thread_state: Arc<Mutex<ThreadState>>,
    thread_list_state_permit: Arc<Semaphore>,
    update: AutoSessionNameUpdate,
) {
    let update_kind = update.kind();
    let completed_turn_count = update.completed_turn_count();
    tokio::spawn(async move {
        if let Err(err) = maybe_update_auto_session_name(
            thread_id,
            thread,
            thread_manager,
            outgoing,
            session_script_registry,
            session_script_outgoing,
            thread_state,
            thread_list_state_permit,
            update,
        )
        .await
        {
            error!(
                %thread_id,
                update_kind,
                completed_turn_count,
                error = %err,
                "failed to update generated session name"
            );
        }
    });
}

async fn maybe_update_auto_session_name(
    thread_id: ThreadId,
    thread: Arc<XedocThread>,
    thread_manager: Arc<ThreadManager>,
    outgoing: ThreadScopedOutgoingMessageSender,
    session_script_registry: SessionScriptRegistry,
    session_script_outgoing: Arc<OutgoingMessageSender>,
    thread_state: Arc<Mutex<ThreadState>>,
    thread_list_state_permit: Arc<Semaphore>,
    update: AutoSessionNameUpdate,
) -> anyhow::Result<()> {
    let update_kind = update.kind();
    let completed_turn_count = update.completed_turn_count();
    if !update.has_required_turn_progress() {
        debug!(
            thread_id = %thread_id,
            update_kind = update_kind,
            completed_turn_count = completed_turn_count,
            "skipping generated session name: no completed turn progress"
        );
        return Ok(());
    }
    let config = thread.config().await;
    if !config.auto_session_name {
        debug!(
            thread_id = %thread_id,
            update_kind = update_kind,
            completed_turn_count = completed_turn_count,
            "skipping generated session name: auto_session_name disabled"
        );
        return Ok(());
    }
    if thread.config_snapshot().await.ephemeral {
        debug!(
            thread_id = %thread_id,
            update_kind = update_kind,
            completed_turn_count = completed_turn_count,
            "skipping generated session name: ephemeral thread"
        );
        return Ok(());
    }

    let Some((current_title, title_source)) = read_title_state(&thread, thread_id).await? else {
        debug!(
            thread_id = %thread_id,
            update_kind = update_kind,
            completed_turn_count = completed_turn_count,
            "skipping generated session name: missing thread title metadata"
        );
        return Ok(());
    };
    if !update.should_update_session_name(title_source) {
        debug!(
            thread_id = %thread_id,
            update_kind = update_kind,
            completed_turn_count = completed_turn_count,
            ?title_source,
            "skipping generated session name: title source is not eligible"
        );
        return Ok(());
    }

    let current_name = (!current_title.trim().is_empty()).then_some(current_title.as_str());
    let started = Instant::now();
    let mut attempt = 0;
    let generated_name = loop {
        if !update.still_applies_to(&*thread_state.lock().await) {
            debug!(
                %thread_id,
                update_kind,
                attempt,
                "stopping generated session name attempts: turn changed"
            );
            return Ok(());
        }
        if attempt > 0 {
            let latest_config = thread.config().await;
            if !latest_config.auto_session_name
                || thread.config_snapshot().await.ephemeral
                || read_title_state(&thread, thread_id)
                    .await?
                    .is_none_or(|(title, source)| title != current_title || source != title_source)
            {
                debug!(
                    %thread_id,
                    update_kind,
                    attempt,
                    "stopping generated session name attempts: settings or title changed"
                );
                return Ok(());
            }
        }
        attempt += 1;
        let failure = match tokio::time::timeout(
            GENERATED_SESSION_NAME_ATTEMPT_TIMEOUT,
            thread.generate_session_name_with_partial_response(
                current_name,
                update.partial_response(),
            ),
        )
        .await
        {
            Ok(Ok(Some(name))) => break name,
            Ok(Ok(None)) => "model returned no usable name".to_string(),
            Ok(Err(err)) => err.to_string(),
            Err(_) => "session name request timed out".to_string(),
        };
        if attempt == GENERATED_SESSION_NAME_MAX_ATTEMPTS {
            error!(
                %thread_id,
                update_kind,
                completed_turn_count,
                provider = %config.model_provider_id,
                session_model = ?config.model,
                attempts = attempt,
                timeout_ms = GENERATED_SESSION_NAME_ATTEMPT_TIMEOUT.as_millis(),
                elapsed_ms = started.elapsed().as_millis(),
                error = %failure,
                "automatic session naming failed after all attempts"
            );
            return Ok(());
        }
        warn!(
            %thread_id,
            update_kind,
            completed_turn_count,
            provider = %config.model_provider_id,
            session_model = ?config.model,
            attempt,
            max_attempts = GENERATED_SESSION_NAME_MAX_ATTEMPTS,
            timeout_ms = GENERATED_SESSION_NAME_ATTEMPT_TIMEOUT.as_millis(),
            elapsed_ms = started.elapsed().as_millis(),
            error = %failure,
            "generated session name attempt failed; retrying"
        );
        tokio::time::sleep(Duration::from_secs(attempt)).await;
    };

    let _permit = thread_list_state_permit
        .acquire_owned()
        .await
        .map_err(|err| anyhow::anyhow!("thread list state permit closed: {err}"))?;
    let config = thread.config().await;
    if !config.auto_session_name || thread.config_snapshot().await.ephemeral {
        debug!(
            thread_id = %thread_id,
            update_kind = update_kind,
            completed_turn_count = completed_turn_count,
            "skipping generated session name: settings changed before persist"
        );
        return Ok(());
    }
    let latest_completed_turn_count = {
        let mut state = thread_state.lock().await;
        if !update.still_applies_to(&state) {
            debug!(
                thread_id = %thread_id,
                update_kind = update_kind,
                completed_turn_count = completed_turn_count,
                latest_completed_turn_count = state.completed_turn_count,
                "skipping generated session name: update no longer applies"
            );
            return Ok(());
        }
        if let AutoSessionNameUpdate::MidTurn(request) = &update {
            state.note_mid_turn_auto_session_name_applied(&request.turn_id);
        }
        state.completed_turn_count
    };
    let Some((latest_title, latest_source)) = read_title_state(&thread, thread_id).await? else {
        debug!(
            thread_id = %thread_id,
            update_kind = update_kind,
            completed_turn_count = completed_turn_count,
            "skipping generated session name: missing latest thread title metadata"
        );
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
        debug!(
            thread_id = %thread_id,
            update_kind = update_kind,
            completed_turn_count = completed_turn_count,
            latest_completed_turn_count = latest_completed_turn_count,
            ?title_source,
            ?latest_source,
            "skipping generated session name: title state changed before persist"
        );
        return Ok(());
    }

    debug!(
        thread_id = %thread_id,
        update_kind = update_kind,
        completed_turn_count = completed_turn_count,
        generated_name_chars = generated_name.chars().count(),
        "persisting generated session name"
    );
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
                thread_name: Some(generated_name.clone()),
                source: ThreadNameUpdateSource::Generated,
            },
        ))
        .await;
    session_script_registry
        .publish_session_title_updated(&session_script_outgoing, thread_id, Some(generated_name))
        .await;
    Ok(())
}

async fn read_title_state(
    thread: &XedocThread,
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
