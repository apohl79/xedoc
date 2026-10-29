//! The bottom pane is the interactive footer of the chat UI.
//!
//! The pane owns the [`ChatComposer`] (editable prompt input) and a stack of transient
//! [`BottomPaneView`]s (popups/modals) that temporarily replace the composer for focused
//! interactions like selection lists.
//!
//! Input routing is layered: `BottomPane` decides which local surface receives a key (view vs
//! composer), while higher-level intent such as "interrupt" or "quit" is decided by the parent
//! widget (`ChatWidget`). This split matters for Ctrl+C/Ctrl+D: the bottom pane gives the active
//! view the first chance to consume Ctrl+C (typically to dismiss itself), then lets an active
//! composer history search consume Ctrl+C as cancellation, and `ChatWidget` may treat an unhandled
//! Ctrl+C as an interrupt or as the first press of a double-press quit shortcut.
//!
//! Some UI is time-based rather than input-based, such as the transient "press again to quit"
//! hint. The pane schedules redraws so those hints can expire even when the UI is otherwise idle.
pub use xedoc_tui_input::key_hint;
pub use xedoc_tui_input::keymap;
pub use xedoc_tui_render::city_lights;
pub use xedoc_tui_render::render;
pub use xedoc_tui_render::terminal_palette;
pub use xedoc_tui_render::wrapping;
pub use xedoc_tui_status::status;
pub use xedoc_tui_status::status_indicator_widget;
pub use xedoc_tui_status::token_usage;
pub use xedoc_tui_transcript::line_truncation;
pub use xedoc_tui_transcript::live_wrap;

pub mod app_command {
    pub use xedoc_tui_events::AppCommand;
}

pub mod app_event {
    pub use xedoc_tui_events::*;
}

pub mod app_event_sender {
    pub use xedoc_tui_events::AppEventSender;
}

pub mod tui {
    pub use xedoc_tui_frame::FrameRequester;
}

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::active_agent_list::ActiveAgentList;
use crate::active_task_list::ActiveTaskList;
use crate::app_event::AppEvent;
use crate::app_event::HistoryLookupResponse;
use crate::app_event_sender::AppEventSender;
use crate::key_hint::KeyBinding;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::RuntimeKeymap;
use crate::keymap::primary_binding;
use crate::pending_thread_approvals::PendingThreadApprovals;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableItem;
use crate::terminal_palette::effective_stdout_color_level;
use crate::tui::FrameRequester;
use crate::unified_exec_footer::UnifiedExecFooter;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::time::Duration;
use std::time::Instant;
use xedoc_app_server_protocol::SkillMetadata;
use xedoc_app_server_protocol::ToolRequestUserInputParams;
use xedoc_features::Features;
use xedoc_file_search::FileMatch;
use xedoc_plugin::PluginCapabilitySummary;
use xedoc_protocol::ThreadId;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::plan_tool::PlanItemArg;
use xedoc_protocol::user_input::TextElement;
use xedoc_tui_events::ResolvedAppServerRequest;
pub use xedoc_tui_overlays::bottom_pane::BottomPaneView;
pub use xedoc_tui_overlays::bottom_pane::ViewCompletion;
pub use xedoc_tui_overlays::bottom_pane::approval_overlay;
pub use xedoc_tui_overlays::bottom_pane::bottom_pane_view;
pub use xedoc_tui_overlays::bottom_pane::list_selection_view;
pub use xedoc_tui_overlays::bottom_pane::mcp_server_elicitation;

mod active_agent_list;
mod active_task_list;
pub use active_agent_list::ActiveAgentEntry;
use slash_commands::ServiceTierCommand;
use slash_commands::SessionExtensionCommand;
pub use xedoc_tui_completion::prompt_args;
pub use xedoc_tui_completion::slash_commands;
pub use xedoc_tui_composer::bottom_pane::ChatComposer;
pub use xedoc_tui_composer::bottom_pane::CollaborationModeIndicator;
pub use xedoc_tui_composer::bottom_pane::GoalStatusIndicator;
pub use xedoc_tui_composer::bottom_pane::HistoryEntry;
pub use xedoc_tui_composer::bottom_pane::InputResult;
use xedoc_tui_composer::bottom_pane::PendingInputPreview;
pub use xedoc_tui_composer::bottom_pane::QUIT_SHORTCUT_TIMEOUT;
pub use xedoc_tui_composer::bottom_pane::chat_composer;
pub use xedoc_tui_composer::bottom_pane::effort_ignition;
#[cfg(any(test, feature = "test-support"))]
pub use xedoc_tui_composer::bottom_pane::goal_status_indicator_line;
pub use xedoc_tui_hooks::browser::HooksBrowserView;
pub use xedoc_tui_input::LocalImageAttachment;
pub use xedoc_tui_input::MentionBinding;
pub use xedoc_tui_input::QueuedInputAction;
pub use xedoc_tui_overlays::bottom_pane::ApplyPatchApprovalRequest;
pub use xedoc_tui_overlays::bottom_pane::ApprovalOverlay;
pub use xedoc_tui_overlays::bottom_pane::ApprovalRequest;
pub use xedoc_tui_overlays::bottom_pane::ColumnWidthMode;
pub use xedoc_tui_overlays::bottom_pane::ExecApprovalRequest;
pub use xedoc_tui_overlays::bottom_pane::ListSelectionView;
pub use xedoc_tui_overlays::bottom_pane::McpElicitationApprovalRequest;
pub use xedoc_tui_overlays::bottom_pane::McpServerElicitationFormRequest;
pub use xedoc_tui_overlays::bottom_pane::McpServerElicitationOverlay;
pub use xedoc_tui_overlays::bottom_pane::OnSelectionChangedCallback;
pub use xedoc_tui_overlays::bottom_pane::PermissionsApprovalRequest;
pub use xedoc_tui_overlays::bottom_pane::RequestUserInputOverlay;
pub use xedoc_tui_overlays::bottom_pane::ScriptedInteractionView;
pub use xedoc_tui_overlays::bottom_pane::SelectionRowDisplay;
pub use xedoc_tui_overlays::bottom_pane::SelectionToggle;
pub use xedoc_tui_overlays::bottom_pane::SelectionViewParams;
pub use xedoc_tui_overlays::bottom_pane::SideContentWidth;
pub use xedoc_tui_overlays::bottom_pane::SkillsToggleItem;
pub use xedoc_tui_overlays::bottom_pane::SkillsToggleView;
pub use xedoc_tui_overlays::bottom_pane::StatusLineItem;
pub use xedoc_tui_overlays::bottom_pane::StatusLineSetupView;
pub use xedoc_tui_overlays::bottom_pane::StatusSurfacePreviewData;
pub use xedoc_tui_overlays::bottom_pane::StatusSurfacePreviewItem;
pub use xedoc_tui_overlays::bottom_pane::TerminalTitleItem;
pub use xedoc_tui_overlays::bottom_pane::TerminalTitleSetupView;
pub use xedoc_tui_overlays::bottom_pane::build_action_required_title_text;
pub use xedoc_tui_overlays::bottom_pane::custom_prompt_view;
pub use xedoc_tui_overlays::bottom_pane::format_requested_permissions_rule;
pub use xedoc_tui_overlays::bottom_pane::popup_content_width;
#[cfg(any(test, feature = "test-support"))]
pub use xedoc_tui_overlays::bottom_pane::preview_line_for_title_items;
pub use xedoc_tui_overlays::bottom_pane::side_by_side_layout_widths;
pub use xedoc_tui_overlays::bottom_pane::status_line_from_segments;
mod pending_thread_approvals;
pub use xedoc_tui_completion::popup_consts;
mod unified_exec_footer;
pub use xedoc_tui_overlays::bottom_pane::SelectionTab;

/// How long the "press again to quit" hint stays visible.
///
/// This is shared between:
/// - `ChatWidget`: arming the double-press quit shortcut.
/// - `BottomPane`/`ChatComposer`: rendering and expiring the footer hint.
///
/// Keeping a single value ensures Ctrl+C and Ctrl+D behave identically.
const APPROVAL_PROMPT_TYPING_IDLE_DELAY: Duration = Duration::from_secs(1);

/// Whether Ctrl+C/Ctrl+D require a second press to quit.
///
/// This UX experiment was enabled by default, but requiring a double press to quit feels janky in
/// practice (especially for users accustomed to shells and other TUIs). Disable it for now while we
/// rethink a better quit/interrupt design.
pub const DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED: bool = false;

/// The result of offering a cancellation key to a bottom-pane surface.
///
/// This is primarily used for Ctrl+C routing: active views can consume the key to dismiss
/// themselves, and the caller can decide what higher-level action (if any) to take when the key is
/// not handled locally.
pub use xedoc_tui_overlays::bottom_pane::CancellationEvent;

use crate::prompt_args::parse_slash_name;
use crate::status_indicator_widget::StatusDetailsCapitalization;
use crate::status_indicator_widget::StatusIndicatorWidget;
pub use xedoc_tui_overlays::bottom_pane::ExperimentalFeatureItem;
pub use xedoc_tui_overlays::bottom_pane::ExperimentalFeaturesView;
pub use xedoc_tui_overlays::bottom_pane::SELECTION_TOGGLE_BLOCKED_PREFIX;
pub use xedoc_tui_overlays::bottom_pane::SELECTION_TOGGLE_UNAVAILABLE_PREFIX;
pub use xedoc_tui_overlays::bottom_pane::SelectionAction;
pub use xedoc_tui_overlays::bottom_pane::SelectionItem;

struct DelayedApprovalRequest {
    request: ApprovalRequest,
    features: Features,
}

struct QueuedScriptedInteraction {
    request: xedoc_app_server_protocol::ExtensionInteractionRequestParams,
    model_router_settings: bool,
}

#[derive(Clone)]
struct ComposerRuntimeContext {
    model: String,
    effort: Option<String>,
    fast: bool,
}

/// Pane displayed in the lower half of the chat UI.
///
/// This is the owning container for the prompt input (`ChatComposer`) and the view stack
/// (`BottomPaneView`). It performs local input routing and renders time-based hints, while leaving
/// process-level decisions (quit, interrupt, shutdown) to `ChatWidget`.
pub struct BottomPane {
    /// Composer is retained even when a BottomPaneView is displayed so the
    /// input state is retained when the view is closed.
    composer: ChatComposer,

    /// Stack of views displayed instead of the composer (e.g. popups/modals).
    view_stack: Vec<Box<dyn BottomPaneView>>,
    delayed_approval_requests: VecDeque<DelayedApprovalRequest>,
    queued_scripted_interactions: VecDeque<QueuedScriptedInteraction>,
    last_composer_activity_at: Option<Instant>,

    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,
    thread_id: Option<ThreadId>,

    has_input_focus: bool,
    enhanced_keys_supported: bool,
    disable_paste_burst: bool,
    is_task_running: bool,
    is_model_router_running: bool,
    active_turn_started_at: Option<Instant>,
    runtime_context: Option<ComposerRuntimeContext>,
    model_router_mode: Option<String>,
    active_turn_running: bool,
    can_disconnect_active_turn: bool,
    esc_backtrack_hint: bool,
    animations_enabled: bool,

    /// Inline status indicator shown above the composer while a task is running.
    status: Option<StatusIndicatorWidget>,
    /// Unified exec session summary source.
    ///
    /// When a status row exists, this summary is mirrored inline in that row;
    /// when no status row exists, it renders as its own footer row.
    unified_exec_footer: UnifiedExecFooter,
    /// Preview of pending steers and queued drafts shown above the composer.
    pending_input_preview: PendingInputPreview,
    /// Current update_plan checklist shown directly above the composer while a task runs.
    active_task_list: ActiveTaskList,
    /// Running subagents shown above the active task list.
    active_agent_list: ActiveAgentList,
    /// Inactive threads with pending approval requests.
    pending_thread_approvals: PendingThreadApprovals,
    context_window_percent: Option<i64>,
    context_window_used_tokens: Option<i64>,
    keymap: RuntimeKeymap,
}

pub struct BottomPaneParams {
    pub app_event_tx: AppEventSender,
    pub frame_requester: FrameRequester,
    pub has_input_focus: bool,
    pub enhanced_keys_supported: bool,
    pub placeholder_text: String,
    pub disable_paste_burst: bool,
    pub animations_enabled: bool,
    pub skills: Option<Vec<SkillMetadata>>,
}

impl BottomPane {
    pub fn new(params: BottomPaneParams) -> Self {
        let BottomPaneParams {
            app_event_tx,
            frame_requester,
            has_input_focus,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
            animations_enabled,
            skills,
        } = params;
        let mut composer = ChatComposer::new(
            has_input_focus,
            app_event_tx.clone(),
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
        );
        composer.set_frame_requester(frame_requester.clone());
        let keymap = RuntimeKeymap::defaults();
        composer.set_keymap_bindings(&keymap);
        composer.set_skill_mentions(skills);
        Self {
            composer,
            view_stack: Vec::new(),
            delayed_approval_requests: VecDeque::new(),
            queued_scripted_interactions: VecDeque::new(),
            last_composer_activity_at: None,
            app_event_tx,
            frame_requester: frame_requester.clone(),
            thread_id: None,
            has_input_focus,
            enhanced_keys_supported,
            disable_paste_burst,
            is_task_running: false,
            is_model_router_running: false,
            active_turn_started_at: None,
            runtime_context: None,
            model_router_mode: None,
            active_turn_running: false,
            can_disconnect_active_turn: false,
            status: None,
            unified_exec_footer: UnifiedExecFooter::new(),
            pending_input_preview: PendingInputPreview::new(),
            active_task_list: ActiveTaskList::new(),
            active_agent_list: ActiveAgentList::new(frame_requester),
            pending_thread_approvals: PendingThreadApprovals::new(),
            esc_backtrack_hint: false,
            animations_enabled,
            context_window_percent: None,
            context_window_used_tokens: None,
            keymap,
        }
    }

    pub fn set_skills(&mut self, skills: Option<Vec<SkillMetadata>>) {
        self.composer.set_skill_mentions(skills);
        self.request_redraw();
    }

    /// Update image-paste behavior for the active composer and repaint immediately.
    ///
    /// Callers use this to keep composer affordances aligned with model capabilities.
    pub fn set_image_paste_enabled(&mut self, enabled: bool) {
        self.composer.set_image_paste_enabled(enabled);
        self.request_redraw();
    }

    /// Mirrors the effective reasoning effort into the composer so its next
    /// visible frame can play a one-shot Max/Ultra effect.
    pub fn set_active_reasoning_effort(&mut self, effort: Option<&ReasoningEffort>) {
        let animations_enabled = effort_ignition::effort_animation_enabled(
            self.animations_enabled,
            effective_stdout_color_level(),
        );
        if self
            .composer
            .set_active_reasoning_effort(effort, animations_enabled)
        {
            self.request_redraw();
        }
    }

    pub fn set_runtime_context(&mut self, model: &str, effort: Option<String>, fast: bool) {
        self.runtime_context = Some(ComposerRuntimeContext {
            model: model.to_string(),
            effort,
            fast,
        });
    }

    pub fn set_model_router_mode(&mut self, mode: Option<String>) {
        self.model_router_mode = mode;
        self.request_redraw();
    }

    /// Returns router mode, elapsed time, and the displayed model pair for alternate renderers.
    pub fn browser_context(&self) -> (Option<&str>, Option<u64>, Option<&str>, Option<&str>) {
        (
            self.model_router_mode.as_deref(),
            self.active_turn_started_at
                .map(|started| started.elapsed().as_secs()),
            self.runtime_context
                .as_ref()
                .map(|context| context.model.as_str()),
            self.runtime_context
                .as_ref()
                .and_then(|context| context.effort.as_deref()),
        )
    }

    /// Reports the composer height independently of activity surfaces.
    pub fn browser_composer_height(&self, width: u16) -> u16 {
        ChatComposerRightReserveRenderable {
            composer: &self.composer,
            right_reserve: 0,
            active_turn_elapsed_seconds: None,
            runtime_context: self.runtime_context.as_ref(),
            model_router_mode: self.model_router_mode.as_deref(),
        }
        .desired_height(width)
    }

    pub fn browser_popup_height(&self, width: u16) -> u16 {
        self.composer.browser_popup_height(width)
    }

    /// Whether an alternate renderer must retain the active terminal dialog.
    pub fn browser_has_dialog(&self) -> bool {
        self.active_view().is_some()
    }

    /// Return the active question without terminal layout.
    pub fn browser_question(&self) -> Option<bottom_pane_view::BrowserQuestion> {
        self.active_view()
            .and_then(BottomPaneView::browser_question)
    }

    /// Route native question controls through the shared view and completion cleanup.
    pub fn handle_browser_question(
        &mut self,
        request_id: &str,
        index: usize,
        action: &str,
        option: Option<usize>,
        text: Option<&str>,
    ) {
        let Some(view) = self.view_stack.last_mut() else {
            return;
        };
        if !view.handle_browser_question(request_id, index, action, option, text) {
            return;
        }
        if view.is_complete() {
            let completion = view.completion();
            self.pop_active_view_with_completion(completion);
        }
        self.request_redraw();
    }

    /// Returns the shared status line without terminal layout.
    pub fn browser_status_line(&self) -> Option<String> {
        self.composer.status_line_text()
    }

    pub fn browser_status_line_styled(&self) -> Option<Line<'static>> {
        self.composer.browser_status_line()
    }

    /// Establishes a restored thread's effort without replaying its one-shot animation.
    pub fn set_active_reasoning_effort_baseline(&mut self, effort: Option<&ReasoningEffort>) {
        self.composer.set_active_reasoning_effort_baseline(effort);
    }

    pub fn set_plugin_mentions(&mut self, plugins: Option<Vec<PluginCapabilitySummary>>) {
        self.composer.set_plugin_mentions(plugins);
        self.request_redraw();
    }

    pub fn set_plugins_command_enabled(&mut self, enabled: bool) {
        self.composer.set_plugins_command_enabled(enabled);
        self.request_redraw();
    }

    pub fn set_mentions_v2_enabled(&mut self, enabled: bool) {
        self.composer.set_mentions_v2_enabled(enabled);
        self.request_redraw();
    }

    pub fn take_mention_bindings(&mut self) -> Vec<MentionBinding> {
        self.composer.take_mention_bindings()
    }

    pub fn take_recent_submission_mention_bindings(&mut self) -> Vec<MentionBinding> {
        self.composer.take_recent_submission_mention_bindings()
    }

    /// Add a staged slash-command draft to the composer's local recall list.
    ///
    /// This should be called exactly once after `ChatWidget` dispatches a recognized command.
    /// Slash recall records the submitted command text regardless of whether the command succeeds.
    pub fn record_pending_slash_command_history(&mut self) {
        self.composer.record_pending_slash_command_history();
    }

    /// Replace all bottom-pane keymap caches from one resolved runtime keymap.
    ///
    /// The bottom pane owns several input surfaces: composer, overlays, and
    /// selection views. Applying one snapshot through this method keeps those
    /// surfaces synchronized after config reloads or interactive remaps. Callers
    /// should not update the composer directly unless they deliberately want
    /// overlays and selection views to continue using the previous bindings.
    pub fn set_keymap_bindings(&mut self, keymap: &RuntimeKeymap) {
        self.keymap = keymap.clone();
        self.composer.set_keymap_bindings(keymap);
        let interrupt_binding = primary_binding(&keymap.chat.interrupt_turn);
        self.pending_input_preview
            .set_interrupt_binding(interrupt_binding);
        if let Some(status) = self.status.as_mut() {
            status.set_interrupt_binding(interrupt_binding);
        }
        self.request_redraw();
    }

    /// Clear pending attachments and mention bindings e.g. when a slash command doesn't submit text.
    pub fn drain_pending_submission_state(&mut self) {
        let _ = self.take_recent_submission_images_with_placeholders();
        let _ = self.take_remote_image_urls();
        let _ = self.take_recent_submission_mention_bindings();
        let _ = self.take_mention_bindings();
    }

    pub fn set_collaboration_modes_enabled(&mut self, enabled: bool) {
        self.composer.set_collaboration_modes_enabled(enabled);
        self.request_redraw();
    }

    pub fn set_collaboration_mode_indicator(
        &mut self,
        indicator: Option<CollaborationModeIndicator>,
    ) {
        self.composer.set_collaboration_mode_indicator(indicator);
        self.request_redraw();
    }

    pub fn set_goal_status_indicator(&mut self, indicator: Option<GoalStatusIndicator>) {
        self.composer.set_goal_status_indicator(indicator);
        self.request_redraw();
    }

    pub fn set_ide_context_active(&mut self, active: bool) {
        self.composer.set_ide_context_active(active);
        self.request_redraw();
    }

    pub fn set_session_name(&mut self, session_name: Option<String>) {
        if self.composer.set_session_name(session_name) {
            self.request_redraw();
        }
    }

    pub fn set_personality_command_enabled(&mut self, enabled: bool) {
        self.composer.set_personality_command_enabled(enabled);
        self.request_redraw();
    }

    pub fn set_service_tier_commands_enabled(&mut self, enabled: bool) {
        self.composer.set_service_tier_commands_enabled(enabled);
        self.request_redraw();
    }

    pub fn set_service_tier_commands(&mut self, commands: Vec<ServiceTierCommand>) {
        self.composer.set_service_tier_commands(commands);
        self.request_redraw();
    }

    pub fn set_session_extension_commands(&mut self, commands: Vec<SessionExtensionCommand>) {
        self.composer.set_session_extension_commands(commands);
        self.request_redraw();
    }

    pub fn set_goal_command_enabled(&mut self, enabled: bool) {
        self.composer.set_goal_command_enabled(enabled);
        self.request_redraw();
    }

    pub fn set_side_conversation_active(&mut self, active: bool) {
        self.composer.set_side_conversation_active(active);
        self.request_redraw();
    }

    pub fn set_placeholder_text(&mut self, placeholder: String) {
        self.composer.set_placeholder_text(placeholder);
        self.request_redraw();
    }

    pub fn set_parent_owned_thread(&mut self) {
        self.composer.set_parent_owned_thread();
        self.request_redraw();
    }

    /// Update the key hint shown next to queued messages so it matches the
    /// binding that `ChatWidget` actually listens for.
    pub fn set_queued_message_edit_binding(&mut self, binding: Option<KeyBinding>) {
        self.pending_input_preview.set_edit_binding(binding);
        self.request_redraw();
    }

    pub fn set_vim_enabled(&mut self, enabled: bool) {
        self.composer.set_vim_enabled(enabled);
        self.request_redraw();
    }

    pub fn toggle_vim_enabled(&mut self) -> bool {
        let enabled = self.composer.toggle_vim_enabled();
        self.request_redraw();
        enabled
    }

    pub fn status_widget(&self) -> Option<&StatusIndicatorWidget> {
        self.status.as_ref()
    }

    pub fn skills(&self) -> Option<&Vec<SkillMetadata>> {
        self.composer.skills()
    }

    pub fn plugins(&self) -> Option<&Vec<PluginCapabilitySummary>> {
        self.composer.plugins()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn context_window_percent(&self) -> Option<i64> {
        self.context_window_percent
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn context_window_used_tokens(&self) -> Option<i64> {
        self.context_window_used_tokens
    }

    fn active_view(&self) -> Option<&dyn BottomPaneView> {
        self.view_stack.last().map(std::convert::AsRef::as_ref)
    }

    fn push_view(&mut self, view: Box<dyn BottomPaneView>) {
        self.view_stack.push(view);
        self.schedule_active_view_frame();
        self.request_redraw();
    }

    fn pop_active_view_with_completion(&mut self, completion: Option<ViewCompletion>) {
        if self.view_stack.pop().is_some() {
            match completion {
                Some(ViewCompletion::Accepted) => {
                    while self
                        .view_stack
                        .last()
                        .is_some_and(|view| view.dismiss_after_child_accept())
                    {
                        self.view_stack.pop();
                    }
                }
                Some(ViewCompletion::Cancelled) => {
                    if let Some(view) = self.view_stack.last_mut() {
                        view.clear_dismiss_after_child_accept();
                    }
                }
                None => {}
            }
            self.on_view_stack_depth_decreased();
        }
    }

    fn on_view_stack_depth_decreased(&mut self) {
        if self.view_stack.is_empty() {
            self.on_active_view_complete();
        }
    }

    fn approval_prompt_delay_remaining(&self, now: Instant) -> Option<Duration> {
        self.last_composer_activity_at.and_then(|last_activity_at| {
            last_activity_at
                .checked_add(APPROVAL_PROMPT_TYPING_IDLE_DELAY)
                .and_then(|show_at| show_at.checked_duration_since(now))
                .filter(|delay| !delay.is_zero())
        })
    }

    fn record_composer_activity_at(&mut self, now: Instant) {
        self.last_composer_activity_at = Some(now);
        if !self.delayed_approval_requests.is_empty()
            && let Some(delay) = self.approval_prompt_delay_remaining(now)
        {
            self.request_redraw_in(delay);
        }
    }

    fn maybe_show_delayed_approval_requests_at(&mut self, now: Instant) {
        if self.delayed_approval_requests.is_empty() || !self.view_stack.is_empty() {
            return;
        }
        if let Some(delay) = self.approval_prompt_delay_remaining(now) {
            self.request_redraw_in(delay);
            return;
        }

        // Promote the oldest delayed approval once typing has been idle long enough.
        // `ApprovalOverlay` advances its internal queue with `pop()`, so drain the
        // remaining delayed approvals from the back to preserve FIFO display order.
        let Some(first) = self.delayed_approval_requests.pop_front() else {
            return;
        };
        let mut modal = ApprovalOverlay::new(
            first.request,
            self.app_event_tx.clone(),
            first.features,
            self.keymap.approval.clone(),
            self.keymap.list.clone(),
        );
        while let Some(delayed) = self.delayed_approval_requests.pop_back() {
            modal.enqueue_request(delayed.request);
        }
        self.pause_status_timer_for_modal();
        self.push_view(Box::new(modal));
    }

    /// Forward a key event to the active view or the composer.
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> InputResult {
        // If a modal/view is active, handle it here; otherwise forward to composer.
        if !self.view_stack.is_empty() {
            if key_event.kind == KeyEventKind::Release {
                return InputResult::None;
            }

            // We need three pieces of information after routing the key:
            // whether Esc completed the view, whether the view finished for any
            // reason, and whether a paste-burst timer should be scheduled.
            let (ctrl_c_completed, view_complete, completion, view_in_paste_burst) = {
                let last_index = self.view_stack.len() - 1;
                let view = &mut self.view_stack[last_index];
                let prefer_esc =
                    key_event.code == KeyCode::Esc && view.prefer_esc_to_handle_key_event();
                let ctrl_c_completed = key_event.code == KeyCode::Esc
                    && !prefer_esc
                    && matches!(view.on_ctrl_c(), CancellationEvent::Handled)
                    && view.is_complete();
                if ctrl_c_completed {
                    (true, true, view.completion(), false)
                } else {
                    view.handle_key_event(key_event);
                    (
                        false,
                        view.is_complete(),
                        view.completion(),
                        view.is_in_paste_burst(),
                    )
                }
            };

            if ctrl_c_completed {
                self.pop_active_view_with_completion(completion);
                if let Some(next_view) = self.view_stack.last()
                    && next_view.is_in_paste_burst()
                {
                    self.request_redraw_in(ChatComposer::recommended_paste_flush_delay());
                }
            } else if view_complete {
                self.pop_active_view_with_completion(completion);
            } else if view_in_paste_burst {
                self.request_redraw_in(ChatComposer::recommended_paste_flush_delay());
            }
            self.request_redraw();
            InputResult::None
        } else {
            // If a task is running and a status line is visible, allow the
            // configured action to interrupt even while the composer has focus.
            // When a popup is active, prefer dismissing it over interrupting the task.
            if self.should_interrupt_running_task(key_event)
                && let Some(status) = &self.status
            {
                // Send Op::Interrupt
                status.interrupt();
                self.request_redraw();
                return InputResult::None;
            }
            let records_composer_activity =
                matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                    && !key_hint::has_ctrl_or_alt(key_event.modifiers)
                    && matches!(
                        key_event.code,
                        KeyCode::Char(_)
                            | KeyCode::Backspace
                            | KeyCode::Delete
                            | KeyCode::Enter
                            | KeyCode::Tab
                    );
            let (input_result, needs_redraw) = self.composer.handle_key_event(key_event);
            if records_composer_activity {
                self.record_composer_activity_at(Instant::now());
            }
            if needs_redraw {
                self.request_redraw();
            }
            if self.composer.is_in_paste_burst() {
                self.request_redraw_in(ChatComposer::recommended_paste_flush_delay());
            }
            input_result
        }
    }

    /// Handles a Ctrl+C press within the bottom pane.
    ///
    /// An active modal view is given the first chance to consume the key (typically to dismiss
    /// itself). If no view is active, Ctrl+C cancels active history search before falling back to
    /// clearing draft composer input.
    ///
    /// This method may show the quit shortcut hint as a user-visible acknowledgement that Ctrl+C
    /// was received, but it does not decide whether the process should exit; `ChatWidget` owns the
    /// quit/interrupt state machine and uses the result to decide what happens next.
    pub fn on_ctrl_c(&mut self) -> CancellationEvent {
        if let Some(view) = self.view_stack.last_mut() {
            let event = view.on_ctrl_c();
            let view_complete = view.is_complete();
            let completion = view.completion();
            if matches!(event, CancellationEvent::Handled) {
                if view_complete {
                    self.pop_active_view_with_completion(completion);
                }
                self.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')));
                self.request_redraw();
            }
            event
        } else if self.composer.cancel_history_search() {
            self.request_redraw();
            CancellationEvent::Handled
        } else if self.composer_is_empty() {
            CancellationEvent::NotHandled
        } else {
            self.view_stack.pop();
            self.clear_composer_for_ctrl_c();
            self.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')));
            self.request_redraw();
            CancellationEvent::Handled
        }
    }

    pub fn handle_paste(&mut self, pasted: String) {
        let has_pasted_text = !pasted.is_empty();
        if let Some(view) = self.view_stack.last_mut() {
            let needs_redraw = view.handle_paste(pasted);
            let view_complete = view.is_complete();
            if view_complete {
                self.view_stack.clear();
                self.on_active_view_complete();
            }
            if needs_redraw || view_complete {
                self.request_redraw();
            }
        } else {
            let needs_redraw = self.composer.handle_paste(pasted);
            if has_pasted_text {
                self.record_composer_activity_at(Instant::now());
            }
            if needs_redraw {
                self.request_redraw();
            }
        }
    }

    pub fn insert_str(&mut self, text: &str) {
        self.composer.insert_str(text);
        self.request_redraw();
    }

    pub fn pre_draw_tick(&mut self) {
        self.pre_draw_tick_at(Instant::now());
    }

    fn pre_draw_tick_at(&mut self, now: Instant) {
        self.composer.sync_popups();
        self.maybe_show_delayed_approval_requests_at(now);
        self.tick_active_view(now);
        self.schedule_active_view_frame();
    }

    fn tick_active_view(&mut self, now: Instant) {
        let Some(view) = self.view_stack.last_mut() else {
            return;
        };
        let needs_redraw = view.pre_draw_tick(now);
        let view_complete = view.is_complete();
        if view_complete {
            self.view_stack.clear();
            self.on_active_view_complete();
        }
        if needs_redraw || view_complete {
            self.request_redraw();
        }
    }

    fn schedule_active_view_frame(&self) {
        if let Some(delay) = self
            .active_view()
            .and_then(BottomPaneView::next_frame_delay)
        {
            self.request_redraw_in(delay);
        }
    }

    /// Replace the composer text with `text`.
    ///
    /// This is intended for fresh input where mention linkage does not need to
    /// survive; it routes to `ChatComposer::set_text_content`, which resets
    /// mention bindings.
    pub fn set_composer_text(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        self.composer
            .set_text_content(text, text_elements, local_image_paths);
        self.composer.move_cursor_to_end();
        self.request_redraw();
    }

    /// Replace the composer text while preserving mention link targets.
    ///
    /// Use this when rehydrating a draft after a local validation/gating
    /// failure (for example unsupported image submit) so previously selected
    /// mention targets remain stable across retry.
    pub fn set_composer_text_with_mention_bindings(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
        mention_bindings: Vec<MentionBinding>,
    ) {
        self.composer.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.composer.move_cursor_to_end();
        self.request_redraw();
    }

    #[allow(dead_code)]
    pub fn set_composer_input_enabled(&mut self, enabled: bool, placeholder: Option<String>) {
        self.composer.set_input_enabled(enabled, placeholder);
        self.request_redraw();
    }

    pub fn show_shutdown_in_progress(&mut self) {
        self.view_stack.clear();
        self.composer.show_shutdown_in_progress();
        self.request_redraw();
    }

    pub fn clear_composer_for_ctrl_c(&mut self) {
        if let Some(text) = self.composer.clear_for_ctrl_c() {
            if let Some(thread_id) = self.thread_id {
                self.app_event_tx
                    .send(AppEvent::AppendMessageHistoryEntry { thread_id, text });
            } else {
                tracing::warn!(
                    "failed to append Ctrl+C-cleared draft to history: no active thread id"
                );
            }
        }
        self.request_redraw();
    }

    /// Get the current composer text (for tests and programmatic checks).
    pub fn composer_text(&self) -> String {
        self.composer.current_text()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn composer_cursor(&self) -> usize {
        self.composer.cursor()
    }

    pub fn composer_draft_snapshot(&self) -> chat_composer::ComposerDraftSnapshot {
        self.composer.draft_snapshot()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn composer_text_elements(&self) -> Vec<TextElement> {
        self.composer.text_elements()
    }

    pub fn composer_local_images(&self) -> Vec<LocalImageAttachment> {
        self.composer.local_images()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn composer_local_image_paths(&self) -> Vec<PathBuf> {
        self.composer.local_image_paths()
    }

    pub fn composer_text_with_pending(&self) -> String {
        self.composer.current_text_with_pending()
    }

    /// Returns whether the composer currently accepts interactive draft edits.
    pub fn composer_input_enabled(&self) -> bool {
        self.composer.input_enabled()
    }

    pub fn composer_pending_pastes(&self) -> Vec<(String, String)> {
        self.composer.pending_pastes()
    }

    pub fn apply_external_edit(&mut self, text: String) {
        self.composer.apply_external_edit(text);
        self.request_redraw();
    }

    pub fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.composer.set_footer_hint_override(items);
        self.request_redraw();
    }

    /// Applies the externally decided Plan-mode nudge visibility to the footer presentation.
    pub fn set_plan_mode_nudge_visible(&mut self, visible: bool) {
        if self.composer.set_plan_mode_nudge_visible(visible) {
            self.request_redraw();
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn plan_mode_nudge_visible(&self) -> bool {
        self.composer.plan_mode_nudge_visible()
    }

    pub fn set_remote_image_urls(&mut self, urls: Vec<String>) {
        self.composer.set_remote_image_urls(urls);
        self.request_redraw();
    }

    pub fn remote_image_urls(&self) -> Vec<String> {
        self.composer.remote_image_urls()
    }

    pub fn take_remote_image_urls(&mut self) -> Vec<String> {
        let urls = self.composer.take_remote_image_urls();
        self.request_redraw();
        urls
    }

    pub fn set_composer_pending_pastes(&mut self, pending_pastes: Vec<(String, String)>) {
        self.composer.set_pending_pastes(pending_pastes);
        self.request_redraw();
    }

    /// Update the status indicator header (defaults to "Working") and details below it.
    ///
    /// Passing `None` clears any existing details. Returns whether the active status indicator
    /// was updated and requested a redraw.
    pub fn update_status(
        &mut self,
        header: String,
        details: Option<String>,
        details_capitalization: StatusDetailsCapitalization,
        details_max_lines: usize,
    ) -> bool {
        if let Some(status) = self.status.as_mut() {
            status.update_header(header);
            status.update_details(details, details_capitalization, details_max_lines.max(1));
            self.request_redraw();
            return true;
        }
        false
    }

    /// Show the transient "press again to quit" hint for `key`.
    ///
    /// `ChatWidget` owns the quit shortcut state machine (it decides when quit is
    /// allowed), while the bottom pane owns rendering. We also schedule a redraw
    /// after [`QUIT_SHORTCUT_TIMEOUT`] so the hint disappears even if the user
    /// stops typing and no other events trigger a draw.
    pub fn show_quit_shortcut_hint(&mut self, key: KeyBinding) {
        if !DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
            return;
        }

        self.composer
            .show_quit_shortcut_hint(key, self.has_input_focus);
        let frame_requester = self.frame_requester.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::time::sleep(QUIT_SHORTCUT_TIMEOUT).await;
                frame_requester.schedule_frame();
            });
        } else {
            // In tests (and other non-Tokio contexts), fall back to a thread so
            // the hint can still expire without requiring an explicit draw.
            std::thread::spawn(move || {
                std::thread::sleep(QUIT_SHORTCUT_TIMEOUT);
                frame_requester.schedule_frame();
            });
        }
        self.request_redraw();
    }

    /// Clear the "press again to quit" hint immediately.
    pub fn clear_quit_shortcut_hint(&mut self) {
        self.composer.clear_quit_shortcut_hint(self.has_input_focus);
        self.request_redraw();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn quit_shortcut_hint_visible(&self) -> bool {
        self.composer.quit_shortcut_hint_visible()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn status_indicator_visible(&self) -> bool {
        self.status.is_some()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn active_task_list_visible(&self) -> bool {
        !self.active_task_list.is_empty()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn active_agent_list_visible(&self) -> bool {
        !self.active_agent_list.is_empty()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn status_line_text(&self) -> Option<String> {
        self.composer.status_line_text()
    }

    pub fn show_esc_backtrack_hint(&mut self) {
        self.esc_backtrack_hint = true;
        self.composer.set_esc_backtrack_hint(/*show*/ true);
        self.request_redraw();
    }

    pub fn clear_esc_backtrack_hint(&mut self) {
        if self.esc_backtrack_hint {
            self.esc_backtrack_hint = false;
            self.composer.set_esc_backtrack_hint(/*show*/ false);
            self.request_redraw();
        }
    }

    // esc_backtrack_hint_visible removed; hints are controlled internally.

    pub fn set_task_running(&mut self, running: bool) {
        let was_running = self.is_task_running;
        self.is_task_running = running;
        self.composer.set_task_running(running);

        if running {
            if !was_running {
                self.active_task_list.clear();
                if self.status.is_none() {
                    self.status = Some(StatusIndicatorWidget::new(
                        self.app_event_tx.clone(),
                        self.frame_requester.clone(),
                        self.animations_enabled,
                    ));
                }
                if let Some(status) = self.status.as_mut() {
                    status.set_interrupt_hint_visible(/*visible*/ true);
                    status.set_interrupt_binding(primary_binding(&self.keymap.chat.interrupt_turn));
                    status.set_detach_hint_visible(
                        self.can_disconnect_active_turn && self.active_turn_running,
                    );
                }
                self.sync_status_inline_message();
                self.request_redraw();
            }
        } else {
            // Hide the status indicator when a task completes, but keep other modal views.
            self.hide_status_indicator();
            self.active_task_list.clear();
        }
    }

    pub fn set_model_router_running(&mut self, running: bool) {
        self.is_model_router_running = running;
    }

    /// Sets the start time for the active agent turn shown on the composer border.
    pub fn set_active_turn_started_at(&mut self, started_at: Option<Instant>) {
        self.active_turn_started_at = started_at;
    }

    pub fn set_queue_submissions(&mut self, queue_submissions: bool) {
        self.composer.set_queue_submissions(queue_submissions);
    }

    /// Hide the status indicator while leaving task-running state untouched.
    pub fn hide_status_indicator(&mut self) {
        if self.status.take().is_some() {
            self.request_redraw();
        }
    }

    pub fn ensure_status_indicator(&mut self) {
        if self.status.is_none() {
            self.status = Some(StatusIndicatorWidget::new(
                self.app_event_tx.clone(),
                self.frame_requester.clone(),
                self.animations_enabled,
            ));
            if let Some(status) = self.status.as_mut() {
                status.set_interrupt_binding(primary_binding(&self.keymap.chat.interrupt_turn));
                status.set_detach_hint_visible(
                    self.can_disconnect_active_turn && self.active_turn_running,
                );
            }
            self.sync_status_inline_message();
            self.request_redraw();
        }
    }

    pub fn set_interrupt_hint_visible(&mut self, visible: bool) {
        if let Some(status) = self.status.as_mut() {
            status.set_interrupt_hint_visible(visible);
            self.request_redraw();
        }
    }

    pub fn set_can_disconnect_active_turn(&mut self, can_disconnect: bool) {
        self.can_disconnect_active_turn = can_disconnect;
        if let Some(status) = self.status.as_mut() {
            status.set_detach_hint_visible(can_disconnect && self.active_turn_running);
            self.request_redraw();
        }
    }

    pub fn can_disconnect_active_turn(&self) -> bool {
        self.can_disconnect_active_turn
    }

    pub fn set_active_turn_running(&mut self, running: bool) {
        self.active_turn_running = running;
        if let Some(status) = self.status.as_mut() {
            status.set_detach_hint_visible(running && self.can_disconnect_active_turn);
            self.request_redraw();
        }
    }

    pub fn set_active_task_list(&mut self, tasks: Vec<PlanItemArg>) {
        self.active_task_list.set_tasks(tasks);
        self.request_redraw();
    }

    pub fn set_active_agents(&mut self, agents: Vec<ActiveAgentEntry>) {
        self.active_agent_list.set_agents(agents);
        self.request_redraw();
    }

    pub fn set_context_window(&mut self, percent: Option<i64>, used_tokens: Option<i64>) {
        if self.context_window_percent == percent && self.context_window_used_tokens == used_tokens
        {
            return;
        }

        self.context_window_percent = percent;
        self.context_window_used_tokens = used_tokens;
        self.composer
            .set_context_window(percent, self.context_window_used_tokens);
        self.request_redraw();
    }

    /// Show a generic list selection view with the provided items.
    pub fn show_selection_view(&mut self, mut params: list_selection_view::SelectionViewParams) {
        self.apply_standard_popup_hint(&mut params);
        let view = list_selection_view::ListSelectionView::new(
            params,
            self.app_event_tx.clone(),
            self.keymap.list.clone(),
        );
        self.push_view(Box::new(view));
    }

    fn apply_standard_popup_hint(&self, params: &mut list_selection_view::SelectionViewParams) {
        if !params.allow_cancel {
            if params.footer_hint.is_none()
                || params.footer_hint.as_ref() == Some(&popup_consts::standard_popup_hint_line())
            {
                params.footer_hint = None;
            }
            return;
        }
        if params.footer_hint.is_none()
            || params.footer_hint.as_ref() == Some(&popup_consts::standard_popup_hint_line())
        {
            params.footer_hint = Some(self.standard_popup_hint_line());
        }
    }

    /// Replace the active selection view when it matches `view_id`.
    pub fn replace_selection_view_if_active(
        &mut self,
        view_id: &'static str,
        mut params: list_selection_view::SelectionViewParams,
    ) -> bool {
        let is_match = self
            .view_stack
            .last()
            .is_some_and(|view| view.view_id() == Some(view_id));
        if !is_match {
            return false;
        }

        self.view_stack.pop();
        self.apply_standard_popup_hint(&mut params);
        let view = list_selection_view::ListSelectionView::new(
            params,
            self.app_event_tx.clone(),
            self.keymap.list.clone(),
        );
        self.push_view(Box::new(view));
        true
    }

    /// Replace the newest matching selection view without disturbing views stacked above it.
    pub fn replace_selection_view_if_present(
        &mut self,
        view_id: &'static str,
        mut params: list_selection_view::SelectionViewParams,
    ) -> bool {
        let Some(index) = self
            .view_stack
            .iter()
            .rposition(|view| view.view_id() == Some(view_id))
        else {
            return false;
        };

        let replaces_active_view = index + 1 == self.view_stack.len();
        self.apply_standard_popup_hint(&mut params);
        self.view_stack[index] = Box::new(list_selection_view::ListSelectionView::new(
            params,
            self.app_event_tx.clone(),
            self.keymap.list.clone(),
        ));
        if replaces_active_view {
            self.schedule_active_view_frame();
        }
        self.request_redraw();
        true
    }

    pub fn standard_popup_hint_line(&self) -> Line<'static> {
        popup_consts::standard_popup_hint_line_for_keymap(&self.keymap.list)
    }

    pub fn list_keymap(&self) -> crate::keymap::ListKeymap {
        self.keymap.list.clone()
    }

    /// Replace one or more active views whose IDs are in `view_ids` with a
    /// generic list selection view.
    pub fn replace_active_views_with_selection_view(
        &mut self,
        view_ids: &[&'static str],
        mut params: list_selection_view::SelectionViewParams,
    ) -> bool {
        let is_match = self
            .view_stack
            .last()
            .and_then(|view| view.view_id())
            .is_some_and(|view_id| view_ids.contains(&view_id));
        if !is_match {
            return false;
        }

        while self
            .view_stack
            .last()
            .and_then(|view| view.view_id())
            .is_some_and(|view_id| view_ids.contains(&view_id))
        {
            self.view_stack.pop();
        }
        self.apply_standard_popup_hint(&mut params);
        let view = list_selection_view::ListSelectionView::new(
            params,
            self.app_event_tx.clone(),
            self.keymap.list.clone(),
        );
        self.push_view(Box::new(view));
        true
    }

    pub fn selected_index_for_active_view(&self, view_id: &'static str) -> Option<usize> {
        self.view_stack
            .last()
            .filter(|view| view.view_id() == Some(view_id))
            .and_then(|view| view.selected_index())
    }

    pub fn active_tab_id_for_active_view(&self, view_id: &'static str) -> Option<&str> {
        self.view_stack
            .last()
            .filter(|view| view.view_id() == Some(view_id))
            .and_then(|view| view.active_tab_id())
    }

    pub fn dismiss_active_view_if_id(&mut self, view_id: &'static str) -> bool {
        let is_match = self
            .view_stack
            .last()
            .is_some_and(|view| view.view_id() == Some(view_id));
        if !is_match {
            return false;
        }

        self.view_stack.pop();
        self.request_redraw();
        true
    }

    /// Dismiss the newest matching view without disturbing views stacked above it.
    pub fn dismiss_view_by_id(&mut self, view_id: &'static str) -> bool {
        let Some(index) = self
            .view_stack
            .iter()
            .rposition(|view| view.view_id() == Some(view_id))
        else {
            return false;
        };

        let removed_active_view = index + 1 == self.view_stack.len();
        self.view_stack.remove(index);
        if removed_active_view {
            self.schedule_active_view_frame();
        }
        self.request_redraw();
        true
    }

    /// Update the pending-input preview shown above the composer.
    pub fn set_pending_input_preview(
        &mut self,
        queued: Vec<String>,
        pending_steers: Vec<String>,
        rejected_steers: Vec<String>,
    ) {
        self.pending_input_preview.pending_steers = pending_steers;
        self.pending_input_preview.rejected_steers = rejected_steers;
        self.pending_input_preview.queued_messages = queued;
        self.request_redraw();
    }

    /// Update the inactive-thread approval list shown above the composer.
    pub fn set_pending_thread_approvals(&mut self, threads: Vec<String>) {
        if self.pending_thread_approvals.set_threads(threads) {
            self.request_redraw();
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pending_thread_approvals(&self) -> &[String] {
        self.pending_thread_approvals.threads()
    }

    /// Update the unified-exec process set and refresh whichever summary surface is active.
    ///
    /// The summary may be displayed inline in the status row or as a dedicated
    /// footer row depending on whether a status indicator is currently visible.
    pub fn set_unified_exec_processes(&mut self, processes: Vec<String>) {
        if self.unified_exec_footer.set_processes(processes) {
            self.sync_status_inline_message();
            self.request_redraw();
        }
    }

    /// Copy unified-exec summary text into the active status row, if any.
    ///
    /// This keeps status-line inline text synchronized without forcing the
    /// standalone unified-exec footer row to be visible.
    fn sync_status_inline_message(&mut self) {
        if let Some(status) = self.status.as_mut() {
            status.update_inline_message(self.unified_exec_footer.summary_text());
        }
    }

    pub fn composer_is_empty(&self) -> bool {
        self.composer.is_empty()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn composer_is_vim_enabled(&self) -> bool {
        self.composer.is_vim_enabled()
    }

    pub fn composer_should_handle_vim_insert_escape(&self, key_event: KeyEvent) -> bool {
        self.composer.should_handle_vim_insert_escape(key_event)
    }

    pub fn composer_vim_mode_label(&self) -> Option<&'static str> {
        self.composer.vim_mode_label()
    }

    pub fn is_task_running(&self) -> bool {
        self.is_task_running
    }

    pub fn should_interrupt_running_task(&self, key_event: KeyEvent) -> bool {
        let is_agent_command = self
            .composer_text()
            .lines()
            .next()
            .and_then(parse_slash_name)
            .is_some_and(|(name, _, _)| name == "agent");

        self.keymap.chat.interrupt_turn.is_pressed(key_event)
            && (self.is_task_running || self.is_model_router_running)
            && !(is_agent_command && key_event.code == KeyCode::Esc)
            && self.no_modal_or_popup_active()
            && !self.composer_should_handle_vim_insert_escape(key_event)
            && self.status.is_some()
    }

    pub fn terminal_title_requires_action(&self) -> bool {
        self.active_view()
            .is_some_and(bottom_pane_view::BottomPaneView::terminal_title_requires_action)
    }

    pub fn has_active_view(&self) -> bool {
        !self.view_stack.is_empty()
    }

    pub fn active_view_will_interrupt_turn_on_key_event(&self, key_event: KeyEvent) -> bool {
        self.is_task_running
            && self
                .active_view()
                .is_some_and(|view| view.will_interrupt_turn_on_key_event(key_event))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn active_view_id(&self) -> Option<&'static str> {
        self.view_stack.last().and_then(|view| view.view_id())
    }

    /// Return true when the pane is in the regular composer state without any
    /// overlays or popups and not running a task. This is the safe context to
    /// use Esc-Esc for backtracking from the main view.
    pub fn is_normal_backtrack_mode(&self) -> bool {
        !self.is_task_running && self.view_stack.is_empty() && !self.composer.popup_active()
    }

    /// Return true when no popups or modal views are active, regardless of task state.
    pub fn can_launch_external_editor(&self) -> bool {
        self.view_stack.is_empty() && !self.composer.popup_active()
    }

    /// Returns true when the bottom pane has no active modal view and no active composer popup.
    ///
    /// This is the UI-level definition of "no modal/popup is active" for key routing decisions.
    /// It intentionally does not include task state, since some actions are safe while a task is
    /// running and some are not.
    pub fn no_modal_or_popup_active(&self) -> bool {
        self.can_launch_external_editor()
    }

    pub fn show_view(&mut self, view: Box<dyn BottomPaneView>) {
        self.push_view(view);
    }

    /// Called when the agent requests user approval.
    pub fn push_approval_request(&mut self, request: ApprovalRequest, features: &Features) {
        let request = if let Some(view) = self.view_stack.last_mut() {
            match view.try_consume_approval_request(request) {
                Some(request) => request,
                None => {
                    self.request_redraw();
                    return;
                }
            }
        } else {
            request
        };

        let now = Instant::now();
        if !self.delayed_approval_requests.is_empty()
            || !self.view_stack.is_empty()
            || self.approval_prompt_delay_remaining(now).is_some()
        {
            self.delayed_approval_requests
                .push_back(DelayedApprovalRequest {
                    request,
                    features: features.clone(),
                });
            self.maybe_show_delayed_approval_requests_at(now);
        } else {
            // No recent composer activity, so show the approval modal immediately.
            let modal = ApprovalOverlay::new(
                request,
                self.app_event_tx.clone(),
                features.clone(),
                self.keymap.approval.clone(),
                self.keymap.list.clone(),
            );
            self.pause_status_timer_for_modal();
            self.push_view(Box::new(modal));
        }
    }

    /// Called when the agent requests user input.
    pub fn push_user_input_request(&mut self, request: ToolRequestUserInputParams) {
        let request = if let Some(view) = self.view_stack.last_mut() {
            match view.try_consume_user_input_request(request) {
                Some(request) => request,
                None => {
                    self.request_redraw();
                    return;
                }
            }
        } else {
            request
        };

        let modal = RequestUserInputOverlay::new_with_keymap(
            request,
            self.app_event_tx.clone(),
            self.has_input_focus,
            self.enhanced_keys_supported,
            self.disable_paste_burst,
            self.keymap.clone(),
        );
        self.pause_status_timer_for_modal();
        self.set_composer_input_enabled(
            /*enabled*/ false,
            Some("Answer the questions to continue.".to_string()),
        );
        self.push_view(Box::new(modal));
    }

    pub fn push_extension_interaction_request(
        &mut self,
        request: xedoc_app_server_protocol::ExtensionInteractionRequestParams,
    ) {
        self.push_scripted_interaction(request, /*model_router_settings*/ false);
    }

    pub fn push_model_router_settings_request(
        &mut self,
        request: xedoc_app_server_protocol::ExtensionInteractionRequestParams,
    ) {
        self.push_scripted_interaction(request, /*model_router_settings*/ true);
    }

    fn push_scripted_interaction(
        &mut self,
        request: xedoc_app_server_protocol::ExtensionInteractionRequestParams,
        model_router_settings: bool,
    ) {
        if !self.view_stack.is_empty() || !self.queued_scripted_interactions.is_empty() {
            self.queued_scripted_interactions
                .push_back(QueuedScriptedInteraction {
                    request,
                    model_router_settings,
                });
            return;
        }
        self.show_scripted_interaction(request, model_router_settings);
    }

    fn maybe_show_queued_scripted_interaction(&mut self) {
        if !self.view_stack.is_empty() {
            return;
        }
        let Some(QueuedScriptedInteraction {
            request,
            model_router_settings,
        }) = self.queued_scripted_interactions.pop_front()
        else {
            return;
        };
        self.show_scripted_interaction(request, model_router_settings);
    }

    fn show_scripted_interaction(
        &mut self,
        request: xedoc_app_server_protocol::ExtensionInteractionRequestParams,
        model_router_settings: bool,
    ) {
        self.pause_status_timer_for_modal();
        self.set_composer_input_enabled(
            /*enabled*/ false,
            Some(
                if model_router_settings {
                    "Respond to the model-router settings request to continue."
                } else {
                    "Respond to the extension request to continue."
                }
                .to_string(),
            ),
        );
        let view = if model_router_settings {
            ScriptedInteractionView::new_model_router_settings(request, self.app_event_tx.clone())
        } else {
            ScriptedInteractionView::new(request, self.app_event_tx.clone())
        };
        self.push_view(Box::new(view));
    }

    pub fn push_mcp_server_elicitation_request(
        &mut self,
        request: McpServerElicitationFormRequest,
    ) {
        let request = if let Some(view) = self.view_stack.last_mut() {
            match view.try_consume_mcp_server_elicitation_request(request) {
                Some(request) => request,
                None => {
                    self.request_redraw();
                    return;
                }
            }
        } else {
            request
        };

        let modal = McpServerElicitationOverlay::new_with_keymap(
            request,
            self.app_event_tx.clone(),
            self.has_input_focus,
            self.enhanced_keys_supported,
            self.disable_paste_burst,
            self.keymap.list.clone(),
        );
        self.pause_status_timer_for_modal();
        self.set_composer_input_enabled(
            /*enabled*/ false,
            Some("Respond to the MCP server request to continue.".to_string()),
        );
        self.push_view(Box::new(modal));
    }

    pub fn dismiss_app_server_request(&mut self, request: &ResolvedAppServerRequest) -> bool {
        let delayed_len = self.delayed_approval_requests.len();
        self.delayed_approval_requests.retain(|delayed| {
            !approval_overlay::matches_resolved_request(&delayed.request, request)
        });
        let delayed_changed = self.delayed_approval_requests.len() != delayed_len;
        let queued_len = self.queued_scripted_interactions.len();
        self.queued_scripted_interactions.retain(|queued| {
            !matches!(
                request,
                ResolvedAppServerRequest::ExtensionInteraction { request_id }
                    if request_id == &queued.request.request_id
            )
        });
        let queued_changed = self.queued_scripted_interactions.len() != queued_len;

        if self.view_stack.is_empty() {
            if delayed_changed || queued_changed {
                self.request_redraw();
            }
            return delayed_changed || queued_changed;
        }

        let mut changed = delayed_changed || queued_changed;
        let mut completed_indices = Vec::new();
        for index in (0..self.view_stack.len()).rev() {
            let view = &mut self.view_stack[index];
            if !view.dismiss_app_server_request(request) {
                continue;
            }
            changed = true;
            if view.is_complete() {
                completed_indices.push(index);
            }
        }
        if !changed {
            return false;
        }
        for index in completed_indices {
            self.view_stack.remove(index);
        }
        self.on_view_stack_depth_decreased();
        self.request_redraw();
        true
    }

    fn on_active_view_complete(&mut self) {
        self.maybe_show_queued_scripted_interaction();
        if !self.view_stack.is_empty() {
            return;
        }
        self.maybe_show_delayed_approval_requests_at(Instant::now());
        if !self.view_stack.is_empty() {
            return;
        }
        self.resume_status_timer_after_modal();
        self.set_composer_input_enabled(/*enabled*/ true, /*placeholder*/ None);
    }

    fn pause_status_timer_for_modal(&mut self) {
        if let Some(status) = self.status.as_mut() {
            status.pause_timer();
        }
    }

    fn resume_status_timer_after_modal(&mut self) {
        if let Some(status) = self.status.as_mut() {
            status.resume_timer();
        }
    }

    /// Height (terminal rows) required by the current bottom pane.
    pub fn request_redraw(&self) {
        self.frame_requester.schedule_frame();
    }

    pub fn request_redraw_in(&self, dur: Duration) {
        self.frame_requester.schedule_frame_in(dur);
    }

    // --- History helpers ---

    pub fn set_history_metadata(&mut self, thread_id: ThreadId, log_id: u64, entry_count: usize) {
        self.thread_id = Some(thread_id);
        self.composer
            .set_history_metadata(thread_id, log_id, entry_count);
    }

    pub fn flush_paste_burst_if_due(&mut self) -> bool {
        // Give the active view the first chance to flush paste-burst state so
        // overlays that reuse the composer behave consistently.
        if let Some(view) = self.view_stack.last_mut()
            && view.flush_paste_burst_if_due()
        {
            return true;
        }
        self.composer.flush_paste_burst_if_due()
    }

    pub fn is_in_paste_burst(&self) -> bool {
        // A view can hold paste-burst state independently of the primary
        // composer, so check it first.
        self.view_stack
            .last()
            .is_some_and(|view| view.is_in_paste_burst())
            || self.composer.is_in_paste_burst()
    }

    pub fn on_history_lookup_response(&mut self, response: HistoryLookupResponse) {
        let updated = match response {
            HistoryLookupResponse::Entry {
                offset,
                log_id,
                entry,
            } => self
                .composer
                .on_history_entry_response(log_id, offset, entry),
            HistoryLookupResponse::Batch {
                cursor,
                log_id,
                entries,
                next_older_cursor,
            } => {
                self.composer
                    .on_history_batch_response(log_id, cursor, entries, next_older_cursor)
            }
            HistoryLookupResponse::BatchError { cursor, log_id } => {
                self.composer.on_history_batch_error(log_id, cursor)
            }
        };
        if updated {
            self.composer.sync_popups();
            self.request_redraw();
        }
    }

    pub fn record_replayed_user_message_history(&mut self, entry: HistoryEntry) {
        self.composer.record_replayed_user_message_history(entry);
    }

    pub fn on_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        self.composer.on_file_search_result(query, matches);
        self.request_redraw();
    }

    pub fn attach_image(&mut self, path: PathBuf) {
        if self.view_stack.is_empty() {
            self.composer.attach_image(path);
            self.request_redraw();
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn take_recent_submission_images(&mut self) -> Vec<PathBuf> {
        self.composer.take_recent_submission_images()
    }

    pub fn take_recent_submission_images_with_placeholders(&mut self) -> Vec<LocalImageAttachment> {
        self.composer
            .take_recent_submission_images_with_placeholders()
    }

    pub fn prepare_inline_args_submission(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        self.composer.prepare_inline_args_submission(record_history)
    }

    pub fn as_renderable(&'_ self) -> RenderableItem<'_> {
        self.as_renderable_with_composer_right_reserve(/*composer_right_reserve*/ 0)
    }

    pub fn as_renderable_with_composer_right_reserve(
        &'_ self,
        composer_right_reserve: u16,
    ) -> RenderableItem<'_> {
        if let Some(view) = self.active_view() {
            RenderableItem::Borrowed(view)
        } else {
            let mut flex = FlexRenderable::new();
            if let Some(status) = &self.status {
                flex.push(/*flex*/ 0, RenderableItem::Borrowed(status));
            }
            // Avoid double-surfacing the same summary and avoid adding an extra
            // row while the status line is already visible.
            if self.status.is_none() && !self.unified_exec_footer.is_empty() {
                flex.push(
                    /*flex*/ 0,
                    RenderableItem::Borrowed(&self.unified_exec_footer),
                );
            }
            let has_pending_thread_approvals = !self.pending_thread_approvals.is_empty();
            let has_pending_input = !self.pending_input_preview.queued_messages.is_empty()
                || !self.pending_input_preview.pending_steers.is_empty()
                || !self.pending_input_preview.rejected_steers.is_empty();
            let has_status_or_footer =
                self.status.is_some() || !self.unified_exec_footer.is_empty();
            let has_inline_previews = has_pending_thread_approvals || has_pending_input;
            if has_inline_previews && has_status_or_footer {
                flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
            }
            flex.push(
                /*flex*/ 1,
                RenderableItem::Borrowed(&self.pending_thread_approvals),
            );
            if has_pending_thread_approvals && has_pending_input {
                flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
            }
            flex.push(
                /*flex*/ 1,
                RenderableItem::Borrowed(&self.pending_input_preview),
            );
            let has_active_agent_list = !self.active_agent_list.is_empty();
            let has_active_task_list = !self.active_task_list.is_empty();
            if has_active_agent_list && (has_inline_previews || has_status_or_footer) {
                flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
            }
            if has_active_agent_list {
                flex.push(
                    /*flex*/ 0,
                    RenderableItem::Borrowed(&self.active_agent_list),
                );
            }
            if has_active_agent_list && has_active_task_list {
                flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
            }
            if has_active_task_list
                && !has_active_agent_list
                && (has_inline_previews || has_status_or_footer)
            {
                flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
            }
            if has_active_task_list {
                flex.push(
                    /*flex*/ 0,
                    RenderableItem::Borrowed(&self.active_task_list),
                );
            }
            if !has_active_agent_list
                && !has_active_task_list
                && !has_inline_previews
                && has_status_or_footer
            {
                flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
            }
            let mut flex2 = FlexRenderable::new();
            flex2.push(/*flex*/ 1, RenderableItem::Owned(flex.into()));
            let composer = RenderableItem::Owned(Box::new(ChatComposerRightReserveRenderable {
                composer: &self.composer,
                right_reserve: composer_right_reserve,
                active_turn_elapsed_seconds: self
                    .active_turn_started_at
                    .map(|started_at| started_at.elapsed().as_secs()),
                runtime_context: self.runtime_context.as_ref(),
                model_router_mode: self.model_router_mode.as_deref(),
            }));
            flex2.push(/*flex*/ 0, composer);
            RenderableItem::Owned(Box::new(flex2))
        }
    }

    pub fn set_status_line(&mut self, status_line: Option<Line<'static>>) {
        if self.composer.set_status_line(status_line) {
            self.request_redraw();
        }
    }

    pub fn set_status_line_hyperlink(&mut self, url: Option<String>) {
        if self.composer.set_status_line_hyperlink(url) {
            self.request_redraw();
        }
    }

    pub fn set_status_line_enabled(&mut self, enabled: bool) {
        if self.composer.set_status_line_enabled(enabled) {
            self.request_redraw();
        }
    }

    /// Updates the contextual footer label and requests a redraw only when it changed.
    ///
    /// This keeps the footer plumbing cheap during thread transitions where `App` may recompute
    /// the label several times while the visible thread settles.
    pub fn set_active_agent_label(&mut self, active_agent_label: Option<String>) {
        if self.composer.set_active_agent_label(active_agent_label) {
            self.request_redraw();
        }
    }

    pub fn set_side_conversation_context_label(&mut self, label: Option<String>) {
        if self.composer.set_side_conversation_context_label(label) {
            self.request_redraw();
        }
    }
}

struct ChatComposerRightReserveRenderable<'a> {
    composer: &'a chat_composer::ChatComposer,
    right_reserve: u16,
    active_turn_elapsed_seconds: Option<u64>,
    runtime_context: Option<&'a ComposerRuntimeContext>,
    model_router_mode: Option<&'a str>,
}

impl Renderable for ChatComposerRightReserveRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.composer
            .render_with_mask_and_textarea_right_reserve_and_runtime_context(
                area,
                buf,
                /*mask_char*/ None,
                self.right_reserve,
                self.active_turn_elapsed_seconds,
                self.runtime_context.map(|context| {
                    (
                        context.model.as_str(),
                        context.effort.as_deref(),
                        context.fast,
                        self.model_router_mode,
                    )
                }),
            );
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.composer
            .desired_height_with_textarea_right_reserve(width, self.right_reserve)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.composer
            .cursor_pos_with_textarea_right_reserve(area, self.right_reserve)
    }

    fn cursor_style(&self, area: Rect) -> crossterm::cursor::SetCursorStyle {
        self.composer.cursor_style(area)
    }
}

impl Renderable for BottomPane {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.as_renderable().render(area, buf);
    }
    fn desired_height(&self, width: u16) -> u16 {
        self.as_renderable().desired_height(width)
    }
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.as_renderable().cursor_pos(area)
    }

    fn cursor_style(&self, area: Rect) -> crossterm::cursor::SetCursorStyle {
        self.as_renderable().cursor_style(area)
    }
}
