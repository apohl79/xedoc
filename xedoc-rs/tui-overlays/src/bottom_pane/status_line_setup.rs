//! Status line configuration view for customizing the TUI status bar.
//!
//! This module provides an interactive picker for selecting which items appear
//! in the status line at the bottom of the terminal. Users can:
//!
//! - **Select items**: Toggle which information is displayed
//! - **Reorder items**: Use left/right arrows to change display order
//! - **Preview changes**: See a live preview of the configured status line
//!
//! # Available Status Line Items
//!
//! - Model information (name, reasoning level)
//! - Directory paths (current dir, project root)
//! - Git information (branch name)
//! - Permissions profile
//! - Approval mode
//! - Context usage (remaining %, used %, window size)
//! - Usage limits (primary, secondary)
//! - Session info (thread title, thread ID, tokens used)
//! - Application version

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Widget;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use strum::IntoEnumIterator;
use strum_macros::Display;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use xedoc_config::types::StatusLineCommand;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::bottom_pane::multi_select_picker::MultiSelectItem;
use crate::bottom_pane::multi_select_picker::MultiSelectPicker;
use crate::bottom_pane::status_surface_preview::StatusSurfacePreviewData;
use crate::bottom_pane::status_surface_preview::StatusSurfacePreviewItem;
use crate::key_hint;
use crate::keymap::ListKeymap;
use crate::render::renderable::Renderable;

const STATUS_LINE_USE_THEME_COLORS_ITEM_ID: &str = "status-line-use-theme-colors";
const STATUS_LINE_CUSTOM_COMMAND_ITEM_ID: &str = "status-line-custom-command";
const DEFAULT_STATUS_LINE_COMMAND: &str = "~/.xedoc/statusline.sh";

/// Available items that can be displayed in the status line.
///
/// Each variant represents a piece of information that can be shown at the
/// bottom of the TUI. Items are serialized to kebab-case for configuration
/// storage (e.g., `ModelWithReasoning` becomes `model-with-reasoning`).
///
/// Some items are conditionally displayed based on availability:
/// - Git-related items only show when in a git repository
/// - Context/limit items only show when data is available from the API
/// - Thread ID only shows after a session has started
#[derive(EnumIter, EnumString, Display, Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
#[strum(serialize_all = "kebab_case")]
pub enum StatusLineItem {
    /// The current model name.
    #[strum(to_string = "model", serialize = "model-name")]
    ModelName,

    /// Model name with reasoning level suffix.
    ModelWithReasoning,

    /// Current reasoning level.
    Reasoning,

    /// Current working directory path.
    CurrentDir,

    /// Project root directory (if detected).
    #[strum(
        to_string = "project-name",
        serialize = "project",
        serialize = "project-root"
    )]
    ProjectRoot,

    /// Current git branch name (if in a repository).
    GitBranch,

    /// Open pull request number for the current branch.
    PullRequestNumber,

    /// Committed branch diff stats relative to the default branch.
    BranchChanges,

    /// Compact runtime run-state text.
    #[strum(to_string = "run-state", serialize = "status")]
    Status,

    /// Active permission profile or sandbox summary.
    Permissions,

    /// Active command approval mode.
    #[strum(to_string = "approval-mode", serialize = "approval")]
    ApprovalMode,

    /// Percentage of context window remaining.
    ContextRemaining,

    /// Percentage of context window used.
    ///
    /// Also accepts the legacy `context-usage` config value.
    #[strum(to_string = "context-used", serialize = "context-usage")]
    ContextUsed,

    /// Remaining usage on the primary rate limit.
    FiveHourLimit,

    /// Remaining usage on the secondary rate limit.
    WeeklyLimit,

    /// Xedoc application version.
    XedocVersion,

    /// Total context window size in tokens.
    ContextWindowSize,

    /// Total tokens used in the current session.
    UsedTokens,

    /// Total input tokens consumed.
    TotalInputTokens,

    /// Total output tokens generated.
    TotalOutputTokens,

    /// Full thread UUID.
    #[strum(to_string = "thread-id", serialize = "session-id")]
    SessionId,

    /// Whether Fast mode is currently active.
    FastMode,

    /// Whether raw scrollback mode is currently active.
    RawOutput,

    /// Current thread title (if set by user).
    ThreadTitle,

    /// Latest checklist task progress from `update_plan` (if available).
    TaskProgress,
}

impl StatusLineItem {
    /// User-visible description shown in the popup.
    pub fn description(self) -> &'static str {
        match self {
            StatusLineItem::ModelName => "Current model name",
            StatusLineItem::ModelWithReasoning => "Current model name with reasoning level",
            StatusLineItem::Reasoning => "Current reasoning level",
            StatusLineItem::CurrentDir => "Current working directory",
            StatusLineItem::ProjectRoot => "Project name (omitted when unavailable)",
            StatusLineItem::GitBranch => "Current Git branch (omitted when unavailable)",
            StatusLineItem::PullRequestNumber => {
                "Open pull request number for the current branch (omitted when unavailable)"
            }
            StatusLineItem::BranchChanges => {
                "Committed branch changes against the default branch (omitted when unavailable)"
            }
            StatusLineItem::Status => "Compact session run-state text (Ready, Working, Thinking)",
            StatusLineItem::Permissions => "Active permission profile or sandbox mode",
            StatusLineItem::ApprovalMode => "Active command approval mode",
            StatusLineItem::ContextRemaining => {
                "Percentage of context window remaining (omitted when unknown)"
            }
            StatusLineItem::ContextUsed => {
                "Percentage of context window used (omitted when unknown)"
            }
            StatusLineItem::FiveHourLimit => {
                "Remaining usage on the primary usage limit (omitted when unavailable)"
            }
            StatusLineItem::WeeklyLimit => {
                "Remaining usage on the secondary usage limit (omitted when unavailable)"
            }
            StatusLineItem::XedocVersion => "Xedoc application version",
            StatusLineItem::ContextWindowSize => {
                "Total context window size in tokens (omitted when unknown)"
            }
            StatusLineItem::UsedTokens => "Total tokens used in session (omitted when zero)",
            StatusLineItem::TotalInputTokens => "Total input tokens used in session",
            StatusLineItem::TotalOutputTokens => "Total output tokens used in session",
            StatusLineItem::SessionId => "Current thread identifier (omitted until thread starts)",
            StatusLineItem::FastMode => "Whether Fast mode is currently active",
            StatusLineItem::RawOutput => "Whether raw scrollback mode is active",
            StatusLineItem::ThreadTitle => {
                "Current thread title, or thread identifier when unnamed"
            }
            StatusLineItem::TaskProgress => {
                "Latest task progress from update_plan (omitted until available)"
            }
        }
    }

    pub fn preview_item(self) -> StatusSurfacePreviewItem {
        match self {
            StatusLineItem::ModelName => StatusSurfacePreviewItem::Model,
            StatusLineItem::ModelWithReasoning => StatusSurfacePreviewItem::ModelWithReasoning,
            StatusLineItem::Reasoning => StatusSurfacePreviewItem::Reasoning,
            StatusLineItem::CurrentDir => StatusSurfacePreviewItem::CurrentDir,
            StatusLineItem::ProjectRoot => StatusSurfacePreviewItem::ProjectRoot,
            StatusLineItem::GitBranch => StatusSurfacePreviewItem::GitBranch,
            StatusLineItem::PullRequestNumber => StatusSurfacePreviewItem::PullRequestNumber,
            StatusLineItem::BranchChanges => StatusSurfacePreviewItem::BranchChanges,
            StatusLineItem::Status => StatusSurfacePreviewItem::Status,
            StatusLineItem::Permissions => StatusSurfacePreviewItem::Permissions,
            StatusLineItem::ApprovalMode => StatusSurfacePreviewItem::ApprovalMode,
            StatusLineItem::ContextRemaining => StatusSurfacePreviewItem::ContextRemaining,
            StatusLineItem::ContextUsed => StatusSurfacePreviewItem::ContextUsed,
            StatusLineItem::FiveHourLimit => StatusSurfacePreviewItem::FiveHourLimit,
            StatusLineItem::WeeklyLimit => StatusSurfacePreviewItem::WeeklyLimit,
            StatusLineItem::XedocVersion => StatusSurfacePreviewItem::XedocVersion,
            StatusLineItem::ContextWindowSize => StatusSurfacePreviewItem::ContextWindowSize,
            StatusLineItem::UsedTokens => StatusSurfacePreviewItem::UsedTokens,
            StatusLineItem::TotalInputTokens => StatusSurfacePreviewItem::TotalInputTokens,
            StatusLineItem::TotalOutputTokens => StatusSurfacePreviewItem::TotalOutputTokens,
            StatusLineItem::SessionId => StatusSurfacePreviewItem::SessionId,
            StatusLineItem::FastMode => StatusSurfacePreviewItem::FastMode,
            StatusLineItem::RawOutput => StatusSurfacePreviewItem::RawOutput,
            StatusLineItem::ThreadTitle => StatusSurfacePreviewItem::ThreadTitle,
            StatusLineItem::TaskProgress => StatusSurfacePreviewItem::TaskProgress,
        }
    }
}

/// Interactive view for configuring which items appear in the status line.
///
/// Wraps a [`MultiSelectPicker`] with status-line-specific behavior:
/// - Pre-populates items from current configuration
/// - Shows a live preview of the configured status line
/// - Emits [`AppEvent::StatusLineSetup`] on confirmation
/// - Emits [`AppEvent::StatusLineSetupCancelled`] on cancellation
pub struct StatusLineSetupView {
    /// The underlying multi-select picker widget.
    picker: MultiSelectPicker,

    custom_command: Arc<Mutex<CustomStatusLineCommand>>,
    editing_custom_command: Option<String>,
}

#[derive(Clone, Debug)]
struct CustomStatusLineCommand {
    original: Option<StatusLineCommand>,
    command: String,
    edited: bool,
}

impl CustomStatusLineCommand {
    fn new(command: Option<&StatusLineCommand>) -> Self {
        Self {
            original: command.cloned(),
            command: command_display_value(command),
            edited: false,
        }
    }

    fn set_command(&mut self, command: String) {
        self.command = command;
        self.edited = true;
    }

    fn command_for_save(&self, enabled: bool) -> Option<StatusLineCommand> {
        if !enabled {
            return None;
        }

        if !self.edited
            && let Some(original) = self.original.clone()
        {
            return Some(original);
        }

        let command = self.command.trim();
        if let Some(original) = self.original.clone()
            && command == command_display_value(Some(&original)).trim()
        {
            return Some(original);
        }
        (!command.is_empty()).then(|| StatusLineCommand::Command(command.to_string()))
    }
}

impl StatusLineSetupView {
    /// Creates a new status line setup view.
    ///
    /// # Arguments
    ///
    /// * `status_line_items` - Currently configured item IDs (in display order),
    ///   or `None` to start with all items disabled
    /// * `use_theme_colors` - Whether the preview and saved status line use colors from
    ///   the active theme
    /// * `app_event_tx` - Event sender for dispatching configuration changes
    ///
    /// Items from `status_line_items` are shown first (in order) and marked as
    /// enabled. Remaining items are appended and marked as disabled.
    pub fn new(
        status_line_items: Option<&[String]>,
        use_theme_colors: bool,
        custom_command: Option<&StatusLineCommand>,
        preview_data: StatusSurfacePreviewData,
        app_event_tx: AppEventSender,
        list_keymap: ListKeymap,
    ) -> Self {
        let custom_command = Arc::new(Mutex::new(CustomStatusLineCommand::new(custom_command)));
        let custom_command_for_preview = custom_command.clone();
        let custom_command_for_confirm = custom_command.clone();
        let command_description = custom_command
            .lock()
            .ok()
            .map(|command| custom_command_description(&command.command))
            .unwrap_or_else(|| custom_command_description(DEFAULT_STATUS_LINE_COMMAND));
        let mut used_ids = HashSet::new();
        let mut items = vec![MultiSelectItem {
            id: STATUS_LINE_USE_THEME_COLORS_ITEM_ID.to_string(),
            name: "Use theme colors".to_string(),
            description: Some("Apply colors from the active /theme".to_string()),
            enabled: use_theme_colors,
            orderable: false,
            section_break_after: false,
        }];

        items.push(MultiSelectItem {
            id: STATUS_LINE_CUSTOM_COMMAND_ITEM_ID.to_string(),
            name: "Script".to_string(),
            description: Some(command_description),
            enabled: custom_command
                .lock()
                .ok()
                .and_then(|command| command.original.clone())
                .is_some(),
            orderable: false,
            section_break_after: true,
        });

        if let Some(selected_items) = status_line_items.as_ref() {
            for id in *selected_items {
                let Ok(item) = id.parse::<StatusLineItem>() else {
                    continue;
                };
                let item_id = item.to_string();
                if !used_ids.insert(item_id.clone()) {
                    continue;
                }
                items.push(Self::status_line_select_item(
                    item,
                    /*enabled*/ true,
                    &preview_data,
                ));
            }
        }

        for item in StatusLineItem::iter() {
            let item_id = item.to_string();
            if used_ids.contains(&item_id) {
                continue;
            }
            items.push(Self::status_line_select_item(
                item,
                /*enabled*/ false,
                &preview_data,
            ));
        }

        Self {
            picker: MultiSelectPicker::builder(
                "Configure Status Line".to_string(),
                Some("Select items, or enable Script; tab edits it.".to_string()),
                app_event_tx,
            )
            .list_keymap(list_keymap)
            .items(items)
            .enable_ordering()
            .on_preview(move |items| {
                let custom_enabled = items
                    .iter()
                    .any(|item| item.id == STATUS_LINE_CUSTOM_COMMAND_ITEM_ID && item.enabled);
                if custom_enabled {
                    let command = custom_command_for_preview
                        .lock()
                        .ok()
                        .map(|command| command.command.clone())
                        .unwrap_or_else(|| DEFAULT_STATUS_LINE_COMMAND.to_string());
                    return Some(Line::from(vec![
                        "script: ".dim(),
                        display_custom_command(&command).into(),
                    ]));
                }

                let use_theme_colors = items
                    .iter()
                    .find(|item| item.id == STATUS_LINE_USE_THEME_COLORS_ITEM_ID)
                    .map(|item| item.enabled)
                    .unwrap_or(true);
                preview_data.status_line_for_items(
                    items
                        .iter()
                        .filter(|item| item.enabled)
                        .filter_map(|item| item.id.parse::<StatusLineItem>().ok()),
                    use_theme_colors,
                )
            })
            .on_confirm(move |ids, app_event| {
                let use_theme_colors = ids
                    .iter()
                    .any(|id| id == STATUS_LINE_USE_THEME_COLORS_ITEM_ID);
                let custom_enabled = ids
                    .iter()
                    .any(|id| id == STATUS_LINE_CUSTOM_COMMAND_ITEM_ID);
                let command = custom_command_for_confirm
                    .lock()
                    .ok()
                    .and_then(|command| command.command_for_save(custom_enabled));
                let item_ids = ids
                    .iter()
                    .filter_map(|id| id.parse::<StatusLineItem>().ok())
                    .map(|item| item.to_string())
                    .collect::<Vec<_>>();
                app_event.send(AppEvent::StatusLineSetup {
                    item_ids,
                    use_theme_colors,
                    command,
                });
            })
            .on_cancel(|app_event| {
                app_event.send(AppEvent::StatusLineSetupCancelled);
            })
            .build(),
            custom_command,
            editing_custom_command: None,
        }
    }

    fn selected_custom_command(&self) -> bool {
        self.picker.selected_item_id() == Some(STATUS_LINE_CUSTOM_COMMAND_ITEM_ID)
    }

    fn start_custom_command_edit(&mut self) {
        let command = self
            .custom_command
            .lock()
            .ok()
            .map(|command| command.command.clone())
            .unwrap_or_else(|| DEFAULT_STATUS_LINE_COMMAND.to_string());
        self.editing_custom_command = Some(display_custom_command(&command));
    }

    fn save_custom_command_edit(&mut self) {
        let Some(command) = self.editing_custom_command.take() else {
            return;
        };
        let command = command.trim();
        if command.is_empty() {
            return;
        }

        if let Ok(mut state) = self.custom_command.lock() {
            state.set_command(command.to_string());
        }

        let description = custom_command_description(command);
        self.picker
            .update_item(STATUS_LINE_CUSTOM_COMMAND_ITEM_ID, |item| {
                item.description = Some(description);
                item.enabled = true;
            });
    }

    fn handle_custom_command_edit_key(&mut self, key_event: KeyEvent) {
        let Some(command) = self.editing_custom_command.as_mut() else {
            return;
        };
        match key_event {
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => self.save_custom_command_edit(),
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.editing_custom_command = None;
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                command.pop();
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                command.clear();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !key_hint::has_ctrl_or_alt(modifiers) => {
                command.push(c);
            }
            _ => {}
        }
    }

    fn render_custom_command_edit(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        Block::default().render(area, buf);
        let x = area.x.saturating_add(2);
        let width = area.width.saturating_sub(4);
        if width == 0 {
            return;
        }

        let command = self.editing_custom_command.as_deref().unwrap_or_default();
        let rows = [
            Line::from("Edit Status Line Command".bold()),
            Line::from("Command receives the Claude Code statusline JSON on stdin.".dim()),
            Line::from(vec!["> ".dim(), command.to_string().into()]),
            Line::from("enter to save; esc to cancel".dim()),
        ];
        for (idx, row) in rows.into_iter().enumerate() {
            let y = area.y.saturating_add(1 + idx as u16);
            if y >= area.y.saturating_add(area.height) {
                break;
            }
            row.render(Rect::new(x, y, width, 1), buf);
        }
    }

    /// Converts a [`StatusLineItem`] into a [`MultiSelectItem`] for the picker.
    fn status_line_select_item(
        item: StatusLineItem,
        enabled: bool,
        preview_data: &StatusSurfacePreviewData,
    ) -> MultiSelectItem {
        let default_name = item.to_string();
        let default_description = item.description();
        let (name, description) = match item {
            StatusLineItem::FiveHourLimit | StatusLineItem::WeeklyLimit => (
                preview_data.rate_limit_item_name(item.preview_item(), &default_name),
                preview_data.rate_limit_item_description(item.preview_item(), default_description),
            ),
            _ => (default_name, default_description.to_string()),
        };

        MultiSelectItem {
            id: item.to_string(),
            name,
            description: Some(description),
            enabled,
            orderable: true,
            section_break_after: false,
        }
    }
}

impl BottomPaneView for StatusLineSetupView {
    fn handle_key_event(&mut self, key_event: crossterm::event::KeyEvent) {
        if self.editing_custom_command.is_some() {
            self.handle_custom_command_edit_key(key_event);
            return;
        }
        if key_event.code == KeyCode::Tab && self.selected_custom_command() {
            self.start_custom_command_edit();
            return;
        }
        self.picker.handle_key_event(key_event);
    }

    fn is_complete(&self) -> bool {
        self.picker.complete
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if self.editing_custom_command.is_some() {
            self.editing_custom_command = None;
            return CancellationEvent::Handled;
        }
        self.picker.close();
        CancellationEvent::Handled
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if let Some(command) = self.editing_custom_command.as_mut() {
            command.push_str(&pasted);
            true
        } else {
            false
        }
    }
}

impl Renderable for StatusLineSetupView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if self.editing_custom_command.is_some() {
            self.render_custom_command_edit(area, buf);
        } else {
            self.picker.render(area, buf)
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        if self.editing_custom_command.is_some() {
            6
        } else {
            self.picker.desired_height(width)
        }
    }
}

fn command_display_value(command: Option<&StatusLineCommand>) -> String {
    match command {
        Some(StatusLineCommand::Command(command)) if !command.trim().is_empty() => {
            command.trim().to_string()
        }
        Some(StatusLineCommand::Args(args)) => {
            let command = args
                .iter()
                .filter(|arg| !arg.trim().is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            display_custom_command(&command)
        }
        _ => DEFAULT_STATUS_LINE_COMMAND.to_string(),
    }
}

fn custom_command_description(command: &str) -> String {
    format!("{} (tab to edit)", display_custom_command(command))
}

fn display_custom_command(command: &str) -> String {
    let command = command.trim();
    if command.is_empty() {
        DEFAULT_STATUS_LINE_COMMAND.to_string()
    } else {
        command.to_string()
    }
}
