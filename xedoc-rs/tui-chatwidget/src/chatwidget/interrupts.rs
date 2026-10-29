//! Queue prompt overlays and deferred tool activity while another interrupt is visible.

use std::collections::VecDeque;

use crate::approval_events::ApplyPatchApprovalRequestEvent;
use crate::approval_events::ExecApprovalRequestEvent;
use xedoc_app_server_protocol::McpServerElicitationRequestParams;
use xedoc_app_server_protocol::RequestId as AppServerRequestId;
use xedoc_app_server_protocol::ThreadItem;
use xedoc_app_server_protocol::ToolRequestUserInputParams;
use xedoc_protocol::request_permissions::RequestPermissionsEvent;
use xedoc_tui_events::ResolvedAppServerRequest;

use super::ChatWidget;

#[derive(Debug)]
pub enum QueuedInterrupt {
    ExecApproval(ExecApprovalRequestEvent),
    ApplyPatchApproval(ApplyPatchApprovalRequestEvent),
    Elicitation {
        request_id: AppServerRequestId,
        params: McpServerElicitationRequestParams,
    },
    RequestPermissions(RequestPermissionsEvent),
    RequestUserInput(ToolRequestUserInputParams),
    ItemStarted(ThreadItem),
    ItemCompleted(ThreadItem),
}

#[derive(Default)]
pub struct InterruptManager {
    queue: VecDeque<QueuedInterrupt>,
}

impl InterruptManager {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn push_exec_approval(&mut self, ev: ExecApprovalRequestEvent) {
        self.queue.push_back(QueuedInterrupt::ExecApproval(ev));
    }

    pub fn push_apply_patch_approval(&mut self, ev: ApplyPatchApprovalRequestEvent) {
        self.queue
            .push_back(QueuedInterrupt::ApplyPatchApproval(ev));
    }

    pub fn push_elicitation(
        &mut self,
        request_id: AppServerRequestId,
        params: McpServerElicitationRequestParams,
    ) {
        self.queue
            .push_back(QueuedInterrupt::Elicitation { request_id, params });
    }

    pub fn push_request_permissions(&mut self, ev: RequestPermissionsEvent) {
        self.queue
            .push_back(QueuedInterrupt::RequestPermissions(ev));
    }

    pub fn push_user_input(&mut self, ev: ToolRequestUserInputParams) {
        self.queue.push_back(QueuedInterrupt::RequestUserInput(ev));
    }

    pub fn push_item_started(&mut self, item: ThreadItem) {
        self.queue.push_back(QueuedInterrupt::ItemStarted(item));
    }

    pub fn push_item_completed(&mut self, item: ThreadItem) {
        self.queue.push_back(QueuedInterrupt::ItemCompleted(item));
    }

    pub fn remove_resolved_prompt(&mut self, request: &ResolvedAppServerRequest) -> bool {
        let original_len = self.queue.len();
        self.queue
            .retain(|queued| !queued.matches_resolved_prompt(request));
        self.queue.len() != original_len
    }

    pub fn flush_all(&mut self, chat: &mut ChatWidget) {
        while let Some(q) = self.queue.pop_front() {
            match q {
                QueuedInterrupt::ExecApproval(ev) => chat.handle_exec_approval_now(ev),
                QueuedInterrupt::ApplyPatchApproval(ev) => chat.handle_apply_patch_approval_now(ev),
                QueuedInterrupt::Elicitation { request_id, params } => {
                    chat.handle_elicitation_request_now(request_id, params);
                }
                QueuedInterrupt::RequestPermissions(ev) => chat.handle_request_permissions_now(ev),
                QueuedInterrupt::RequestUserInput(ev) => chat.handle_request_user_input_now(ev),
                QueuedInterrupt::ItemStarted(item) => chat.handle_queued_item_started_now(item),
                QueuedInterrupt::ItemCompleted(item) => {
                    chat.handle_queued_item_completed_now(item);
                }
            }
        }
    }
}

impl QueuedInterrupt {
    fn matches_resolved_prompt(&self, request: &ResolvedAppServerRequest) -> bool {
        match self {
            QueuedInterrupt::ExecApproval(ev) => {
                matches!(request, ResolvedAppServerRequest::ExecApproval { id }
                    if ev.effective_approval_id() == id.as_str())
            }
            QueuedInterrupt::ApplyPatchApproval(ev) => {
                matches!(request, ResolvedAppServerRequest::FileChangeApproval { id }
                    if ev.call_id == id.as_str())
            }
            QueuedInterrupt::Elicitation { request_id, params } => {
                matches!(request, ResolvedAppServerRequest::McpElicitation {
                    server_name,
                    request_id: resolved_request_id,
                } if params.server_name == server_name.as_str() && request_id == resolved_request_id)
            }
            QueuedInterrupt::RequestPermissions(ev) => {
                matches!(request, ResolvedAppServerRequest::PermissionsApproval { id }
                    if ev.call_id == id.as_str())
            }
            QueuedInterrupt::RequestUserInput(ev) => {
                matches!(request, ResolvedAppServerRequest::UserInput { call_id }
                    if ev.item_id == call_id.as_str())
            }
            QueuedInterrupt::ItemStarted(_) | QueuedInterrupt::ItemCompleted(_) => false,
        }
    }
}
