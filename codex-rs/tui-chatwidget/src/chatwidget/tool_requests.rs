//! Interactive tool request surfaces for `ChatWidget`.
//!
//! This module owns approval, permission, elicitation, and user-input prompts
//! that block on user decisions.

use super::*;

impl ChatWidget {
    pub(super) fn on_exec_approval_request(&mut self, _id: String, ev: ExecApprovalRequestEvent) {
        self.defer_or_handle(
            ev,
            InterruptManager::push_exec_approval,
            Self::handle_exec_approval_now,
        );
    }

    pub(super) fn on_apply_patch_approval_request(
        &mut self,
        _id: String,
        ev: ApplyPatchApprovalRequestEvent,
    ) {
        self.defer_or_handle(
            ev,
            InterruptManager::push_apply_patch_approval,
            Self::handle_apply_patch_approval_now,
        );
    }

    pub(super) fn on_elicitation_request(
        &mut self,
        request_id: AppServerRequestId,
        params: McpServerElicitationRequestParams,
    ) {
        self.defer_or_handle(
            (request_id, params),
            |q, (request_id, params)| q.push_elicitation(request_id, params),
            |s, (request_id, params)| s.handle_elicitation_request_now(request_id, params),
        );
    }

    pub(super) fn on_request_user_input(&mut self, ev: ToolRequestUserInputParams) {
        self.defer_or_handle(
            ev,
            InterruptManager::push_user_input,
            Self::handle_request_user_input_now,
        );
    }

    pub(super) fn on_request_permissions(&mut self, ev: RequestPermissionsEvent) {
        self.defer_or_handle(
            ev,
            InterruptManager::push_request_permissions,
            Self::handle_request_permissions_now,
        );
    }

    pub fn handle_exec_approval_now(&mut self, ev: ExecApprovalRequestEvent) {
        self.flush_answer_stream_with_separator();
        let command = shlex::try_join(ev.command.iter().map(String::as_str))
            .unwrap_or_else(|_| ev.command.join(" "));
        self.notify(Notification::ExecApprovalRequested { command });

        let available_decisions = ev.effective_available_decisions();
        let request = ApprovalRequest::Exec(ExecApprovalRequest {
            thread_id: self.thread_id.unwrap_or_default(),
            thread_label: None,
            id: ev.effective_approval_id(),
            environment_id: ev.environment_id,
            command: ev.command,
            reason: ev.reason,
            available_decisions,
            network_approval_context: ev.network_approval_context,
            additional_permissions: ev.additional_permissions,
        });
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub fn handle_apply_patch_approval_now(&mut self, ev: ApplyPatchApprovalRequestEvent) {
        self.flush_answer_stream_with_separator();

        let changed_paths = ev.changes.keys().cloned().collect();
        let request = ApprovalRequest::ApplyPatch(ApplyPatchApprovalRequest {
            thread_id: self.thread_id.unwrap_or_default(),
            thread_label: None,
            id: ev.call_id,
            reason: ev.reason,
            changes: ev.changes,
            cwd: self.config.cwd.clone(),
        });
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
        self.notify(Notification::EditApprovalRequested {
            cwd: self.config.cwd.to_path_buf(),
            changes: changed_paths,
        });
    }

    pub fn handle_elicitation_request_now(
        &mut self,
        request_id: AppServerRequestId,
        params: McpServerElicitationRequestParams,
    ) {
        self.flush_answer_stream_with_separator();

        self.notify(Notification::ElicitationRequested {
            server_name: params.server_name.clone(),
        });

        let thread_id = ThreadId::from_string(&params.thread_id)
            .unwrap_or_else(|_| self.thread_id.unwrap_or_default());
        if let Some(request) = McpServerElicitationFormRequest::from_app_server_request(
            thread_id,
            request_id.clone(),
            &params,
        ) {
            self.bottom_pane
                .push_mcp_server_elicitation_request(request);
        } else {
            match params.request {
                McpServerElicitationRequest::Form { message, .. } => {
                    let request = ApprovalRequest::McpElicitation(McpElicitationApprovalRequest {
                        thread_id,
                        thread_label: None,
                        server_name: params.server_name,
                        request_id,
                        message,
                    });
                    self.bottom_pane
                        .push_approval_request(request, &self.config.features);
                }
                McpServerElicitationRequest::OpenAiForm { .. }
                | McpServerElicitationRequest::Url { .. } => {
                    self.app_event_tx.resolve_elicitation(
                        thread_id,
                        params.server_name,
                        request_id,
                        codex_app_server_protocol::McpServerElicitationAction::Decline,
                        /*content*/ None,
                        /*meta*/ None,
                    );
                }
            }
        }
        self.request_redraw();
    }

    pub fn push_approval_request(&mut self, request: ApprovalRequest) {
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub fn push_mcp_server_elicitation_request(
        &mut self,
        request: McpServerElicitationFormRequest,
    ) {
        self.bottom_pane
            .push_mcp_server_elicitation_request(request);
        self.request_redraw();
    }

    pub fn handle_request_user_input_now(&mut self, ev: ToolRequestUserInputParams) {
        self.flush_answer_stream_with_separator();
        let question_count = ev.questions.len();
        let summary = Notification::user_input_request_summary(&ev.questions);
        let title = match (question_count, summary.as_deref()) {
            (1, Some(summary)) => summary.to_string(),
            (1, None) => "Question requested".to_string(),
            (count, _) => format!("{count} questions requested"),
        };
        self.notify(Notification::PlanModePrompt { title });
        self.bottom_pane.push_user_input_request(ev);
        self.request_redraw();
    }

    pub fn handle_request_permissions_now(&mut self, ev: RequestPermissionsEvent) {
        self.flush_answer_stream_with_separator();
        let request = ApprovalRequest::Permissions(PermissionsApprovalRequest {
            thread_id: self.thread_id.unwrap_or_default(),
            thread_label: None,
            call_id: ev.call_id,
            environment_id: ev.environment_id,
            reason: ev.reason,
            permissions: ev.permissions,
        });
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }
}
