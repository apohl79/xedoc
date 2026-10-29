use super::App;
use crate::session_resume::read_session_model;
use crate::session_state::ThreadSessionState;
use xedoc_app_server_protocol::AskForApproval;
use xedoc_app_server_protocol::Thread;
use xedoc_protocol::ThreadId;
use xedoc_protocol::models::ActivePermissionProfile;
use xedoc_protocol::models::PermissionProfile;

impl App {
    pub(super) async fn sync_active_thread_service_tier_to_cached_session(&mut self) {
        let Some(active_thread_id) = self.active_thread_id else {
            return;
        };

        let service_tier = self.chat_widget.current_service_tier().map(str::to_string);
        let update_session = |session: &mut ThreadSessionState| {
            session.service_tier = service_tier.clone();
        };

        if self.primary_thread_id == Some(active_thread_id)
            && let Some(session) = self.primary_session_configured.as_mut()
        {
            update_session(session);
        }

        if let Some(channel) = self.thread_event_channels.get(&active_thread_id) {
            let mut store = channel.store.lock().await;
            if let Some(session) = store.session.as_mut() {
                update_session(session);
            }
        }
    }

    pub(super) async fn sync_active_thread_permission_settings_to_cached_session(&mut self) {
        let Some(active_thread_id) = self.active_thread_id else {
            return;
        };

        let approval_policy = AskForApproval::from(self.config.permissions.approval_policy.value());
        let permission_profile = self
            .chat_widget
            .config_ref()
            .permissions
            .permission_profile()
            .clone();
        let active_permission_profile = self
            .chat_widget
            .config_ref()
            .permissions
            .active_permission_profile();
        let update_session = |session: &mut ThreadSessionState| {
            session.approval_policy = approval_policy;
            session.permission_profile = permission_profile.clone();
            session.active_permission_profile = active_permission_profile.clone();
        };

        if self.primary_thread_id == Some(active_thread_id)
            && let Some(session) = self.primary_session_configured.as_mut()
        {
            update_session(session);
        }

        if let Some(channel) = self.thread_event_channels.get(&active_thread_id) {
            let mut store = channel.store.lock().await;
            if let Some(session) = store.session.as_mut() {
                update_session(session);
            }
        }
    }

    pub(super) async fn session_state_for_thread_read(
        &self,
        thread_id: ThreadId,
        thread: &Thread,
    ) -> ThreadSessionState {
        let permission_profile = self.current_permission_profile();
        let active_permission_profile = self.current_active_permission_profile();
        let mut session = if let Some(mut session) = self.primary_session_configured.clone() {
            if session.thread_id != thread_id {
                // `thread/read` does not include thread settings, so do not carry
                // thread-scoped state from the currently active session.
                session.collaboration_mode = None;
                session.personality = None;
            }
            session
        } else {
            ThreadSessionState {
                thread_id,
                forked_from_id: None,
                fork_parent_title: None,
                thread_name: None,
                model: self.chat_widget.current_model().to_string(),
                model_provider_id: self.config.model_provider_id.clone(),
                service_tier: self.chat_widget.current_service_tier().map(str::to_string),
                approval_policy: AskForApproval::from(
                    self.config.permissions.approval_policy.value(),
                ),
                permission_profile: permission_profile.clone(),
                active_permission_profile: active_permission_profile.clone(),
                cwd: thread.cwd.clone(),
                runtime_workspace_roots: self.config.workspace_roots.clone(),
                instruction_source_paths: Vec::new(),
                reasoning_effort: self.chat_widget.current_reasoning_effort(),
                collaboration_mode: None,
                personality: None,
                message_history: None,
                network_proxy: None,
                rollout_path: thread.path.clone(),
            }
        };
        session.thread_id = thread_id;
        session.thread_name = thread.name.clone();
        session.model_provider_id = thread.model_provider.clone();
        session.set_cwd_retargeting_implicit_runtime_workspace_root(thread.cwd.clone());
        session.permission_profile = permission_profile;
        session.active_permission_profile = active_permission_profile;
        session.instruction_source_paths = Vec::new();
        session.rollout_path = thread.path.clone();
        if let Some(model) =
            read_session_model(self.state_db.as_deref(), thread_id, thread.path.as_deref()).await
        {
            session.model = model;
        } else if thread.path.is_some() {
            session.model.clear();
        }
        session.message_history = None;
        session
    }

    fn current_permission_profile(&self) -> PermissionProfile {
        self.chat_widget
            .config_ref()
            .permissions
            .permission_profile()
            .clone()
    }

    fn current_active_permission_profile(&self) -> Option<ActivePermissionProfile> {
        self.chat_widget
            .config_ref()
            .permissions
            .active_permission_profile()
    }
}
