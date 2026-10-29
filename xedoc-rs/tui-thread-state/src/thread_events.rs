//! Thread event buffering and replay state for the TUI app.
//!
//! This module owns the per-thread event store used when the TUI switches between the main
//! conversation, subagents, and side conversations. It keeps buffered app-server notifications,
//! pending interactive request replay state, active-turn tracking, and saved composer state close
//! together with the replay behavior that consumes them.

use super::*;
use std::borrow::Cow;

#[derive(Debug, Clone)]
pub struct ThreadEventSnapshot {
    pub session: Option<ThreadSessionState>,
    pub turns: Vec<Turn>,
    pub events: Vec<ThreadBufferedEvent>,
    pub input_state: Option<ThreadInputState>,
}

#[derive(Debug, Clone)]
pub enum ThreadBufferedEvent {
    Notification(ServerNotification),
    Request(ServerRequest),
    HistoryEntryResponse(HistoryLookupResponse),
}

#[derive(Debug, Clone)]
pub struct PendingPrimaryThreadEvent {
    pub thread_id: ThreadId,
    pub event: ThreadBufferedEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadEventAttachment {
    Live,
    ReplayOnly,
}

#[derive(Debug)]
pub struct ThreadEventStore {
    pub session: Option<ThreadSessionState>,
    pub turns: Vec<Turn>,
    pub buffer: VecDeque<ThreadBufferedEvent>,
    pub pending_interactive_replay: PendingInteractiveReplayState,
    pub active_turn_id: Option<String>,
    pub input_state: Option<ThreadInputState>,
    pub capacity: usize,
    pub active: bool,
}

impl ThreadEventStore {
    pub fn event_survives_session_refresh(event: &ThreadBufferedEvent) -> bool {
        matches!(
            event,
            ThreadBufferedEvent::Request(_)
                | ThreadBufferedEvent::Notification(ServerNotification::HookStarted(_))
                | ThreadBufferedEvent::Notification(ServerNotification::HookCompleted(_))
                | ThreadBufferedEvent::Notification(ServerNotification::McpServerStatusUpdated(_))
        )
    }

    pub fn new(capacity: usize) -> Self {
        Self {
            session: None,
            turns: Vec::new(),
            buffer: VecDeque::new(),
            pending_interactive_replay: PendingInteractiveReplayState::default(),
            active_turn_id: None,
            input_state: None,
            capacity,
            active: false,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new_with_session(
        capacity: usize,
        session: ThreadSessionState,
        turns: Vec<Turn>,
    ) -> Self {
        let mut store = Self::new(capacity);
        store.session = Some(session);
        store.set_turns(turns);
        store
    }

    pub fn set_session(&mut self, session: ThreadSessionState, turns: Vec<Turn>) {
        self.session = Some(session);
        self.set_turns(turns);
    }

    pub fn rebase_buffer_after_session_refresh(&mut self) {
        self.buffer.retain(Self::event_survives_session_refresh);
    }

    /// Drops buffered events and pending request state that belonged to a disconnected server.
    ///
    /// A local daemon restart invalidates JSON-RPC request IDs, so retaining approvals or other
    /// buffered transport events would let the TUI send stale resolutions to the replacement
    /// connection. `thread/resume` repopulates the authoritative thread state after reconnecting.
    pub fn reset_after_app_server_reconnect(&mut self) {
        self.buffer.clear();
        self.pending_interactive_replay.clear();
    }

    pub fn set_turns(&mut self, turns: Vec<Turn>) {
        self.active_turn_id = turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.status, TurnStatus::InProgress))
            .map(|turn| turn.id.clone());
        self.turns = turns;
    }

    pub fn push_notification(&mut self, notification: ServerNotification) {
        self.push_notification_inner(Cow::Owned(notification));
    }

    pub fn push_notification_ref(&mut self, notification: &ServerNotification) {
        self.push_notification_inner(Cow::Borrowed(notification));
    }

    fn push_notification_inner(&mut self, notification: Cow<'_, ServerNotification>) {
        self.pending_interactive_replay
            .note_server_notification(notification.as_ref());
        match notification.as_ref() {
            ServerNotification::TurnStarted(turn) => {
                self.active_turn_id = Some(turn.turn.id.clone());
            }
            ServerNotification::TurnCompleted(turn)
                if self.active_turn_id.as_deref() == Some(turn.turn.id.as_str()) =>
            {
                self.active_turn_id = None;
            }
            ServerNotification::ThreadClosed(_) => {
                self.active_turn_id = None;
            }
            _ => {}
        }

        // These notifications are either handled before routing or ignored by ChatWidget on
        // replay. In particular, raw response items and streamed output can carry large payloads,
        // so cloning them into every thread's replay buffer only retains data the TUI cannot use.
        if matches!(
            notification.as_ref(),
            ServerNotification::RawResponseItemCompleted(_)
                | ServerNotification::FileChangePatchUpdated(_)
                | ServerNotification::ServerRequestResolved(_)
                | ServerNotification::McpToolCallProgress(_)
                | ServerNotification::CommandExecOutputDelta(_)
                | ServerNotification::ProcessOutputDelta(_)
                | ServerNotification::ProcessExited(_)
        ) {
            return;
        }

        self.buffer
            .push_back(ThreadBufferedEvent::Notification(notification.into_owned()));
        if self.buffer.len() > self.capacity
            && let Some(removed) = self.buffer.pop_front()
            && let ThreadBufferedEvent::Request(request) = &removed
        {
            self.pending_interactive_replay
                .note_evicted_server_request(request);
        }
    }

    pub fn push_request(&mut self, request: ServerRequest) {
        self.pending_interactive_replay
            .note_server_request(&request);
        self.buffer.push_back(ThreadBufferedEvent::Request(request));
        if self.buffer.len() > self.capacity
            && let Some(removed) = self.buffer.pop_front()
            && let ThreadBufferedEvent::Request(request) = &removed
        {
            self.pending_interactive_replay
                .note_evicted_server_request(request);
        }
    }

    pub fn pending_replay_requests(&self) -> Vec<ServerRequest> {
        self.buffer
            .iter()
            .filter_map(|event| match event {
                ThreadBufferedEvent::Request(request)
                    if self
                        .pending_interactive_replay
                        .should_replay_snapshot_request(request) =>
                {
                    Some(request.clone())
                }
                ThreadBufferedEvent::Request(_)
                | ThreadBufferedEvent::Notification(_)
                | ThreadBufferedEvent::HistoryEntryResponse(_) => None,
            })
            .collect()
    }

    pub fn file_change_changes(
        &self,
        turn_id: &str,
        item_id: &str,
    ) -> Option<Vec<xedoc_app_server_protocol::FileUpdateChange>> {
        self.buffer
            .iter()
            .rev()
            .find_map(|event| match event {
                ThreadBufferedEvent::Notification(ServerNotification::ItemStarted(
                    notification,
                )) if turn_id_matches(turn_id, &notification.turn_id) => {
                    file_change_item_changes(&notification.item, item_id)
                }
                ThreadBufferedEvent::Notification(ServerNotification::ItemCompleted(
                    notification,
                )) if turn_id_matches(turn_id, &notification.turn_id) => {
                    file_change_item_changes(&notification.item, item_id)
                }
                ThreadBufferedEvent::Request(_)
                | ThreadBufferedEvent::Notification(_)
                | ThreadBufferedEvent::HistoryEntryResponse(_) => None,
            })
            .or_else(|| {
                self.turns
                    .iter()
                    .rev()
                    .filter(|turn| turn_id_matches(turn_id, &turn.id))
                    .flat_map(|turn| turn.items.iter().rev())
                    .find_map(|item| file_change_item_changes(item, item_id))
            })
    }

    pub fn snapshot(&self) -> ThreadEventSnapshot {
        ThreadEventSnapshot {
            session: self.session.clone(),
            turns: self.turns.clone(),
            // Thread switches replay buffered events into a rebuilt ChatWidget. Only replay
            // interactive prompts that are still pending, or answered approvals/input will reappear.
            events: self
                .buffer
                .iter()
                .filter(|event| match event {
                    ThreadBufferedEvent::Request(request) => self
                        .pending_interactive_replay
                        .should_replay_snapshot_request(request),
                    ThreadBufferedEvent::Notification(_)
                    | ThreadBufferedEvent::HistoryEntryResponse(_) => true,
                })
                .cloned()
                .collect(),
            input_state: self.input_state.clone(),
        }
    }

    pub fn note_outbound_op<T>(&mut self, op: T)
    where
        T: Into<AppCommand>,
    {
        self.pending_interactive_replay.note_outbound_op(op);
    }

    pub fn op_can_change_pending_replay_state<T>(op: T) -> bool
    where
        T: Into<AppCommand>,
    {
        PendingInteractiveReplayState::op_can_change_state(op)
    }

    pub fn has_pending_thread_approvals(&self) -> bool {
        self.pending_interactive_replay
            .has_pending_thread_approvals()
    }

    pub fn pending_interaction(&self) -> Option<PendingThreadInteraction> {
        if self
            .pending_interactive_replay
            .has_pending_thread_user_input()
        {
            Some(PendingThreadInteraction::UserInput)
        } else if self
            .pending_interactive_replay
            .has_pending_thread_approvals()
        {
            Some(PendingThreadInteraction::Approval)
        } else {
            None
        }
    }

    pub fn active_turn_id(&self) -> Option<&str> {
        self.active_turn_id.as_deref()
    }

    pub fn clear_active_turn_id(&mut self) {
        self.active_turn_id = None;
    }
}

fn turn_id_matches(request_turn_id: &str, candidate_turn_id: &str) -> bool {
    request_turn_id.is_empty() || request_turn_id == candidate_turn_id
}

fn file_change_item_changes(
    item: &ThreadItem,
    item_id: &str,
) -> Option<Vec<xedoc_app_server_protocol::FileUpdateChange>> {
    match item {
        ThreadItem::FileChange { id, changes, .. } if id == item_id => Some(changes.clone()),
        _ => None,
    }
}

#[derive(Debug)]
pub struct ThreadEventChannel {
    pub sender: mpsc::Sender<ThreadBufferedEvent>,
    pub receiver: Option<mpsc::Receiver<ThreadBufferedEvent>>,
    pub store: Arc<Mutex<ThreadEventStore>>,
    attachment: ThreadEventAttachment,
}

impl ThreadEventChannel {
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new(capacity))),
            attachment: ThreadEventAttachment::Live,
        }
    }

    pub fn mark_replay_only(&mut self) {
        self.attachment = ThreadEventAttachment::ReplayOnly;
    }

    pub fn attachment(&self) -> ThreadEventAttachment {
        self.attachment
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new_with_session(
        capacity: usize,
        session: ThreadSessionState,
        turns: Vec<Turn>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new_with_session(
                capacity, session, turns,
            ))),
            attachment: ThreadEventAttachment::Live,
        }
    }
}
