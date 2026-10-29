//! The chat composer is the bottom-pane text input state machine.
//!
//! It is responsible for:
//!
//! - Editing the input buffer (a [`TextArea`]), including placeholder "elements" for attachments.
//! - Routing keys to the active popup (slash commands, file search, skill/apps mentions).
//! - Promoting typed slash commands into atomic elements when the command name is completed.
//! - Handling submit vs newline on Enter.
//! - Toggling the session model-router mode with Shift+Left and opening /model with
//!   Shift+Right when no popup is active.
//! - Turning raw key streams into explicit paste operations on platforms where terminals
//!   don't provide reliable bracketed paste (notably Windows).
//!
//! # Key Event Routing
//!
//! Most key handling goes through [`ChatComposer::handle_key_event`], which dispatches to a
//! popup-specific handler if a popup is visible and otherwise to
//! [`ChatComposer::handle_key_event_without_popup`]. After every handled key, we call
//! [`ChatComposer::sync_popups`] so UI state follows the latest buffer/cursor.
//!
//! # Completion and Popup Dismissal
//!
//! Popup targeting resolves an editable token range around the cursor and treats atomic text
//! elements as hard boundaries. When that range begins immediately after an atomic element, the
//! insertion path first adds a horizontal separator and shifts the range so the completion cannot
//! merge with the atomic element. After replacing the range, completion leaves the cursor after one
//! horizontal separator. It advances across an existing separator when no suffix would be joined;
//! otherwise it inserts a space, preserving the existing separator before a non-whitespace suffix.
//! It also inserts a space rather than crossing a line break.
//!
//! `Esc` records the active token as dismissed. A completed value that begins with `@` or `$` is
//! also re-dismissed before popup synchronization, because separator affinity can still identify
//! the completed token to the left of the cursor. Synchronization keeps the popup hidden only while
//! the query, complete token text, and ordinal among matching occurrences remain the same.
//! Whitespace and atomic-element edges delimit occurrences, while nested sigils do not. This
//! preserves dismissal across offset-only edits without suppressing a later identical token.
//!
//! Slash-command dismissal is tracked separately. Pressing `Esc` while the command popup is active
//! records the first-line `/name` token and keeps the popup closed while that token is unchanged.
//! Editing the command token clears the dismissal and allows the popup to reopen.
//!
//! # History Navigation (↑/↓)
//!
//! The Up/Down history path is managed by [`ChatComposerHistory`]. It merges:
//!
//! - Persistent cross-session history (text-only; no element ranges or attachments).
//! - Local in-session history (full text + text elements + local/remote image attachments).
//!
//! When recalling a local entry, the composer rehydrates text elements and both attachment kinds
//! (local image paths + remote image URLs).
//! When recalling a persistent entry, only the text is restored.
//! Recalled entries move the cursor to end-of-line so repeated Up/Down presses keep shell-like
//! history traversal semantics instead of dropping to column 0.
//! `Ctrl+R` opens a reverse incremental search mode. The footer becomes the search input; once the
//! query is non-empty, the composer body previews the current match. `Enter` accepts the preview as
//! an editable draft and `Esc` restores the draft that was active when search started.
//!
//! Slash commands are staged for local history instead of being recorded immediately. Command
//! recall is a two-phase handoff: stage the submitted slash text here, then record it after
//! `ChatWidget` dispatches the command.
//!
//! # Submission and Prompt Expansion
//!
//! `Enter` submits immediately. `Tab` requests queuing while a task is running; if no task is
//! running, `Tab` submits just like Enter so input is never dropped.
//! `Tab` does not submit when entering a `!` shell command.
//!
//! On submit/queue paths, the composer:
//!
//! - Expands pending paste placeholders so element ranges align with the final text.
//! - Trims whitespace and rebases text elements accordingly.
//! - Prunes local attached images so only placeholders that survive expansion are sent.
//! - Preserves remote image URLs as separate attachments even when text is empty.
//!
//! When these paths clear the visible textarea after a successful submit or slash-command
//! dispatch, they intentionally preserve the textarea kill buffer. That lets users `Ctrl+K` part
//! of a draft, perform a composer action such as changing reasoning level, and then `Ctrl+Y` the
//! killed text back into the now-empty draft.
//!
//! The numeric auto-submit path used by the slash popup performs the same pending-paste expansion
//! and attachment pruning, and clears pending paste state on success.
//! Slash commands with arguments (like `/plan` and `/review`) reuse the same preparation path so
//! pasted content and text elements are preserved when extracting args.
//!
//! # Parent-Owned Thread Mode
//!
//! Parent-owned subagent threads keep the draft editable while blocking agent-directed submission.
//! On the `Enter` and `Tab` submission paths, normal prompts, disallowed slash commands, and `!`
//! shell commands return `ParentOwnedInputBlocked` without clearing the draft. Bare local and
//! navigation slash commands remain available so users can leave or manage the view.
//!
//! # Reasoning Effort Animations
//!
//! The composer observes the effective reasoning tier whenever model-dependent surfaces refresh.
//! The first observation, session configuration, and restored threads establish a baseline;
//! genuine changes to Max/Ultra can then queue composer and status-line transitions when motion
//! and color support allow them.
//! Repeated selections do not restart either effect, dropping below Max clears them, and
//! restoration clears any transition queued while replaying the saved session. Rendering advances
//! active transitions through the frame requester until they finish.
//!
//! # Large Paste Placeholders
//!
//! Large pastes insert an element placeholder in the buffer and store the full text in
//! `pending_pastes`. The placeholder label is derived from the pasted character count:
//!
//! - First paste of a given size uses `[Pasted Content N chars]`.
//! - Additional pending pastes of the same size add a numeric suffix (`#2`, `#3`, ...), where the
//!   next suffix is computed from the placeholders that still exist in `pending_pastes`.
//! - When all placeholders for a size are cleared or deleted, the next paste of that size reuses
//!   the base label without a suffix.
//!
//! # Remote Image Rows (Up/Down/Delete)
//!
//! Remote image URLs are rendered as non-editable `[Image #N]` rows above the textarea (inside the
//! same composer block). These rows represent image attachments rehydrated from app-server/backtrack
//! history; TUI users can remove them, but cannot type into that row region.
//!
//! Keyboard behavior:
//!
//! - `Up` at textarea cursor `0` enters remote-row selection at the last remote image.
//! - `Up`/`Down` move selection between remote rows.
//! - `Down` on the last row clears selection and returns control to the textarea.
//! - `Delete`/`Backspace` remove the selected remote image row.
//!
//! Placeholder numbering is unified across remote and local images:
//!
//! - Remote rows occupy `[Image #1]..[Image #M]`.
//! - Local placeholders are offset after that range (`[Image #M+1]..`).
//! - Deleting a remote row relabels local placeholders to keep numbering contiguous.
//!
//! # Non-bracketed Paste Bursts
//!
//! On some terminals (especially on Windows), pastes arrive as a rapid sequence of
//! `KeyCode::Char` and `KeyCode::Enter` key events instead of a single paste event.
//!
//! To avoid misinterpreting these bursts as real typing (and to prevent transient UI effects like
//! shortcut overlays toggling on a pasted `?`), we feed "plain" character events into
//! [`PasteBurst`](super::paste_burst::PasteBurst), which buffers bursts and later flushes them
//! through [`ChatComposer::handle_paste`].
//!
//! The burst detector intentionally treats ASCII and non-ASCII differently:
//!
//! - ASCII: we briefly hold the first fast char (flicker suppression) until we know whether the
//!   stream is paste-like.
//! - non-ASCII: we do not hold the first char (IME input would feel dropped), but we still allow
//!   burst detection for actual paste streams.
//!
//! The burst detector can also be disabled (`disable_paste_burst`), which bypasses the state
//! machine and treats the key stream as normal typing. When toggling from enabled → disabled, the
//! composer flushes/clears any in-flight burst state so it cannot leak into subsequent input.
//!
//! For the detailed burst state machine, see `xedoc-rs/tui/src/bottom_pane/paste_burst.rs`.
//!
//! # PasteBurst Integration Points
//!
//! The burst detector is consulted in a few specific places:
//!
//! - [`ChatComposer::handle_input_basic`]: flushes any due burst first, then intercepts plain char
//!   input to either buffer it or insert normally.
//! - [`ChatComposer::handle_non_ascii_char`]: handles the non-ASCII/IME path without holding the
//!   first char, while still allowing paste detection via retro-capture.
//! - [`ChatComposer::flush_paste_burst_if_due`]/[`ChatComposer::handle_paste_burst_flush`]: called
//!   from UI ticks to turn a pending burst into either an explicit paste (`handle_paste`) or a
//!   normal typed character.
//!
//! # Input Disabled Mode
//!
//! The composer can be temporarily read-only (`input_enabled = false`). In that mode it ignores
//! edits and renders a placeholder prompt instead of the editable textarea. This is part of the
//! overall state machine, since it affects which transitions are even possible from a given UI
//! state.
//!
#![allow(clippy::disallowed_methods)]

use crate::color::blend;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::has_ctrl_or_alt;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::terminal_palette::StdoutColorLevel;
use crate::terminal_palette::best_color_for_level;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::default_fg;
use crate::terminal_palette::effective_stdout_color_level;
use crate::terminal_palette::rgb_color;
use crate::ui_consts::FOOTER_INDENT_COLS;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Alignment;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Margin;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::block::Title;
use xedoc_message_history::HistoryBatchCursor;

use xedoc_protocol::openai_models::ReasoningEffort;

use super::chat_composer_history::ChatComposerHistory;
use super::chat_composer_history::HistoryEntry;
use super::chat_composer_history::HistoryEntryResponse;
use super::chat_composer_history::HistorySearchResult;
use super::command_popup::CommandItem;
use super::effort_ignition::EffortIgnition;
use super::effort_ignition::EffortTier;
use super::effort_ignition::IGNITION_FRAME_TICK;
use super::effort_ignition::IgnitionStyle;
use super::effort_status_line::EFFORT_STATUS_LINE_FRAME_TICK;
use super::effort_status_line::EffortStatusLineTransition;
use super::file_search_popup::FileSearchPopup;
use super::footer::CollaborationModeIndicator;
use super::footer::FooterKeyHints;
use super::footer::FooterMode;
use super::footer::FooterProps;
use super::footer::GoalStatusIndicator;
use super::footer::SummaryLeft;
use super::footer::can_show_left_with_context;
use super::footer::configured_status_line;
use super::footer::context_window_line;
use super::footer::esc_hint_mode;
use super::footer::footer_height;
use super::footer::footer_hint_items_width;
use super::footer::footer_line_width;
use super::footer::inset_footer_hint_area;
use super::footer::max_left_width_for_right;
use super::footer::passive_footer_status_line;
use super::footer::render_context_right;
use super::footer::render_footer_from_props;
use super::footer::render_footer_hint_items;
use super::footer::render_footer_line;
use super::footer::reset_mode_after_activity;
use super::footer::side_conversation_context_line;
use super::footer::single_line_footer_layout;
use super::footer::status_line_right_indicator_line;
use super::footer::toggle_shortcut_mode;
use super::footer::uses_passive_footer_status_layout;
use super::footer::with_active_agent_context_line;
use super::mentions_v2::MentionV2Popup;
use super::mentions_v2::MentionV2Selection;
use super::paste_burst::CharDecision;
use super::paste_burst::PasteBurst;
use super::prompt_args::parse_slash_name;
use super::skill_popup::MentionItem;
use super::skill_popup::SkillPopup;
use super::slash_commands::BuiltinCommandFlags;
use super::slash_commands::ServiceTierCommand;
use super::slash_commands::SessionExtensionCommand;
use super::slash_commands::SlashCommandItem;
use crate::bottom_pane::paste_burst::FlushResult;
use crate::city_lights::CityLightsStylize;
use crate::city_lights::{self};
use crate::history_cell::sanitize_user_text;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::EditorKeymap;
use crate::keymap::RuntimeKeymap;
use crate::keymap::VimNormalKeymap;
use crate::keymap::primary_binding;
use crate::onboarding::mark_underlined_hyperlink;
use crate::render::Insets;
use crate::render::RectExt;
use crate::render::renderable::Renderable;
use crate::slash_command::SlashCommand;
use xedoc_protocol::ThreadId;
use xedoc_protocol::user_input::ByteRange;
use xedoc_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use xedoc_protocol::user_input::TextElement;

mod attachment_state;
mod completion_target;
mod draft_state;
mod footer_state;
mod history_search;
mod popup_state;
mod slash_input;

use self::attachment_state::AttachmentState;
use self::draft_state::ComposerMentionBinding;
use self::draft_state::DraftState;
use self::footer_state::FooterState;
use self::history_search::HistorySearchSession;
use self::popup_state::ActivePopup;
use self::popup_state::DismissedToken;
use self::popup_state::PopupState;
use self::slash_input::SlashInput;
use self::slash_input::SlashValidation;
use self::slash_input::SubmissionValidation;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::MentionBinding;
use crate::bottom_pane::QueuedInputAction;
use crate::bottom_pane::textarea::TextArea;
use crate::clipboard_paste::normalize_pasted_path;
use crate::clipboard_paste::pasted_image_format;
use crate::history_cell;
use crate::skills_helpers::skill_display_name;
use crate::terminal_palette::indexed_color;
use crate::tui::FrameRequester;
use crate::ui_consts::LIVE_PREFIX_COLS;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use xedoc_app_server_protocol::SkillMetadata;
use xedoc_file_search::FileMatch;
use xedoc_file_search::MatchType;
use xedoc_plugin::PluginCapabilitySummary;

use ratatui::style::Color;

/// If the pasted content exceeds this number of characters, replace it with a
/// placeholder in the UI.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;
const STATUSLINE_SESSION_NAME_COLOR_INDEX: u8 = 141;
const ACTIVE_TURN_TIMER_FOREGROUND_ALPHA: f32 = 0.75;

fn session_name_color() -> Color {
    indexed_color(STATUSLINE_SESSION_NAME_COLOR_INDEX)
}

fn runtime_context_separator_style() -> Style {
    runtime_context_separator_style_for(default_bg())
}

fn runtime_context_separator_style_for(terminal_background: Option<(u8, u8, u8)>) -> Style {
    city_lights::composer_session_title_style()
        .fg(rgb_color(terminal_background.unwrap_or(city_lights::CL_BG)))
}

fn router_mode_style(mode: &str, background: Option<Color>) -> Style {
    let foreground = match mode {
        "shadow-subagents" | "shadow-full" => Color::Rgb(255, 165, 0),
        "subagents" => Color::Blue,
        "full" => Color::Green,
        "off" => Color::DarkGray,
        _ => Color::DarkGray,
    };
    Style::default()
        .fg(foreground)
        .bg(background.unwrap_or_default())
}

fn active_turn_timer_style() -> Style {
    active_turn_timer_style_for(default_fg(), effective_stdout_color_level())
}

fn active_turn_timer_style_for(
    terminal_foreground: Option<(u8, u8, u8)>,
    color_level: StdoutColorLevel,
) -> Style {
    let style = city_lights::composer_session_title_style();
    let Some(terminal_foreground) = terminal_foreground else {
        return style.dim();
    };
    let color = blend(
        terminal_foreground,
        city_lights::CL_SESSION_TITLE_BG,
        ACTIVE_TURN_TIMER_FOREGROUND_ALPHA,
    );
    match color_level {
        StdoutColorLevel::TrueColor | StdoutColorLevel::Ansi256 => {
            style.fg(best_color_for_level(color, color_level))
        }
        StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown => style.dim(),
    }
}

fn user_input_too_large_message(actual_chars: usize) -> String {
    format!(
        "Message exceeds the maximum length of {MAX_USER_INPUT_TEXT_CHARS} characters ({actual_chars} provided)."
    )
}

/// Result returned when the user interacts with the text area.
#[derive(Debug, PartialEq)]
pub enum InputResult {
    Submitted {
        text: String,
        text_elements: Vec<TextElement>,
    },
    Queued {
        text: String,
        text_elements: Vec<TextElement>,
        action: QueuedInputAction,
        pending_pastes: Vec<(String, String)>,
    },
    /// A bare slash command parsed by the composer.
    ///
    /// Callers that dispatch this variant are also responsible for resolving any pending local
    /// command-history entry that the composer staged before clearing the visible input.
    Command(SlashCommand),
    /// A bare model service-tier command parsed by the composer.
    ServiceTierCommand(ServiceTierCommand),
    /// An approved session extension command and its trimmed argument text.
    SessionExtensionCommand(SessionExtensionCommand, String),
    /// An inline slash command and its trimmed argument text.
    ///
    /// The `TextElement` ranges are rebased into the argument string, while any pending local
    /// command-history entry still represents the original command invocation that should be
    /// committed only if dispatch accepts it.
    CommandWithArgs(SlashCommand, String, Vec<TextElement>),
    HistoryRecalled(HistoryEntry),
    /// Agent-directed input was attempted while viewing a parent-owned spawned child thread.
    ParentOwnedInputBlocked,
    None,
}

fn parent_owned_command_is_allowed(command: SlashCommand, args: &str) -> bool {
    args.is_empty()
        && matches!(
            command,
            SlashCommand::New
                | SlashCommand::Clear
                | SlashCommand::Resume
                | SlashCommand::Side
                | SlashCommand::Btw
                | SlashCommand::Agent
                | SlashCommand::MultiAgents
                | SlashCommand::Vim
                | SlashCommand::Keymap
                | SlashCommand::Experimental
                | SlashCommand::Quit
                | SlashCommand::Exit
                | SlashCommand::Logout
                | SlashCommand::Copy
                | SlashCommand::Raw
                | SlashCommand::ToolRendering
                | SlashCommand::Diff
                | SlashCommand::Mention
                | SlashCommand::Skills
                | SlashCommand::Hooks
                | SlashCommand::Status
                | SlashCommand::Ide
                | SlashCommand::DebugConfig
                | SlashCommand::Title
                | SlashCommand::Statusline
                | SlashCommand::Theme
                | SlashCommand::Ps
                | SlashCommand::Stop
                | SlashCommand::Mcp
                | SlashCommand::Plugins
                | SlashCommand::Rollout
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingPasteHandling {
    Expand,
    Preserve,
}

/// Feature flags for reusing the chat composer in other bottom-pane surfaces.
///
/// The default keeps today's behavior intact. Other call sites can opt out of
/// specific behaviors by constructing a config with those flags set to `false`.
#[derive(Clone, Copy, Debug)]
pub struct ChatComposerConfig {
    /// Whether command/file/skill popups are allowed to appear.
    pub popups_enabled: bool,
    /// Whether `/...` input is parsed and dispatched as slash commands.
    pub slash_commands_enabled: bool,
    /// Whether pasting a file path can attach local images.
    pub image_paste_enabled: bool,
}

impl Default for ChatComposerConfig {
    fn default() -> Self {
        Self {
            popups_enabled: true,
            slash_commands_enabled: true,
            image_paste_enabled: true,
        }
    }
}

impl ChatComposerConfig {
    /// A minimal preset for plain-text inputs embedded in other surfaces.
    ///
    /// This disables popups, slash commands, and image-path attachment behavior
    /// so the composer behaves like a simple notes field.
    pub const fn plain_text() -> Self {
        Self {
            popups_enabled: false,
            slash_commands_enabled: false,
            image_paste_enabled: false,
        }
    }
}

pub struct ChatComposer {
    draft: DraftState,
    popups: PopupState,
    app_event_tx: AppEventSender,
    history: ChatComposerHistory,
    footer: FooterState,
    has_focus: bool,
    frame_requester: Option<FrameRequester>,
    effort_tier: Option<EffortTier>,
    effort_animation_style: Option<IgnitionStyle>,
    effort_ignition: Option<EffortIgnition>,
    effort_status_line_transition: Option<EffortStatusLineTransition>,
    effort_observed: bool,
    attachments: AttachmentState,
    placeholder_text: String,
    session_name: Option<String>,
    blocks_direct_input: bool,
    is_task_running: bool,
    queue_submissions: bool,
    /// Slash-command draft staged for local recall after application-level dispatch.
    ///
    /// This slot is intentionally separate from `ChatComposerHistory` so inline slash commands can
    /// prepare their argument text without also double-recording the full command invocation.
    pending_slash_command_history: Option<HistoryEntry>,
    skills: Option<Vec<SkillMetadata>>,
    plugins: Option<Vec<PluginCapabilitySummary>>,
    collaboration_modes_enabled: bool,
    config: ChatComposerConfig,
    plugins_command_enabled: bool,
    service_tier_commands_enabled: bool,
    service_tier_commands: Vec<ServiceTierCommand>,
    session_extension_commands: Vec<SessionExtensionCommand>,
    mentions_v2_enabled: bool,
    goal_command_enabled: bool,
    personality_command_enabled: bool,
    side_conversation_active: bool,
    history_search: Option<HistorySearchSession>,
    submit_keys: Vec<KeyBinding>,
    queue_keys: Vec<KeyBinding>,
    toggle_shortcuts_keys: Vec<KeyBinding>,
    history_search_previous_keys: Vec<KeyBinding>,
    history_search_next_keys: Vec<KeyBinding>,
    editor_keymap: EditorKeymap,
    vim_normal_keymap: VimNormalKeymap,
}

/// A resolved legacy `$` target plus any catalog built while disambiguating shell syntax.
struct MentionCompletionTarget {
    range: Range<usize>,
    query: String,
    prebuilt_mentions: Option<Vec<MentionItem>>,
}

#[derive(Clone, Debug)]
struct ComposerDraft {
    text: String,
    text_elements: Vec<TextElement>,
    local_image_paths: Vec<PathBuf>,
    remote_image_urls: Vec<String>,
    mention_bindings: Vec<MentionBinding>,
    pending_pastes: Vec<(String, String)>,
    cursor: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComposerDraftSnapshot {
    pub text: String,
    pub text_elements: Vec<TextElement>,
    pub local_images: Vec<LocalImageAttachment>,
    pub remote_image_urls: Vec<String>,
    pub mention_bindings: Vec<MentionBinding>,
    pub pending_pastes: Vec<(String, String)>,
}

const FOOTER_SPACING_HEIGHT: u16 = 0;

/// Builds the one-line nudge that replaces the ambient footer without adding layout height.
fn plan_mode_nudge_line() -> Line<'static> {
    Line::from(vec![
        "Create a plan?".cl_magenta(),
        "  ".into(),
        key_hint::shift(KeyCode::Tab).into(),
        " use Plan mode".into(),
        "   ".into(),
        key_hint::plain(KeyCode::Esc).into(),
        " dismiss".into(),
    ])
}

impl ChatComposer {
    fn slash_input(&self) -> SlashInput<'_> {
        SlashInput::new(
            self.slash_commands_enabled(),
            self.draft.is_bash_mode,
            self.builtin_command_flags(),
            &self.service_tier_commands,
            &self.session_extension_commands,
        )
    }

    fn builtin_command_flags(&self) -> BuiltinCommandFlags {
        BuiltinCommandFlags {
            collaboration_modes_enabled: self.collaboration_modes_enabled,
            plugins_command_enabled: self.plugins_command_enabled,
            service_tier_commands_enabled: self.service_tier_commands_enabled,
            goal_command_enabled: self.goal_command_enabled,
            personality_command_enabled: self.personality_command_enabled,
            side_conversation_active: self.side_conversation_active,
        }
    }

    pub fn new(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
    ) -> Self {
        Self::new_with_config(
            has_input_focus,
            app_event_tx,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
            ChatComposerConfig::default(),
        )
    }

    /// Construct a composer with explicit feature gating.
    ///
    /// This enables reuse in contexts like request-user-input where we want
    /// the same visuals and editing behavior without slash commands or popups.
    pub fn new_with_config(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
        config: ChatComposerConfig,
    ) -> Self {
        let use_shift_enter_hint = enhanced_keys_supported;
        let default_keymap = RuntimeKeymap::defaults();
        let default_editor_keymap = default_keymap.editor.clone();
        let default_vim_normal_keymap = default_keymap.vim_normal.clone();

        let mut this = Self {
            draft: DraftState::new(),
            popups: PopupState::default(),
            app_event_tx,
            history: ChatComposerHistory::new(),
            footer: FooterState {
                quit_shortcut_expires_at: None,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                esc_backtrack_hint: false,
                use_shift_enter_hint,
                mode: FooterMode::ComposerEmpty,
                hint_override: None,
                plan_mode_nudge_visible: false,
                flash: None,
                context_window_percent: None,
                context_window_used_tokens: None,
                collaboration_mode_indicator: None,
                goal_status_indicator: None,
                ide_context_active: false,
                status_line_value: None,
                status_line_hyperlink_url: None,
                status_line_enabled: false,
                side_conversation_context_label: None,
                active_agent_label: None,
                external_editor_key: Some(key_hint::ctrl(KeyCode::Char('g'))),
                show_transcript_key: Some(key_hint::ctrl(KeyCode::Char('t'))),
                insert_newline_key: footer_insert_newline_key(
                    &default_keymap.editor.insert_newline,
                    use_shift_enter_hint,
                ),
                queue_key: Some(key_hint::plain(KeyCode::Tab)),
                toggle_shortcuts_key: Some(key_hint::plain(KeyCode::Char('?'))),
                history_search_key: primary_binding(
                    &default_keymap.composer.history_search_previous,
                ),
                reasoning_down_key: primary_binding(&default_keymap.chat.decrease_reasoning_effort),
                reasoning_up_key: primary_binding(&default_keymap.chat.increase_reasoning_effort),
            },
            has_focus: has_input_focus,
            frame_requester: None,
            effort_tier: None,
            effort_animation_style: None,
            effort_ignition: None,
            effort_status_line_transition: None,
            effort_observed: false,
            attachments: AttachmentState::default(),
            placeholder_text,
            session_name: None,
            blocks_direct_input: false,
            is_task_running: false,
            queue_submissions: false,
            pending_slash_command_history: None,
            skills: None,
            plugins: None,
            collaboration_modes_enabled: false,
            config,
            plugins_command_enabled: false,
            service_tier_commands_enabled: false,
            service_tier_commands: Vec::new(),
            session_extension_commands: Vec::new(),
            mentions_v2_enabled: false,
            goal_command_enabled: false,
            personality_command_enabled: false,
            side_conversation_active: false,
            history_search: None,
            submit_keys: vec![key_hint::plain(KeyCode::Enter)],
            queue_keys: vec![key_hint::plain(KeyCode::Tab)],
            toggle_shortcuts_keys: vec![
                key_hint::plain(KeyCode::Char('?')),
                key_hint::shift(KeyCode::Char('?')),
            ],
            history_search_previous_keys: default_keymap.composer.history_search_previous.clone(),
            history_search_next_keys: default_keymap.composer.history_search_next.clone(),
            editor_keymap: default_editor_keymap,
            vim_normal_keymap: default_vim_normal_keymap,
        };
        // Apply configuration via the setter to keep side-effects centralized.
        this.set_disable_paste_burst(disable_paste_burst);
        this
    }

    pub fn set_frame_requester(&mut self, frame_requester: FrameRequester) {
        self.frame_requester = Some(frame_requester);
    }

    /// Records the effective reasoning tier, captures the outgoing status
    /// line, and queues the one-shot effects for a genuine Max/Ultra change
    /// after the initial baseline.
    pub fn set_active_reasoning_effort(
        &mut self,
        effort: Option<&ReasoningEffort>,
        animations_enabled: bool,
    ) -> bool {
        let tier = EffortTier::from_effort(effort);
        let is_baseline = !self.effort_observed;
        self.effort_observed = true;
        if self.effort_tier == tier {
            return false;
        }
        self.effort_tier = tier;
        self.effort_ignition = None;
        self.effort_status_line_transition = None;
        if let Some(tier) = tier
            && !is_baseline
            && animations_enabled
        {
            let style = IgnitionStyle::random(self.effort_animation_style);
            self.effort_ignition = Some(EffortIgnition::new(tier, style));
            self.effort_animation_style = Some(style);
            if self.footer.status_line_enabled
                && !self.footer.plan_mode_nudge_visible
                && let Some(previous) = passive_footer_status_line(&self.footer_props())
            {
                self.effort_status_line_transition =
                    Some(EffortStatusLineTransition::new(tier, previous));
            }
            if let Some(frame_requester) = &self.frame_requester {
                frame_requester.schedule_frame();
            }
        }
        true
    }

    /// Establishes the current tier without retaining or starting a one-shot effect.
    pub fn set_active_reasoning_effort_baseline(&mut self, effort: Option<&ReasoningEffort>) {
        self.effort_tier = EffortTier::from_effort(effort);
        self.effort_observed = true;
        self.effort_ignition = None;
        self.effort_status_line_transition = None;
    }

    pub fn set_skill_mentions(&mut self, skills: Option<Vec<SkillMetadata>>) {
        self.skills = skills;
        self.sync_popups();
    }

    pub fn set_plugin_mentions(&mut self, plugins: Option<Vec<PluginCapabilitySummary>>) {
        self.plugins = plugins;
        self.sync_popups();
    }

    pub fn set_plugins_command_enabled(&mut self, enabled: bool) {
        self.plugins_command_enabled = enabled;
    }

    pub fn set_mentions_v2_enabled(&mut self, enabled: bool) {
        self.mentions_v2_enabled = enabled;
        self.history.set_at_mention_restore_enabled(enabled);
        self.sync_popups();
    }

    /// Toggle composer-side image paste handling.
    ///
    /// This only affects whether image-like paste content is converted into attachments; the
    /// `ChatWidget` layer still performs capability checks before images are submitted.
    pub fn set_image_paste_enabled(&mut self, enabled: bool) {
        self.config.image_paste_enabled = enabled;
    }

    pub fn take_mention_bindings(&mut self) -> Vec<MentionBinding> {
        let elements = self.current_mention_elements();
        let mut ordered = Vec::new();
        for (id, sigil, mention) in elements {
            if let Some(binding) = self.draft.mention_bindings.remove(&id)
                && binding.sigil == sigil
                && binding.mention == mention
            {
                ordered.push(MentionBinding {
                    sigil: binding.sigil,
                    mention: binding.mention,
                    path: binding.path,
                });
            }
        }
        self.draft.mention_bindings.clear();
        ordered
    }

    pub fn set_collaboration_modes_enabled(&mut self, enabled: bool) {
        self.collaboration_modes_enabled = enabled;
    }

    pub fn set_service_tier_commands_enabled(&mut self, enabled: bool) {
        self.service_tier_commands_enabled = enabled;
    }

    pub fn set_service_tier_commands(&mut self, commands: Vec<ServiceTierCommand>) {
        self.service_tier_commands = commands;
        self.sync_popups();
    }

    /// Replace the approved session extension commands available in this composer.
    pub fn set_session_extension_commands(&mut self, commands: Vec<SessionExtensionCommand>) {
        self.session_extension_commands = commands;
        self.sync_popups();
    }

    pub fn set_goal_command_enabled(&mut self, enabled: bool) {
        self.goal_command_enabled = enabled;
    }

    /// Replace composer, editor, and footer-hint key bindings from one runtime snapshot.
    ///
    /// Submit and queue bindings are cached here because composer dispatch must
    /// check them before generic textarea editing. The embedded textarea receives
    /// the same snapshot's editor bindings so a live remap cannot leave submit
    /// keys updated while cursor/editing keys still use old defaults.
    pub fn set_keymap_bindings(&mut self, keymap: &RuntimeKeymap) {
        self.submit_keys = keymap.composer.submit.clone();
        self.queue_keys = keymap.composer.queue.clone();
        self.toggle_shortcuts_keys = keymap.composer.toggle_shortcuts.clone();
        self.history_search_previous_keys = keymap.composer.history_search_previous.clone();
        self.history_search_next_keys = keymap.composer.history_search_next.clone();
        self.editor_keymap = keymap.editor.clone();
        self.vim_normal_keymap = keymap.vim_normal.clone();
        self.draft.textarea.set_keymap_bindings(keymap);
        self.footer.external_editor_key = primary_binding(&keymap.app.open_external_editor);
        self.footer.show_transcript_key = primary_binding(&keymap.app.open_transcript);
        self.footer.insert_newline_key = footer_insert_newline_key(
            &keymap.editor.insert_newline,
            self.footer.use_shift_enter_hint,
        );
        self.footer.queue_key = primary_binding(&keymap.composer.queue);
        self.footer.toggle_shortcuts_key = primary_binding(&keymap.composer.toggle_shortcuts);
        self.footer.history_search_key = primary_binding(&keymap.composer.history_search_previous);
        self.footer.reasoning_down_key = primary_binding(&keymap.chat.decrease_reasoning_effort);
        self.footer.reasoning_up_key = primary_binding(&keymap.chat.increase_reasoning_effort);
    }

    pub fn set_collaboration_mode_indicator(
        &mut self,
        indicator: Option<CollaborationModeIndicator>,
    ) {
        self.footer.collaboration_mode_indicator = indicator;
    }

    pub fn set_goal_status_indicator(&mut self, indicator: Option<GoalStatusIndicator>) {
        self.footer.goal_status_indicator = indicator;
    }

    pub fn set_ide_context_active(&mut self, active: bool) {
        self.footer.ide_context_active = active;
    }

    pub fn set_session_name(&mut self, session_name: Option<String>) -> bool {
        let session_name = session_name.and_then(|name| {
            let trimmed = name.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });
        if self.session_name == session_name {
            return false;
        }
        self.session_name = session_name;
        true
    }

    pub fn set_personality_command_enabled(&mut self, enabled: bool) {
        self.personality_command_enabled = enabled;
    }

    pub fn set_side_conversation_active(&mut self, active: bool) {
        self.side_conversation_active = active;
    }

    /// Centralized feature gating keeps config checks out of call sites.
    fn popups_enabled(&self) -> bool {
        self.config.popups_enabled
    }

    fn slash_commands_enabled(&self) -> bool {
        self.config.slash_commands_enabled
    }

    fn image_paste_enabled(&self) -> bool {
        self.config.image_paste_enabled
    }
    fn layout_areas(&self, area: Rect) -> [Rect; 4] {
        self.layout_areas_with_textarea_right_reserve(area, /*textarea_right_reserve*/ 0)
    }

    fn layout_areas_with_textarea_right_reserve(
        &self,
        area: Rect,
        textarea_right_reserve: u16,
    ) -> [Rect; 4] {
        let footer_props = self.footer_props();
        let footer_total_height = self.footer_total_height(&footer_props);
        let popup_constraint = match &self.popups.active {
            ActivePopup::Command(popup) => {
                Constraint::Max(popup.calculate_required_height(area.width))
            }
            ActivePopup::File(popup) => Constraint::Max(popup.calculate_required_height()),
            ActivePopup::Skill(popup) => {
                Constraint::Max(popup.calculate_required_height(area.width))
            }
            ActivePopup::MentionV2(popup) => {
                Constraint::Max(popup.calculate_required_height(area.width))
            }
            ActivePopup::None => Constraint::Max(footer_total_height),
        };
        let [composer_rect, popup_rect] =
            Layout::vertical([Constraint::Min(3), popup_constraint]).areas(area);
        let left_inset = LIVE_PREFIX_COLS + 1;
        let mut textarea_rect = composer_rect.inset(Insets::tlbr(
            /*top*/ 1,
            left_inset,
            /*bottom*/ 1,
            /*right*/ 1u16.saturating_add(textarea_right_reserve),
        ));
        let remote_images_height = self
            .attachments
            .remote_image_lines()
            .len()
            .try_into()
            .unwrap_or(u16::MAX)
            .min(textarea_rect.height.saturating_sub(1));
        let remote_images_separator = u16::from(remote_images_height > 0);
        let consumed = remote_images_height.saturating_add(remote_images_separator);
        let remote_images_rect = Rect {
            x: textarea_rect.x,
            y: textarea_rect.y,
            width: textarea_rect.width,
            height: remote_images_height,
        };
        textarea_rect.y = textarea_rect.y.saturating_add(consumed);
        textarea_rect.height = textarea_rect.height.saturating_sub(consumed);
        [composer_rect, remote_images_rect, textarea_rect, popup_rect]
    }

    fn footer_spacing(footer_hint_height: u16) -> u16 {
        if footer_hint_height == 0 {
            0
        } else {
            FOOTER_SPACING_HEIGHT
        }
    }

    fn footer_hint_height(&self, footer_props: &FooterProps) -> u16 {
        self.custom_footer_height()
            .unwrap_or_else(|| footer_height(footer_props))
    }

    fn footer_content_rects(
        popup_rect: Rect,
        footer_hint_height: u16,
        persistent_status_line_height: u16,
    ) -> [Rect; 2] {
        let footer_content_height = footer_hint_height + persistent_status_line_height;
        let footer_spacing = Self::footer_spacing(footer_content_height);
        let content_rect = if footer_spacing > 0 && footer_content_height > 0 {
            let [_, content_rect] = Layout::vertical([
                Constraint::Length(footer_spacing),
                Constraint::Length(footer_content_height),
            ])
            .areas(popup_rect);
            content_rect
        } else {
            popup_rect
        };

        Layout::vertical([
            Constraint::Length(persistent_status_line_height),
            Constraint::Length(footer_hint_height),
        ])
        .areas(content_rect)
    }

    fn persistent_status_line(&self, footer_props: &FooterProps) -> Option<Line<'static>> {
        if !footer_props.status_line_enabled {
            return None;
        }

        let normal_footer_renders_status_line = self.custom_footer_height().is_none()
            && self.history_search_footer_line().is_none()
            && !self.footer.plan_mode_nudge_visible
            && uses_passive_footer_status_layout(footer_props);
        if normal_footer_renders_status_line {
            return None;
        }

        configured_status_line(footer_props)
    }

    fn footer_total_height(&self, footer_props: &FooterProps) -> u16 {
        let footer_hint_height = self.footer_hint_height(footer_props);
        let footer_content_height =
            footer_hint_height + u16::from(self.persistent_status_line(footer_props).is_some());
        footer_content_height + Self::footer_spacing(footer_content_height)
    }

    pub fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_pos_with_textarea_right_reserve(area, /*textarea_right_reserve*/ 0)
    }

    pub fn cursor_pos_with_textarea_right_reserve(
        &self,
        area: Rect,
        textarea_right_reserve: u16,
    ) -> Option<(u16, u16)> {
        if !self.draft.input_enabled || self.attachments.selected_remote_image_index.is_some() {
            return None;
        }

        if let Some(pos) = self.history_search_cursor_pos(area) {
            return Some(pos);
        }

        let [_, _, textarea_rect, _] =
            self.layout_areas_with_textarea_right_reserve(area, textarea_right_reserve);
        let state = *self.draft.textarea_state.borrow();
        self.draft
            .textarea
            .cursor_pos_with_state(textarea_rect, state)
    }
    /// Returns true if the composer currently contains no user-entered input.
    pub fn is_empty(&self) -> bool {
        self.draft.textarea.is_empty() && !self.draft.is_bash_mode && self.attachments.is_empty()
    }

    /// Record local persistent-history metadata so the composer can navigate
    /// cross-session history.
    pub fn set_history_metadata(&mut self, thread_id: ThreadId, log_id: u64, entry_count: usize) {
        self.history.set_metadata(thread_id, log_id, entry_count);
    }

    /// Integrate an asynchronous response to an on-demand history lookup.
    ///
    /// If the entry is present and the offset still matches the active history cursor, the
    /// composer rehydrates the entry immediately. This path intentionally routes through
    /// [`Self::apply_history_entry`] so cursor placement remains aligned with keyboard history
    /// recall semantics.
    pub fn on_history_entry_response(
        &mut self,
        log_id: u64,
        offset: usize,
        entry: Option<String>,
    ) -> bool {
        match self
            .history
            .on_entry_response(log_id, offset, entry, &self.app_event_tx)
        {
            HistoryEntryResponse::Found(entry) => {
                // Persistent ↑/↓ history is text-only (backwards-compatible and avoids persisting
                // attachments), but local in-session ↑/↓ history can rehydrate elements and image paths.
                self.apply_history_entry(entry);
                true
            }
            HistoryEntryResponse::Search(result) => {
                self.apply_history_search_result(result);
                true
            }
            HistoryEntryResponse::Ignored => false,
        }
    }

    pub fn on_history_batch_response(
        &mut self,
        log_id: u64,
        cursor: HistoryBatchCursor,
        entries: Vec<crate::app_event::HistoryBatchEntryResponse>,
        next_older_cursor: Option<HistoryBatchCursor>,
    ) -> bool {
        let result = self.history.on_batch_response(
            log_id,
            cursor,
            entries,
            next_older_cursor,
            &self.app_event_tx,
        );
        self.apply_history_batch_result(result)
    }

    /// Applies a failed batch lookup without conflating it with history exhaustion.
    ///
    /// The history state machine either schedules a bounded retry, preserves an existing match, or
    /// returns the search UI to its idle draft when no match has been selected yet.
    pub fn on_history_batch_error(&mut self, log_id: u64, cursor: HistoryBatchCursor) -> bool {
        let result = self
            .history
            .on_batch_error(log_id, cursor, &self.app_event_tx);
        self.apply_history_batch_result(result)
    }

    fn apply_history_batch_result(&mut self, result: Option<HistorySearchResult>) -> bool {
        let Some(result) = result else {
            return false;
        };
        self.apply_history_search_result(result);
        true
    }

    pub fn record_replayed_user_message_history(&mut self, entry: HistoryEntry) {
        self.history.record_replayed_submission(entry);
    }

    /// Integrate pasted text into the composer.
    ///
    /// Acts as the only place where paste text is integrated, both for:
    ///
    /// - Real/explicit paste events surfaced by the terminal, and
    /// - Non-bracketed "paste bursts" that [`PasteBurst`](super::paste_burst::PasteBurst) buffers
    ///   and later flushes here.
    ///
    /// Behavior:
    ///
    /// - If the paste is larger than `LARGE_PASTE_CHAR_THRESHOLD` chars, inserts a placeholder
    ///   element (expanded on submit) and stores the full text in `pending_pastes`.
    /// - Otherwise, if the paste looks like an image path, attaches the image and inserts a
    ///   trailing space so the user can keep typing naturally.
    /// - Otherwise, inserts the pasted text directly into the textarea.
    ///
    /// In all cases, clears any paste-burst Enter suppression state so a real paste cannot affect
    /// the next user Enter key, then syncs popup state.
    pub fn handle_paste(&mut self, pasted: String) -> bool {
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let pasted = sanitize_user_text(&pasted);
        let char_count = pasted.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let placeholder = self.next_large_paste_placeholder(char_count);
            self.draft.textarea.insert_element(&placeholder);
            self.draft.pending_pastes.push((placeholder, pasted));
        } else if char_count > 1
            && self.image_paste_enabled()
            && self.handle_paste_image_path(pasted.clone())
        {
            self.draft.textarea.insert_str(" ");
        } else {
            self.insert_str(&pasted);
        }
        self.draft.paste_burst.clear_after_explicit_paste();
        self.sync_popups();
        true
    }

    pub fn handle_paste_image_path(&mut self, pasted: String) -> bool {
        let Some(path_buf) = normalize_pasted_path(&pasted) else {
            return false;
        };

        // normalize_pasted_path already handles Windows → WSL path conversion,
        // so we can directly try to read the image dimensions.
        match image::image_dimensions(&path_buf) {
            Ok((width, height)) => {
                tracing::info!("OK: {pasted}");
                tracing::debug!("image dimensions={}x{}", width, height);
                let format = pasted_image_format(&path_buf);
                tracing::debug!("attached image format={}", format.label());
                self.attach_image(path_buf);
                true
            }
            Err(err) => {
                tracing::trace!("ERR: {err}");
                false
            }
        }
    }

    /// Enable or disable paste-burst handling.
    ///
    /// `disable_paste_burst` is an escape hatch for terminals/platforms where the burst heuristic
    /// is unwanted or has already been handled elsewhere.
    ///
    /// When transitioning from enabled → disabled, we "defuse" any in-flight burst state so it
    /// cannot affect subsequent normal typing:
    ///
    /// - First, flush any held/buffered text immediately via
    ///   [`PasteBurst::flush_before_modified_input`], and feed it through `handle_paste(String)`.
    ///   This preserves user input and routes it through the same integration path as explicit
    ///   pastes (large-paste placeholders, image-path detection, and popup sync).
    /// - Then clear the burst timing and Enter-suppression window via
    ///   [`PasteBurst::clear_after_explicit_paste`].
    ///
    /// We intentionally do not use `clear_window_after_non_char()` here: it clears timing state
    /// without emitting any buffered text, which can leave a non-empty buffer unable to flush
    /// later (because `flush_if_due()` relies on `last_plain_char_time` to time out).
    pub fn set_disable_paste_burst(&mut self, disabled: bool) {
        let was_disabled = self.draft.disable_paste_burst;
        self.draft.disable_paste_burst = disabled;
        if disabled && !was_disabled {
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
            self.draft.paste_burst.clear_after_explicit_paste();
        }
    }

    /// Replace the composer content with text from an external editor.
    /// Clears paste placeholders, preserves document references, and keeps attachments whose
    /// placeholder labels still appear in the new text. Image placeholders
    /// are renumbered to `[Image #M+1]..[Image #N]` (where `M` is the number of
    /// remote images). Cursor is placed at the end after rebuilding elements.
    pub fn apply_external_edit(&mut self, text: String) {
        self.draft
            .pending_pastes
            .retain(|(anchor, _)| anchor.starts_with("[Doc #") && text.contains(anchor));
        let (text, _) = self.imported_text_for_textarea(text, Vec::new());

        // Count placeholder occurrences in the new text.
        let mut placeholder_counts: HashMap<String, usize> = HashMap::new();
        for placeholder in self
            .attachments
            .local_images
            .iter()
            .map(|image| &image.placeholder)
        {
            if placeholder_counts.contains_key(placeholder) {
                continue;
            }
            let count = text.match_indices(placeholder).count();
            if count > 0 {
                placeholder_counts.insert(placeholder.clone(), count);
            }
        }

        // Keep attachments only while we have matching occurrences left.
        let mut kept_images = Vec::new();
        for img in self.attachments.local_images.drain(..) {
            if let Some(count) = placeholder_counts.get_mut(&img.placeholder)
                && *count > 0
            {
                *count -= 1;
                kept_images.push(img);
            }
        }
        self.attachments.local_images = kept_images;

        // Rebuild textarea so placeholders become elements again.
        self.draft.textarea.set_text_clearing_elements("");
        let mut remaining: HashMap<&str, usize> = HashMap::new();
        for img in &self.attachments.local_images {
            *remaining.entry(img.placeholder.as_str()).or_insert(0) += 1;
        }
        for (anchor, _) in &self.draft.pending_pastes {
            remaining.insert(anchor, text.match_indices(anchor).count());
        }

        let mut occurrences: Vec<(usize, &str)> = Vec::new();
        for placeholder in remaining.keys() {
            for (pos, _) in text.match_indices(placeholder) {
                occurrences.push((pos, *placeholder));
            }
        }
        occurrences.sort_unstable_by_key(|(pos, _)| *pos);

        let mut idx = 0usize;
        for (pos, ph) in occurrences {
            let Some(count) = remaining.get_mut(ph) else {
                continue;
            };
            if *count == 0 {
                continue;
            }
            if pos > idx {
                self.draft.textarea.insert_str(&text[idx..pos]);
            }
            self.draft.textarea.insert_element(ph);
            *count -= 1;
            idx = pos + ph.len();
        }
        if idx < text.len() {
            self.draft.textarea.insert_str(&text[idx..]);
        }

        // Keep local image placeholders normalized in attachment order after the
        // remote-image prefix.
        self.attachments
            .relabel_local_images(&mut self.draft.textarea);
        self.draft
            .textarea
            .set_cursor(self.draft.textarea.text().len());
        self.sync_popups();
    }

    /// Enable or disable Vim editing for the composer textarea.
    ///
    /// The composer clears any in-flight paste-burst state when the mode
    /// changes because Vim normal mode treats rapid character sequences as
    /// commands, not as candidate literal paste text. It also resets transient
    /// footer mode so the visible hints match the new editing surface.
    pub fn set_vim_enabled(&mut self, enabled: bool) {
        self.draft.textarea.set_vim_enabled(enabled);
        self.draft.paste_burst.clear_after_explicit_paste();
        self.footer.mode = reset_mode_after_activity(self.footer.mode);
    }

    /// Toggle Vim editing and return the new enabled state.
    ///
    /// This is the app-level command target for the configurable Vim toggle
    /// keybinding; callers should use the returned value for status messages
    /// instead of rereading state after additional composer mutations.
    pub fn toggle_vim_enabled(&mut self) -> bool {
        let enabled = !self.draft.textarea.is_vim_enabled();
        self.set_vim_enabled(enabled);
        enabled
    }

    /// Return whether Vim editing is enabled for tests that assert mode transitions.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_vim_enabled(&self) -> bool {
        self.draft.textarea.is_vim_enabled()
    }

    pub fn vim_mode_label(&self) -> Option<&'static str> {
        self.draft.textarea.vim_mode_label()
    }

    /// Return whether Escape should be routed to the textarea before popups.
    ///
    /// Vim insert mode owns Escape as a transition back to normal mode. The app
    /// event layer asks this before running generic Escape behavior so the same
    /// key does not both leave insert mode and dismiss unrelated UI.
    pub fn should_handle_vim_insert_escape(&self, key_event: KeyEvent) -> bool {
        self.draft
            .textarea
            .should_handle_vim_insert_escape(key_event)
    }

    fn vim_mode_indicator_span(&self) -> Option<Span<'static>> {
        self.draft
            .textarea
            .vim_mode_label()
            .map(|label| match label {
                "Normal" => "Vim: Normal".cl_magenta(),
                "Insert" => "Vim: Insert".cl_green(),
                _ => unreachable!(),
            })
    }

    fn mode_indicator_line(&self, show_cycle_hint: bool) -> Option<Line<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if let Some(vim_mode) = self.vim_mode_indicator_span() {
            spans.push(vim_mode);
        }
        if let Some(indicators) = status_line_right_indicator_line(
            self.footer.collaboration_mode_indicator,
            self.footer.goal_status_indicator.as_ref(),
            self.footer.ide_context_active,
            show_cycle_hint,
        ) {
            if !spans.is_empty() {
                spans.push(" | ".dim());
            }
            spans.extend(indicators.spans);
        }
        if spans.is_empty() {
            None
        } else {
            Some(Line::from(spans))
        }
    }

    fn right_footer_line_with_context(&self) -> Line<'static> {
        let indicator_line = status_line_right_indicator_line(
            /*collaboration_mode_indicator*/ None,
            self.footer.goal_status_indicator.as_ref(),
            self.footer.ide_context_active,
            /*show_cycle_hint*/ false,
        );
        if let Some(line) = with_active_agent_context_line(
            indicator_line,
            self.footer.active_agent_label.as_deref(),
        ) {
            return line;
        }

        let mut line = context_window_line(
            self.footer.context_window_percent,
            self.footer.context_window_used_tokens,
        );
        if let Some(vim_mode) = self.vim_mode_indicator_span() {
            line.spans.push(" | ".dim());
            line.spans.push(vim_mode);
        }
        line
    }

    pub fn current_text_with_pending(&self) -> String {
        let text = self.current_text();
        if self.draft.pending_pastes.is_empty() {
            return text;
        }

        let (text, _) = Self::expand_pending_pastes(
            &text,
            self.current_text_elements(),
            &self.draft.pending_pastes,
        );
        text
    }

    /// Returns whether the composer currently accepts interactive draft edits.
    pub fn input_enabled(&self) -> bool {
        self.draft.input_enabled
    }

    pub fn pending_pastes(&self) -> Vec<(String, String)> {
        self.draft.pending_pastes.clone()
    }

    pub fn set_pending_pastes(&mut self, pending_pastes: Vec<(String, String)>) {
        let text = self.current_text();
        self.draft.pending_pastes = pending_pastes
            .into_iter()
            .filter(|(placeholder, _)| text.contains(placeholder))
            .collect();
    }

    /// Override the footer hint items displayed beneath the composer. Passing
    /// `None` restores the default shortcut footer.
    pub fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.footer.hint_override = items;
    }

    /// Updates whether the Plan-mode nudge replaces the ambient footer row.
    ///
    /// Returns `true` only when the rendered footer can change so callers can avoid scheduling
    /// redundant redraws while reevaluating nudge policy on routine composer updates.
    pub fn set_plan_mode_nudge_visible(&mut self, visible: bool) -> bool {
        if self.footer.plan_mode_nudge_visible == visible {
            return false;
        }
        self.footer.plan_mode_nudge_visible = visible;
        true
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn plan_mode_nudge_visible(&self) -> bool {
        self.footer.plan_mode_nudge_visible
    }

    pub fn set_remote_image_urls(&mut self, urls: Vec<String>) {
        self.attachments
            .set_remote_image_urls(urls, &mut self.draft.textarea);
        self.sync_popups();
    }

    pub fn remote_image_urls(&self) -> Vec<String> {
        self.attachments.remote_image_urls()
    }

    pub fn take_remote_image_urls(&mut self) -> Vec<String> {
        let urls = self
            .attachments
            .take_remote_image_urls(&mut self.draft.textarea);
        self.sync_popups();
        urls
    }

    /// Replace the entire composer content with `text` and reset cursor.
    ///
    /// This is the "fresh draft" path: it clears pending paste payloads and
    /// mention link targets. Callers restoring a previously submitted draft
    /// that must keep sigiled mention target resolution should use
    /// [`Self::set_text_content_with_mention_bindings`] instead.
    pub fn set_text_content(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        self.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            Vec::new(),
        );
    }

    /// Replace the entire composer content while restoring mention link targets.
    ///
    /// Mention popup insertion stores both visible text (for example `$file`)
    /// and hidden mention bindings used to resolve the canonical target during
    /// submission. Use this method when restoring an interrupted or blocked
    /// draft; if callers restore only text and images, mentions can appear
    /// intact to users while resolving to the wrong target or dropping on
    /// retry.
    ///
    /// This helper intentionally places the cursor at the start of the restored text. Callers
    /// that need end-of-line restore behavior (for example shell-style history recall) should call
    /// [`Self::move_cursor_to_end`] after this method.
    pub fn set_text_content_with_mention_bindings(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
        mention_bindings: Vec<MentionBinding>,
    ) {
        // Clear any existing content, placeholders, and attachments first.
        self.draft.textarea.set_text_clearing_elements("");
        self.draft.is_bash_mode = false;
        self.draft.pending_pastes.clear();
        self.draft.mention_bindings.clear();

        let (text, text_elements) = self.imported_text_for_textarea(text, text_elements);
        self.draft
            .textarea
            .set_text_with_elements(&text, &text_elements);
        self.attachments
            .reset_local_images(local_image_paths, &mut self.draft.textarea);

        self.bind_mentions_from_snapshot(mention_bindings);
        self.draft.textarea.set_cursor(/*pos*/ 0);
        self.sync_popups();
    }

    fn current_cursor(&self) -> usize {
        self.draft.textarea.cursor() + if self.draft.is_bash_mode { 1 } else { 0 }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn cursor(&self) -> usize {
        self.current_cursor()
    }

    fn history_navigation_cursor(&self) -> usize {
        if self.draft.is_bash_mode && self.draft.textarea.cursor() == 0 {
            0
        } else if self.draft.textarea.is_vim_normal_mode()
            && !self.draft.textarea.text().is_empty()
            && self.draft.textarea.cursor() == self.draft.textarea.vim_normal_end_cursor()
        {
            self.current_text().len()
        } else {
            self.current_cursor()
        }
    }

    fn set_current_cursor(&mut self, cursor: usize) {
        let visible_cursor = if self.draft.is_bash_mode {
            cursor.saturating_sub(1)
        } else {
            cursor
        };
        self.draft
            .textarea
            .set_cursor(visible_cursor.min(self.draft.textarea.text().len()));
    }

    fn current_text_elements(&self) -> Vec<TextElement> {
        let shift = if self.draft.is_bash_mode { 1 } else { 0 };
        self.draft
            .textarea
            .text_elements()
            .into_iter()
            .filter_map(|element| Self::shift_text_element(element, shift))
            .collect()
    }

    fn shift_text_element(element: TextElement, shift: isize) -> Option<TextElement> {
        let start = element.byte_range.start.checked_add_signed(shift)?;
        let end = element.byte_range.end.checked_add_signed(shift)?;
        if start >= end {
            return None;
        }

        Some(element.map_range(|_| (start..end).into()))
    }

    fn snapshot_draft(&self) -> ComposerDraft {
        ComposerDraft {
            text: self.current_text(),
            text_elements: self.current_text_elements(),
            local_image_paths: self.attachments.local_image_paths(),
            remote_image_urls: self.attachments.remote_image_urls(),
            mention_bindings: self.snapshot_mention_bindings(),
            pending_pastes: self.draft.pending_pastes.clone(),
            cursor: self.current_cursor(),
        }
    }

    fn restore_draft(&mut self, draft: ComposerDraft) {
        let ComposerDraft {
            text,
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
            cursor,
        } = draft;
        self.set_remote_image_urls(remote_image_urls);
        self.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.set_pending_pastes(pending_pastes);
        self.set_current_cursor(cursor);
        self.sync_popups();
    }

    /// Update the placeholder text without changing input enablement.
    pub fn set_placeholder_text(&mut self, placeholder: String) {
        self.placeholder_text = placeholder;
    }

    pub fn set_parent_owned_thread(&mut self) {
        self.blocks_direct_input = true;
        self.placeholder_text = "Viewing sub-agent — direct input is disabled".to_string();
    }

    /// Move the cursor to the end of the current text buffer.
    pub fn move_cursor_to_end(&mut self) {
        self.draft
            .textarea
            .set_cursor(self.draft.textarea.text().len());
        self.sync_popups();
    }

    fn move_cursor_to_history_entry_end(&mut self) {
        let cursor = if self.draft.textarea.is_vim_normal_mode() {
            self.draft.textarea.vim_normal_end_cursor()
        } else {
            self.draft.textarea.text().len()
        };
        self.draft.textarea.set_cursor(cursor);
        self.sync_popups();
    }

    /// Convert canonical composer text into the textarea's internal representation.
    ///
    /// Shell mode stores the leading `!` as prompt state instead of editable text,
    /// so full-buffer imports must absorb that prefix before rebuilding the textarea.
    fn imported_text_for_textarea(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
    ) -> (String, Vec<TextElement>) {
        if let Some(stripped) = text.strip_prefix('!') {
            self.draft.is_bash_mode = true;
            (
                stripped.to_string(),
                text_elements
                    .into_iter()
                    .filter_map(|element| Self::shift_text_element(element, /*shift*/ -1))
                    .collect(),
            )
        } else {
            self.draft.is_bash_mode = false;
            (text, text_elements)
        }
    }

    pub fn clear_for_ctrl_c(&mut self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let previous = self.current_text();
        let text_elements = self.current_text_elements();
        let local_image_paths = self.attachments.local_image_paths();
        let pending_pastes = std::mem::take(&mut self.draft.pending_pastes);
        let remote_image_urls = self.attachments.remote_image_urls();
        let mention_bindings = self.snapshot_mention_bindings();
        self.set_text_content(String::new(), Vec::new(), Vec::new());
        self.attachments.clear_remote_image_urls();
        self.history.reset_navigation();
        self.history.record_local_submission(HistoryEntry {
            text: previous.clone(),
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
        });
        Some(previous)
    }

    /// Get the current composer text.
    pub fn current_text(&self) -> String {
        if self.draft.is_bash_mode {
            format!("!{}", self.draft.textarea.text())
        } else {
            self.draft.textarea.text().to_string()
        }
    }

    /// Rehydrate a history entry into the composer with shell-like cursor placement.
    ///
    /// This path restores text, elements, images, mention bindings, and pending paste payloads,
    /// then moves the cursor to the active mode's history boundary. If a caller reused
    /// [`Self::set_text_content_with_mention_bindings`] directly for history recall and forgot the
    /// final cursor move, repeated Up/Down would stop navigating history because cursor-gating
    /// treats interior positions as normal editing mode.
    fn apply_history_entry(&mut self, entry: HistoryEntry) {
        let HistoryEntry {
            text,
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
        } = entry;
        self.set_remote_image_urls(remote_image_urls);
        self.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.set_pending_pastes(pending_pastes);
        self.move_cursor_to_history_entry_end();
    }

    pub fn text_elements(&self) -> Vec<TextElement> {
        self.current_text_elements()
    }

    pub fn draft_snapshot(&self) -> ComposerDraftSnapshot {
        ComposerDraftSnapshot {
            text: self.current_text(),
            text_elements: self.text_elements(),
            local_images: self.local_images(),
            remote_image_urls: self.remote_image_urls(),
            mention_bindings: self.mention_bindings(),
            pending_pastes: self.pending_pastes(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn local_image_paths(&self) -> Vec<PathBuf> {
        self.attachments.local_image_paths()
    }

    pub fn status_line_text(&self) -> Option<String> {
        self.footer.status_line_text()
    }

    pub fn browser_status_line(&self) -> Option<Line<'static>> {
        self.footer.status_line_value.clone()
    }

    pub fn local_images(&self) -> Vec<LocalImageAttachment> {
        self.attachments.local_images()
    }

    pub fn mention_bindings(&self) -> Vec<MentionBinding> {
        self.snapshot_mention_bindings()
    }

    pub fn take_recent_submission_mention_bindings(&mut self) -> Vec<MentionBinding> {
        std::mem::take(&mut self.draft.recent_submission_mention_bindings)
    }

    /// Commit the staged slash-command draft to local Up-arrow recall.
    ///
    /// Call this after command dispatch. Calling it more than once is harmless because the pending
    /// slot is consumed on the first call.
    pub fn record_pending_slash_command_history(&mut self) {
        if let Some(entry) = self.pending_slash_command_history.take() {
            self.history.record_local_submission(entry);
        }
    }

    /// Insert an attachment placeholder and track it for the next submission.
    pub fn attach_image(&mut self, path: PathBuf) {
        self.attachments
            .attach_image(&mut self.draft.textarea, path);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn take_recent_submission_images(&mut self) -> Vec<PathBuf> {
        self.attachments.take_recent_submission_images()
    }

    pub fn take_recent_submission_images_with_placeholders(&mut self) -> Vec<LocalImageAttachment> {
        self.attachments
            .take_recent_submission_images_with_placeholders()
    }

    /// Flushes any due paste-burst state.
    ///
    /// Call this from a UI tick to turn paste-burst transient state into explicit textarea edits:
    ///
    /// - If a burst times out, flush it via `handle_paste(String)`.
    /// - If only the first ASCII char was held (flicker suppression) and no burst followed, emit it
    ///   as normal typed input.
    ///
    /// This also allows a single "held" ASCII char to render even when it turns out not to be part
    /// of a paste burst.
    pub fn flush_paste_burst_if_due(&mut self) -> bool {
        self.handle_paste_burst_flush(Instant::now())
    }

    /// Returns whether the composer is currently in any paste-burst related transient state.
    ///
    /// This includes actively buffering, having a non-empty burst buffer, or holding the first
    /// ASCII char for flicker suppression.
    pub fn is_in_paste_burst(&self) -> bool {
        self.draft.paste_burst.is_active()
    }

    /// Returns a delay that reliably exceeds the paste-burst timing threshold.
    ///
    /// Use this in tests to avoid boundary flakiness around the `PasteBurst` timeout.
    pub fn recommended_paste_flush_delay() -> Duration {
        PasteBurst::recommended_flush_delay()
    }

    /// Integrate results from an asynchronous file search.
    pub fn on_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        // Only apply if user is still editing a token starting with `query`.
        let current_opt = if self.mentions_v2_enabled {
            self.current_mentions_v2_token()
        } else {
            Self::current_at_token(&self.draft.textarea)
        };
        let Some(current_token) = current_opt else {
            return;
        };

        if !current_token.starts_with(&query) {
            return;
        }

        match &mut self.popups.active {
            ActivePopup::File(popup) => {
                popup.set_matches(&query, matches);
            }
            ActivePopup::MentionV2(popup) => {
                popup.set_file_matches(&query, matches);
            }
            _ => {}
        }
    }

    /// Show the transient "press again to quit" hint for `key`.
    ///
    /// The owner (`BottomPane`/`ChatWidget`) is responsible for scheduling a
    /// redraw after [`super::QUIT_SHORTCUT_TIMEOUT`] so the hint can disappear
    /// even when the UI is otherwise idle.
    pub fn show_quit_shortcut_hint(&mut self, key: KeyBinding, has_focus: bool) {
        self.footer.quit_shortcut_expires_at = Instant::now()
            .checked_add(super::QUIT_SHORTCUT_TIMEOUT)
            .or_else(|| Some(Instant::now()));
        self.footer.quit_shortcut_key = key;
        self.footer.mode = FooterMode::QuitShortcutReminder;
        self.set_has_focus(has_focus);
    }

    /// Clear the "press again to quit" hint immediately.
    pub fn clear_quit_shortcut_hint(&mut self, has_focus: bool) {
        self.footer.quit_shortcut_expires_at = None;
        self.footer.mode = reset_mode_after_activity(self.footer.mode);
        self.set_has_focus(has_focus);
    }

    /// Whether the quit shortcut hint should currently be shown.
    ///
    /// This is time-based rather than event-based: it may become false without
    /// any additional user input, so the UI schedules a redraw when the hint
    /// expires.
    pub fn quit_shortcut_hint_visible(&self) -> bool {
        self.footer
            .quit_shortcut_expires_at
            .is_some_and(|expires_at| Instant::now() < expires_at)
    }

    fn next_large_paste_placeholder(&self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let prefix = format!("{base} #");
        let mut max_suffix = 0usize;

        for (placeholder, _) in &self.draft.pending_pastes {
            if placeholder == &base {
                max_suffix = max_suffix.max(1);
                continue;
            }
            if let Some(suffix) = placeholder.strip_prefix(&prefix)
                && let Ok(value) = suffix.parse::<usize>()
            {
                max_suffix = max_suffix.max(value);
            }
        }

        if max_suffix == 0 {
            base
        } else {
            format!("{base} #{}", max_suffix + 1)
        }
    }

    pub fn insert_str(&mut self, text: &str) {
        self.draft.textarea.insert_str(text);
        self.sync_bash_mode_from_text();
        self.sync_popups();
    }

    /// Handle a key event coming from the main UI.
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if !self.draft.input_enabled {
            return (InputResult::None, false);
        }

        if matches!(key_event.kind, KeyEventKind::Release) {
            return (InputResult::None, false);
        }

        if self.history_search.is_some() {
            return self.handle_history_search_key(key_event);
        }

        if !self.popups.active()
            && key_event.modifiers == KeyModifiers::SHIFT
            && key_event.kind == KeyEventKind::Press
        {
            match key_event.code {
                KeyCode::Left => {
                    self.app_event_tx
                        .send(AppEvent::ModelRouterModeCycleRequested {
                            direction:
                                xedoc_app_server_protocol::ModelRouterModeCycleDirection::Toggle,
                        });
                    return (InputResult::None, true);
                }
                KeyCode::Right => {
                    self.app_event_tx.send(AppEvent::OpenModelPicker);
                    return (InputResult::None, true);
                }
                _ => {}
            }
        }

        if Self::is_history_search_key(&key_event, &self.history_search_previous_keys) {
            return self.begin_history_search();
        }

        let result = match &mut self.popups.active {
            ActivePopup::Command(_) => self.handle_key_event_with_slash_popup(key_event),
            ActivePopup::File(_) => self.handle_key_event_with_file_popup(key_event),
            ActivePopup::Skill(_) => self.handle_key_event_with_skill_popup(key_event),
            ActivePopup::MentionV2(_) => self.handle_key_event_with_mentions_v2_popup(key_event),
            ActivePopup::None => self.handle_key_event_without_popup(key_event),
        };
        self.reset_vim_mode_after_successful_dispatch(&result.0);
        // Update (or hide/show) popup after processing the key.
        self.sync_popups();
        result
    }

    /// Return true if any popup or history search is active.
    pub fn popup_active(&self) -> bool {
        self.history_search.is_some() || self.popups.active()
    }

    #[inline]
    fn clamp_to_char_boundary(text: &str, pos: usize) -> usize {
        let mut p = pos.min(text.len());
        if p < text.len() && !text.is_char_boundary(p) {
            p = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= p)
                .last()
                .unwrap_or(0);
        }
        p
    }

    /// Handle non-ASCII character input (often IME) while still supporting paste-burst detection.
    ///
    /// This handler exists because non-ASCII input often comes from IMEs, where characters can
    /// legitimately arrive in short bursts that should **not** be treated as paste.
    ///
    /// The key differences from the ASCII path:
    ///
    /// - We never hold the first character (`PasteBurst::on_plain_char_no_hold`), because holding a
    ///   non-ASCII char can feel like dropped input.
    /// - If a burst is detected, we may need to retroactively remove already-inserted text before
    ///   the cursor and move it into the paste buffer (see `PasteBurst::decide_begin_buffer`).
    ///
    /// Because this path mixes "insert immediately" with "maybe retro-grab later", it must clamp
    /// the cursor to a UTF-8 char boundary before slicing `textarea.text()`.
    #[inline]
    fn handle_non_ascii_char(&mut self, input: KeyEvent, now: Instant) -> (InputResult, bool) {
        if self.draft.disable_paste_burst {
            // When burst detection is disabled, treat IME/non-ASCII input as normal typing.
            // In particular, do not retro-capture or buffer already-inserted prefix text.
            self.draft.textarea.input(input);
            let text_after = self.draft.textarea.text();
            self.draft
                .pending_pastes
                .retain(|(placeholder, _)| text_after.contains(placeholder));
            return (InputResult::None, true);
        }
        if let KeyEvent {
            code: KeyCode::Char(ch),
            ..
        } = input
        {
            if self.draft.paste_burst.try_append_char_if_active(ch, now) {
                return (InputResult::None, true);
            }
            // Non-ASCII input often comes from IMEs and can arrive in quick bursts.
            // We do not want to hold the first char (flicker suppression) on this path, but we
            // still want to detect paste-like bursts. Before applying any non-ASCII input, flush
            // any existing burst buffer (including a pending first char from the ASCII path) so
            // we don't carry that transient state forward.
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
            if let Some(decision) = self.draft.paste_burst.on_plain_char_no_hold(now) {
                match decision {
                    CharDecision::BufferAppend => {
                        self.draft.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    CharDecision::BeginBuffer { retro_chars } => {
                        // For non-ASCII we inserted prior chars immediately, so if this turns out
                        // to be paste-like we need to retroactively grab & remove the already-
                        // inserted prefix from the textarea before buffering the burst.
                        let cur = self.draft.textarea.cursor();
                        let txt = self.draft.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) = self.draft.paste_burst.decide_begin_buffer(
                            now,
                            before,
                            retro_chars as usize,
                        ) {
                            if !grab.grabbed.is_empty() {
                                self.draft
                                    .textarea
                                    .replace_range(grab.start_byte..safe_cur, "");
                            }
                            // seed the paste burst buffer with everything (grabbed + new)
                            self.draft.paste_burst.append_char_to_buffer(ch, now);
                            return (InputResult::None, true);
                        }
                        // If decide_begin_buffer opted not to start buffering,
                        // fall through to normal insertion below.
                    }
                    _ => unreachable!("on_plain_char_no_hold returned unexpected variant"),
                }
            }
        }
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }
        self.draft.textarea.input(input);

        let text_after = self.draft.textarea.text();
        self.draft
            .pending_pastes
            .retain(|(placeholder, _)| text_after.contains(placeholder));
        (InputResult::None, true)
    }

    /// Handle key events when file search popup is visible.
    fn handle_key_event_with_file_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if self.handle_shortcut_overlay_key(&key_event) {
            return (InputResult::None, true);
        }
        if key_event.code == KeyCode::Esc {
            let next_mode = esc_hint_mode(self.footer.mode, self.is_task_running);
            if next_mode != self.footer.mode {
                self.footer.mode = next_mode;
                return (InputResult::None, true);
            }
        } else {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }
        let ActivePopup::File(popup) = &mut self.popups.active else {
            unreachable!();
        };

        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => {
                popup.page_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => {
                popup.page_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if let Some((range, query)) = completion_target::current_prefixed_token_range(
                    &self.draft.textarea,
                    '@',
                    /*allow_empty*/ false,
                ) {
                    self.popups.dismissed_file_token =
                        Some(DismissedToken::new(&self.draft.textarea, range, query));
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                let Some(sel) = popup.selected_match() else {
                    self.popups.active = ActivePopup::None;
                    return if key_event.code == KeyCode::Enter {
                        self.handle_key_event_without_popup(key_event)
                    } else {
                        (InputResult::None, true)
                    };
                };

                let sel_path = sel.path.to_string_lossy().to_string();
                let sel_match_type = sel.match_type;
                if let Some((token_range, _)) =
                    self.current_editable_at_token_range_with_options(/*allow_empty*/ false)
                {
                    if key_event.code == KeyCode::Tab && sel_match_type == MatchType::Directory {
                        self.insert_selected_directory_path(token_range, &sel_path);
                        return (InputResult::None, true);
                    }
                    self.insert_selected_file_path(token_range, &sel_path);
                    return (InputResult::None, true);
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        }
    }

    /// Handle key events when the legacy skill mention popup is visible.
    fn handle_key_event_with_skill_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if self.handle_shortcut_overlay_key(&key_event) {
            return (InputResult::None, true);
        }
        self.footer.mode = reset_mode_after_activity(self.footer.mode);

        let ActivePopup::Skill(popup) = &mut self.popups.active else {
            unreachable!();
        };

        let mut selected_mention: Option<(String, Option<String>)> = None;
        let mut close_popup = false;

        let result = match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if let Some(target) = self.current_mention_target() {
                    self.popups.dismissed_mention_token = Some(DismissedToken::new(
                        &self.draft.textarea,
                        target.range,
                        target.query,
                    ));
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let Some(mention) = popup.selected_mention() {
                    selected_mention = Some((mention.insert_text.clone(), mention.path.clone()));
                }
                close_popup = true;
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        };

        if close_popup {
            if let Some((insert_text, path)) = selected_mention
                && let Some(target) = self.current_mention_target()
            {
                self.insert_selected_mention(target.range, &insert_text, path.as_deref());
            }
            self.popups.active = ActivePopup::None;
        }

        result
    }

    fn handle_key_event_with_mentions_v2_popup(
        &mut self,
        key_event: KeyEvent,
    ) -> (InputResult, bool) {
        if self.handle_shortcut_overlay_key(&key_event) {
            return (InputResult::None, true);
        }
        self.footer.mode = reset_mode_after_activity(self.footer.mode);
        let can_switch_search_mode = self.current_editable_at_token().is_some();

        let ActivePopup::MentionV2(popup) = &mut self.popups.active else {
            unreachable!();
        };

        let mut selected: Option<MentionV2Selection> = None;
        let mut selected_enters_directory = false;
        let mut close_popup = false;
        let mut submit_without_popup = false;

        let result = match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => {
                popup.page_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => {
                popup.page_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if can_switch_search_mode {
                    popup.previous_search_mode();
                    (InputResult::None, true)
                } else {
                    self.handle_input_basic(key_event)
                }
            }
            KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if can_switch_search_mode {
                    popup.next_search_mode();
                    (InputResult::None, true)
                } else {
                    self.handle_input_basic(key_event)
                }
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if let Some((range, query)) = self.current_mentions_v2_token_range() {
                    self.popups.dismissed_mention_token =
                        Some(DismissedToken::new(&self.draft.textarea, range, query));
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                selected_enters_directory = popup.selected_is_directory();
                selected = popup.selected();
                close_popup = true;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                selected = popup.selected();
                close_popup = true;
                submit_without_popup = selected.is_none();
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        };

        if close_popup {
            let token_range = self
                .current_editable_at_token_range_with_options(/*allow_empty*/ true)
                .map(|(range, _)| range);
            if let (Some(selected), Some(token_range)) = (selected, token_range) {
                match selected {
                    MentionV2Selection::File(path) => {
                        let path = path.to_string_lossy();
                        if selected_enters_directory {
                            self.insert_selected_directory_path(token_range, path.as_ref());
                        } else {
                            self.insert_selected_file_path(token_range, path.as_ref());
                        }
                    }
                    MentionV2Selection::Tool { insert_text, path } => {
                        self.insert_selected_mention(token_range, &insert_text, path.as_deref());
                    }
                }
            }
            if !selected_enters_directory {
                self.popups.active = ActivePopup::None;
            }
            if submit_without_popup {
                return self.handle_key_event_without_popup(key_event);
            }
        }

        result
    }

    fn is_image_path(path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        lower.ends_with(".png")
            || lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".gif")
            || lower.ends_with(".webp")
    }

    /// Leaves the cursor after one horizontal separator following a completion.
    ///
    /// Another separator is preserved before any non-whitespace suffix so subsequent typing does
    /// not merge into it. Line breaks are never reused as separators; a space is inserted before
    /// them so subsequent typing stays on the completed token's line.
    fn advance_past_completion_separator(&mut self) {
        let cursor = self.draft.textarea.cursor();
        let existing_separator_len = self.draft.textarea.text()[cursor..]
            .chars()
            .next()
            .filter(|c| {
                c.is_whitespace()
                    && !matches!(
                        *c,
                        '\n' | '\r'
                            | '\u{000B}'
                            | '\u{000C}'
                            | '\u{0085}'
                            | '\u{2028}'
                            | '\u{2029}'
                    )
            })
            .map(char::len_utf8);
        if let Some(separator_len) = existing_separator_len {
            let after_separator = cursor + separator_len;
            let separator_precedes_suffix = self.draft.textarea.text()[after_separator..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace());
            if separator_precedes_suffix {
                self.draft.textarea.insert_str(" ");
            } else {
                self.draft.textarea.set_cursor(after_separator);
            }
        } else {
            self.draft.textarea.insert_str(" ");
        }
    }

    /// Dismisses popup synchronization only for the exact token occurrence just inserted.
    ///
    /// Matching both range and text prevents an identical token later in the draft from inheriting
    /// the completed token's dismissal state.
    fn dismiss_completed_prefixed_token(
        &mut self,
        prefix: char,
        inserted_range: Range<usize>,
        inserted_text: &str,
    ) {
        let Some(completed_token) = inserted_text.strip_prefix(prefix) else {
            return;
        };
        // Completion leaves the cursor on separator whitespace, where normal token affinity would
        // otherwise immediately reopen the popup for sigil-prefixed inserted text.
        let Some((current_range, current_token)) = completion_target::current_prefixed_token_range(
            &self.draft.textarea,
            prefix,
            /*allow_empty*/ true,
        ) else {
            return;
        };
        if current_range != inserted_range || current_token != completed_token {
            return;
        }

        if prefix == '@' && !self.mentions_v2_enabled {
            self.popups.dismissed_file_token = Some(DismissedToken::new(
                &self.draft.textarea,
                current_range,
                current_token,
            ));
        } else {
            self.popups.dismissed_mention_token = Some(DismissedToken::new(
                &self.draft.textarea,
                current_range,
                current_token,
            ));
        }
    }

    /// Expands a leading `~/` to the home directory for filesystem access.
    fn expand_user_path(path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix(r"~\"))
            && let Some(home) = dirs::home_dir()
        {
            return home.join(rest);
        }
        PathBuf::from(path)
    }

    fn insert_selected_file_path(&mut self, token_range: Range<usize>, selected_path: &str) {
        let token_range = self.separate_completion_from_adjacent_element(token_range);
        if Self::is_image_path(selected_path) {
            let path_buf = Self::expand_user_path(selected_path);
            match image::image_dimensions(&path_buf) {
                Ok((width, height)) => {
                    tracing::debug!("selected image dimensions={}x{}", width, height);
                    let start_idx = token_range.start;
                    self.draft.textarea.replace_range(token_range, "");
                    self.draft.textarea.set_cursor(start_idx);
                    self.attach_image(path_buf);
                    self.advance_past_completion_separator();
                }
                Err(err) => {
                    tracing::trace!("image dimensions lookup failed: {err}");
                    self.insert_selected_path(token_range, selected_path);
                }
            }
        } else {
            self.insert_selected_path(token_range, selected_path);
        }
    }

    fn trim_text_elements(
        original: &str,
        trimmed: &str,
        elements: Vec<TextElement>,
    ) -> Vec<TextElement> {
        if trimmed.is_empty() || elements.is_empty() {
            return Vec::new();
        }
        let trimmed_start = original.len().saturating_sub(original.trim_start().len());
        let trimmed_end = trimmed_start.saturating_add(trimmed.len());

        elements
            .into_iter()
            .filter_map(|elem| {
                let start = elem.byte_range.start;
                let end = elem.byte_range.end;
                if end <= trimmed_start || start >= trimmed_end {
                    return None;
                }
                let new_start = start.saturating_sub(trimmed_start);
                let new_end = end.saturating_sub(trimmed_start).min(trimmed.len());
                if new_start >= new_end {
                    return None;
                }
                let placeholder = trimmed.get(new_start..new_end).map(str::to_string);
                Some(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ))
            })
            .collect()
    }

    /// Expand large-paste placeholders using element ranges and rebuild other element spans.
    pub fn expand_pending_pastes(
        text: &str,
        mut elements: Vec<TextElement>,
        pending_pastes: &[(String, String)],
    ) -> (String, Vec<TextElement>) {
        if pending_pastes.is_empty() || elements.is_empty() {
            return (text.to_string(), elements);
        }

        // Stage 1: index pending paste payloads by placeholder for deterministic replacements.
        let mut pending_by_placeholder: HashMap<&str, VecDeque<&str>> = HashMap::new();
        for (placeholder, actual) in pending_pastes {
            pending_by_placeholder
                .entry(placeholder.as_str())
                .or_default()
                .push_back(actual.as_str());
        }

        // Stage 2: walk elements in order and rebuild text/spans in a single pass.
        elements.sort_by_key(|elem| elem.byte_range.start);

        let mut rebuilt = String::with_capacity(text.len());
        let mut rebuilt_elements = Vec::with_capacity(elements.len());
        let mut cursor = 0usize;

        for elem in elements {
            let start = elem.byte_range.start.min(text.len());
            let end = elem.byte_range.end.min(text.len());
            if start > end {
                continue;
            }
            if start > cursor {
                rebuilt.push_str(&text[cursor..start]);
            }
            let elem_text = &text[start..end];
            let placeholder = elem.placeholder(text).map(str::to_string);
            let replacement = placeholder
                .as_deref()
                .and_then(|ph| pending_by_placeholder.get_mut(ph))
                .and_then(VecDeque::pop_front);
            if let Some(actual) = replacement {
                // Stage 3: inline actual paste payloads and drop their placeholder elements.
                rebuilt.push_str(actual);
            } else {
                // Stage 4: keep non-paste elements, updating their byte ranges for the new text.
                let new_start = rebuilt.len();
                rebuilt.push_str(elem_text);
                let new_end = rebuilt.len();
                let placeholder = placeholder.or_else(|| Some(elem_text.to_string()));
                rebuilt_elements.push(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ));
            }
            cursor = end;
        }

        // Stage 5: append any trailing text that followed the last element.
        if cursor < text.len() {
            rebuilt.push_str(&text[cursor..]);
        }

        (rebuilt, rebuilt_elements)
    }

    pub fn skills(&self) -> Option<&Vec<SkillMetadata>> {
        self.skills.as_ref()
    }

    pub fn plugins(&self) -> Option<&Vec<PluginCapabilitySummary>> {
        self.plugins.as_ref()
    }

    fn mentions_enabled(&self) -> bool {
        let skills_ready = self
            .skills
            .as_ref()
            .is_some_and(|skills| !skills.is_empty());
        let plugins_ready = self
            .plugins
            .as_ref()
            .is_some_and(|plugins| !plugins.is_empty());
        skills_ready || plugins_ready
    }

    fn current_prefixed_token(
        textarea: &TextArea,
        prefix: char,
        allow_empty: bool,
    ) -> Option<String> {
        completion_target::current_prefixed_token_range(textarea, prefix, allow_empty)
            .map(|(_, token)| token)
    }

    /// Extract the `@token` that the cursor is currently positioned on, if any.
    ///
    /// The returned string **does not** include the leading `@`.
    fn current_at_token(textarea: &TextArea) -> Option<String> {
        Self::current_prefixed_token(textarea, '@', /*allow_empty*/ false)
    }

    /// Returns the active prefixed token only when its sigil and name remain editable plaintext.
    ///
    /// Atomic elements and tokens whose mention prefix is already atomic are excluded so bound
    /// mentions are not offered for completion again.
    fn current_editable_prefixed_token_range(
        &self,
        prefix: char,
        allow_empty: bool,
    ) -> Option<(Range<usize>, String)> {
        let (range, token) = completion_target::current_prefixed_token_range(
            &self.draft.textarea,
            prefix,
            allow_empty,
        )?;
        completion_target::prefixed_token_range_is_editable(
            &self.draft.textarea,
            prefix,
            &range,
            &token,
        )
        .then_some((range, token))
    }

    fn current_editable_at_token_range_with_options(
        &self,
        allow_empty: bool,
    ) -> Option<(Range<usize>, String)> {
        self.current_editable_prefixed_token_range('@', allow_empty)
    }

    fn current_editable_at_token_with_options(&self, allow_empty: bool) -> Option<String> {
        self.current_editable_at_token_range_with_options(allow_empty)
            .map(|(_, token)| token)
    }

    fn current_editable_at_token(&self) -> Option<String> {
        self.current_editable_at_token_with_options(/*allow_empty*/ false)
    }

    fn current_mentions_v2_token_range(&self) -> Option<(Range<usize>, String)> {
        if !self.mentions_v2_enabled {
            return None;
        }
        self.current_editable_at_token_range_with_options(/*allow_empty*/ true)
    }

    fn current_mentions_v2_token(&self) -> Option<String> {
        self.current_mentions_v2_token_range()
            .map(|(_, token)| token)
    }

    /// Resolves the active legacy `$` target without eagerly cloning the mention catalog.
    ///
    /// Definite shell syntax is rejected. Ambiguous shell-like queries are accepted only when a
    /// bindable mention matches. The query-independent catalog built during that check is carried
    /// forward only when the selected target is also ambiguous.
    fn current_mention_target(&self) -> Option<MentionCompletionTarget> {
        if !self.mentions_enabled() {
            return None;
        }
        let mentions: OnceCell<Vec<MentionItem>> = OnceCell::new();
        let (range, query) = {
            let dollar_query_is_completable =
                |query: &str| match completion_target::dollar_query_kind(query) {
                    completion_target::DollarQueryKind::Completable => true,
                    completion_target::DollarQueryKind::AmbiguousShellParameter => mentions
                        .get_or_init(|| {
                            self.mention_items()
                                .into_iter()
                                .filter(|mention| {
                                    mention.path.is_some()
                                        && Self::mention_token_from_insert_text(
                                            &mention.insert_text,
                                        )
                                        .is_some()
                                })
                                .collect()
                        })
                        .iter()
                        .any(|mention| mention.fuzzy_match_query(query).is_some()),
                    completion_target::DollarQueryKind::ShellVariable
                    | completion_target::DollarQueryKind::DefiniteShellParameter
                    | completion_target::DollarQueryKind::Invalid => false,
                };
            let (range, query) =
                completion_target::current_prefixed_token_range_with_dollar_predicate(
                    &self.draft.textarea,
                    '$',
                    /*allow_empty*/ true,
                    dollar_query_is_completable,
                )?;
            if !completion_target::prefixed_token_range_is_editable(
                &self.draft.textarea,
                '$',
                &range,
                &query,
            ) || !dollar_query_is_completable(&query)
            {
                return None;
            }
            (range, query)
        };
        let prebuilt_mentions = if matches!(
            completion_target::dollar_query_kind(&query),
            completion_target::DollarQueryKind::AmbiguousShellParameter
        ) {
            mentions.into_inner()
        } else {
            None
        };

        Some(MentionCompletionTarget {
            range,
            query,
            prebuilt_mentions,
        })
    }

    /// Replace the active `@token` (the one under the cursor) with `path`.
    fn insert_selected_path(&mut self, token_range: Range<usize>, path: &str) {
        // If the path contains whitespace, wrap it in double quotes so the
        // local prompt arg parser treats it as a single argument. Avoid adding
        // quotes when the path already contains one to keep behavior simple.
        let needs_quotes = path.chars().any(char::is_whitespace);
        let inserted = if needs_quotes && !path.contains('"') {
            format!("\"{path}\"")
        } else {
            path.to_string()
        };

        // Replace just the active `@token` so unrelated text elements, such as
        // large-paste placeholders, remain atomic and can still expand on submit.
        let start_idx = token_range.start;
        self.draft.textarea.replace_range(token_range, &inserted);
        let inserted_range = start_idx..start_idx.saturating_add(inserted.len());
        self.draft.textarea.set_cursor(inserted_range.end);
        self.advance_past_completion_separator();
        self.dismiss_completed_prefixed_token('@', inserted_range, &inserted);
    }

    fn insert_selected_directory_path(&mut self, token_range: Range<usize>, path: &str) {
        let start_idx = token_range.start;
        let mut path = path.to_string();
        if !path.ends_with('/') && !path.ends_with('\\') {
            let separator = if path.contains('\\') && !path.contains('/') {
                '\\'
            } else {
                '/'
            };
            path.push(separator);
        }
        let inserted = format!("@{path}");

        self.draft.textarea.replace_range(token_range, &inserted);
        self.draft
            .textarea
            .set_cursor(start_idx.saturating_add(inserted.len()));
    }

    fn insert_selected_mention(
        &mut self,
        token_range: Range<usize>,
        insert_text: &str,
        path: Option<&str>,
    ) {
        let token_range = self.separate_completion_from_adjacent_element(token_range);
        // Remove the active token and insert the selected mention as an atomic element.
        let start_idx = token_range.start;
        self.draft.textarea.replace_range(token_range, "");
        self.draft.textarea.set_cursor(start_idx);
        let id = self.draft.textarea.insert_element(insert_text);
        let inserted_range = start_idx..start_idx.saturating_add(insert_text.len());

        if let (Some(path), Some((sigil, mention))) =
            (path, Self::mention_token_from_insert_text(insert_text))
        {
            self.draft.mention_bindings.insert(
                id,
                ComposerMentionBinding {
                    sigil,
                    mention,
                    path: path.to_string(),
                },
            );
        }

        self.advance_past_completion_separator();
        if let Some(sigil) = insert_text.chars().next()
            && matches!(sigil, '$' | '@')
        {
            self.dismiss_completed_prefixed_token(sigil, inserted_range, insert_text);
        }
    }

    /// Inserts a leading separator when a completion starts directly after an atomic element.
    ///
    /// The returned range is shifted to keep replacing the same editable token after insertion.
    fn separate_completion_from_adjacent_element(
        &mut self,
        mut token_range: Range<usize>,
    ) -> Range<usize> {
        let starts_after_element = self
            .draft
            .textarea
            .text_element_ranges()
            .any(|range| range.end == token_range.start);
        if starts_after_element {
            self.draft
                .textarea
                .replace_range(token_range.start..token_range.start, " ");
            token_range.start += 1;
            token_range.end += 1;
        }
        token_range
    }

    fn mention_token_from_insert_text(insert_text: &str) -> Option<(char, String)> {
        let sigil = insert_text.chars().next()?;
        if !matches!(sigil, '$' | '@') {
            return None;
        }
        let name = &insert_text[sigil.len_utf8()..];
        if name.is_empty() {
            return None;
        }
        if name
            .as_bytes()
            .iter()
            .all(|byte| is_mention_name_char(*byte))
        {
            Some((sigil, name.to_string()))
        } else {
            None
        }
    }

    fn current_mention_elements(&self) -> Vec<(u64, char, String)> {
        self.draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|snapshot| {
                Self::mention_token_from_insert_text(snapshot.text.as_str())
                    .map(|(sigil, mention)| (snapshot.id, sigil, mention))
            })
            .collect()
    }

    fn snapshot_mention_bindings(&self) -> Vec<MentionBinding> {
        let mut ordered = Vec::new();
        for (id, sigil, mention) in self.current_mention_elements() {
            if let Some(binding) = self.draft.mention_bindings.get(&id)
                && binding.sigil == sigil
                && binding.mention == mention
            {
                ordered.push(MentionBinding {
                    sigil: binding.sigil,
                    mention: binding.mention.clone(),
                    path: binding.path.clone(),
                });
            }
        }
        ordered
    }

    fn bind_mentions_from_snapshot(&mut self, mention_bindings: Vec<MentionBinding>) {
        self.draft.mention_bindings.clear();
        if mention_bindings.is_empty() {
            return;
        }

        let text = self.draft.textarea.text().to_string();
        let mut scan_from = 0usize;
        for binding in mention_bindings {
            let token = format!("{}{}", binding.sigil, binding.mention);
            let Some(range) =
                find_next_mention_token_range(text.as_str(), token.as_str(), scan_from)
            else {
                continue;
            };

            let id = if let Some(id) = self.draft.textarea.add_element_range(range.clone()) {
                Some(id)
            } else {
                self.draft
                    .textarea
                    .element_id_for_exact_range(range.clone())
            };

            if let Some(id) = id {
                self.draft.mention_bindings.insert(
                    id,
                    ComposerMentionBinding {
                        sigil: binding.sigil,
                        mention: binding.mention,
                        path: binding.path,
                    },
                );
                scan_from = range.end;
            }
        }
    }

    fn plugin_at_mention_highlights(&self) -> Vec<(Range<usize>, Style)> {
        self.draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|snapshot| {
                let binding = self.draft.mention_bindings.get(&snapshot.id)?;
                if !binding.path.starts_with("plugin://") || !snapshot.text.starts_with('@') {
                    return None;
                }
                Some((
                    snapshot.range,
                    Style::default().fg(Color::Rgb(160, 107, 234)),
                ))
            })
            .collect()
    }

    /// Prepare text for submission/queuing. Returns None if submission should be suppressed.
    /// On success, clears pending paste payloads because placeholders have been expanded.
    ///
    /// When `record_history` is true, the final submission is stored for ↑/↓ recall.
    fn prepare_submission_text(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        self.prepare_submission_text_with_options(
            record_history,
            SlashValidation::Immediate,
            PendingPasteHandling::Expand,
        )
    }

    fn prepare_submission_text_with_options(
        &mut self,
        record_history: bool,
        slash_validation: SlashValidation,
        pending_paste_handling: PendingPasteHandling,
    ) -> Option<(String, Vec<TextElement>)> {
        let mut text = self.current_text();
        let original_input = text.clone();
        let original_text_elements = self.current_text_elements();
        let original_mention_bindings = self.snapshot_mention_bindings();
        let original_local_image_paths = self.attachments.local_image_paths();
        let original_pending_pastes = self.draft.pending_pastes.clone();
        let mut text_elements = original_text_elements.clone();
        let input_starts_with_space = original_input.starts_with(' ');
        self.draft.recent_submission_mention_bindings.clear();
        self.draft.textarea.set_text_clearing_elements("");
        self.draft.is_bash_mode = false;

        if pending_paste_handling == PendingPasteHandling::Expand
            && !self.draft.pending_pastes.is_empty()
        {
            // Expand placeholders so element byte ranges stay aligned.
            let (expanded, expanded_elements) =
                Self::expand_pending_pastes(&text, text_elements, &self.draft.pending_pastes);
            text = expanded;
            text_elements = expanded_elements;
        }

        let expanded_input = text.clone();

        // If there is neither text nor attachments, suppress submission entirely.
        text = text.trim().to_string();
        text_elements = Self::trim_text_elements(&expanded_input, &text, text_elements);

        if slash_validation == SlashValidation::Immediate
            && let SubmissionValidation::UnknownCommand(name) = self
                .slash_input()
                .validate_submission(&text, input_starts_with_space)
        {
            let message = format!(
                r#"Unrecognized command '/{name}'. Type "/" for a list of supported commands."#
            );
            self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::new_info_event(message, /*hint*/ None),
            )));
            self.set_text_content_with_mention_bindings(
                original_input.clone(),
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.draft
                .pending_pastes
                .clone_from(&original_pending_pastes);
            self.draft.textarea.set_cursor(original_input.len());
            return None;
        }

        let actual_chars = text.chars().count();
        if actual_chars > MAX_USER_INPUT_TEXT_CHARS {
            let message = user_input_too_large_message(actual_chars);
            self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::new_error_event(message),
            )));
            self.set_text_content_with_mention_bindings(
                original_input.clone(),
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.draft
                .pending_pastes
                .clone_from(&original_pending_pastes);
            self.draft.textarea.set_cursor(original_input.len());
            return None;
        }
        self.attachments
            .prune_local_images_for_submission(&text, &text_elements);
        if text.is_empty() && self.attachments.is_empty() {
            return None;
        }
        self.draft.recent_submission_mention_bindings = original_mention_bindings.clone();
        if record_history && (!text.is_empty() || !self.attachments.is_empty()) {
            self.history.record_local_submission(HistoryEntry {
                text: text.clone(),
                text_elements: text_elements.clone(),
                local_image_paths: self.attachments.local_image_paths(),
                remote_image_urls: self.attachments.remote_image_urls(),
                mention_bindings: original_mention_bindings,
                pending_pastes: if pending_paste_handling == PendingPasteHandling::Preserve {
                    original_pending_pastes.clone()
                } else {
                    Vec::new()
                },
            });
        }
        self.draft.pending_pastes.clear();
        Some((text, text_elements))
    }

    /// Common logic for handling message submission/queuing.
    /// Returns the appropriate InputResult based on `should_queue`.
    fn handle_submission(&mut self, should_queue: bool) -> (InputResult, bool) {
        let result = self.handle_submission_with_time(should_queue, Instant::now());
        self.reset_vim_mode_after_successful_dispatch(&result.0);
        result
    }

    fn reset_vim_mode_after_successful_dispatch(&mut self, result: &InputResult) {
        if matches!(
            result,
            InputResult::Submitted { .. }
                | InputResult::Queued { .. }
                | InputResult::Command(_)
                | InputResult::ServiceTierCommand(_)
                | InputResult::SessionExtensionCommand(_, _)
                | InputResult::CommandWithArgs(_, _, _)
        ) {
            self.draft.textarea.enter_vim_normal_mode();
        }
    }

    fn handle_submission_with_time(
        &mut self,
        should_queue: bool,
        now: Instant,
    ) -> (InputResult, bool) {
        // Preserve newlines that are part of a paste before applying parent-owned submission
        // policy. Queued startup input still flushes the burst into its queued message below.
        let in_slash_context = self.slash_commands_enabled()
            && !self.draft.is_bash_mode
            && (matches!(self.popups.active, ActivePopup::Command(_))
                || self
                    .draft
                    .textarea
                    .text()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .starts_with('/'));
        if !should_queue
            && !self.draft.disable_paste_burst
            && self.draft.paste_burst.is_active()
            && !in_slash_context
            && self.draft.paste_burst.append_newline_if_active(now)
        {
            return (InputResult::None, true);
        }
        if !should_queue
            && !in_slash_context
            && !self.draft.disable_paste_burst
            && self
                .draft
                .paste_burst
                .newline_should_insert_instead_of_submit(now)
        {
            self.draft.textarea.insert_str("\n");
            self.draft.paste_burst.extend_window(now);
            return (InputResult::None, true);
        }

        if let Some(result) = self.handle_parent_owned_submission() {
            return result;
        }
        if should_queue {
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
            let raw_text = self.draft.textarea.text();
            let defer_slash_validation = self.slash_input().should_parse_on_dequeue(raw_text);
            let preserve_pending_pastes = defer_slash_validation
                && !self.draft.pending_pastes.is_empty()
                && parse_slash_name(raw_text)
                    .is_some_and(|(name, _, _)| name == SlashCommand::Goal.command());
            let pending_pastes = if preserve_pending_pastes {
                self.draft.pending_pastes.clone()
            } else {
                Vec::new()
            };
            if let Some((text, text_elements)) = self.prepare_submission_text_with_options(
                /*record_history*/ true,
                if defer_slash_validation {
                    SlashValidation::Deferred
                } else {
                    SlashValidation::Immediate
                },
                if preserve_pending_pastes {
                    PendingPasteHandling::Preserve
                } else {
                    PendingPasteHandling::Expand
                },
            ) {
                let action = slash_input::queued_input_action(&text, defer_slash_validation);
                return (
                    InputResult::Queued {
                        text,
                        text_elements,
                        action,
                        pending_pastes,
                    },
                    true,
                );
            }
            return (InputResult::None, true);
        }

        // If the first line is a bare built-in slash command (no args),
        // dispatch it even when the slash popup isn't visible. This preserves
        // the workflow: type a prefix ("/di"), press Tab to complete to
        // "/diff ", then press Enter/Ctrl+Shift+Q to run it. Tab moves the cursor beyond
        // the '/name' token and our caret-based heuristic hides the popup,
        // but Enter/Ctrl+Shift+Q should still dispatch the command rather than submit
        // literal text.
        if let Some(result) = self.try_dispatch_bare_slash_command() {
            return (result, true);
        }

        let original_input = self.current_text();
        let original_text_elements = self.current_text_elements();
        let original_mention_bindings = self.snapshot_mention_bindings();
        let original_local_image_paths = self.attachments.local_image_paths();
        let original_pending_pastes = self.draft.pending_pastes.clone();
        if let Some(result) = self.try_dispatch_slash_command_with_args() {
            return (result, true);
        }

        if let Some((text, text_elements)) =
            self.prepare_submission_text(/*record_history*/ true)
        {
            if should_queue {
                (
                    InputResult::Queued {
                        text,
                        text_elements,
                        action: QueuedInputAction::Plain,
                        pending_pastes: Vec::new(),
                    },
                    true,
                )
            } else {
                // Do not clear local attachments here; ChatWidget drains them via
                // take_recent_submission_images().
                (
                    InputResult::Submitted {
                        text,
                        text_elements,
                    },
                    true,
                )
            }
        } else {
            // Restore text if submission was suppressed.
            self.set_text_content_with_mention_bindings(
                original_input,
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.draft.pending_pastes = original_pending_pastes;
            (InputResult::None, true)
        }
    }

    fn handle_parent_owned_submission(&mut self) -> Option<(InputResult, bool)> {
        if !self.blocks_direct_input {
            return None;
        }

        let text = self.current_text();
        let allowed_slash_command = parse_slash_name(&text).is_some_and(|(name, args, _)| {
            matches!(
                self.slash_input().command(name),
                Some(SlashCommandItem::Builtin(command))
                    if name == command.command()
                        && parent_owned_command_is_allowed(command, args)
            )
        });
        if text.starts_with('/') && allowed_slash_command {
            return None;
        }

        Some((InputResult::ParentOwnedInputBlocked, true))
    }

    /// Check if the first line is a bare slash command (no args) and dispatch it.
    /// Returns Some(InputResult) if a command was dispatched, None otherwise.
    fn try_dispatch_bare_slash_command(&mut self) -> Option<InputResult> {
        let command = self
            .slash_input()
            .bare_command(self.draft.textarea.text())?;
        if self.reject_slash_command_if_unavailable(&command) {
            self.stage_slash_command_history(&command);
            self.record_pending_slash_command_history();
            return Some(InputResult::None);
        }
        self.stage_slash_command_history(&command);
        self.draft.textarea.set_text_clearing_elements("");
        self.draft.is_bash_mode = false;
        Some(match command {
            SlashCommandItem::Builtin(cmd) => InputResult::Command(cmd),
            SlashCommandItem::ServiceTier(command) => InputResult::ServiceTierCommand(command),
            SlashCommandItem::SessionExtension(command) => {
                InputResult::SessionExtensionCommand(command, String::new())
            }
        })
    }

    /// Check if the input is a slash command with args (e.g., /review args) and dispatch it.
    /// Returns Some(InputResult) if a command was dispatched, None otherwise.
    fn try_dispatch_slash_command_with_args(&mut self) -> Option<InputResult> {
        let text = self.draft.textarea.text().to_string();
        let inline_command = self.slash_input().inline_command(&text)?;
        let command = inline_command.command;
        if self.reject_slash_command_if_unavailable(&command) {
            self.stage_slash_command_history(&command);
            self.record_pending_slash_command_history();
            return Some(InputResult::None);
        }

        self.stage_slash_command_history(&command);

        let mut args_elements = slash_input::args_elements(
            inline_command.rest,
            inline_command.rest_offset,
            &self.draft.textarea.text_elements(),
        );
        let trimmed_rest = inline_command.rest.trim();
        args_elements = Self::trim_text_elements(inline_command.rest, trimmed_rest, args_elements);
        match command {
            SlashCommandItem::Builtin(cmd) => Some(InputResult::CommandWithArgs(
                cmd,
                trimmed_rest.to_string(),
                args_elements,
            )),
            SlashCommandItem::SessionExtension(command) => Some(
                InputResult::SessionExtensionCommand(command, trimmed_rest.to_string()),
            ),
            SlashCommandItem::ServiceTier(_) => None,
        }
    }

    /// Expand pending placeholders and extract normalized inline-command args.
    ///
    /// Inline-arg commands are initially dispatched using the raw draft so command rejection does
    /// not consume user input. Once a command needs its args, this helper performs the usual
    /// submission preparation (paste expansion, element trimming) and rebases element ranges from
    /// full-text offsets to command-arg offsets.
    ///
    /// Callers that already staged slash-command history should normally pass `false` for
    /// `record_history`; otherwise a command such as `/plan investigate` would be entered into
    /// local recall through both the slash-command path and the message-submission path.
    pub fn prepare_inline_args_submission(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        let (prepared_text, prepared_elements) = self.prepare_submission_text(record_history)?;
        let (prepared_rest, prepared_rest_offset) = slash_input::prepared_args(&prepared_text)?;
        let mut args_elements =
            slash_input::args_elements(prepared_rest, prepared_rest_offset, &prepared_elements);
        let trimmed_rest = prepared_rest.trim();
        args_elements = Self::trim_text_elements(prepared_rest, trimmed_rest, args_elements);
        Some((trimmed_rest.to_string(), args_elements))
    }

    fn reject_slash_command_if_unavailable(&self, command: &SlashCommandItem) -> bool {
        if !self.is_task_running || command.available_during_task() {
            return false;
        }
        let message = format!(
            "'/{}' is disabled while a task is in progress.",
            command.command()
        );
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::new_error_event(message),
        )));
        true
    }

    /// Stage the current slash-command text for later local recall.
    ///
    /// Staging snapshots the rich composer state before the textarea is cleared. `ChatWidget`
    /// commits the staged entry after dispatch so command recall follows the submitted text, not
    /// the command outcome.
    fn stage_slash_command_history(&mut self, command: &SlashCommandItem) {
        if matches!(command, SlashCommandItem::Builtin(SlashCommand::Clear)) {
            return;
        }
        self.stage_slash_command_history_text(self.draft.textarea.text().trim().to_string());
    }

    /// Stage a popup-selected command using its canonical command text.
    ///
    /// Popup filtering text can be partial, so recording the selected command avoids recalling
    /// `/di` after the user actually accepted `/diff`.
    fn stage_selected_slash_command_history(&mut self, command: &CommandItem) {
        if matches!(command, CommandItem::Builtin(SlashCommand::Clear)) {
            return;
        }
        self.stage_slash_command_history_text(format!("/{}", command.command()));
    }

    /// Store the provided command text and the current composer adornments in the pending slot.
    ///
    /// The pending entry intentionally has the same shape as other local history entries so recall
    /// can rehydrate attachments, mention bindings, and pending paste placeholders if command
    /// workflows start carrying those through in the future.
    fn stage_slash_command_history_text(&mut self, text: String) {
        self.pending_slash_command_history = Some(HistoryEntry {
            text,
            text_elements: self.draft.textarea.text_elements(),
            local_image_paths: self.attachments.local_image_paths(),
            remote_image_urls: self.attachments.remote_image_urls(),
            mention_bindings: self.snapshot_mention_bindings(),
            pending_pastes: self.draft.pending_pastes.clone(),
        });
    }

    fn handle_remote_image_selection_key(
        &mut self,
        key_event: &KeyEvent,
    ) -> Option<(InputResult, bool)> {
        self.attachments
            .handle_remote_image_selection_key(key_event, &mut self.draft.textarea)
    }

    /// Handle key event when no popup is visible.
    fn handle_key_event_without_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if let Some((result, redraw)) = self.handle_remote_image_selection_key(&key_event) {
            return (result, redraw);
        }
        if self.attachments.selected_remote_image_index.is_some() {
            self.attachments.clear_remote_image_selection();
        }
        if self.handle_shortcut_overlay_key(&key_event) {
            return (InputResult::None, true);
        }
        if self.draft.is_bash_mode && key_event.code == KeyCode::Esc {
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
            if self.draft.textarea.is_empty() {
                self.draft.is_bash_mode = false;
                return (InputResult::None, true);
            }
        }
        if self.should_handle_vim_insert_escape(key_event) {
            return self.handle_input_basic(key_event);
        }
        if self.draft.textarea.is_vim_normal_mode() && self.draft.textarea.is_vim_operator_pending()
        {
            return self.handle_input_basic(key_event);
        }
        if self.draft.textarea.is_vim_normal_mode()
            && self.is_empty()
            && matches!(
                key_event,
                KeyEvent {
                    code: KeyCode::Char('/'),
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Press | KeyEventKind::Repeat,
                    ..
                }
            )
        {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
            self.draft.textarea.set_text_clearing_elements("/");
            self.draft
                .textarea
                .set_cursor(self.draft.textarea.text().len());
            self.draft.textarea.enter_vim_insert_mode();
            return (InputResult::None, true);
        }
        if self.draft.textarea.is_vim_normal_mode()
            && self.is_empty()
            && matches!(
                key_event,
                KeyEvent {
                    code: KeyCode::Char('!'),
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Press | KeyEventKind::Repeat,
                    ..
                }
            )
        {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
            self.draft.is_bash_mode = true;
            self.draft.textarea.enter_vim_insert_mode();
            return (InputResult::None, true);
        }
        if key_event.code == KeyCode::Esc {
            if self.is_empty() {
                let next_mode = esc_hint_mode(self.footer.mode, self.is_task_running);
                if next_mode != self.footer.mode {
                    self.footer.mode = next_mode;
                    return (InputResult::None, true);
                }
            }
        } else {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }
        if self.queue_keys.is_pressed(key_event)
            && (self.is_task_running || self.queue_submissions || !self.is_bang_shell_command())
        {
            return self.handle_submission(self.is_task_running || self.queue_submissions);
        }

        if self.submit_keys.is_pressed(key_event) {
            return self.handle_submission(self.queue_submissions);
        }

        if let KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: crossterm::event::KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            ..
        } = key_event
            && self.is_empty()
        {
            return (InputResult::None, false);
        }

        let (history_up_pressed, history_down_pressed) = if self.draft.textarea.is_vim_normal_mode()
        {
            if self.draft.textarea.is_vim_operator_pending() {
                (false, false)
            } else {
                (
                    self.vim_normal_keymap.move_up.is_pressed(key_event),
                    self.vim_normal_keymap.move_down.is_pressed(key_event),
                )
            }
        } else {
            (
                self.editor_keymap.move_up.is_pressed(key_event),
                self.editor_keymap.move_down.is_pressed(key_event),
            )
        };
        if history_up_pressed || history_down_pressed {
            if self
                .history
                .should_handle_navigation(&self.current_text(), self.history_navigation_cursor())
            {
                let replace_entry = if history_up_pressed {
                    self.history.navigate_up(&self.app_event_tx)
                } else {
                    self.history.navigate_down(&self.app_event_tx)
                };
                if let Some(entry) = replace_entry {
                    self.apply_history_entry(entry.clone());
                    return (InputResult::HistoryRecalled(entry), true);
                }
            }
            return self.handle_input_basic(key_event);
        }

        self.handle_input_basic(key_event)
    }

    fn is_bang_shell_command(&self) -> bool {
        self.current_text().trim_start().starts_with('!')
    }

    fn shell_mode_footer_line(&self) -> Option<Line<'static>> {
        self.is_bang_shell_command()
            .then_some(())
            .map(|_| Line::from(vec![Span::from("Shell mode").light_red()]))
    }

    /// Applies any due `PasteBurst` flush at time `now`.
    ///
    /// Converts [`PasteBurst::flush_if_due`] results into concrete textarea mutations.
    ///
    /// Callers:
    ///
    /// - UI ticks via [`ChatComposer::flush_paste_burst_if_due`], so held first-chars can render.
    /// - Input handling via [`ChatComposer::handle_input_basic`], so a due burst does not lag.
    fn handle_paste_burst_flush(&mut self, now: Instant) -> bool {
        match self.draft.paste_burst.flush_if_due(now) {
            FlushResult::Paste(pasted) => {
                self.handle_paste(pasted);
                true
            }
            FlushResult::Typed(ch) => {
                self.insert_str(ch.to_string().as_str());
                true
            }
            FlushResult::None => false,
        }
    }

    /// Handles keys that mutate the textarea, including paste-burst detection.
    ///
    /// Acts as the lowest-level keypath for keys that mutate the textarea. It is also where plain
    /// character streams are converted into explicit paste operations on terminals that do not
    /// reliably provide bracketed paste.
    ///
    /// Ordering is important:
    ///
    /// - Always flush any *due* paste burst first so buffered text does not lag behind unrelated
    ///   edits.
    /// - Then handle the incoming key, intercepting only "plain" (no Ctrl/Alt) char input.
    /// - For non-plain keys, flush via `flush_before_modified_input()` before applying the key;
    ///   otherwise `clear_window_after_non_char()` can leave buffered text waiting without a
    ///   timestamp to time out against.
    fn handle_input_basic(&mut self, input: KeyEvent) -> (InputResult, bool) {
        // Ignore key releases here to avoid treating them as additional input
        // (e.g., appending the same character twice via paste-burst logic).
        if !matches!(input.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return (InputResult::None, false);
        }

        self.handle_input_basic_with_time(input, Instant::now())
    }

    fn handle_input_basic_with_time(
        &mut self,
        input: KeyEvent,
        now: Instant,
    ) -> (InputResult, bool) {
        // If we have a buffered non-bracketed paste burst and enough time has
        // elapsed since the last char, flush it before handling a new input.
        self.handle_paste_burst_flush(now);

        if !matches!(input.code, KeyCode::Esc) {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }

        // If we're capturing a burst and receive Enter, accumulate it instead of inserting.
        if matches!(input.code, KeyCode::Enter)
            && !self.draft.disable_paste_burst
            && self.draft.paste_burst.is_active()
            && self.draft.paste_burst.append_newline_if_active(now)
        {
            return (InputResult::None, true);
        }

        // Intercept plain Char inputs to optionally accumulate into a burst buffer.
        //
        // This is intentionally limited to "plain" (no Ctrl/Alt) chars so shortcuts keep their
        // normal semantics, and so we can aggressively flush/clear any burst state when non-char
        // keys are pressed.
        if let KeyEvent {
            code: KeyCode::Char(ch),
            modifiers,
            ..
        } = input
        {
            let has_ctrl_or_alt = has_ctrl_or_alt(modifiers);
            if !has_ctrl_or_alt
                && !self.draft.disable_paste_burst
                && self.draft.textarea.allows_paste_burst()
            {
                // Non-ASCII characters (e.g., from IMEs) can arrive in quick bursts, so avoid
                // holding the first char while still allowing burst detection for paste input.
                if !ch.is_ascii() {
                    return self.handle_non_ascii_char(input, now);
                }

                match self.draft.paste_burst.on_plain_char(ch, now) {
                    CharDecision::BufferAppend => {
                        self.draft.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    CharDecision::BeginBuffer { retro_chars } => {
                        let cur = self.draft.textarea.cursor();
                        let txt = self.draft.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) = self.draft.paste_burst.decide_begin_buffer(
                            now,
                            before,
                            retro_chars as usize,
                        ) {
                            if !grab.grabbed.is_empty() {
                                self.draft
                                    .textarea
                                    .replace_range(grab.start_byte..safe_cur, "");
                            }
                            self.draft.paste_burst.append_char_to_buffer(ch, now);
                            return (InputResult::None, true);
                        }
                        // If decide_begin_buffer opted not to start buffering,
                        // fall through to normal insertion below.
                    }
                    CharDecision::BeginBufferFromPending => {
                        // First char was held; now append the current one.
                        self.draft.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    CharDecision::RetainFirstChar => {
                        // Keep the first fast char pending momentarily.
                        return (InputResult::None, true);
                    }
                }
            }
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
        }

        // Flush any buffered burst before applying a non-char input (arrow keys, etc).
        //
        // `clear_window_after_non_char()` clears `last_plain_char_time`. If we cleared that while
        // `PasteBurst.buffer` is non-empty, `flush_if_due()` would no longer have a timestamp to
        // time out against, and the buffered paste could remain stuck until another plain char
        // arrives.
        if !matches!(input.code, KeyCode::Char(_) | KeyCode::Enter)
            && let Some(pasted) = self.draft.paste_burst.flush_before_modified_input()
        {
            self.handle_paste(pasted);
        }
        // For non-char inputs (or after flushing), handle normally.
        // Track element removals so we can drop any corresponding placeholders without scanning
        // the full text. (Placeholders are atomic elements; when deleted, the element disappears.)
        let elements_before = if self.draft.pending_pastes.is_empty() && self.attachments.is_empty()
        {
            None
        } else {
            Some(self.draft.textarea.element_payloads())
        };

        if self.draft.is_bash_mode
            && matches!(input.code, KeyCode::Backspace)
            && self.draft.textarea.cursor() == 0
        {
            self.draft.is_bash_mode = false;
            return (InputResult::None, true);
        }

        self.draft.textarea.input(input);
        self.sync_bash_mode_from_text();

        if let Some(elements_before) = elements_before {
            self.reconcile_deleted_elements(elements_before);
        }

        // Update paste-burst heuristic for plain Char (no Ctrl/Alt) events.
        let crossterm::event::KeyEvent {
            code, modifiers, ..
        } = input;
        match code {
            KeyCode::Char(_) => {
                let has_ctrl_or_alt = has_ctrl_or_alt(modifiers);
                if has_ctrl_or_alt {
                    self.draft.paste_burst.clear_window_after_non_char();
                }
            }
            KeyCode::Enter => {
                // Keep burst window alive (supports blank lines in paste).
            }
            _ => {
                // Other keys: clear burst window (buffer should have been flushed above if needed).
                self.draft.paste_burst.clear_window_after_non_char();
            }
        }

        (InputResult::None, true)
    }

    fn sync_bash_mode_from_text(&mut self) {
        if !self.draft.is_bash_mode && self.draft.textarea.text().starts_with('!') {
            self.draft.textarea.replace_range(0..1, "");
            self.draft.is_bash_mode = true;
        }
    }

    fn reconcile_deleted_elements(&mut self, elements_before: Vec<String>) {
        let elements_after: HashSet<String> =
            self.draft.textarea.element_payloads().into_iter().collect();

        let removed_payloads = elements_before
            .into_iter()
            .filter(|payload| !elements_after.contains(payload))
            .collect::<Vec<_>>();
        for removed in &removed_payloads {
            self.draft.pending_pastes.retain(|(ph, _)| ph != removed);
        }
        self.attachments
            .remove_deleted_local_placeholders(&removed_payloads, &mut self.draft.textarea);
    }

    /// Handle the dedicated shortcut-overlay toggle key(s).
    ///
    /// This only toggles when the composer is empty and no paste burst is in
    /// progress, so typing/pasting `?` still inserts text instead of opening
    /// help. The bound key list intentionally supports terminal-variant
    /// modifier reporting (for example `?` vs `shift-?`).
    fn handle_shortcut_overlay_key(&mut self, key_event: &KeyEvent) -> bool {
        if key_event.kind != KeyEventKind::Press {
            return false;
        }

        let toggles = self.toggle_shortcuts_keys.is_pressed(*key_event)
            && self.is_empty()
            && !self.is_in_paste_burst();

        if !toggles {
            return false;
        }

        let next = toggle_shortcut_mode(
            self.footer.mode,
            self.quit_shortcut_hint_visible(),
            self.is_empty(),
        );
        let changed = next != self.footer.mode;
        self.footer.mode = next;
        changed
    }

    fn footer_props(&self) -> FooterProps {
        let mode = self.footer_mode();
        let is_wsl = {
            #[cfg(target_os = "linux")]
            {
                mode == FooterMode::ShortcutOverlay && crate::clipboard_paste::is_probably_wsl()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        };

        FooterProps {
            mode,
            esc_backtrack_hint: self.footer.esc_backtrack_hint,
            use_shift_enter_hint: self.footer.use_shift_enter_hint,
            is_task_running: self.is_task_running,
            queue_submissions: self.queue_submissions,
            quit_shortcut_key: self.footer.quit_shortcut_key,
            collaboration_modes_enabled: self.collaboration_modes_enabled,
            is_wsl,
            status_line_value: self.footer.status_line_value.clone(),
            status_line_enabled: self.footer.status_line_enabled,
            key_hints: FooterKeyHints {
                toggle_shortcuts: self.footer.toggle_shortcuts_key,
                queue: self.footer.queue_key,
                insert_newline: self.footer.insert_newline_key,
                external_editor: self.footer.external_editor_key,
                edit_previous: Some(key_hint::plain(KeyCode::Esc)),
                show_transcript: self.footer.show_transcript_key,
                history_search: self.footer.history_search_key,
                reasoning_down: self.footer.reasoning_down_key,
                reasoning_up: self.footer.reasoning_up_key,
            },
            active_agent_label: self.footer.active_agent_label.clone(),
        }
    }

    /// Resolve the effective footer mode via a small priority waterfall.
    ///
    /// The base mode is derived solely from whether the composer is empty:
    /// `ComposerEmpty` iff empty, otherwise `ComposerHasDraft`. Transient
    /// modes (Esc hint, overlay, quit reminder) can override that base when
    /// their conditions are active.
    fn footer_mode(&self) -> FooterMode {
        if self.history_search.is_some() {
            return FooterMode::HistorySearch;
        }

        let base_mode = if self.is_empty() {
            FooterMode::ComposerEmpty
        } else {
            FooterMode::ComposerHasDraft
        };

        match self.footer.mode {
            FooterMode::HistorySearch => FooterMode::HistorySearch,
            FooterMode::EscHint => FooterMode::EscHint,
            FooterMode::ShortcutOverlay => FooterMode::ShortcutOverlay,
            FooterMode::QuitShortcutReminder if self.quit_shortcut_hint_visible() => {
                FooterMode::QuitShortcutReminder
            }
            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
                if self.quit_shortcut_hint_visible() =>
            {
                FooterMode::QuitShortcutReminder
            }
            FooterMode::QuitShortcutReminder => base_mode,
            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => base_mode,
        }
    }

    fn custom_footer_height(&self) -> Option<u16> {
        if self.footer.flash_visible() {
            return Some(1);
        }
        self.footer
            .hint_override
            .as_ref()
            .map(|items| if items.is_empty() { 0 } else { 1 })
    }

    pub fn sync_popups(&mut self) {
        self.sync_slash_command_elements();
        if self.history_search.is_some() {
            if self.popups.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.popups.current_file_query = None;
            }
            self.popups.active = ActivePopup::None;
            self.popups.dismissed_file_token = None;
            self.popups.dismissed_mention_token = None;
            return;
        }
        if !self.popups_enabled() {
            self.popups.active = ActivePopup::None;
            return;
        }
        let mut mentions_v2_token = self.current_mentions_v2_token_range();
        let mut file_token = if self.mentions_v2_enabled {
            None
        } else {
            self.current_editable_at_token_range_with_options(/*allow_empty*/ false)
        };
        let browsing_history = self
            .history
            .should_handle_navigation(&self.current_text(), self.history_navigation_cursor());
        // When browsing input history (shell-style Up/Down recall), skip all popup
        // synchronization so nothing steals focus from continued history navigation.
        if browsing_history {
            if self.popups.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.popups.current_file_query = None;
            }
            self.popups.active = ActivePopup::None;
            return;
        }
        let mut mention_target = self.current_mention_target();
        let at_token_start = mentions_v2_token
            .as_ref()
            .or(file_token.as_ref())
            .map(|(range, _)| range.start);
        let mention_token_start = mention_target.as_ref().map(|target| target.range.start);
        if let (Some(at_token_start), Some(mention_token_start)) =
            (at_token_start, mention_token_start)
        {
            if at_token_start > mention_token_start {
                mention_target = None;
            } else if mention_token_start > at_token_start {
                mentions_v2_token = None;
                file_token = None;
            }
        }

        let allow_command_popup = self.slash_commands_enabled()
            && !self.draft.is_bash_mode
            && file_token.is_none()
            && mentions_v2_token.is_none()
            && mention_target.is_none();
        self.sync_command_popup(allow_command_popup);

        if matches!(self.popups.active, ActivePopup::Command(_)) {
            if self.popups.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.popups.current_file_query = None;
            }
            self.popups.dismissed_file_token = None;
            self.popups.dismissed_mention_token = None;
            return;
        }

        if let Some((range, token)) = mentions_v2_token {
            self.sync_mentions_v2_popup(range, token);
            return;
        }

        if let Some(target) = mention_target {
            if self.popups.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.popups.current_file_query = None;
            }
            self.sync_mention_popup(target);
            return;
        }
        self.popups.dismissed_mention_token = None;

        if let Some((range, token)) = file_token {
            self.sync_file_search_popup(range, token);
            return;
        }

        if self.popups.current_file_query.is_some() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
            self.popups.current_file_query = None;
        }
        self.popups.dismissed_file_token = None;
        if matches!(
            self.popups.active,
            ActivePopup::File(_) | ActivePopup::Skill(_) | ActivePopup::MentionV2(_)
        ) {
            self.popups.active = ActivePopup::None;
        }
    }

    /// Synchronize `self.command_popup` with the current text in the
    /// textarea. This must be called after every modification that can change
    /// the text so the popup is shown/updated/hidden as appropriate.
    fn sync_command_popup(&mut self, allow: bool) {
        let text = self.draft.textarea.text();
        let first_line_end = text.find('\n').unwrap_or(text.len());
        let first_line = &text[..first_line_end];
        // Keep an explicitly dismissed popup closed until the command token changes.
        let command_token = slash_input::command_popup_filter_text(first_line, /*cursor*/ 0);
        if let Some(command_token) = command_token.as_deref()
            && self.popups.dismissed_command_token.as_deref() == Some(command_token)
        {
            return;
        }
        self.popups.dismissed_command_token = None;

        if !allow {
            if matches!(self.popups.active, ActivePopup::Command(_)) {
                self.popups.active = ActivePopup::None;
            }
            return;
        }
        // Determine whether the caret is inside the initial '/name' token on the first line.
        let cursor = self.draft.textarea.cursor();
        let caret_on_first_line = cursor <= first_line_end;

        let is_editing_slash_command_name = caret_on_first_line
            && self
                .slash_input()
                .is_editing_command_name(first_line, cursor);
        let command_filter_text = caret_on_first_line
            .then(|| slash_input::command_popup_filter_text(first_line, cursor))
            .flatten();

        // If the cursor is currently positioned within an `@token`, prefer the
        // file-search popup over the slash popup so users can insert a file path
        // as an argument to the command (e.g., "/review @docs/...").
        if Self::current_at_token(&self.draft.textarea).is_some() {
            if matches!(self.popups.active, ActivePopup::Command(_)) {
                self.popups.active = ActivePopup::None;
            }
            return;
        }
        match &mut self.popups.active {
            ActivePopup::Command(popup) => {
                if is_editing_slash_command_name {
                    if let Some(command_filter_text) = command_filter_text.as_deref() {
                        popup.on_composer_text_change(command_filter_text.to_string());
                    }
                } else {
                    self.popups.active = ActivePopup::None;
                }
            }
            _ => {
                if is_editing_slash_command_name
                    && let Some(command_filter_text) = command_filter_text.as_deref()
                {
                    let command_popup = self.slash_input().command_popup(command_filter_text);
                    self.popups.active = ActivePopup::Command(command_popup);
                }
            }
        }
    }

    /// Synchronize the legacy file-search popup with the current `@` token.
    fn sync_file_search_popup(&mut self, range: Range<usize>, query: String) {
        if self
            .popups
            .dismissed_file_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        if query.is_empty() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
        } else {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(query.clone()));
        }

        match &mut self.popups.active {
            ActivePopup::File(popup) => {
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
            }
            _ => {
                let mut popup = FileSearchPopup::new();
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
                self.popups.active = ActivePopup::File(popup);
            }
        }

        if query.is_empty() {
            self.popups.current_file_query = None;
        } else {
            self.popups.current_file_query = Some(query);
        }
        self.popups.dismissed_file_token = None;
    }

    fn sync_mention_popup(&mut self, target: MentionCompletionTarget) {
        let MentionCompletionTarget {
            range,
            query,
            prebuilt_mentions,
        } = target;
        if self
            .popups
            .dismissed_mention_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        let mentions = prebuilt_mentions.unwrap_or_else(|| self.mention_items());
        if mentions.is_empty() {
            self.popups.active = ActivePopup::None;
            return;
        }

        match &mut self.popups.active {
            ActivePopup::Skill(popup) => {
                popup.set_query(&query);
                popup.set_mentions(mentions);
            }
            _ => {
                let mut popup = SkillPopup::new(mentions);
                popup.set_query(&query);
                self.popups.active = ActivePopup::Skill(popup);
            }
        }
    }

    fn sync_mentions_v2_popup(&mut self, range: Range<usize>, query: String) {
        if self
            .popups
            .dismissed_mention_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        if query.is_empty() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
            self.popups.current_file_query = None;
        } else {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(query.clone()));
            self.popups.current_file_query = Some(query.clone());
        }

        let candidates = super::mentions_v2::build_search_catalog(
            self.skills.as_deref(),
            self.plugins.as_deref(),
        );

        match &mut self.popups.active {
            ActivePopup::MentionV2(popup) => {
                popup.set_query(&query);
                popup.set_candidates(candidates);
            }
            _ => {
                let mut popup = MentionV2Popup::new(candidates);
                popup.set_query(&query);
                self.popups.active = ActivePopup::MentionV2(popup);
            }
        }

        self.popups.dismissed_mention_token = None;
    }

    fn mention_items(&self) -> Vec<MentionItem> {
        let mut mentions = Vec::new();
        if let Some(skills) = self.skills.as_ref() {
            for skill in skills {
                let display_name = skill_display_name(skill);
                let description = skill_description(skill);
                let skill_name = skill.name.clone();
                let search_terms = if display_name == skill.name {
                    vec![skill_name.clone()]
                } else {
                    vec![skill_name.clone(), display_name.clone()]
                };
                mentions.push(MentionItem {
                    display_name,
                    description,
                    insert_text: format!("${skill_name}"),
                    search_terms,
                    path: Some(skill.path.to_string_lossy().into_owned()),
                    category_tag: Some("[Skill]".to_string()),
                    sort_rank: 1,
                });
            }
        }

        if let Some(plugins) = self.plugins.as_ref() {
            for plugin in plugins {
                let (plugin_name, marketplace_name) = plugin
                    .config_name
                    .split_once('@')
                    .unwrap_or((plugin.config_name.as_str(), ""));
                let mut capability_labels = Vec::new();
                if plugin.has_skills {
                    capability_labels.push("skills".to_string());
                }
                if !plugin.mcp_server_names.is_empty() {
                    let mcp_server_count = plugin.mcp_server_names.len();
                    capability_labels.push(if mcp_server_count == 1 {
                        "1 MCP server".to_string()
                    } else {
                        format!("{mcp_server_count} MCP servers")
                    });
                }
                let description = plugin.description.clone().or_else(|| {
                    Some(if capability_labels.is_empty() {
                        "Plugin".to_string()
                    } else {
                        format!("Plugin · {}", capability_labels.join(" · "))
                    })
                });
                let mut search_terms = vec![plugin_name.to_string(), plugin.config_name.clone()];
                if plugin.display_name != plugin_name {
                    search_terms.push(plugin.display_name.clone());
                }
                if !marketplace_name.is_empty() {
                    search_terms.push(marketplace_name.to_string());
                }
                mentions.push(MentionItem {
                    display_name: plugin.display_name.clone(),
                    description,
                    insert_text: format!("${plugin_name}"),
                    search_terms,
                    path: Some(format!("plugin://{}", plugin.config_name)),
                    category_tag: Some("[Plugin]".to_string()),
                    sort_rank: 0,
                });
            }
        }

        mentions
    }

    fn set_has_focus(&mut self, has_focus: bool) {
        self.has_focus = has_focus;
    }

    #[allow(dead_code)]
    pub fn set_input_enabled(&mut self, enabled: bool, placeholder: Option<String>) {
        self.draft.input_enabled = enabled;
        self.draft.input_disabled_placeholder = if enabled { None } else { placeholder };

        // Avoid leaving interactive popups open while input is blocked.
        if !enabled && self.popups.active() {
            self.popups.active = ActivePopup::None;
        }
    }

    pub fn show_shutdown_in_progress(&mut self) {
        self.set_input_enabled(/*enabled*/ false, Some("Shutting down...".to_string()));
        self.footer.quit_shortcut_expires_at = None;
        self.footer.mode = FooterMode::ComposerEmpty;
        self.footer.hint_override = Some(Vec::new());
        self.footer.plan_mode_nudge_visible = false;
        self.footer.flash = None;
    }

    pub fn set_task_running(&mut self, running: bool) {
        self.is_task_running = running;
    }

    pub fn set_queue_submissions(&mut self, queue_submissions: bool) {
        self.queue_submissions = queue_submissions;
    }

    pub fn set_context_window(&mut self, percent: Option<i64>, used_tokens: Option<i64>) {
        if self.footer.context_window_percent == percent
            && self.footer.context_window_used_tokens == used_tokens
        {
            return;
        }
        self.footer.context_window_percent = percent;
        self.footer.context_window_used_tokens = used_tokens;
    }

    pub fn set_esc_backtrack_hint(&mut self, show: bool) {
        self.footer.esc_backtrack_hint = show;
        if show {
            self.footer.mode = esc_hint_mode(self.footer.mode, self.is_task_running);
        } else {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }
    }

    pub fn set_status_line(&mut self, status_line: Option<Line<'static>>) -> bool {
        if self.footer.status_line_value == status_line {
            return false;
        }
        self.footer.status_line_value = status_line;
        true
    }

    pub fn set_status_line_hyperlink(&mut self, url: Option<String>) -> bool {
        if self.footer.status_line_hyperlink_url == url {
            return false;
        }
        self.footer.status_line_hyperlink_url = url;
        true
    }

    pub fn set_status_line_enabled(&mut self, enabled: bool) -> bool {
        if self.footer.status_line_enabled == enabled {
            return false;
        }
        self.footer.status_line_enabled = enabled;
        true
    }

    pub fn set_side_conversation_context_label(&mut self, label: Option<String>) -> bool {
        if self.footer.side_conversation_context_label == label {
            return false;
        }
        self.footer.side_conversation_context_label = label;
        true
    }

    /// Replaces the contextual footer label for the currently viewed agent.
    ///
    /// Returning `false` means the value was unchanged, so callers can skip redraw work. This
    /// field is intentionally just cached presentation state; `ChatComposer` does not infer which
    /// thread is active on its own.
    pub fn set_active_agent_label(&mut self, active_agent_label: Option<String>) -> bool {
        if self.footer.active_agent_label == active_agent_label {
            return false;
        }
        self.footer.active_agent_label = active_agent_label;
        true
    }
}

fn footer_insert_newline_key(
    bindings: &[KeyBinding],
    enhanced_keys_supported: bool,
) -> Option<KeyBinding> {
    let shift_enter = key_hint::shift(KeyCode::Enter);
    if enhanced_keys_supported && bindings.contains(&shift_enter) {
        return Some(shift_enter);
    }

    let plain_enter = key_hint::plain(KeyCode::Enter);
    bindings
        .iter()
        .copied()
        .find(|binding| *binding != plain_enter)
        .or_else(|| bindings.first().copied())
}

fn skill_description(skill: &SkillMetadata) -> Option<String> {
    let description = skill
        .interface
        .as_ref()
        .and_then(|interface| interface.short_description.as_deref())
        .or(skill.short_description.as_deref())
        .unwrap_or(&skill.description);
    let trimmed = description.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')
}

fn ends_plaintext_at_mention(bytes: &[u8], index: usize) -> bool {
    bytes.get(index).is_none_or(|byte| {
        byte.is_ascii_whitespace()
            || *byte == b'.'
                && bytes.get(index + 1).is_none_or(|next| {
                    next.is_ascii_whitespace()
                        || !next.is_ascii_alphanumeric() && *next != b'_' && *next != b'-'
                })
            || !matches!(*byte, b'.' | b'/' | b'\\')
                && !byte.is_ascii_alphanumeric()
                && *byte != b'_'
                && *byte != b'-'
    })
}

fn ends_plaintext_at_dollar_mention(bytes: &[u8], index: usize) -> bool {
    bytes
        .get(index)
        .is_none_or(|byte| !is_mention_name_char(*byte))
}

fn starts_plaintext_at_mention(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }

    text.get(..index)
        .and_then(|prefix| prefix.chars().next_back())
        .is_some_and(|ch| ch.is_whitespace() || !is_mention_name_char_char(ch))
}

fn is_mention_name_char_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

fn find_next_mention_token_range(text: &str, token: &str, from: usize) -> Option<Range<usize>> {
    if token.is_empty() || from >= text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    let token_bytes = token.as_bytes();
    let sigil = *token_bytes.first()?;
    let mut index = from;

    while index < bytes.len() {
        if bytes[index] != sigil {
            index += 1;
            continue;
        }

        let end = index.saturating_add(token_bytes.len());
        if end > bytes.len() {
            return None;
        }
        if &bytes[index..end] != token_bytes {
            index += 1;
            continue;
        }

        // Fix for restored `@` mentions: rebinding must not attach to embedded substrings such
        // as email addresses, while preserving the existing `$` mention matching behavior.
        let starts_plaintext_mention = if sigil == b'@' {
            starts_plaintext_at_mention(text, index)
        } else {
            true
        };
        // Fix for restored `@` mentions: mirror history encoding's trailing boundary so path-like
        // text such as `@sample/pkg` is not rebound as the plain `@sample` mention.
        let ends_plaintext_mention = if sigil == b'@' {
            ends_plaintext_at_mention(bytes, end)
        } else {
            ends_plaintext_at_dollar_mention(bytes, end)
        };

        if starts_plaintext_mention && ends_plaintext_mention {
            return Some(index..end);
        }

        index = end;
    }

    None
}

impl Renderable for ChatComposer {
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_pos_with_textarea_right_reserve(area, /*textarea_right_reserve*/ 0)
    }

    fn cursor_style(&self, _area: Rect) -> crossterm::cursor::SetCursorStyle {
        if self.draft.textarea.uses_vim_insert_cursor() {
            crossterm::cursor::SetCursorStyle::SteadyBar
        } else {
            crossterm::cursor::SetCursorStyle::DefaultUserShape
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.desired_height_with_textarea_right_reserve(width, /*textarea_right_reserve*/ 0)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_with_mask(area, buf, /*mask_char*/ None);
    }
}

impl ChatComposer {
    pub fn desired_height_with_textarea_right_reserve(
        &self,
        width: u16,
        textarea_right_reserve: u16,
    ) -> u16 {
        let footer_props = self.footer_props();
        let footer_total_height = self.footer_total_height(&footer_props);
        let cols_with_margin = LIVE_PREFIX_COLS + 2;
        let inner_width =
            width.saturating_sub(cols_with_margin.saturating_add(textarea_right_reserve));
        let remote_images_height: u16 = self
            .attachments
            .remote_image_lines()
            .len()
            .try_into()
            .unwrap_or(u16::MAX);
        let remote_images_separator = u16::from(remote_images_height > 0);
        self.draft.textarea.desired_height(inner_width)
            + remote_images_height
            + remote_images_separator
            + 2
            + self
                .active_popup_height(width)
                .unwrap_or(footer_total_height)
    }

    pub fn browser_popup_height(&self, width: u16) -> u16 {
        self.active_popup_height(width).unwrap_or_default()
    }

    fn active_popup_height(&self, width: u16) -> Option<u16> {
        match &self.popups.active {
            ActivePopup::None => None,
            ActivePopup::Command(popup) => Some(popup.calculate_required_height(width)),
            ActivePopup::File(popup) => Some(popup.calculate_required_height()),
            ActivePopup::Skill(popup) => Some(popup.calculate_required_height(width)),
            ActivePopup::MentionV2(popup) => Some(popup.calculate_required_height(width)),
        }
    }
}

impl ChatComposer {
    pub fn render_with_mask(&self, area: Rect, buf: &mut Buffer, mask_char: Option<char>) {
        self.render_with_mask_and_textarea_right_reserve(
            area, buf, mask_char, /*textarea_right_reserve*/ 0,
        );
    }

    pub fn render_with_mask_and_textarea_right_reserve(
        &self,
        area: Rect,
        buf: &mut Buffer,
        mask_char: Option<char>,
        textarea_right_reserve: u16,
    ) {
        self.render_with_mask_and_textarea_right_reserve_and_runtime_context(
            area,
            buf,
            mask_char,
            textarea_right_reserve,
            /*active_turn_elapsed_seconds*/ None,
            /*runtime_context*/ None,
        );
    }

    pub fn render_with_mask_and_textarea_right_reserve_and_runtime_context(
        &self,
        area: Rect,
        buf: &mut Buffer,
        mask_char: Option<char>,
        textarea_right_reserve: u16,
        active_turn_elapsed_seconds: Option<u64>,
        runtime_context: Option<(&str, Option<&str>, bool, Option<&str>)>,
    ) {
        let [composer_rect, remote_images_rect, textarea_rect, popup_rect] =
            self.layout_areas_with_textarea_right_reserve(area, textarea_right_reserve);
        match &self.popups.active {
            ActivePopup::Command(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::File(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::Skill(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::MentionV2(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::None => {
                let footer_props = self.footer_props();
                let show_cycle_hint = !footer_props.is_task_running
                    && self.footer.collaboration_mode_indicator.is_some();
                let show_shortcuts_hint = match footer_props.mode {
                    FooterMode::ComposerEmpty => !self.is_in_paste_burst(),
                    FooterMode::ComposerHasDraft => false,
                    FooterMode::HistorySearch
                    | FooterMode::QuitShortcutReminder
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint => false,
                };
                let show_queue_hint = match footer_props.mode {
                    FooterMode::ComposerHasDraft => footer_props.is_task_running,
                    FooterMode::HistorySearch
                    | FooterMode::QuitShortcutReminder
                    | FooterMode::ComposerEmpty
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint => false,
                };
                let footer_hint_height = self.footer_hint_height(&footer_props);
                let persistent_status_line = self.persistent_status_line(&footer_props);
                let persistent_status_line_height = u16::from(persistent_status_line.is_some());
                let [persistent_status_line_rect, hint_rect] = Self::footer_content_rects(
                    popup_rect,
                    footer_hint_height,
                    persistent_status_line_height,
                );
                if let Some(line) = persistent_status_line {
                    let available_width = persistent_status_line_rect
                        .width
                        .saturating_sub(FOOTER_INDENT_COLS as u16)
                        as usize;
                    render_footer_line(
                        persistent_status_line_rect,
                        buf,
                        truncate_line_with_ellipsis_if_overflow(line, available_width),
                    );
                }
                if let Some(line) = self.history_search_footer_line() {
                    render_footer_line(hint_rect, buf, line);
                } else if self.footer.plan_mode_nudge_visible {
                    let available_width =
                        hint_rect.width.saturating_sub(FOOTER_INDENT_COLS as u16) as usize;
                    render_footer_line(
                        hint_rect,
                        buf,
                        truncate_line_with_ellipsis_if_overflow(
                            plan_mode_nudge_line(),
                            available_width,
                        ),
                    );
                } else {
                    let available_width =
                        hint_rect.width.saturating_sub(FOOTER_INDENT_COLS as u16) as usize;
                    let status_line_active = uses_passive_footer_status_layout(&footer_props);
                    let combined_status_line = if status_line_active {
                        passive_footer_status_line(&footer_props)
                    } else {
                        None
                    };
                    let transition_visible = status_line_active
                        && !self.footer.flash_visible()
                        && self.footer.hint_override.is_none();
                    let transition_active = transition_visible
                        && self
                            .effort_status_line_transition
                            .as_ref()
                            .is_some_and(|transition| !transition.is_finished());
                    let combined_status_line = if transition_visible
                        && let Some(transition) = &self.effort_status_line_transition
                        && !transition.is_finished()
                    {
                        transition.render_line(
                            combined_status_line.as_ref(),
                            hint_rect.width.saturating_sub(FOOTER_INDENT_COLS as u16),
                        )
                    } else {
                        combined_status_line
                    };
                    let mut truncated_status_line = if status_line_active {
                        combined_status_line.as_ref().map(|line| {
                            truncate_line_with_ellipsis_if_overflow(line.clone(), available_width)
                        })
                    } else {
                        None
                    };
                    let left_mode_indicator = if status_line_active {
                        None
                    } else {
                        self.footer.collaboration_mode_indicator
                    };
                    let active_footer_hint_override = self.footer.hint_override.as_ref();
                    let mut left_width = if self.footer.flash_visible() {
                        self.footer
                            .flash
                            .as_ref()
                            .map(|flash| flash.line.width() as u16)
                            .unwrap_or(0)
                    } else if let Some(items) = active_footer_hint_override {
                        footer_hint_items_width(items)
                    } else if status_line_active {
                        truncated_status_line
                            .as_ref()
                            .map(|line| line.width() as u16)
                            .unwrap_or(0)
                    } else {
                        footer_line_width(
                            &footer_props,
                            left_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        )
                    };
                    let right_line =
                        if let Some(label) = self.footer.side_conversation_context_label.as_ref() {
                            Some(side_conversation_context_line(label))
                        } else if let Some(line) = self.shell_mode_footer_line() {
                            Some(line)
                        } else if transition_active {
                            None
                        } else if status_line_active {
                            let full = with_active_agent_context_line(
                                self.mode_indicator_line(show_cycle_hint),
                                footer_props.active_agent_label.as_deref(),
                            );
                            let compact = with_active_agent_context_line(
                                self.mode_indicator_line(/*show_cycle_hint*/ false),
                                footer_props.active_agent_label.as_deref(),
                            );
                            let full_width = full.as_ref().map(|l| l.width() as u16).unwrap_or(0);
                            if can_show_left_with_context(hint_rect, left_width, full_width) {
                                full
                            } else {
                                compact
                            }
                        } else {
                            Some(self.right_footer_line_with_context())
                        };
                    let right_width = right_line.as_ref().map(|l| l.width() as u16).unwrap_or(0);
                    if status_line_active
                        && let Some(max_left) = max_left_width_for_right(hint_rect, right_width)
                        && left_width > max_left
                        && let Some(line) = combined_status_line.as_ref().map(|line| {
                            truncate_line_with_ellipsis_if_overflow(line.clone(), max_left as usize)
                        })
                    {
                        left_width = line.width() as u16;
                        truncated_status_line = Some(line);
                    }
                    let can_show_left_and_context =
                        can_show_left_with_context(hint_rect, left_width, right_width);
                    let has_override =
                        self.footer.flash_visible() || active_footer_hint_override.is_some();
                    let single_line_layout = if has_override || status_line_active {
                        None
                    } else {
                        match footer_props.mode {
                            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => {
                                // Both of these modes render the single-line footer style (with
                                // either the shortcuts hint or the optional queue hint). We still
                                // want the single-line collapse rules so the mode label can win over
                                // the context indicator on narrow widths.
                                Some(single_line_footer_layout(
                                    hint_rect,
                                    right_width,
                                    left_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                    footer_props.key_hints,
                                ))
                            }
                            FooterMode::EscHint
                            | FooterMode::HistorySearch
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay => None,
                        }
                    };
                    let show_right = if matches!(
                        footer_props.mode,
                        FooterMode::EscHint
                            | FooterMode::HistorySearch
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay
                    ) {
                        false
                    } else {
                        single_line_layout
                            .as_ref()
                            .map(|(_, show_context)| *show_context)
                            .unwrap_or(can_show_left_and_context)
                    };

                    if let Some((summary_left, _)) = single_line_layout {
                        match summary_left {
                            SummaryLeft::Default => {
                                if status_line_active {
                                    if let Some(line) = truncated_status_line.clone() {
                                        render_footer_line(hint_rect, buf, line);
                                    } else {
                                        render_footer_from_props(
                                            hint_rect,
                                            buf,
                                            &footer_props,
                                            left_mode_indicator,
                                            show_cycle_hint,
                                            show_shortcuts_hint,
                                            show_queue_hint,
                                        );
                                    }
                                } else {
                                    render_footer_from_props(
                                        hint_rect,
                                        buf,
                                        &footer_props,
                                        left_mode_indicator,
                                        show_cycle_hint,
                                        show_shortcuts_hint,
                                        show_queue_hint,
                                    );
                                }
                            }
                            SummaryLeft::Custom(line) => {
                                render_footer_line(hint_rect, buf, line);
                            }
                            SummaryLeft::None => {}
                        }
                    } else if self.footer.flash_visible() {
                        if let Some(flash) = self.footer.flash.as_ref() {
                            flash.line.render(inset_footer_hint_area(hint_rect), buf);
                        }
                    } else if let Some(items) = active_footer_hint_override {
                        render_footer_hint_items(hint_rect, buf, items);
                    } else if status_line_active {
                        if let Some(line) = truncated_status_line {
                            render_footer_line(hint_rect, buf, line);
                        }
                    } else {
                        render_footer_from_props(
                            hint_rect,
                            buf,
                            &footer_props,
                            self.footer.collaboration_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        );
                    }
                    if show_right && let Some(line) = &right_line {
                        render_context_right(hint_rect, buf, line);
                    }
                    if status_line_active
                        && let Some(url) = self.footer.status_line_hyperlink_url.as_deref()
                    {
                        mark_underlined_hyperlink(buf, hint_rect, url);
                    }
                    if transition_visible
                        && let Some(transition) = &self.effort_status_line_transition
                        && !transition.is_finished()
                        && let Some(frame_requester) = &self.frame_requester
                    {
                        frame_requester.schedule_frame_in(EFFORT_STATUS_LINE_FRAME_TICK);
                    }
                }
            }
        }
        let style = Style::default();
        let mut block = Block::default()
            .style(style)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(city_lights::composer_border_style());
        if let Some(session_name) = self.session_name.as_ref() {
            let available_width = composer_rect.width.saturating_sub(4) as usize;
            if available_width >= 2 {
                let name_color = session_name_color();
                let title_style = city_lights::composer_session_title_style().fg(name_color);
                let title_line = Line::from(vec![Span::styled(session_name.clone(), title_style)]);
                let title_text = truncate_line_with_ellipsis_if_overflow(
                    title_line,
                    available_width.saturating_sub(2),
                );
                let mut title_spans = vec![Span::styled(" ", title_style)];
                title_spans.extend(title_text.spans);
                title_spans.push(Span::styled(" ", title_style));
                block = block.title(Title {
                    content: Line::from(title_spans),
                    alignment: Some(Alignment::Right),
                    position: None,
                });
            }
        }
        let mut runtime_context_spans = Vec::new();
        let field_style = city_lights::composer_session_title_style();
        if let Some((model, effort, fast, router_mode)) = runtime_context {
            runtime_context_spans.push(Span::styled(" ", field_style));
            runtime_context_spans.push(Span::styled(
                "↳",
                router_mode_style(
                    router_mode.unwrap_or("off"),
                    city_lights::composer_model_name_style().bg,
                ),
            ));
            runtime_context_spans.push(Span::styled(" ", field_style));
            runtime_context_spans.push(Span::styled(
                model,
                city_lights::composer_model_name_style(),
            ));
            if let Some(effort) = effort {
                runtime_context_spans.push(Span::styled("│", runtime_context_separator_style()));
                runtime_context_spans.push(Span::styled(
                    effort,
                    city_lights::composer_runtime_context_style(effort),
                ));
            }
            runtime_context_spans.push(Span::styled(" ", field_style));
            if fast {
                runtime_context_spans.push("─".into());
                runtime_context_spans.push(Span::styled(" ", field_style));
                runtime_context_spans.push(
                    Span::from("fast")
                        .red()
                        .bg(city_lights::composer_session_title_style()
                            .bg
                            .unwrap_or_default()),
                );
                runtime_context_spans.push(Span::styled(" ", field_style));
            }
        }
        if let Some(elapsed_seconds) = active_turn_elapsed_seconds {
            if !runtime_context_spans.is_empty() {
                runtime_context_spans.push("─".into());
            }
            runtime_context_spans.push(Span::styled(" ", field_style));
            let timer = Span::styled(
                crate::status_indicator_widget::fmt_elapsed_compact(elapsed_seconds),
                active_turn_timer_style(),
            );
            runtime_context_spans.push(timer);
            runtime_context_spans.push(Span::styled(" ", field_style));
            if let Some(frame_requester) = &self.frame_requester {
                frame_requester.schedule_frame_in(Duration::from_secs(/*secs*/ 1));
            }
        }
        if !runtime_context_spans.is_empty() {
            block =
                block.title_bottom(Line::from(runtime_context_spans).alignment(Alignment::Right));
        }
        block.render_ref(composer_rect, buf);
        if !remote_images_rect.is_empty() {
            Paragraph::new(self.attachments.remote_image_lines())
                .style(style)
                .render_ref(remote_images_rect, buf);
        }
        if !textarea_rect.is_empty() {
            let prompt = if self.draft.input_enabled {
                if self.draft.is_bash_mode {
                    Span::from("!").cl_red().bold()
                } else if let Some(tier) = self.effort_tier {
                    let charge = self
                        .effort_ignition
                        .as_ref()
                        .map(EffortIgnition::charge_alpha)
                        .unwrap_or(1.0);
                    tier.prompt(charge)
                } else {
                    "❯".cl_magenta()
                }
            } else {
                "❯".dim()
            };
            buf.set_span(
                textarea_rect.x - LIVE_PREFIX_COLS,
                textarea_rect.y,
                &prompt,
                textarea_rect.width,
            );
        }

        let mut state = self.draft.textarea_state.borrow_mut();
        let textarea_is_empty = self.draft.textarea.text().is_empty() && !self.draft.is_bash_mode;
        if self.draft.input_enabled {
            if let Some(mask_char) = mask_char {
                self.draft
                    .textarea
                    .render_ref_masked(textarea_rect, buf, &mut state, mask_char);
            } else {
                let mut highlights = self.plugin_at_mention_highlights();
                let search_highlight_style =
                    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
                highlights.extend(
                    self.history_search_highlight_ranges()
                        .into_iter()
                        .map(|range| (range, search_highlight_style)),
                );
                if highlights.is_empty() {
                    StatefulWidgetRef::render_ref(
                        &(&self.draft.textarea),
                        textarea_rect,
                        buf,
                        &mut state,
                    );
                } else {
                    self.draft.textarea.render_ref_styled_with_highlights(
                        textarea_rect,
                        buf,
                        &mut state,
                        Style::default(),
                        &highlights,
                    );
                }
            }
        }
        if !self.draft.input_enabled || textarea_is_empty {
            let text = if self.draft.input_enabled {
                self.placeholder_text.as_str().to_string()
            } else {
                self.draft
                    .input_disabled_placeholder
                    .as_deref()
                    .unwrap_or("Input disabled.")
                    .to_string()
            };
            if !textarea_rect.is_empty() {
                let placeholder = Span::from(text).dim();
                Line::from(vec![placeholder])
                    .render_ref(textarea_rect.inner(Margin::new(0, 0)), buf);
            }
        }
        if matches!(self.popups.active, ActivePopup::None)
            && let Some(ignition) = &self.effort_ignition
            && !ignition.is_finished()
        {
            let protected_top = if remote_images_rect.is_empty() {
                textarea_rect.y
            } else {
                remote_images_rect.y
            };
            let protected = Rect::new(
                composer_rect.x,
                protected_top,
                composer_rect.width,
                textarea_rect.bottom().saturating_sub(protected_top),
            );
            if ignition.render(composer_rect, protected, buf)
                && let Some(frame_requester) = &self.frame_requester
            {
                frame_requester.schedule_frame_in(IGNITION_FRAME_TICK);
            }
        }
    }
}
