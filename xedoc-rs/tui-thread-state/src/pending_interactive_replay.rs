use std::collections::HashMap;
use std::collections::HashSet;
use xedoc_app_server_protocol::RequestId as AppServerRequestId;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ServerRequest;
use xedoc_app_server_protocol::ThreadItem;
use xedoc_tui_events::AppCommand;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ElicitationRequestKey {
    server_name: String,
    request_id: AppServerRequestId,
}

impl ElicitationRequestKey {
    fn new(server_name: String, request_id: AppServerRequestId) -> Self {
        Self {
            server_name,
            request_id,
        }
    }
}

#[derive(Debug, Default)]
// Tracks which interactive prompts are still unresolved in the thread-event buffer.
//
// Thread snapshots are replayed when switching threads/agents. Most events should replay
// verbatim, but interactive prompts (approvals, request_user_input, MCP elicitations) must
// only replay if they are still pending. This state is updated from:
// - inbound events (`note_event`)
// - outbound ops that resolve a prompt (`note_outbound_op`)
// - buffer eviction (`note_evicted_event`)
//
// We keep both fast lookup sets (for snapshot filtering by call_id/request key) and
// turn-indexed queues/vectors so turn completion or interruption can clear
// stale prompts tied to a turn. `request_user_input` removal is FIFO because
// the overlay answers queued prompts in FIFO order for a shared `turn_id`.
pub struct PendingInteractiveReplayState {
    exec_approval_call_ids: HashSet<String>,
    exec_approval_call_ids_by_turn_id: HashMap<String, Vec<String>>,
    patch_approval_call_ids: HashSet<String>,
    patch_approval_call_ids_by_turn_id: HashMap<String, Vec<String>>,
    elicitation_requests: HashSet<ElicitationRequestKey>,
    request_permissions_call_ids: HashSet<String>,
    request_permissions_call_ids_by_turn_id: HashMap<String, Vec<String>>,
    request_user_input_call_ids: HashSet<String>,
    request_user_input_call_ids_by_turn_id: HashMap<String, Vec<String>>,
    extension_interaction_request_ids: HashSet<String>,
    extension_interaction_request_ids_by_turn_id: HashMap<String, Vec<String>>,
    pending_requests_by_request_id: HashMap<AppServerRequestId, PendingInteractiveRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingInteractiveRequest {
    ExecApproval {
        turn_id: String,
        approval_id: String,
    },
    PatchApproval {
        turn_id: String,
        item_id: String,
    },
    Elicitation(ElicitationRequestKey),
    RequestPermissions {
        turn_id: String,
        item_id: String,
    },
    RequestUserInput {
        turn_id: String,
        item_id: String,
    },
    ExtensionInteraction {
        turn_id: String,
        request_id: String,
    },
}

impl PendingInteractiveReplayState {
    pub fn op_can_change_state<T>(op: T) -> bool
    where
        T: Into<AppCommand>,
    {
        let op: AppCommand = op.into();
        matches!(
            &op,
            AppCommand::ExecApproval { .. }
                | AppCommand::PatchApproval { .. }
                | AppCommand::ResolveElicitation { .. }
                | AppCommand::RequestPermissionsResponse { .. }
                | AppCommand::UserInputAnswer { .. }
                | AppCommand::ExtensionInteractionResponse { .. }
                | AppCommand::Shutdown
        )
    }

    pub fn note_outbound_op<T>(&mut self, op: T)
    where
        T: Into<AppCommand>,
    {
        let op: AppCommand = op.into();
        match &op {
            AppCommand::ExecApproval { id, turn_id, .. } => {
                self.exec_approval_call_ids.remove(id);
                if let Some(turn_id) = turn_id {
                    Self::remove_call_id_from_turn_map_entry(
                        &mut self.exec_approval_call_ids_by_turn_id,
                        turn_id,
                        id,
                    );
                }
                self.pending_requests_by_request_id
                    .retain(|_, pending| !matches!(pending, PendingInteractiveRequest::ExecApproval { approval_id, .. } if approval_id == id));
            }
            AppCommand::PatchApproval { id, .. } => {
                self.patch_approval_call_ids.remove(id);
                Self::remove_call_id_from_turn_map(
                    &mut self.patch_approval_call_ids_by_turn_id,
                    id,
                );
                self.pending_requests_by_request_id
                    .retain(|_, pending| !matches!(pending, PendingInteractiveRequest::PatchApproval { item_id, .. } if item_id == id));
            }
            AppCommand::ResolveElicitation {
                server_name,
                request_id,
                ..
            } => {
                self.elicitation_requests
                    .remove(&ElicitationRequestKey::new(
                        server_name.to_string(),
                        request_id.clone(),
                    ));
                self.pending_requests_by_request_id.retain(
                    |_, pending| {
                        !matches!(pending, PendingInteractiveRequest::Elicitation(key) if key.server_name == *server_name && key.request_id == *request_id)
                    },
                );
            }
            AppCommand::RequestPermissionsResponse { id, .. } => {
                self.request_permissions_call_ids.remove(id);
                Self::remove_call_id_from_turn_map(
                    &mut self.request_permissions_call_ids_by_turn_id,
                    id,
                );
                self.pending_requests_by_request_id.retain(
                    |_, pending| {
                        !matches!(pending, PendingInteractiveRequest::RequestPermissions { item_id, .. } if item_id == id)
                    },
                );
            }
            // `Op::UserInputAnswer` identifies the turn, not the prompt call_id. The UI
            // answers queued prompts for the same turn in FIFO order, so remove the oldest
            // queued call_id for that turn.
            AppCommand::UserInputAnswer { id, .. } => {
                let mut remove_turn_entry = false;
                if let Some(call_ids) = self.request_user_input_call_ids_by_turn_id.get_mut(id) {
                    if !call_ids.is_empty() {
                        let call_id = call_ids.remove(0);
                        self.request_user_input_call_ids.remove(&call_id);
                        self.pending_requests_by_request_id.retain(
                            |_, pending| {
                                !matches!(pending, PendingInteractiveRequest::RequestUserInput { item_id, .. } if *item_id == call_id)
                            },
                        );
                    }
                    if call_ids.is_empty() {
                        remove_turn_entry = true;
                    }
                }
                if remove_turn_entry {
                    self.request_user_input_call_ids_by_turn_id.remove(id);
                }
            }
            AppCommand::ExtensionInteractionResponse { request_id, .. } => {
                self.extension_interaction_request_ids.remove(request_id);
                Self::remove_call_id_from_turn_map(
                    &mut self.extension_interaction_request_ids_by_turn_id,
                    request_id,
                );
                self.pending_requests_by_request_id.retain(
                    |_, pending| {
                        !matches!(pending, PendingInteractiveRequest::ExtensionInteraction { request_id: pending_request_id, .. } if pending_request_id == request_id)
                    },
                );
            }
            AppCommand::Shutdown => self.clear(),
            _ => {}
        }
    }

    pub fn note_server_request(&mut self, request: &ServerRequest) {
        match request {
            ServerRequest::CommandExecutionRequestApproval { request_id, params } => {
                let approval_id = params
                    .approval_id
                    .clone()
                    .unwrap_or_else(|| params.item_id.clone());
                self.exec_approval_call_ids.insert(approval_id.clone());
                self.exec_approval_call_ids_by_turn_id
                    .entry(params.turn_id.clone())
                    .or_default()
                    .push(approval_id);
                self.pending_requests_by_request_id.insert(
                    request_id.clone(),
                    PendingInteractiveRequest::ExecApproval {
                        turn_id: params.turn_id.clone(),
                        approval_id: params
                            .approval_id
                            .clone()
                            .unwrap_or_else(|| params.item_id.clone()),
                    },
                );
            }
            ServerRequest::FileChangeRequestApproval { request_id, params } => {
                self.patch_approval_call_ids.insert(params.item_id.clone());
                self.patch_approval_call_ids_by_turn_id
                    .entry(params.turn_id.clone())
                    .or_default()
                    .push(params.item_id.clone());
                self.pending_requests_by_request_id.insert(
                    request_id.clone(),
                    PendingInteractiveRequest::PatchApproval {
                        turn_id: params.turn_id.clone(),
                        item_id: params.item_id.clone(),
                    },
                );
            }
            ServerRequest::McpServerElicitationRequest { request_id, params } => {
                let key =
                    ElicitationRequestKey::new(params.server_name.clone(), request_id.clone());
                self.elicitation_requests.insert(key.clone());
                self.pending_requests_by_request_id.insert(
                    request_id.clone(),
                    PendingInteractiveRequest::Elicitation(key),
                );
            }
            ServerRequest::ToolRequestUserInput { request_id, params } => {
                self.request_user_input_call_ids
                    .insert(params.item_id.clone());
                self.request_user_input_call_ids_by_turn_id
                    .entry(params.turn_id.clone())
                    .or_default()
                    .push(params.item_id.clone());
                self.pending_requests_by_request_id.insert(
                    request_id.clone(),
                    PendingInteractiveRequest::RequestUserInput {
                        turn_id: params.turn_id.clone(),
                        item_id: params.item_id.clone(),
                    },
                );
            }
            ServerRequest::PermissionsRequestApproval { request_id, params } => {
                self.request_permissions_call_ids
                    .insert(params.item_id.clone());
                self.request_permissions_call_ids_by_turn_id
                    .entry(params.turn_id.clone())
                    .or_default()
                    .push(params.item_id.clone());
                self.pending_requests_by_request_id.insert(
                    request_id.clone(),
                    PendingInteractiveRequest::RequestPermissions {
                        turn_id: params.turn_id.clone(),
                        item_id: params.item_id.clone(),
                    },
                );
            }
            ServerRequest::ExtensionInteractionRequest { request_id, params } => {
                self.extension_interaction_request_ids
                    .insert(params.request_id.clone());
                self.extension_interaction_request_ids_by_turn_id
                    .entry(params.turn_id.clone())
                    .or_default()
                    .push(params.request_id.clone());
                self.pending_requests_by_request_id.insert(
                    request_id.clone(),
                    PendingInteractiveRequest::ExtensionInteraction {
                        turn_id: params.turn_id.clone(),
                        request_id: params.request_id.clone(),
                    },
                );
            }
            _ => {}
        }
    }

    pub fn note_server_notification(&mut self, notification: &ServerNotification) {
        match notification {
            ServerNotification::ItemStarted(notification) => match &notification.item {
                ThreadItem::CommandExecution { id, .. } => {
                    self.exec_approval_call_ids.remove(id);
                    Self::remove_call_id_from_turn_map(
                        &mut self.exec_approval_call_ids_by_turn_id,
                        id,
                    );
                }
                ThreadItem::FileChange { id, .. } => {
                    self.patch_approval_call_ids.remove(id);
                    Self::remove_call_id_from_turn_map(
                        &mut self.patch_approval_call_ids_by_turn_id,
                        id,
                    );
                }
                _ => {}
            },
            ServerNotification::TurnCompleted(notification) => {
                self.clear_exec_approval_turn(&notification.turn.id);
                self.clear_patch_approval_turn(&notification.turn.id);
                self.clear_request_permissions_turn(&notification.turn.id);
                self.clear_request_user_input_turn(&notification.turn.id);
                self.clear_extension_interaction_turn(&notification.turn.id);
            }
            ServerNotification::ServerRequestResolved(notification) => {
                self.remove_request(&notification.request_id);
            }
            ServerNotification::ThreadClosed(_) => self.clear(),
            _ => {}
        }
    }

    pub fn note_evicted_server_request(&mut self, request: &ServerRequest) {
        match request {
            ServerRequest::CommandExecutionRequestApproval { params, .. } => {
                let approval_id = params
                    .approval_id
                    .clone()
                    .unwrap_or_else(|| params.item_id.clone());
                self.exec_approval_call_ids.remove(&approval_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.exec_approval_call_ids_by_turn_id,
                    &params.turn_id,
                    &approval_id,
                );
            }
            ServerRequest::FileChangeRequestApproval { params, .. } => {
                self.patch_approval_call_ids.remove(&params.item_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.patch_approval_call_ids_by_turn_id,
                    &params.turn_id,
                    &params.item_id,
                );
            }
            ServerRequest::McpServerElicitationRequest { request_id, params } => {
                self.elicitation_requests
                    .remove(&ElicitationRequestKey::new(
                        params.server_name.clone(),
                        request_id.clone(),
                    ));
            }
            ServerRequest::ToolRequestUserInput { params, .. } => {
                self.request_user_input_call_ids.remove(&params.item_id);
                let mut remove_turn_entry = false;
                if let Some(call_ids) = self
                    .request_user_input_call_ids_by_turn_id
                    .get_mut(&params.turn_id)
                {
                    call_ids.retain(|call_id| call_id != &params.item_id);
                    if call_ids.is_empty() {
                        remove_turn_entry = true;
                    }
                }
                if remove_turn_entry {
                    self.request_user_input_call_ids_by_turn_id
                        .remove(&params.turn_id);
                }
            }
            ServerRequest::PermissionsRequestApproval { params, .. } => {
                self.request_permissions_call_ids.remove(&params.item_id);
                let mut remove_turn_entry = false;
                if let Some(call_ids) = self
                    .request_permissions_call_ids_by_turn_id
                    .get_mut(&params.turn_id)
                {
                    call_ids.retain(|call_id| call_id != &params.item_id);
                    if call_ids.is_empty() {
                        remove_turn_entry = true;
                    }
                }
                if remove_turn_entry {
                    self.request_permissions_call_ids_by_turn_id
                        .remove(&params.turn_id);
                }
            }
            ServerRequest::ExtensionInteractionRequest { params, .. } => {
                self.extension_interaction_request_ids
                    .remove(&params.request_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.extension_interaction_request_ids_by_turn_id,
                    &params.turn_id,
                    &params.request_id,
                );
            }
            _ => {}
        }
        self.pending_requests_by_request_id
            .retain(|_, pending| !Self::request_matches_server_request(pending, request));
    }

    pub fn should_replay_snapshot_request(&self, request: &ServerRequest) -> bool {
        match request {
            ServerRequest::CommandExecutionRequestApproval { params, .. } => self
                .exec_approval_call_ids
                .contains(params.approval_id.as_ref().unwrap_or(&params.item_id)),
            ServerRequest::FileChangeRequestApproval { params, .. } => {
                self.patch_approval_call_ids.contains(&params.item_id)
            }
            ServerRequest::McpServerElicitationRequest { request_id, params } => self
                .elicitation_requests
                .contains(&ElicitationRequestKey::new(
                    params.server_name.clone(),
                    request_id.clone(),
                )),
            ServerRequest::ToolRequestUserInput { params, .. } => {
                self.request_user_input_call_ids.contains(&params.item_id)
            }
            ServerRequest::PermissionsRequestApproval { params, .. } => {
                self.request_permissions_call_ids.contains(&params.item_id)
            }
            ServerRequest::ExtensionInteractionRequest { params, .. } => self
                .extension_interaction_request_ids
                .contains(&params.request_id),
            _ => true,
        }
    }

    pub fn has_pending_thread_approvals(&self) -> bool {
        !self.exec_approval_call_ids.is_empty()
            || !self.patch_approval_call_ids.is_empty()
            || !self.elicitation_requests.is_empty()
            || !self.request_permissions_call_ids.is_empty()
            || !self.extension_interaction_request_ids.is_empty()
    }

    pub fn has_pending_thread_user_input(&self) -> bool {
        !self.request_user_input_call_ids.is_empty()
    }

    fn clear_request_user_input_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.request_user_input_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.request_user_input_call_ids.remove(&call_id);
            }
        }
        self.pending_requests_by_request_id.retain(
            |_, pending| {
                !matches!(pending, PendingInteractiveRequest::RequestUserInput { turn_id: pending_turn_id, .. } if pending_turn_id == turn_id)
            },
        );
    }

    fn clear_request_permissions_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.request_permissions_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.request_permissions_call_ids.remove(&call_id);
            }
        }
        self.pending_requests_by_request_id.retain(
            |_, pending| {
                !matches!(pending, PendingInteractiveRequest::RequestPermissions { turn_id: pending_turn_id, .. } if pending_turn_id == turn_id)
            },
        );
    }

    fn clear_exec_approval_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.exec_approval_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.exec_approval_call_ids.remove(&call_id);
            }
        }
        self.pending_requests_by_request_id.retain(
            |_, pending| {
                !matches!(pending, PendingInteractiveRequest::ExecApproval { turn_id: pending_turn_id, .. } if pending_turn_id == turn_id)
            },
        );
    }

    fn clear_patch_approval_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.patch_approval_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.patch_approval_call_ids.remove(&call_id);
            }
        }
        self.pending_requests_by_request_id.retain(
            |_, pending| {
                !matches!(pending, PendingInteractiveRequest::PatchApproval { turn_id: pending_turn_id, .. } if pending_turn_id == turn_id)
            },
        );
    }

    fn clear_extension_interaction_turn(&mut self, turn_id: &str) {
        if let Some(request_ids) = self
            .extension_interaction_request_ids_by_turn_id
            .remove(turn_id)
        {
            for request_id in request_ids {
                self.extension_interaction_request_ids.remove(&request_id);
            }
        }
        self.pending_requests_by_request_id.retain(
            |_, pending| {
                !matches!(pending, PendingInteractiveRequest::ExtensionInteraction { turn_id: pending_turn_id, .. } if pending_turn_id == turn_id)
            },
        );
    }

    fn remove_call_id_from_turn_map(
        call_ids_by_turn_id: &mut HashMap<String, Vec<String>>,
        call_id: &str,
    ) {
        call_ids_by_turn_id.retain(|_, call_ids| {
            call_ids.retain(|queued_call_id| queued_call_id != call_id);
            !call_ids.is_empty()
        });
    }

    fn remove_call_id_from_turn_map_entry(
        call_ids_by_turn_id: &mut HashMap<String, Vec<String>>,
        turn_id: &str,
        call_id: &str,
    ) {
        let mut remove_turn_entry = false;
        if let Some(call_ids) = call_ids_by_turn_id.get_mut(turn_id) {
            call_ids.retain(|queued_call_id| queued_call_id != call_id);
            if call_ids.is_empty() {
                remove_turn_entry = true;
            }
        }
        if remove_turn_entry {
            call_ids_by_turn_id.remove(turn_id);
        }
    }

    pub fn clear(&mut self) {
        self.exec_approval_call_ids.clear();
        self.exec_approval_call_ids_by_turn_id.clear();
        self.patch_approval_call_ids.clear();
        self.patch_approval_call_ids_by_turn_id.clear();
        self.elicitation_requests.clear();
        self.request_permissions_call_ids.clear();
        self.request_permissions_call_ids_by_turn_id.clear();
        self.request_user_input_call_ids.clear();
        self.request_user_input_call_ids_by_turn_id.clear();
        self.extension_interaction_request_ids.clear();
        self.extension_interaction_request_ids_by_turn_id.clear();
        self.pending_requests_by_request_id.clear();
    }

    fn remove_request(&mut self, request_id: &AppServerRequestId) {
        let Some(pending) = self.pending_requests_by_request_id.remove(request_id) else {
            return;
        };
        match pending {
            PendingInteractiveRequest::ExecApproval {
                turn_id,
                approval_id,
            } => {
                self.exec_approval_call_ids.remove(&approval_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.exec_approval_call_ids_by_turn_id,
                    &turn_id,
                    &approval_id,
                );
            }
            PendingInteractiveRequest::PatchApproval { turn_id, item_id } => {
                self.patch_approval_call_ids.remove(&item_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.patch_approval_call_ids_by_turn_id,
                    &turn_id,
                    &item_id,
                );
            }
            PendingInteractiveRequest::Elicitation(key) => {
                self.elicitation_requests.remove(&key);
            }
            PendingInteractiveRequest::RequestPermissions { turn_id, item_id } => {
                self.request_permissions_call_ids.remove(&item_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.request_permissions_call_ids_by_turn_id,
                    &turn_id,
                    &item_id,
                );
            }
            PendingInteractiveRequest::RequestUserInput { turn_id, item_id } => {
                self.request_user_input_call_ids.remove(&item_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.request_user_input_call_ids_by_turn_id,
                    &turn_id,
                    &item_id,
                );
            }
            PendingInteractiveRequest::ExtensionInteraction {
                turn_id,
                request_id,
            } => {
                self.extension_interaction_request_ids.remove(&request_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.extension_interaction_request_ids_by_turn_id,
                    &turn_id,
                    &request_id,
                );
            }
        }
    }

    fn request_matches_server_request(
        pending: &PendingInteractiveRequest,
        request: &ServerRequest,
    ) -> bool {
        match (pending, request) {
            (
                PendingInteractiveRequest::ExecApproval {
                    turn_id,
                    approval_id,
                },
                ServerRequest::CommandExecutionRequestApproval { params, .. },
            ) => {
                turn_id == &params.turn_id
                    && approval_id == params.approval_id.as_ref().unwrap_or(&params.item_id)
            }
            (
                PendingInteractiveRequest::PatchApproval { turn_id, item_id },
                ServerRequest::FileChangeRequestApproval { params, .. },
            ) => turn_id == &params.turn_id && item_id == &params.item_id,
            (
                PendingInteractiveRequest::Elicitation(key),
                ServerRequest::McpServerElicitationRequest { request_id, params },
            ) => key.server_name == params.server_name && key.request_id == *request_id,
            (
                PendingInteractiveRequest::RequestPermissions { turn_id, item_id },
                ServerRequest::PermissionsRequestApproval { params, .. },
            ) => turn_id == &params.turn_id && item_id == &params.item_id,
            (
                PendingInteractiveRequest::RequestUserInput { turn_id, item_id },
                ServerRequest::ToolRequestUserInput { params, .. },
            ) => turn_id == &params.turn_id && item_id == &params.item_id,
            (
                PendingInteractiveRequest::ExtensionInteraction {
                    turn_id,
                    request_id,
                },
                ServerRequest::ExtensionInteractionRequest { params, .. },
            ) => turn_id == &params.turn_id && request_id == &params.request_id,
            _ => false,
        }
    }
}
