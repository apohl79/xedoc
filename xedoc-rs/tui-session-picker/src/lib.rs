#![allow(clippy::disallowed_methods)]

mod app_server_session {
    pub use xedoc_tui_app_server_session::*;
}

mod city_lights {
    pub use xedoc_tui_render::city_lights::*;
}

mod clipboard_paste {
    pub use xedoc_tui_input::clipboard_paste::*;
}

mod color {
    pub use xedoc_tui_render::color::*;
}

pub mod custom_terminal {
    pub use xedoc_tui_transcript::custom_terminal::*;
}

mod git_action_directives {
    pub use xedoc_tui_transcript::git_action_directives::*;
}

pub mod history_cell {
    pub use xedoc_tui_transcript::history_cell::*;
}

pub mod inline_visualization {
    pub use xedoc_tui_transcript::inline_visualization::*;
}

pub mod key_hint {
    pub use xedoc_tui_input::key_hint::*;
}

mod keymap {
    pub use xedoc_tui_input::keymap::*;
}

mod legacy_core {
    pub use xedoc_core_config::config;
}

mod markdown {
    pub use xedoc_tui_transcript::markdown::*;
}

mod pager_overlay {
    pub use xedoc_tui_overlays::pager_overlay::*;
}

mod session_resume {
    use std::path::Path;
    use xedoc_protocol::ThreadId;

    pub async fn resolve_session_thread_id(
        path: &Path,
        id_str_if_uuid: Option<&str>,
    ) -> Option<ThreadId> {
        match id_str_if_uuid {
            Some(id_str) => ThreadId::from_string(id_str).ok(),
            None => xedoc_rollout::read_session_meta_line(path)
                .await
                .ok()
                .map(|session_meta| session_meta.meta.id),
        }
    }
}

mod status {
    pub use xedoc_tui_status::status::*;
}

mod terminal_palette {
    pub use xedoc_tui_render::terminal_palette::*;
}

mod text_formatting {
    pub use xedoc_tui_transcript::text_formatting::*;
}

mod thread_transcript {
    use xedoc_protocol::ThreadId;
    use xedoc_tui_app_server_session::AppServerSession;

    pub use xedoc_tui_transcript::thread_transcript::RawReasoningVisibility;
    pub use xedoc_tui_transcript::thread_transcript::TranscriptCells;
    pub use xedoc_tui_transcript::thread_transcript::thread_to_transcript_cells;

    pub async fn load_session_transcript(
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        raw_reasoning_visibility: RawReasoningVisibility,
        xedoc_home: Option<&std::path::Path>,
    ) -> std::io::Result<TranscriptCells> {
        let thread = app_server
            .thread_read(thread_id, /*include_turns*/ true)
            .await
            .map_err(std::io::Error::other)?;
        Ok(thread_to_transcript_cells(
            thread,
            raw_reasoning_visibility,
            xedoc_home,
        ))
    }
}

mod wrapping {
    pub use xedoc_tui_render::wrapping::*;
}

use crate::city_lights::CityLightsStylize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::app_server_session::AppServerSession;
use crate::clipboard_paste::normalize_pasted_search_query;
use crate::color::blend;
use crate::color::is_light;
use crate::git_action_directives::parse_assistant_markdown;
use crate::inline_visualization::InlineVisualizationContext;
use crate::key_hint::KeyBindingListExt;
use crate::key_hint::is_plain_text_key_event;
use crate::keymap::ListKeymap;
use crate::keymap::PagerKeymap;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use crate::markdown::append_markdown;
use crate::pager_overlay::Overlay;
use crate::session_resume::resolve_session_thread_id;
use crate::status::format_directory_display;
use crate::terminal_palette::best_color;
use crate::terminal_palette::default_bg;
use crate::text_formatting::truncate_text;
use crate::thread_transcript::RawReasoningVisibility;
use crate::thread_transcript::TranscriptCells;
use crate::thread_transcript::load_session_transcript;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use chrono::DateTime;
use chrono::Utc;
use color_eyre::eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Styled as _;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::warn;
use unicode_width::UnicodeWidthStr;
use xedoc_app_server_protocol::Thread;
use xedoc_app_server_protocol::ThreadItem;
use xedoc_app_server_protocol::ThreadListCwdFilter;
use xedoc_app_server_protocol::ThreadListParams;
use xedoc_app_server_protocol::ThreadSortKey;
use xedoc_app_server_protocol::ThreadSourceKind;
use xedoc_app_server_protocol::ThreadStatus;
use xedoc_config::types::SessionPickerViewMode;
use xedoc_protocol::ThreadId;
use xedoc_tui_frame::FrameRequester;
use xedoc_utils_path as path_utils;

#[derive(Clone, Debug)]
pub enum SessionPickerEvent {
    Key(KeyEvent),
    Paste(String),
    Resize,
    Draw,
}

/// Terminal operations needed to host the session picker event loop.
///
/// Implementations adapt their terminal runtime without exposing picker state
/// or making the picker crate depend on the parent TUI application.
pub trait SessionPickerTui {
    fn enter_alt_screen(&mut self) -> std::io::Result<()>;

    fn leave_alt_screen(&mut self) -> std::io::Result<()>;

    fn frame_requester(&self) -> FrameRequester;

    fn event_stream(
        &self,
    ) -> Pin<Box<dyn tokio_stream::Stream<Item = SessionPickerEvent> + Send + 'static>>;

    fn terminal_size(&self) -> std::io::Result<ratatui::layout::Size>;

    fn viewport_area(&self) -> Rect;

    fn draw(
        &mut self,
        height: u16,
        draw_fn: impl FnOnce(&mut custom_terminal::Frame),
    ) -> std::io::Result<()>;
}

const PAGE_SIZE: usize = 25;
const LOAD_NEAR_THRESHOLD: usize = 5;
const SESSION_META_INDENT_WIDTH: usize = 2;
const SESSION_META_DATE_WIDTH: usize = 12;
const SESSION_META_FIELD_GAP_WIDTH: usize = 2;
const SESSION_META_MIN_CWD_WIDTH: usize = 30;
const SESSION_META_MAX_CWD_WIDTH: usize = 72;
const SESSION_META_BRANCH_ICON: &str = "";
const SESSION_META_CWD_ICON: &str = "⌁";
const FOOTER_COMPACT_BREAKPOINT: u16 = 120;
const FOOTER_HINT_LEFT_PADDING: usize = 1;
const FOOTER_HINT_GAP: usize = 3;
const PICKER_CHROME_HEIGHT: u16 = 8;
const PICKER_LIST_HORIZONTAL_INSET: u16 = 4;

pub fn resume_source_kinds(include_non_interactive: bool) -> Vec<ThreadSourceKind> {
    let mut source_kinds = vec![ThreadSourceKind::Cli, ThreadSourceKind::VsCode];
    if include_non_interactive {
        source_kinds.extend([ThreadSourceKind::Exec, ThreadSourceKind::AppServer]);
    }
    source_kinds
}

#[derive(Debug, Clone)]
pub struct SessionTarget {
    pub path: Option<PathBuf>,
    pub thread_id: ThreadId,
}

impl SessionTarget {
    pub fn display_label(&self) -> String {
        self.path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| format!("thread {}", self.thread_id))
    }
}

#[derive(Debug, Clone)]
pub enum SessionSelection {
    StartFresh,
    Resume(SessionTarget),
    Fork(SessionTarget),
    Exit,
}

#[derive(Clone, Copy, Debug)]
pub enum SessionPickerAction {
    Resume,
    Fork,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPickerLaunchContext {
    Startup,
    ExistingSession,
}

impl SessionPickerAction {
    fn title(self) -> &'static str {
        match self {
            SessionPickerAction::Resume => "Resume a previous session",
            SessionPickerAction::Fork => "Fork a previous session",
        }
    }

    fn action_label(self) -> &'static str {
        match self {
            SessionPickerAction::Resume => "resume",
            SessionPickerAction::Fork => "fork",
        }
    }

    fn selection(self, path: Option<PathBuf>, thread_id: ThreadId) -> SessionSelection {
        let target_session = SessionTarget { path, thread_id };
        match self {
            SessionPickerAction::Resume => SessionSelection::Resume(target_session),
            SessionPickerAction::Fork => SessionSelection::Fork(target_session),
        }
    }
}

#[derive(Clone)]
struct PageLoadRequest {
    cursor: Option<PageCursor>,
    request_token: usize,
    search_token: Option<usize>,
    cwd_filter: Option<PathBuf>,
    provider_filter: ProviderFilter,
    sort_key: ThreadSortKey,
}

enum PickerLoadRequest {
    Page(PageLoadRequest),
    Preview { thread_id: ThreadId },
    Transcript { thread_id: ThreadId },
}

#[derive(Clone)]
enum ProviderFilter {
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionFilterMode {
    Cwd,
    All,
}

impl SessionFilterMode {
    fn from_show_all(show_all: bool, filter_cwd: Option<&Path>) -> Self {
        if show_all || filter_cwd.is_none() {
            Self::All
        } else {
            Self::Cwd
        }
    }

    fn toggle(self, filter_cwd: Option<&Path>) -> Self {
        match self {
            Self::Cwd => Self::All,
            Self::All if filter_cwd.is_some() => Self::Cwd,
            Self::All => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarControl {
    Filter,
    Sort,
}

impl ToolbarControl {
    fn previous(self) -> Self {
        match self {
            Self::Filter => Self::Sort,
            Self::Sort => Self::Filter,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Filter => Self::Sort,
            Self::Sort => Self::Filter,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionListDensity {
    Comfortable,
    Dense,
}

impl SessionListDensity {
    fn toggle(self) -> Self {
        match self {
            Self::Comfortable => Self::Dense,
            Self::Dense => Self::Comfortable,
        }
    }
}

impl From<SessionPickerViewMode> for SessionListDensity {
    fn from(mode: SessionPickerViewMode) -> Self {
        match mode {
            SessionPickerViewMode::Comfortable => Self::Comfortable,
            SessionPickerViewMode::Dense => Self::Dense,
        }
    }
}

impl From<SessionListDensity> for SessionPickerViewMode {
    fn from(density: SessionListDensity) -> Self {
        match density {
            SessionListDensity::Comfortable => Self::Comfortable,
            SessionListDensity::Dense => Self::Dense,
        }
    }
}

type PickerLoader = Arc<dyn Fn(PickerLoadRequest) + Send + Sync>;
enum BackgroundEvent {
    Page {
        request_token: usize,
        search_token: Option<usize>,
        page: std::io::Result<PickerPage>,
    },
    Preview {
        thread_id: ThreadId,
        preview: std::io::Result<Vec<TranscriptPreviewLine>>,
    },
    Transcript {
        thread_id: ThreadId,
        transcript: std::io::Result<TranscriptCells>,
    },
}

#[derive(Clone)]
enum PageCursor {
    AppServer(String),
}

struct PickerPage {
    rows: Vec<Row>,
    next_cursor: Option<PageCursor>,
    num_scanned_files: usize,
    reached_scan_cap: bool,
}

#[derive(Clone)]
struct SessionPickerViewPersistence {
    xedoc_home: PathBuf,
}

struct SessionPickerRunOptions {
    show_all: bool,
    filter_cwd: Option<PathBuf>,
    local_filter_cwd: Option<PathBuf>,
    action: SessionPickerAction,
    launch_context: SessionPickerLaunchContext,
    provider_filter: ProviderFilter,
    initial_density: SessionListDensity,
    view_persistence: Option<SessionPickerViewPersistence>,
    pager_keymap: PagerKeymap,
    list_keymap: ListKeymap,
}

/// Interactive session picker that lists app-server threads with simple search,
/// lazy transcript previews, and pagination.
///
/// Sessions render as compact multi-line records with stable metadata first and
/// the conversation preview last. Users can focus Sort/Filter toolbar controls
/// with Tab, change the focused control with the arrow keys, and expand the
/// selected session with Ctrl+E to load recent transcript context on demand.
///
/// Sessions are loaded on-demand via cursor-based pagination. The backend
/// `thread/list` API returns pages ordered by the selected sort key, and the
/// picker deduplicates across pages to handle overlapping windows when new
/// sessions appear during pagination.
///
/// Filtering happens in two layers:
/// 1. Provider, source, and eligible working-directory filtering at the backend.
/// 2. Typed search filtering over loaded rows in the picker.
pub async fn run_resume_picker_with_app_server(
    tui: &mut impl SessionPickerTui,
    config: &Config,
    show_all: bool,
    include_non_interactive: bool,
    app_server: AppServerSession,
) -> Result<SessionSelection> {
    run_resume_picker_with_launch_context(
        tui,
        config,
        show_all,
        include_non_interactive,
        app_server,
        SessionPickerLaunchContext::Startup,
    )
    .await
}

pub async fn run_resume_picker_from_existing_session_with_app_server(
    tui: &mut impl SessionPickerTui,
    config: &Config,
    show_all: bool,
    include_non_interactive: bool,
    app_server: AppServerSession,
) -> Result<SessionSelection> {
    run_resume_picker_with_launch_context(
        tui,
        config,
        show_all,
        include_non_interactive,
        app_server,
        SessionPickerLaunchContext::ExistingSession,
    )
    .await
}

async fn run_resume_picker_with_launch_context(
    tui: &mut impl SessionPickerTui,
    config: &Config,
    show_all: bool,
    include_non_interactive: bool,
    app_server: AppServerSession,
    launch_context: SessionPickerLaunchContext,
) -> Result<SessionSelection> {
    let (bg_tx, bg_rx) = mpsc::unbounded_channel();
    let uses_remote_workspace = app_server.uses_remote_workspace();
    let cwd_filter = picker_cwd_filter(
        config.cwd.as_path(),
        /*show_all*/ false,
        uses_remote_workspace,
        app_server.remote_cwd_override(),
    );
    let local_filter_cwd = local_picker_cwd_filter(&cwd_filter, uses_remote_workspace);
    let runtime_keymap = picker_runtime_keymap(config)?;
    let options = SessionPickerRunOptions {
        show_all,
        filter_cwd: cwd_filter,
        local_filter_cwd,
        action: SessionPickerAction::Resume,
        launch_context,
        provider_filter: ProviderFilter::Any,
        initial_density: SessionListDensity::from(config.tui_session_picker_view),
        view_persistence: Some(SessionPickerViewPersistence {
            xedoc_home: config.xedoc_home.to_path_buf(),
        }),
        pager_keymap: runtime_keymap.pager,
        list_keymap: runtime_keymap.list,
    };
    run_session_picker_with_loader(
        tui,
        options,
        spawn_app_server_page_loader(
            app_server,
            include_non_interactive,
            raw_reasoning_visibility(config),
            (!uses_remote_workspace).then(|| config.xedoc_home.to_path_buf()),
            bg_tx,
        ),
        bg_rx,
    )
    .await
}

pub async fn run_fork_picker_with_app_server(
    tui: &mut impl SessionPickerTui,
    config: &Config,
    show_all: bool,
    app_server: AppServerSession,
) -> Result<SessionSelection> {
    let (bg_tx, bg_rx) = mpsc::unbounded_channel();
    let uses_remote_workspace = app_server.uses_remote_workspace();
    let cwd_filter = picker_cwd_filter(
        config.cwd.as_path(),
        /*show_all*/ false,
        uses_remote_workspace,
        app_server.remote_cwd_override(),
    );
    let local_filter_cwd = local_picker_cwd_filter(&cwd_filter, uses_remote_workspace);
    let runtime_keymap = picker_runtime_keymap(config)?;
    let options = SessionPickerRunOptions {
        show_all,
        filter_cwd: cwd_filter,
        local_filter_cwd,
        action: SessionPickerAction::Fork,
        launch_context: SessionPickerLaunchContext::Startup,
        provider_filter: ProviderFilter::Any,
        initial_density: SessionListDensity::from(config.tui_session_picker_view),
        view_persistence: Some(SessionPickerViewPersistence {
            xedoc_home: config.xedoc_home.to_path_buf(),
        }),
        pager_keymap: runtime_keymap.pager,
        list_keymap: runtime_keymap.list,
    };
    run_session_picker_with_loader(
        tui,
        options,
        spawn_app_server_page_loader(
            app_server,
            /*include_non_interactive*/ false,
            raw_reasoning_visibility(config),
            (!uses_remote_workspace).then(|| config.xedoc_home.to_path_buf()),
            bg_tx,
        ),
        bg_rx,
    )
    .await
}

async fn run_session_picker_with_loader(
    tui: &mut impl SessionPickerTui,
    options: SessionPickerRunOptions,
    picker_loader: PickerLoader,
    bg_rx: mpsc::UnboundedReceiver<BackgroundEvent>,
) -> Result<SessionSelection> {
    let alt = AltScreenGuard::enter(tui);
    let mut state = PickerState::new(
        alt.tui.frame_requester(),
        picker_loader,
        options.provider_filter,
        options.show_all,
        options.filter_cwd,
        options.action,
    );
    state.local_filter_cwd = options.local_filter_cwd;
    state.density = options.initial_density;
    state.view_persistence = options.view_persistence;
    state.pager_keymap = options.pager_keymap;
    state.list_keymap = options.list_keymap;
    state.launch_context = options.launch_context;
    state.start_initial_load();
    state.request_frame();

    let mut tui_events = alt.tui.event_stream().fuse();
    let mut background_events = UnboundedReceiverStream::new(bg_rx).fuse();

    loop {
        tokio::select! {
            Some(ev) = tui_events.next() => {
                if state.overlay.is_some() {
                    state.handle_overlay_event(alt.tui, ev)?;
                    continue;
                }
                match ev {
                    SessionPickerEvent::Key(key) => {
                        if matches!(key.kind, KeyEventKind::Release) {
                            continue;
                        }
                        if let Some(sel) = state.handle_key(key).await? {
                            return Ok(sel);
                        }
                    }
                    SessionPickerEvent::Paste(pasted) => {
                        state.handle_paste(pasted);
                    }
                    SessionPickerEvent::Draw | SessionPickerEvent::Resize => {
                        if let Ok(size) = alt.tui.terminal_size() {
                            let list_height =
                                size.height.saturating_sub(PICKER_CHROME_HEIGHT) as usize;
                            state.update_viewport(list_height, list_viewport_width(size.width));
                            state.ensure_minimum_rows_for_view(list_height);
                        }
                        draw_picker(alt.tui, &state)?;
                        if state.note_transcript_loading_frame_drawn() {
                            state.open_pending_transcript_if_ready();
                        }
                    }
                }
            }
            Some(event) = background_events.next() => {
                state.handle_background_event(event).await?;
            }
            else => break,
        }
    }

    // Fallback – treat as cancel/new
    Ok(SessionSelection::StartFresh)
}

fn raw_reasoning_visibility(config: &Config) -> RawReasoningVisibility {
    if config.show_raw_agent_reasoning {
        RawReasoningVisibility::Visible
    } else {
        RawReasoningVisibility::Hidden
    }
}

fn local_picker_cwd_filter(
    cwd_filter: &Option<PathBuf>,
    uses_remote_workspace: bool,
) -> Option<PathBuf> {
    if uses_remote_workspace {
        None
    } else {
        cwd_filter.clone()
    }
}

fn picker_runtime_keymap(config: &Config) -> Result<RuntimeKeymap> {
    RuntimeKeymap::from_config(&config.tui_keymap)
        .map_err(|err| color_eyre::eyre::eyre!("invalid keymap configuration: {err}"))
}

fn picker_cwd_filter(
    config_cwd: &Path,
    show_all: bool,
    uses_remote_workspace: bool,
    remote_cwd_override: Option<&Path>,
) -> Option<PathBuf> {
    if show_all {
        None
    } else if uses_remote_workspace {
        remote_cwd_override.map(Path::to_path_buf)
    } else {
        Some(config_cwd.to_path_buf())
    }
}

fn spawn_app_server_page_loader(
    app_server: AppServerSession,
    include_non_interactive: bool,
    raw_reasoning_visibility: RawReasoningVisibility,
    xedoc_home: Option<PathBuf>,
    bg_tx: mpsc::UnboundedSender<BackgroundEvent>,
) -> PickerLoader {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel::<PickerLoadRequest>();

    tokio::spawn(async move {
        let mut app_server = app_server;
        while let Some(request) = request_rx.recv().await {
            match request {
                PickerLoadRequest::Page(request) => {
                    let cursor = request.cursor.map(|PageCursor::AppServer(cursor)| cursor);
                    let page = load_app_server_page(
                        &mut app_server,
                        cursor,
                        request.cwd_filter.as_deref(),
                        request.provider_filter,
                        request.sort_key,
                        include_non_interactive,
                    )
                    .await;
                    let _ = bg_tx.send(BackgroundEvent::Page {
                        request_token: request.request_token,
                        search_token: request.search_token,
                        page,
                    });
                }
                PickerLoadRequest::Preview { thread_id } => {
                    let preview =
                        load_transcript_preview(&mut app_server, thread_id, xedoc_home.as_deref())
                            .await;
                    let _ = bg_tx.send(BackgroundEvent::Preview { thread_id, preview });
                }
                PickerLoadRequest::Transcript { thread_id } => {
                    let transcript = load_session_transcript(
                        &mut app_server,
                        thread_id,
                        raw_reasoning_visibility,
                        xedoc_home.as_deref(),
                    )
                    .await;
                    let _ = bg_tx.send(BackgroundEvent::Transcript {
                        thread_id,
                        transcript,
                    });
                }
            }
        }
        if let Err(err) = app_server.shutdown().await {
            warn!(%err, "Failed to shut down app-server picker session");
        }
    });

    Arc::new(move |request: PickerLoadRequest| {
        let _ = request_tx.send(request);
    })
}

/// Returns the human-readable column header for the given sort key.
fn sort_key_label(sort_key: ThreadSortKey) -> &'static str {
    match sort_key {
        ThreadSortKey::CreatedAt => "Created",
        ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt => "Updated",
    }
}

/// RAII guard that ensures we leave the alt-screen on scope exit.
struct AltScreenGuard<'a, T: SessionPickerTui> {
    tui: &'a mut T,
}

impl<'a, T: SessionPickerTui> AltScreenGuard<'a, T> {
    fn enter(tui: &'a mut T) -> Self {
        let _ = tui.enter_alt_screen();
        Self { tui }
    }
}

impl<T: SessionPickerTui> Drop for AltScreenGuard<'_, T> {
    fn drop(&mut self) {
        let _ = self.tui.leave_alt_screen();
    }
}

struct PickerState {
    requester: FrameRequester,
    relative_time_reference: Option<DateTime<Utc>>,
    pagination: PaginationState,
    all_rows: Vec<Row>,
    filtered_rows: Vec<Row>,
    seen_rows: HashSet<SeenRowKey>,
    selected: usize,
    scroll_top: usize,
    pending_page_down_target: Option<usize>,
    frozen_footer_percent: Option<u8>,
    query: String,
    search_state: SearchState,
    next_request_token: usize,
    next_search_token: usize,
    picker_loader: PickerLoader,
    view_rows: Option<usize>,
    view_width: Option<u16>,
    provider_filter: ProviderFilter,
    filter_mode: SessionFilterMode,
    filter_cwd: Option<PathBuf>,
    local_filter_cwd: Option<PathBuf>,
    toolbar_focus: ToolbarControl,
    density: SessionListDensity,
    launch_context: SessionPickerLaunchContext,
    view_persistence: Option<SessionPickerViewPersistence>,
    action: SessionPickerAction,
    sort_key: ThreadSortKey,
    inline_error: Option<String>,
    expanded_thread_id: Option<ThreadId>,
    transcript_previews: HashMap<ThreadId, TranscriptPreviewState>,
    transcript_cells: HashMap<ThreadId, SessionTranscriptState>,
    pending_transcript_open: Option<ThreadId>,
    transcript_loading_frame_shown: bool,
    overlay: Option<Overlay>,
    pager_keymap: PagerKeymap,
    list_keymap: ListKeymap,
}

struct PaginationState {
    next_cursor: Option<PageCursor>,
    num_scanned_files: usize,
    reached_scan_cap: bool,
    loading: LoadingState,
}

#[derive(Clone, Copy, Debug)]
enum LoadingState {
    Idle,
    Pending(PendingLoad),
}

#[derive(Clone, Copy, Debug)]
struct PendingLoad {
    request_token: usize,
    search_token: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
enum SearchState {
    Idle,
    Active { token: usize },
}

#[derive(Clone)]
enum TranscriptPreviewState {
    Loading,
    Loaded(Vec<TranscriptPreviewLine>),
    Failed,
}

enum SessionTranscriptState {
    Loading,
    Loaded(TranscriptCells),
    Failed,
}

#[derive(Clone)]
struct TranscriptPreviewLine {
    speaker: TranscriptPreviewSpeaker,
    text: String,
}

#[derive(Clone, Copy)]
enum TranscriptPreviewSpeaker {
    User,
    Assistant,
}

enum LoadTrigger {
    Scroll,
    Search { token: usize },
}

impl LoadingState {
    fn is_pending(&self) -> bool {
        matches!(self, LoadingState::Pending(_))
    }
}

async fn load_app_server_page(
    app_server: &mut AppServerSession,
    cursor: Option<String>,
    cwd_filter: Option<&Path>,
    provider_filter: ProviderFilter,
    sort_key: ThreadSortKey,
    include_non_interactive: bool,
) -> std::io::Result<PickerPage> {
    let response = app_server
        .thread_list(thread_list_params(
            cursor,
            cwd_filter,
            provider_filter,
            sort_key,
            include_non_interactive,
        ))
        .await
        .map_err(std::io::Error::other)?;
    let num_scanned_files = response.data.len();

    Ok(PickerPage {
        rows: response
            .data
            .into_iter()
            .filter_map(row_from_app_server_thread)
            .collect(),
        next_cursor: response.next_cursor.map(PageCursor::AppServer),
        num_scanned_files,
        reached_scan_cap: false,
    })
}

async fn load_transcript_preview(
    app_server: &mut AppServerSession,
    thread_id: ThreadId,
    xedoc_home: Option<&Path>,
) -> std::io::Result<Vec<TranscriptPreviewLine>> {
    const MAX_PREVIEW_LINES: usize = 6;

    let thread = app_server
        .thread_read(thread_id, /*include_turns*/ true)
        .await
        .map_err(std::io::Error::other)?;
    let cwd = thread.cwd.as_path();
    let inline_visualization_context = xedoc_home.and_then(|xedoc_home| {
        ThreadId::from_string(&thread.id)
            .ok()
            .and_then(|thread_id| InlineVisualizationContext::new(xedoc_home, thread_id))
    });
    let mut lines = thread
        .turns
        .iter()
        .flat_map(|turn| turn.items.iter())
        .filter_map(|item| match item {
            ThreadItem::UserMessage { content, .. } => Some(TranscriptPreviewLine {
                speaker: TranscriptPreviewSpeaker::User,
                text: content
                    .iter()
                    .filter_map(|input| match input {
                        xedoc_app_server_protocol::UserInput::Text { text, .. } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            }),
            ThreadItem::AgentMessage { text, .. } => {
                let visible_markdown = parse_assistant_markdown(text, cwd).visible_markdown;
                let rewritten = crate::inline_visualization::rewrite_inline_visualizations(
                    &visible_markdown,
                    inline_visualization_context.as_ref(),
                );
                let mut text = rewritten.markdown.into_owned();
                for (placeholder, link) in &rewritten.trusted_file_links {
                    text = text.replace(
                        &format!(
                            "{}  \n[{}]({placeholder})",
                            link.markdown_label, link.markdown_destination_label
                        ),
                        &format!("{}  \n{}", link.display_label, link.destination),
                    );
                }
                Some(TranscriptPreviewLine {
                    speaker: TranscriptPreviewSpeaker::Assistant,
                    text,
                })
            }
            _ => None,
        })
        .flat_map(|line| {
            line.text
                .lines()
                .filter(|text| !text.trim().is_empty())
                .map(move |text| TranscriptPreviewLine {
                    speaker: line.speaker,
                    text: text.trim().to_string(),
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if lines.len() > MAX_PREVIEW_LINES {
        lines.drain(..lines.len() - MAX_PREVIEW_LINES);
    }
    Ok(lines)
}

impl SearchState {
    fn active_token(&self) -> Option<usize> {
        match self {
            SearchState::Idle => None,
            SearchState::Active { token } => Some(*token),
        }
    }

    fn is_active(&self) -> bool {
        self.active_token().is_some()
    }
}

#[derive(Clone)]
struct Row {
    path: Option<PathBuf>,
    preview: String,
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
    is_running: bool,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    cwd: Option<PathBuf>,
    git_branch: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SeenRowKey {
    Path(PathBuf),
    Thread(ThreadId),
}

impl Row {
    fn seen_key(&self) -> Option<SeenRowKey> {
        if let Some(path) = self.path.clone() {
            return Some(SeenRowKey::Path(path));
        }
        self.thread_id.map(SeenRowKey::Thread)
    }

    fn display_preview(&self) -> &str {
        self.thread_name.as_deref().unwrap_or(&self.preview)
    }

    fn matches_query(&self, query: &str) -> bool {
        if self.preview.to_lowercase().contains(query) {
            return true;
        }
        if let Some(thread_name) = self.thread_name.as_ref()
            && thread_name.to_lowercase().contains(query)
        {
            return true;
        }
        if self
            .thread_id
            .is_some_and(|thread_id| thread_id.to_string().to_lowercase().contains(query))
        {
            return true;
        }
        if self
            .git_branch
            .as_ref()
            .is_some_and(|branch| branch.to_lowercase().contains(query))
        {
            return true;
        }
        if self
            .cwd
            .as_ref()
            .is_some_and(|cwd| cwd.to_string_lossy().to_lowercase().contains(query))
        {
            return true;
        }
        false
    }
}

impl PickerState {
    fn new(
        requester: FrameRequester,
        picker_loader: PickerLoader,
        provider_filter: ProviderFilter,
        show_all: bool,
        filter_cwd: Option<PathBuf>,
        action: SessionPickerAction,
    ) -> Self {
        Self {
            requester,
            relative_time_reference: None,
            pagination: PaginationState {
                next_cursor: None,
                num_scanned_files: 0,
                reached_scan_cap: false,
                loading: LoadingState::Idle,
            },
            all_rows: Vec::new(),
            filtered_rows: Vec::new(),
            seen_rows: HashSet::new(),
            selected: 0,
            scroll_top: 0,
            pending_page_down_target: None,
            frozen_footer_percent: None,
            query: String::new(),
            search_state: SearchState::Idle,
            next_request_token: 0,
            next_search_token: 0,
            picker_loader,
            view_rows: None,
            view_width: None,
            provider_filter,
            filter_mode: SessionFilterMode::from_show_all(show_all, filter_cwd.as_deref()),
            local_filter_cwd: filter_cwd.clone(),
            filter_cwd,
            toolbar_focus: ToolbarControl::Filter,
            density: SessionListDensity::Comfortable,
            launch_context: SessionPickerLaunchContext::Startup,
            view_persistence: None,
            action,
            sort_key: ThreadSortKey::UpdatedAt,
            inline_error: None,
            expanded_thread_id: None,
            transcript_previews: HashMap::new(),
            transcript_cells: HashMap::new(),
            pending_transcript_open: None,
            transcript_loading_frame_shown: false,
            overlay: None,
            pager_keymap: RuntimeKeymap::defaults().pager,
            list_keymap: RuntimeKeymap::defaults().list,
        }
    }

    fn request_frame(&self) {
        self.requester.schedule_frame();
    }

    fn is_transcript_loading(&self) -> bool {
        self.pending_transcript_open.is_some()
    }

    fn note_transcript_loading_frame_drawn(&mut self) -> bool {
        if self.pending_transcript_open.is_some() {
            self.transcript_loading_frame_shown = true;
            true
        } else {
            false
        }
    }

    fn open_pending_transcript_if_ready(&mut self) {
        if !self.transcript_loading_frame_shown {
            return;
        }
        let Some(thread_id) = self.pending_transcript_open else {
            return;
        };
        let Some(SessionTranscriptState::Loaded(cells)) = self.transcript_cells.get(&thread_id)
        else {
            return;
        };
        self.overlay = Some(Overlay::new_transcript(
            cells.clone(),
            self.pager_keymap.clone(),
        ));
        self.pending_transcript_open = None;
        self.transcript_loading_frame_shown = false;
        self.request_frame();
    }

    fn begin_transcript_loading(&mut self, thread_id: ThreadId) {
        self.pending_transcript_open = Some(thread_id);
        self.transcript_loading_frame_shown = false;
        self.request_frame();
    }

    fn handle_overlay_event(
        &mut self,
        tui: &mut impl SessionPickerTui,
        event: SessionPickerEvent,
    ) -> Result<()> {
        let Some(overlay) = &mut self.overlay else {
            return Ok(());
        };
        match event {
            SessionPickerEvent::Key(key_event) => {
                overlay.handle_key_event(tui.viewport_area(), &tui.frame_requester(), key_event);
            }
            SessionPickerEvent::Draw | SessionPickerEvent::Resize => {
                tui.draw(u16::MAX, |frame| {
                    overlay.render(frame.area(), frame.buffer_mut());
                })?;
            }
            SessionPickerEvent::Paste(_) => {}
        }
        if overlay.is_done() {
            self.overlay = None;
            self.request_frame();
        }
        Ok(())
    }

    fn open_selected_transcript(&mut self) {
        let Some(row) = self.filtered_rows.get(self.selected) else {
            return;
        };
        let Some(thread_id) = row.thread_id else {
            self.inline_error = Some("No transcript available for this session".to_string());
            self.request_frame();
            return;
        };

        match self.transcript_cells.get(&thread_id) {
            Some(SessionTranscriptState::Loaded(_)) => {
                self.begin_transcript_loading(thread_id);
            }
            Some(SessionTranscriptState::Loading) => {
                self.begin_transcript_loading(thread_id);
            }
            Some(SessionTranscriptState::Failed) | None => {
                self.transcript_cells
                    .insert(thread_id, SessionTranscriptState::Loading);
                self.begin_transcript_loading(thread_id);
                (self.picker_loader)(PickerLoadRequest::Transcript { thread_id });
            }
        }
    }

    fn handle_transcript_loading_key(&mut self, key: KeyEvent) -> Option<SessionSelection> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => Some(SessionSelection::Exit),
            _ => None,
        }
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<Option<SessionSelection>> {
        self.inline_error = None;
        if self.is_transcript_loading() {
            return Ok(self.handle_transcript_loading_key(key));
        }
        if !self.list_keymap.page_down.is_pressed(key) {
            self.pending_page_down_target = None;
        }
        // The session picker is always searchable, so plain text belongs to
        // the query first. Modified list bindings still route through the
        // runtime keymap below.
        let allow_plain_char_navigation = !is_plain_text_key_event(key);
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(Some(SessionSelection::Exit));
            }
            _ if self.list_keymap.cancel.is_pressed(key) => {
                if self.query.is_empty() {
                    return Ok(Some(SessionSelection::StartFresh));
                }
                self.clear_query_preserving_selection();
            }
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_selected_transcript();
            }
            KeyEvent {
                code: KeyCode::Char('e'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_selected_expansion();
            }
            KeyEvent {
                code: KeyCode::Char('\u{0014}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^T */ => {
                self.open_selected_transcript();
            }
            KeyEvent {
                code: KeyCode::Char('\u{0005}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^E */ => {
                self.toggle_selected_expansion();
            }
            KeyEvent {
                code: KeyCode::Char('o'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_density().await;
            }
            KeyEvent {
                code: KeyCode::Char('\u{000f}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^O */ => {
                self.toggle_density().await;
            }
            _ if self.list_keymap.accept.is_pressed(key) => {
                if let Some(row) = self.filtered_rows.get(self.selected) {
                    let path = row.path.clone();
                    let thread_id = match row.thread_id {
                        Some(thread_id) => Some(thread_id),
                        None => match path.as_ref() {
                            Some(path) => {
                                resolve_session_thread_id(path.as_path(), /*id_str_if_uuid*/ None)
                                    .await
                            }
                            None => None,
                        },
                    };
                    if let Some(thread_id) = thread_id {
                        return Ok(Some(self.action.selection(path, thread_id)));
                    }
                    self.inline_error = Some(match path {
                        Some(path) => {
                            format!("Failed to read session metadata from {}", path.display())
                        }
                        None => {
                            String::from("Failed to read session metadata from selected session")
                        }
                    });
                    self.request_frame();
                }
            }
            _ if allow_plain_char_navigation && self.list_keymap.move_up.is_pressed(key) => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.ensure_selected_visible();
                }
                self.request_frame();
            }
            _ if allow_plain_char_navigation && self.list_keymap.move_down.is_pressed(key) => {
                if self.selected + 1 < self.filtered_rows.len() {
                    self.selected += 1;
                    self.ensure_selected_visible();
                }
                self.maybe_load_more_for_scroll();
                self.request_frame();
            }
            _ if allow_plain_char_navigation && self.list_keymap.page_up.is_pressed(key) => {
                let step = self.view_rows.unwrap_or(10).max(1);
                if self.selected > 0 {
                    self.selected = self.selected.saturating_sub(step);
                    self.ensure_selected_visible();
                    self.request_frame();
                }
            }
            _ if allow_plain_char_navigation && self.list_keymap.jump_top.is_pressed(key)
                && !self.filtered_rows.is_empty() => {
                    self.selected = 0;
                    self.ensure_selected_visible();
                    self.request_frame();
                }
            _ if allow_plain_char_navigation && self.list_keymap.jump_bottom.is_pressed(key)
                && !self.filtered_rows.is_empty() => {
                    self.selected = self.filtered_rows.len().saturating_sub(1);
                    self.ensure_selected_visible();
                    self.maybe_load_more_for_scroll();
                    self.request_frame();
                }
            _ if allow_plain_char_navigation && self.list_keymap.page_down.is_pressed(key)
                && !self.filtered_rows.is_empty() => {
                    let step = self.view_rows.unwrap_or(10).max(1);
                    let target = self.selected.saturating_add(step);
                    let max_index = self.filtered_rows.len().saturating_sub(1);
                    if target > max_index && self.pagination.next_cursor.is_some() {
                        self.pending_page_down_target = Some(target);
                        self.load_more_if_needed(LoadTrigger::Scroll);
                    } else {
                        self.selected = target.min(max_index);
                        self.ensure_selected_visible();
                        self.maybe_load_more_for_scroll();
                    }
                    self.request_frame();
                }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                self.focus_next_toolbar_control();
                self.request_frame();
            }
            KeyEvent {
                code: KeyCode::BackTab,
                ..
            } => {
                self.focus_previous_toolbar_control();
                self.request_frame();
            }
            _ if allow_plain_char_navigation
                && (self.list_keymap.move_left.is_pressed(key)
                    || self.list_keymap.move_right.is_pressed(key)) =>
            {
                self.change_focused_toolbar_value();
                self.request_frame();
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                let mut new_query = self.query.clone();
                new_query.pop();
                self.set_query(new_query);
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            }
                // basic text input for search
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT)
                => {
                    let mut new_query = self.query.clone();
                    new_query.push(c);
                    self.set_query(new_query);
                }
            _ => {}
        }
        Ok(None)
    }

    fn handle_paste(&mut self, pasted: String) {
        if self.is_transcript_loading() {
            return;
        }
        let Some(pasted) = normalize_pasted_search_query(&pasted) else {
            return;
        };
        let mut new_query = self.query.clone();
        if !new_query.is_empty() && !new_query.ends_with(char::is_whitespace) {
            new_query.push(' ');
        }
        new_query.push_str(&pasted);
        self.set_query(new_query);
    }

    fn start_initial_load(&mut self) {
        self.relative_time_reference = Some(Utc::now());
        self.reset_pagination();
        self.all_rows.clear();
        self.filtered_rows.clear();
        self.seen_rows.clear();
        self.selected = 0;
        self.pending_page_down_target = None;
        self.frozen_footer_percent = None;

        let search_token = if self.query.is_empty() {
            self.search_state = SearchState::Idle;
            None
        } else {
            let token = self.allocate_search_token();
            self.search_state = SearchState::Active { token };
            Some(token)
        };

        let request_token = self.allocate_request_token();
        self.pagination.loading = LoadingState::Pending(PendingLoad {
            request_token,
            search_token,
        });
        self.request_frame();

        (self.picker_loader)(PickerLoadRequest::Page(PageLoadRequest {
            cursor: None,
            request_token,
            search_token,
            cwd_filter: self.active_cwd_filter(),
            provider_filter: self.provider_filter.clone(),
            sort_key: self.sort_key,
        }));
    }

    async fn handle_background_event(&mut self, event: BackgroundEvent) -> Result<()> {
        match event {
            BackgroundEvent::Page {
                request_token,
                search_token,
                page,
            } => {
                let pending = match self.pagination.loading {
                    LoadingState::Pending(pending) => pending,
                    LoadingState::Idle => return Ok(()),
                };
                if pending.request_token != request_token {
                    return Ok(());
                }
                self.pagination.loading = LoadingState::Idle;
                let page = page.map_err(color_eyre::Report::from)?;
                self.ingest_page(page);
                self.complete_pending_page_down();
                let completed_token = pending.search_token.or(search_token);
                self.continue_search_if_token_matches(completed_token);
            }
            BackgroundEvent::Preview { thread_id, preview } => {
                self.transcript_previews.insert(
                    thread_id,
                    match preview {
                        Ok(lines) => TranscriptPreviewState::Loaded(lines),
                        Err(_) => TranscriptPreviewState::Failed,
                    },
                );
                self.request_frame();
            }
            BackgroundEvent::Transcript {
                thread_id,
                transcript,
            } => match transcript {
                Ok(cells) => {
                    let should_open = self.pending_transcript_open == Some(thread_id);
                    self.transcript_cells
                        .insert(thread_id, SessionTranscriptState::Loaded(cells.clone()));
                    if should_open {
                        self.open_pending_transcript_if_ready();
                    }
                    self.request_frame();
                }
                Err(_) => {
                    self.transcript_cells
                        .insert(thread_id, SessionTranscriptState::Failed);
                    if self.pending_transcript_open == Some(thread_id) {
                        self.pending_transcript_open = None;
                        self.transcript_loading_frame_shown = false;
                        self.inline_error = Some("Could not load transcript preview".to_string());
                    }
                    self.request_frame();
                }
            },
        }
        Ok(())
    }

    fn reset_pagination(&mut self) {
        self.pagination.next_cursor = None;
        self.pagination.num_scanned_files = 0;
        self.pagination.reached_scan_cap = false;
        self.pagination.loading = LoadingState::Idle;
        self.frozen_footer_percent = None;
    }

    fn ingest_page(&mut self, page: PickerPage) {
        if let Some(cursor) = page.next_cursor.clone() {
            self.pagination.next_cursor = Some(cursor);
        } else {
            self.pagination.next_cursor = None;
        }
        self.pagination.num_scanned_files = self
            .pagination
            .num_scanned_files
            .saturating_add(page.num_scanned_files);
        if page.reached_scan_cap {
            self.pagination.reached_scan_cap = true;
        }

        for row in page.rows {
            if let Some(seen_key) = row.seen_key() {
                if self.seen_rows.insert(seen_key) {
                    self.all_rows.push(row);
                }
            } else {
                self.all_rows.push(row);
            }
        }

        self.apply_filter();
    }

    fn complete_pending_page_down(&mut self) {
        let Some(target) = self.pending_page_down_target else {
            return;
        };
        if self.filtered_rows.is_empty() {
            return;
        }

        let max_index = self.filtered_rows.len().saturating_sub(1);
        if target > max_index && self.pagination.next_cursor.is_some() {
            self.load_more_if_needed(LoadTrigger::Scroll);
            return;
        }

        self.pending_page_down_target = None;
        self.selected = target.min(max_index);
        self.ensure_selected_visible();
        self.maybe_load_more_for_scroll();
        self.request_frame();
    }

    fn apply_filter(&mut self) {
        let base_iter = self
            .all_rows
            .iter()
            .filter(|row| self.row_matches_filter(row));
        if self.query.is_empty() {
            self.filtered_rows = base_iter.cloned().collect();
        } else {
            let q = self.query.to_lowercase();
            self.filtered_rows = base_iter.filter(|r| r.matches_query(&q)).cloned().collect();
        }
        if self.selected >= self.filtered_rows.len() {
            self.selected = self.filtered_rows.len().saturating_sub(1);
        }
        if self.filtered_rows.is_empty() {
            self.scroll_top = 0;
        }
        self.ensure_selected_visible();
        self.request_frame();
    }

    fn row_matches_filter(&self, row: &Row) -> bool {
        if self.filter_mode == SessionFilterMode::All {
            return true;
        }
        let Some(filter_cwd) = self.local_filter_cwd.as_ref() else {
            return true;
        };
        let Some(row_cwd) = row.cwd.as_ref() else {
            return false;
        };
        paths_match(row_cwd, filter_cwd)
    }

    fn set_query(&mut self, new_query: String) {
        if self.query == new_query {
            return;
        }
        self.query = new_query;
        self.selected = 0;
        self.apply_filter();
        if self.query.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if !self.filtered_rows.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if self.pagination.reached_scan_cap || self.pagination.next_cursor.is_none() {
            self.search_state = SearchState::Idle;
            return;
        }
        let token = self.allocate_search_token();
        self.search_state = SearchState::Active { token };
        self.load_more_if_needed(LoadTrigger::Search { token });
    }

    fn clear_query_preserving_selection(&mut self) {
        let selected_key = self
            .filtered_rows
            .get(self.selected)
            .and_then(Row::seen_key);
        self.query.clear();
        self.search_state = SearchState::Idle;
        self.apply_filter();
        if let Some(selected_key) = selected_key
            && let Some(index) = self
                .filtered_rows
                .iter()
                .position(|row| row.seen_key().as_ref() == Some(&selected_key))
        {
            self.selected = index;
            self.ensure_selected_visible();
            self.request_frame();
        }
    }

    fn continue_search_if_needed(&mut self) {
        let Some(token) = self.search_state.active_token() else {
            return;
        };
        if !self.filtered_rows.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if self.pagination.reached_scan_cap || self.pagination.next_cursor.is_none() {
            self.search_state = SearchState::Idle;
            return;
        }
        self.load_more_if_needed(LoadTrigger::Search { token });
    }

    fn continue_search_if_token_matches(&mut self, completed_token: Option<usize>) {
        let Some(active) = self.search_state.active_token() else {
            return;
        };
        if let Some(token) = completed_token
            && token != active
        {
            return;
        }
        self.continue_search_if_needed();
    }

    fn ensure_selected_visible(&mut self) {
        if self.filtered_rows.is_empty() {
            self.scroll_top = 0;
            return;
        }
        let viewport_rows = self.view_rows.unwrap_or(usize::MAX).max(1);
        if self.selected < self.scroll_top {
            self.scroll_top = self.selected;
        }
        while self.rendered_height_between(self.scroll_top, self.selected)
            > self.available_content_rows(viewport_rows)
            && self.scroll_top < self.selected
        {
            self.scroll_top += 1;
        }
    }

    fn ensure_minimum_rows_for_view(&mut self, minimum_rows: usize) {
        if minimum_rows == 0 {
            return;
        }
        if self.pagination.loading.is_pending() || self.pagination.next_cursor.is_none() {
            return;
        }
        let rendered_rows = if self.filtered_rows.is_empty() {
            0
        } else {
            self.rendered_height_between(/*start*/ 0, self.filtered_rows.len() - 1)
        };
        if rendered_rows >= self.available_content_rows(minimum_rows) {
            return;
        }
        if let Some(token) = self.search_state.active_token() {
            self.load_more_if_needed(LoadTrigger::Search { token });
        } else {
            self.load_more_if_needed(LoadTrigger::Scroll);
        }
    }

    fn update_viewport(&mut self, rows: usize, width: u16) {
        self.view_rows = if rows == 0 { None } else { Some(rows) };
        self.view_width = Some(width);
        self.ensure_selected_visible();
    }

    fn maybe_load_more_for_scroll(&mut self) {
        if self.pagination.loading.is_pending() {
            return;
        }
        if self.pagination.next_cursor.is_none() {
            return;
        }
        if self.filtered_rows.is_empty() {
            return;
        }
        let remaining = self.filtered_rows.len().saturating_sub(self.selected + 1);
        if remaining <= LOAD_NEAR_THRESHOLD {
            self.load_more_if_needed(LoadTrigger::Scroll);
        }
    }

    fn load_more_if_needed(&mut self, trigger: LoadTrigger) {
        if self.pagination.loading.is_pending() {
            return;
        }
        let Some(cursor) = self.pagination.next_cursor.clone() else {
            return;
        };
        self.freeze_footer_percent();
        let request_token = self.allocate_request_token();
        let search_token = match trigger {
            LoadTrigger::Scroll => None,
            LoadTrigger::Search { token } => Some(token),
        };
        self.pagination.loading = LoadingState::Pending(PendingLoad {
            request_token,
            search_token,
        });
        self.request_frame();

        (self.picker_loader)(PickerLoadRequest::Page(PageLoadRequest {
            cursor: Some(cursor),
            request_token,
            search_token,
            cwd_filter: self.active_cwd_filter(),
            provider_filter: self.provider_filter.clone(),
            sort_key: self.sort_key,
        }));
    }

    fn freeze_footer_percent(&mut self) {
        let list_height = self.view_rows.unwrap_or_default().min(u16::MAX as usize) as u16;
        self.frozen_footer_percent = Some(picker_footer_scroll_percent(self, list_height));
    }

    fn allocate_request_token(&mut self) -> usize {
        let token = self.next_request_token;
        self.next_request_token = self.next_request_token.wrapping_add(1);
        token
    }

    fn allocate_search_token(&mut self) -> usize {
        let token = self.next_search_token;
        self.next_search_token = self.next_search_token.wrapping_add(1);
        token
    }

    /// Cycles the sort order between creation time and last-updated time.
    ///
    /// Triggers a full reload because the backend must re-sort all sessions.
    /// The existing `all_rows` are cleared and pagination restarts from the
    /// beginning with the new sort key.
    fn toggle_sort_key(&mut self) {
        self.sort_key = match self.sort_key {
            ThreadSortKey::CreatedAt => ThreadSortKey::UpdatedAt,
            ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt => ThreadSortKey::CreatedAt,
        };
        self.start_initial_load();
    }

    fn toggle_filter_mode(&mut self) {
        let next_filter_mode = self.filter_mode.toggle(self.filter_cwd.as_deref());
        if self.filter_mode == next_filter_mode {
            return;
        }
        self.filter_mode = next_filter_mode;
        self.start_initial_load();
    }

    fn active_cwd_filter(&self) -> Option<PathBuf> {
        match self.filter_mode {
            SessionFilterMode::Cwd => self.filter_cwd.clone(),
            SessionFilterMode::All => None,
        }
    }

    fn focus_previous_toolbar_control(&mut self) {
        self.toolbar_focus = self.toolbar_focus.previous();
    }

    fn focus_next_toolbar_control(&mut self) {
        self.toolbar_focus = self.toolbar_focus.next();
    }

    fn change_focused_toolbar_value(&mut self) {
        match self.toolbar_focus {
            ToolbarControl::Sort => self.toggle_sort_key(),
            ToolbarControl::Filter => self.toggle_filter_mode(),
        }
    }

    async fn toggle_density(&mut self) {
        self.density = self.density.toggle();
        self.ensure_selected_visible();
        if let Err(err) = self.persist_density().await {
            warn!(error = %err, "failed to persist session picker view mode");
            self.inline_error = Some(format!("Failed to save view mode: {err}"));
        }
        self.request_frame();
    }

    async fn persist_density(&self) -> Result<()> {
        let Some(persistence) = &self.view_persistence else {
            return Ok(());
        };

        ConfigEditsBuilder::new(&persistence.xedoc_home)
            .set_session_picker_view(SessionPickerViewMode::from(self.density))
            .apply()
            .await
            .map_err(|err| color_eyre::eyre::eyre!("failed to write config.toml: {err}"))?;

        Ok(())
    }

    fn toggle_selected_expansion(&mut self) {
        let Some(row) = self.filtered_rows.get(self.selected) else {
            return;
        };
        let Some(thread_id) = row.thread_id else {
            return;
        };
        if self.expanded_thread_id == Some(thread_id) {
            self.expanded_thread_id = None;
            self.request_frame();
            return;
        }
        self.expanded_thread_id = Some(thread_id);
        if let std::collections::hash_map::Entry::Vacant(e) =
            self.transcript_previews.entry(thread_id)
        {
            e.insert(TranscriptPreviewState::Loading);
            (self.picker_loader)(PickerLoadRequest::Preview { thread_id });
        }
        self.request_frame();
    }

    fn rendered_height_between(&self, start: usize, end_inclusive: usize) -> usize {
        self.filtered_rows
            .get(start..=end_inclusive)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(offset, row)| {
                let row_idx = start + offset;
                let is_selected = row_idx == self.selected;
                let is_expanded = is_selected
                    && row.thread_id.is_some()
                    && self.expanded_thread_id == row.thread_id;
                render_session_lines(
                    row,
                    self,
                    is_selected,
                    is_expanded,
                    /*is_zebra*/ false,
                    self.view_width.unwrap_or(u16::MAX),
                )
                .len()
            })
            .sum::<usize>()
            + self.row_separator_height() * end_inclusive.saturating_sub(start)
    }

    fn has_more_above(&self) -> bool {
        self.scroll_top > 0
    }

    fn has_more_below(&self, viewport_height: usize) -> bool {
        if self.filtered_rows.is_empty() {
            return false;
        }
        if self.pagination.next_cursor.is_some() {
            return true;
        }
        let capacity = self.available_content_rows(viewport_height);
        let mut used = 0usize;
        for (offset, row) in self.filtered_rows[self.scroll_top..].iter().enumerate() {
            let row_idx = self.scroll_top + offset;
            let is_selected = row_idx == self.selected;
            let is_expanded =
                is_selected && row.thread_id.is_some() && self.expanded_thread_id == row.thread_id;
            let row_height = render_session_lines(
                row,
                self,
                is_selected,
                is_expanded,
                /*is_zebra*/ false,
                self.view_width.unwrap_or(u16::MAX),
            )
            .len();
            let separator_height = usize::from(offset > 0) * self.row_separator_height();
            if used + separator_height + row_height > capacity {
                return true;
            }
            used += separator_height + row_height;
        }
        false
    }

    fn available_content_rows(&self, viewport_height: usize) -> usize {
        viewport_height
            .saturating_sub(usize::from(self.has_more_above()))
            .saturating_sub(usize::from(
                self.pagination.next_cursor.is_some()
                    || self.selected + 1 < self.filtered_rows.len(),
            ))
            .max(1)
    }

    fn row_separator_height(&self) -> usize {
        match self.density {
            SessionListDensity::Comfortable => 1,
            SessionListDensity::Dense => 0,
        }
    }
}

fn row_from_app_server_thread(thread: Thread) -> Option<Row> {
    let thread_id = match ThreadId::from_string(&thread.id) {
        Ok(thread_id) => thread_id,
        Err(err) => {
            warn!(thread_id = thread.id, %err, "Skipping app-server picker row with invalid id");
            return None;
        }
    };
    let preview = thread.preview.trim();
    Some(Row {
        path: thread.path,
        preview: if preview.is_empty() {
            String::from("(no message yet)")
        } else {
            preview.to_string()
        },
        thread_id: Some(thread_id),
        thread_name: thread.name,
        is_running: matches!(thread.status, ThreadStatus::Active { .. }),
        created_at: chrono::DateTime::from_timestamp(thread.created_at, 0)
            .map(|dt| dt.with_timezone(&Utc)),
        updated_at: chrono::DateTime::from_timestamp(thread.updated_at, 0)
            .map(|dt| dt.with_timezone(&Utc)),
        cwd: Some(thread.cwd.to_path_buf()),
        git_branch: thread.git_info.and_then(|git_info| git_info.branch),
    })
}

fn thread_list_params(
    cursor: Option<String>,
    cwd_filter: Option<&Path>,
    provider_filter: ProviderFilter,
    sort_key: ThreadSortKey,
    include_non_interactive: bool,
) -> ThreadListParams {
    ThreadListParams {
        cursor,
        limit: Some(PAGE_SIZE as u32),
        sort_key: Some(sort_key),
        sort_direction: None,
        model_providers: match provider_filter {
            ProviderFilter::Any => Some(Vec::new()),
        },
        source_kinds: Some(crate::resume_source_kinds(include_non_interactive)),
        archived: Some(false),
        parent_thread_id: None,
        ancestor_thread_id: None,
        cwd: cwd_filter.map(|cwd| ThreadListCwdFilter::One(cwd.to_string_lossy().into_owned())),
        use_state_db_only: true,
        search_term: None,
    }
}

fn paths_match(a: &Path, b: &Path) -> bool {
    path_utils::paths_match_after_normalization(a, b)
}

#[cfg_attr(not(test), allow(dead_code))]
fn parse_timestamp_str(ts: &str) -> Option<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}

fn draw_picker(tui: &mut impl SessionPickerTui, state: &PickerState) -> std::io::Result<()> {
    // Render full-screen overlay
    let height = tui.terminal_size()?.height;
    tui.draw(height, |frame| {
        let area = frame.area();
        let [header, _header_gap, search, _search_gap, list, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(area.height.saturating_sub(PICKER_CHROME_HEIGHT)),
            Constraint::Length(4),
        ])
        .areas(area);

        let chrome = |area: Rect| {
            Rect::new(
                area.x.saturating_add(1),
                area.y,
                area.width.saturating_sub(2),
                area.height,
            )
        };

        // Header
        let header_title = if default_bg().is_some_and(is_light) {
            state.action.title().bold().fg(best_color((0, 100, 0)))
        } else {
            state.action.title().bold().cl_cyan()
        };
        let header_line: Line = vec![header_title].into();
        frame.render_widget_ref(header_line, chrome(header));

        // Search line
        let search = chrome(search);
        frame.render_widget_ref(search_line(state, search.width), search);

        let list = Rect::new(
            list.x.saturating_add(2),
            list.y,
            list_viewport_width(list.width),
            list.height,
        );
        render_list(frame, list, state);
        if state.is_transcript_loading() {
            render_transcript_loading_overlay(frame, list);
        }

        render_picker_footer(frame, footer, state, list.height);
    })
}

fn list_viewport_width(width: u16) -> u16 {
    width.saturating_sub(PICKER_LIST_HORIZONTAL_INSET)
}

fn search_line(state: &PickerState, width: u16) -> Line<'_> {
    if let Some(error) = state.inline_error.as_deref() {
        return Line::from(error.cl_red());
    }
    let search = if state.query.is_empty() {
        "Type to search".dim()
    } else {
        format!("Search: {}", state.query).into()
    };
    let mut toolbar = toolbar_line(state, /*compact*/ false);
    if toolbar.width() as u16 > width.saturating_sub(2) {
        toolbar = toolbar_line(state, /*compact*/ true);
    }
    let search_width = UnicodeWidthStr::width(search.content.as_ref());
    let toolbar_width = toolbar.width();
    let spacer_width = width
        .saturating_sub((search_width + toolbar_width) as u16)
        .max(2) as usize;
    let available_search_width = width
        .saturating_sub(toolbar_width as u16)
        .saturating_sub(spacer_width as u16) as usize;
    let search = if search_width > available_search_width {
        let truncated = truncate_text(search.content.as_ref(), available_search_width);
        if state.query.is_empty() {
            truncated.dim()
        } else {
            truncated.into()
        }
    } else {
        search
    };

    let mut spans = vec![search, " ".repeat(spacer_width).into()];
    spans.extend(toolbar.spans);
    spans.into()
}

fn toolbar_line(state: &PickerState, compact: bool) -> Line<'static> {
    let mut spans = Vec::new();
    spans.extend(filter_control_spans(state, compact));
    spans.push("   ".dim());
    spans.extend(sort_control_spans(state, compact));
    spans.into()
}

fn sort_control_spans(state: &PickerState, compact: bool) -> Vec<Span<'static>> {
    let sort_focused = state.toolbar_focus == ToolbarControl::Sort;
    if compact {
        return vec![
            "Sort:".dim(),
            toolbar_value(
                sort_key_label(state.sort_key),
                /*active*/ true,
                sort_focused,
            ),
        ];
    }
    vec![
        "Sort: ".dim(),
        toolbar_value(
            sort_key_label(ThreadSortKey::UpdatedAt),
            state.sort_key == ThreadSortKey::UpdatedAt,
            sort_focused,
        ),
        toolbar_value(
            sort_key_label(ThreadSortKey::CreatedAt),
            state.sort_key == ThreadSortKey::CreatedAt,
            sort_focused,
        ),
    ]
}

fn filter_control_spans(state: &PickerState, compact: bool) -> Vec<Span<'static>> {
    let filter_focused = state.toolbar_focus == ToolbarControl::Filter;
    if compact || state.filter_cwd.is_none() {
        return vec![
            "Filter:".dim(),
            toolbar_value(
                filter_mode_label(state.filter_mode),
                /*active*/ true,
                filter_focused,
            ),
        ];
    }
    vec![
        "Filter: ".dim(),
        toolbar_value(
            filter_mode_label(SessionFilterMode::Cwd),
            state.filter_mode == SessionFilterMode::Cwd,
            filter_focused,
        ),
        toolbar_value(
            filter_mode_label(SessionFilterMode::All),
            state.filter_mode == SessionFilterMode::All,
            filter_focused,
        ),
    ]
}

fn toolbar_value(label: &'static str, active: bool, focused: bool) -> Span<'static> {
    if active {
        let value = format!("[{label}]");
        if focused {
            value.cl_magenta()
        } else {
            value.into()
        }
    } else {
        format!(" {label} ").dim()
    }
}

fn filter_mode_label(filter_mode: SessionFilterMode) -> &'static str {
    match filter_mode {
        SessionFilterMode::Cwd => "Cwd",
        SessionFilterMode::All => "All",
    }
}

struct PickerFooterHint {
    key: &'static str,
    wide_label: String,
    compact_label: String,
    priority: u8,
}

fn render_picker_footer(
    frame: &mut crate::custom_terminal::Frame,
    area: Rect,
    state: &PickerState,
    list_height: u16,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let separator = Rect::new(area.x, area.y, area.width, 1);
    render_picker_footer_separator(
        frame,
        separator,
        picker_footer_progress_label(state, list_height, area.width),
    );

    let lines = footer_hint_lines(state, area.width);
    for (idx, line) in lines.into_iter().enumerate() {
        let y = area.y.saturating_add(1 + idx as u16);
        if y >= area.bottom() {
            break;
        }
        frame.render_widget_ref(line, Rect::new(area.x, y, area.width, 1));
    }
}

fn render_picker_footer_separator(
    frame: &mut crate::custom_terminal::Frame,
    area: Rect,
    progress_label: String,
) {
    if area.width == 0 {
        return;
    }

    let separator = "─".repeat(area.width as usize);
    frame.render_widget_ref(Line::from(separator.dim()), area);

    let progress_width = UnicodeWidthStr::width(progress_label.as_str()) as u16;
    if progress_width < area.width {
        let percent_area = Rect::new(
            area.x + area.width - progress_width - 1,
            area.y,
            progress_width,
            1,
        );
        frame.render_widget_ref(Line::from(progress_label.dim()), percent_area);
    }
}

fn picker_footer_progress_label(state: &PickerState, list_height: u16, width: u16) -> String {
    let position = if state.filtered_rows.is_empty() {
        0
    } else {
        state.selected.saturating_add(1)
    };
    let total = if state.pagination.loading.is_pending() {
        format!("{}…", state.filtered_rows.len())
    } else {
        state.filtered_rows.len().to_string()
    };
    let percent = picker_footer_percent(state, list_height);
    let labels = [
        format!(" {position} / {total} · {percent}% "),
        format!(" {position}/{total} · {percent}% "),
        format!(" {percent}% "),
    ];
    labels
        .into_iter()
        .find(|label| UnicodeWidthStr::width(label.as_str()) < width as usize)
        .unwrap_or_default()
}

fn picker_footer_percent(state: &PickerState, list_height: u16) -> u8 {
    if state.pagination.loading.is_pending() {
        return state.frozen_footer_percent.unwrap_or_else(|| {
            if state.filtered_rows.is_empty() {
                0
            } else {
                picker_footer_scroll_percent(state, list_height)
            }
        });
    }

    picker_footer_scroll_percent(state, list_height)
}

fn picker_footer_scroll_percent(state: &PickerState, list_height: u16) -> u8 {
    if state.filtered_rows.is_empty() {
        return 100;
    }

    let content_rows = state.available_content_rows(list_height as usize);
    let total_height =
        state.rendered_height_between(/*start*/ 0, state.filtered_rows.len() - 1);
    let max_scroll = total_height.saturating_sub(content_rows);
    if max_scroll == 0 {
        return 100;
    }
    let remaining_height =
        state.rendered_height_between(state.scroll_top, state.filtered_rows.len() - 1);
    if remaining_height <= content_rows {
        return 100;
    }

    let skipped_height = if state.scroll_top == 0 {
        0
    } else {
        state.rendered_height_between(/*start*/ 0, state.scroll_top - 1)
    };
    (((skipped_height.min(max_scroll)) as f32 / max_scroll as f32) * 100.0).round() as u8
}

fn footer_hint_lines(state: &PickerState, width: u16) -> Vec<Line<'static>> {
    if state.is_transcript_loading() {
        let hints = [
            PickerFooterHint {
                key: "loading",
                wide_label: String::from("transcript"),
                compact_label: String::from("transcript"),
                priority: 0,
            },
            PickerFooterHint {
                key: "ctrl+c",
                wide_label: String::from("quit"),
                compact_label: String::from("quit"),
                priority: 1,
            },
        ];
        let line = fit_footer_hints(&hints, FooterHintLabelMode::Wide, width)
            .or_else(|| fit_footer_hints(&hints, FooterHintLabelMode::Compact, width))
            .or_else(|| fit_footer_hints(&hints, FooterHintLabelMode::KeyOnly, width))
            .unwrap_or_default();
        return vec![line, Line::default()];
    }

    let action_label = state.action.action_label();
    let (esc_label, esc_compact_label) = if state.query.is_empty() {
        match state.launch_context {
            SessionPickerLaunchContext::Startup => ("start new", "new"),
            SessionPickerLaunchContext::ExistingSession => ("exit", "exit"),
        }
    } else {
        ("clear search", "clear")
    };
    let ctrl_c_label = match state.launch_context {
        SessionPickerLaunchContext::Startup => "quit",
        SessionPickerLaunchContext::ExistingSession => "exit",
    };
    let density_label = match state.density {
        SessionListDensity::Comfortable => "dense view",
        SessionListDensity::Dense => "comfortable view",
    };
    let density_compact_label = match state.density {
        SessionListDensity::Comfortable => "dense",
        SessionListDensity::Dense => "comfy",
    };
    let first_row_hints = vec![
        PickerFooterHint {
            key: "enter",
            wide_label: action_label.to_string(),
            compact_label: action_label.to_string(),
            priority: 0,
        },
        PickerFooterHint {
            key: "esc",
            wide_label: esc_label.to_string(),
            compact_label: esc_compact_label.to_string(),
            priority: 1,
        },
        PickerFooterHint {
            key: "ctrl+c",
            wide_label: ctrl_c_label.to_string(),
            compact_label: ctrl_c_label.to_string(),
            priority: 2,
        },
        PickerFooterHint {
            key: "tab",
            wide_label: String::from("focus sort/filter"),
            compact_label: String::from("focus"),
            priority: 7,
        },
        PickerFooterHint {
            key: "←/→",
            wide_label: String::from("change option"),
            compact_label: String::from("option"),
            priority: 8,
        },
    ];
    let second_row_hints = vec![
        PickerFooterHint {
            key: "ctrl+o",
            wide_label: density_label.to_string(),
            compact_label: density_compact_label.to_string(),
            priority: 3,
        },
        PickerFooterHint {
            key: "ctrl+t",
            wide_label: String::from("transcript"),
            compact_label: String::from("preview"),
            priority: 4,
        },
        PickerFooterHint {
            key: "ctrl+e",
            wide_label: String::from("expand"),
            compact_label: String::from("exp"),
            priority: 6,
        },
        PickerFooterHint {
            key: "↑/↓",
            wide_label: String::from("browse"),
            compact_label: String::from("browse"),
            priority: 5,
        },
    ];

    vec![
        hint_line_for_row(&first_row_hints, width),
        hint_line_for_row(&second_row_hints, width),
    ]
}

fn hint_line_for_row(hints: &[PickerFooterHint], width: u16) -> Line<'static> {
    if width >= FOOTER_COMPACT_BREAKPOINT
        && let Some(line) = fit_footer_hints(hints, FooterHintLabelMode::Wide, width)
    {
        return line;
    }
    if let Some(line) = fit_footer_hints(hints, FooterHintLabelMode::Compact, width) {
        return line;
    }
    if let Some(line) = fit_footer_hints(hints, FooterHintLabelMode::KeyOnly, width) {
        return line;
    }

    let mut retained = (0..hints.len()).collect::<Vec<_>>();
    retained.sort_by_key(|idx| hints[*idx].priority);
    for retain_count in (1..=retained.len()).rev() {
        let mut candidate_indices = retained[..retain_count].to_vec();
        candidate_indices.sort_unstable();
        let candidate = candidate_indices
            .iter()
            .map(|idx| &hints[*idx])
            .collect::<Vec<_>>();
        if let Some(line) = fit_footer_hint_refs(&candidate, FooterHintLabelMode::KeyOnly, width) {
            return line;
        }
    }
    Line::default()
}

fn render_transcript_loading_overlay(frame: &mut crate::custom_terminal::Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let message = "Loading transcript…";
    let message_width = UnicodeWidthStr::width(message) as u16;
    let overlay_width = if area.width >= message_width.saturating_add(10) {
        message_width + 10
    } else {
        area.width
    };
    let overlay_height = if area.height >= 3 { 3 } else { 1 };
    let overlay = Rect::new(
        area.x + area.width.saturating_sub(overlay_width) / 2,
        area.y + area.height.saturating_sub(overlay_height) / 2,
        overlay_width,
        overlay_height,
    );
    let style = transcript_loading_overlay_style();
    for y in overlay.y..overlay.bottom() {
        for x in overlay.x..overlay.right() {
            frame.buffer[(x, y)].set_symbol(" ").set_style(style);
        }
    }

    let message = truncate_text(message, overlay.width as usize);
    let message_width = UnicodeWidthStr::width(message.as_str()) as u16;
    let line = Rect::new(
        overlay.x + overlay.width.saturating_sub(message_width) / 2,
        overlay.y + overlay.height / 2,
        message_width.min(overlay.width),
        1,
    );
    frame.render_widget_ref(Line::from(message.bold()), line);
}

fn transcript_loading_overlay_style() -> Style {
    let Some(bg) = default_bg() else {
        return Style::default().bg(Color::DarkGray);
    };
    let (overlay, alpha) = if is_light(bg) {
        ((0, 0, 0), 0.08)
    } else {
        ((255, 255, 255), 0.14)
    };
    Style::default().bg(best_color(blend(overlay, bg, alpha)))
}

#[derive(Clone, Copy)]
enum FooterHintLabelMode {
    Wide,
    Compact,
    KeyOnly,
}

fn fit_footer_hints(
    hints: &[PickerFooterHint],
    mode: FooterHintLabelMode,
    width: u16,
) -> Option<Line<'static>> {
    let hint_refs = hints.iter().collect::<Vec<_>>();
    fit_footer_hint_refs(&hint_refs, mode, width)
}

fn fit_footer_hint_refs(
    hints: &[&PickerFooterHint],
    mode: FooterHintLabelMode,
    width: u16,
) -> Option<Line<'static>> {
    let gap_width = FOOTER_HINT_GAP;
    if footer_hints_width(hints, mode, gap_width) > width as usize {
        return None;
    }

    let mut spans = vec![
        " ".repeat(FOOTER_HINT_LEFT_PADDING)
            .set_style(footer_hint_label_style()),
    ];
    for (idx, hint) in hints.iter().enumerate() {
        if idx > 0 {
            spans.push(" ".repeat(gap_width).set_style(footer_hint_label_style()));
        }
        spans.push(hint.key.set_style(footer_hint_key_style()));
        let label = match mode {
            FooterHintLabelMode::Wide => Some(hint.wide_label.as_str()),
            FooterHintLabelMode::Compact => Some(hint.compact_label.as_str()),
            FooterHintLabelMode::KeyOnly => None,
        };
        if let Some(label) = label {
            spans.push(" ".set_style(footer_hint_label_style()));
            spans.push(label.to_string().set_style(footer_hint_label_style()));
        }
    }
    Some(spans.into())
}

fn footer_hint_key_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::Black)
    } else {
        Style::default()
    }
}

fn footer_hint_label_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().dim()
    }
}

fn footer_hints_width(
    hints: &[&PickerFooterHint],
    mode: FooterHintLabelMode,
    gap_width: usize,
) -> usize {
    FOOTER_HINT_LEFT_PADDING
        + hints
            .iter()
            .enumerate()
            .map(|(idx, hint)| {
                let label_width = match mode {
                    FooterHintLabelMode::Wide => {
                        1 + UnicodeWidthStr::width(hint.wide_label.as_str())
                    }
                    FooterHintLabelMode::Compact => {
                        1 + UnicodeWidthStr::width(hint.compact_label.as_str())
                    }
                    FooterHintLabelMode::KeyOnly => 0,
                };
                let hint_width = UnicodeWidthStr::width(hint.key) + label_width;
                if idx == 0 {
                    hint_width
                } else {
                    hint_width + gap_width
                }
            })
            .sum::<usize>()
}

fn render_list(frame: &mut crate::custom_terminal::Frame, area: Rect, state: &PickerState) {
    if area.height == 0 {
        return;
    }
    Clear.render(area, frame.buffer);

    let rows = &state.filtered_rows;
    if rows.is_empty() {
        let message = render_empty_state_line(state);
        frame.render_widget_ref(message, area);
        return;
    }

    let show_more_above = state.has_more_above();
    let show_more_below = state.has_more_below(area.height as usize);
    let content_area = Rect::new(
        area.x,
        area.y.saturating_add(u16::from(show_more_above)),
        area.width,
        area.height
            .saturating_sub(u16::from(show_more_above))
            .saturating_sub(u16::from(show_more_below)),
    );
    if show_more_above {
        frame.render_widget_ref(
            more_line("↑ more"),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    let start = state.scroll_top.min(rows.len().saturating_sub(1));
    let mut y = content_area.y;
    for (idx, row) in rows[start..].iter().enumerate() {
        if y >= content_area.y.saturating_add(content_area.height) {
            break;
        }
        let row_idx = start + idx;
        let is_selected = row_idx == state.selected;
        let is_expanded =
            is_selected && row.thread_id.is_some() && state.expanded_thread_id == row.thread_id;
        let is_zebra = row_idx.is_multiple_of(2);
        for line in render_session_lines(row, state, is_selected, is_expanded, is_zebra, area.width)
        {
            if y >= content_area.y.saturating_add(content_area.height) {
                break;
            }
            frame.render_widget_ref(line, Rect::new(area.x, y, area.width, 1));
            y = y.saturating_add(1);
        }
        if state.density == SessionListDensity::Comfortable
            && y < content_area.y.saturating_add(content_area.height)
            && start + idx + 1 < rows.len()
        {
            y = y.saturating_add(1);
        }
    }

    if state.pagination.loading.is_pending()
        && y < content_area.y.saturating_add(content_area.height)
    {
        let loading_line: Line = vec!["  ".into(), "Loading older sessions…".italic().dim()].into();
        let rect = Rect::new(area.x, y, area.width, 1);
        frame.render_widget_ref(loading_line, rect);
    }
    if show_more_below {
        let label = if state.pagination.loading.is_pending() {
            "↓ loading more"
        } else {
            "↓ more"
        };
        frame.render_widget_ref(
            more_line(label),
            Rect::new(
                area.x,
                area.y.saturating_add(area.height.saturating_sub(1)),
                area.width,
                1,
            ),
        );
    }
}

fn more_line(label: &'static str) -> Line<'static> {
    vec![label.dim()].into()
}

fn render_session_lines(
    row: &Row,
    state: &PickerState,
    is_selected: bool,
    is_expanded: bool,
    is_zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    match state.density {
        SessionListDensity::Comfortable => {
            render_comfortable_session_lines(row, state, is_selected, is_expanded, is_zebra, width)
        }
        SessionListDensity::Dense => {
            render_dense_session_lines(row, state, is_selected, is_expanded, is_zebra, width)
        }
    }
}

fn render_comfortable_session_lines(
    row: &Row,
    state: &PickerState,
    is_selected: bool,
    is_expanded: bool,
    is_zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let marker = selection_marker(is_selected, is_expanded);
    let available_title_width = (width as usize)
        .saturating_sub(marker.width())
        .saturating_sub(running_label_width(row.is_running));
    let title_text = truncate_text(row.display_preview(), available_title_width);
    let title = if is_selected {
        selected_session_title_span(title_text)
    } else {
        title_text.into()
    };
    let mut title_line = Line::from(vec![marker, title]);
    if row.is_running {
        let padding = (width as usize)
            .saturating_sub(title_line.width())
            .saturating_sub(RUNNING_LABEL.width());
        title_line.spans.push(" ".repeat(padding).into());
        title_line.spans.push(RUNNING_LABEL.cyan());
    }
    let mut lines = vec![title_line];
    let row_style = if is_selected {
        Some(dense_selected_style())
    } else if is_zebra {
        Some(dense_zebra_style())
    } else {
        None
    };
    if let Some(style) = row_style {
        lines = apply_session_row_background(lines, style, width);
    }
    if row.is_running
        && let Some(status) = lines.first_mut().and_then(|line| line.spans.last_mut())
    {
        status.style = status.style.fg(Color::Cyan);
    }
    if is_expanded {
        lines.extend(render_transcript_preview_lines(row, state, width));
        return lines;
    }

    let reference = state.relative_time_reference.unwrap_or_else(Utc::now);
    let created = format_relative_time(reference, row.created_at);
    let updated = format_relative_time(reference, row.updated_at.or(row.created_at));
    let branch = row.git_branch.as_deref();
    let cwd = row
        .cwd
        .as_ref()
        .map(|path| format_directory_display(path, /*max_width*/ None));
    let footer_lines = render_footer_lines(
        state.sort_key,
        &created,
        &updated,
        branch,
        cwd.as_deref(),
        state.filter_mode == SessionFilterMode::All,
        width,
    );
    if let Some(style) = row_style {
        lines.extend(apply_session_row_background(footer_lines, style, width));
    } else {
        lines.extend(footer_lines);
    }
    lines
}

fn apply_session_row_background(
    lines: Vec<Line<'static>>,
    style: Style,
    width: u16,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| apply_line_background(line, style, width))
        .collect()
}

fn apply_line_background(mut line: Line<'static>, style: Style, width: u16) -> Line<'static> {
    let padding = (width as usize).saturating_sub(line.width());
    if padding > 0 {
        line.spans.push(" ".repeat(padding).set_style(style));
    }
    line.style = line.style.patch(style);
    for span in &mut line.spans {
        span.style = span.style.patch(style);
    }
    line
}

fn render_dense_session_lines(
    row: &Row,
    state: &PickerState,
    is_selected: bool,
    is_expanded: bool,
    is_zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let marker = selection_marker(is_selected, is_expanded);
    let reference = state.relative_time_reference.unwrap_or_else(Utc::now);
    let created = format_relative_time(reference, row.created_at);
    let updated = format_relative_time(reference, row.updated_at.or(row.created_at));
    let date = match state.sort_key {
        ThreadSortKey::CreatedAt => created,
        ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt => updated,
    };
    let cwd = (state.filter_mode == SessionFilterMode::All && width >= 120).then(|| {
        row.cwd
            .as_ref()
            .map(|path| format_directory_display(path, /*max_width*/ None))
            .unwrap_or_else(|| String::from("no cwd"))
    });
    let mut lines = vec![dense_summary_line(DenseSummaryInput {
        marker,
        date: &date,
        title: row.display_preview(),
        cwd: cwd.as_deref(),
        is_running: row.is_running,
        is_selected,
        is_zebra,
        width,
    })];
    if is_expanded {
        lines.extend(render_transcript_preview_lines(row, state, width));
    }
    lines
}

struct DenseSummaryInput<'a> {
    marker: Span<'static>,
    date: &'a str,
    title: &'a str,
    cwd: Option<&'a str>,
    is_running: bool,
    is_selected: bool,
    is_zebra: bool,
    width: u16,
}

fn dense_summary_line(input: DenseSummaryInput<'_>) -> Line<'static> {
    let marker_width = input.marker.width();
    let available = (input.width as usize).saturating_sub(marker_width);
    let columns = dense_columns(available, input.is_running, input.cwd.is_some());
    let title = if input.is_selected {
        selected_session_title_span(dense_column_text(input.title, columns.title_width))
    } else {
        dense_column_text(input.title, columns.title_width).into()
    };

    let mut spans = vec![
        input.marker,
        dense_column_text(input.date, columns.date_width).dim(),
        title,
    ];
    if let (Some(cwd), Some(cwd_width)) = (input.cwd, columns.cwd_width) {
        spans.push(dense_column_text(&format!("{SESSION_META_CWD_ICON} {cwd}"), cwd_width).dim());
    }
    if input.is_running {
        spans.push(RUNNING_LABEL.cyan());
    }
    let mut line = Line::from(spans);
    if input.is_selected {
        let padding = (input.width as usize).saturating_sub(line.width());
        if padding > 0 {
            line.spans
                .push(" ".repeat(padding).set_style(dense_selected_style()));
        }
        line = line.style(dense_selected_style());
    } else if input.is_zebra {
        let padding = (input.width as usize).saturating_sub(line.width());
        if padding > 0 {
            line.spans
                .push(" ".repeat(padding).set_style(dense_zebra_style()));
        }
        line = line.style(dense_zebra_style());
    }
    line
}

struct DenseColumns {
    date_width: usize,
    title_width: usize,
    cwd_width: Option<usize>,
}

fn dense_columns(width: usize, is_running: bool, show_cwd: bool) -> DenseColumns {
    let available_width = width.saturating_sub(running_label_width(is_running));
    let date_width = SESSION_META_DATE_WIDTH.min(available_width);
    let cwd_width = show_cwd.then(|| cwd_column_width(available_width));
    DenseColumns {
        date_width,
        title_width: available_width
            .saturating_sub(date_width)
            .saturating_sub(cwd_width.unwrap_or_default()),
        cwd_width,
    }
}

const RUNNING_LABEL: &str = " RUNNING";

fn running_label_width(is_running: bool) -> usize {
    usize::from(is_running) * RUNNING_LABEL.width()
}

fn dense_zebra_style() -> Style {
    dense_row_background_style(/*selected*/ false)
}

fn dense_selected_style() -> Style {
    selected_session_style().patch(dense_row_background_style(/*selected*/ true))
}

fn dense_row_background_style(selected: bool) -> Style {
    let Some(bg) = default_bg() else {
        return Style::default();
    };
    let (overlay, alpha) = if is_light(bg) {
        ((0, 0, 0), if selected { 0.12 } else { 0.04 })
    } else {
        ((255, 255, 255), if selected { 0.12 } else { 0.055 })
    };
    Style::default().bg(best_color(blend(overlay, bg, alpha)))
}

fn dense_column_text(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let text = truncate_text(text, width.saturating_sub(1));
    let padding = width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
    format!("{text}{}", " ".repeat(padding))
}

fn selection_marker(is_selected: bool, is_expanded: bool) -> Span<'static> {
    match (is_selected, is_expanded) {
        (true, true) => "⌄ ".set_style(selected_session_style().bold()),
        (true, false) => "❯ ".set_style(selected_session_style().bold()),
        (false, _) => "  ".into(),
    }
}

fn selected_session_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::Rgb(160, 107, 234))
    } else {
        Style::default().fg(Color::Yellow)
    }
}

fn selected_session_title_span(title: String) -> Span<'static> {
    title.set_style(selected_session_style())
}

fn render_footer_lines(
    sort_key: ThreadSortKey,
    created: &str,
    updated: &str,
    branch: Option<&str>,
    cwd: Option<&str>,
    show_cwd: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let date = match sort_key {
        ThreadSortKey::CreatedAt => created,
        ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt => updated,
    };
    let mut parts = vec![FooterPart::Date(date.to_string())];
    if show_cwd {
        parts.push(FooterPart::Cwd(cwd.map(str::to_string)));
    }
    parts.push(FooterPart::Branch(branch.map(str::to_string)));
    pack_footer_parts(parts, width)
}

enum FooterPart {
    Date(String),
    Branch(Option<String>),
    Cwd(Option<String>),
}

impl FooterPart {
    fn text(&self) -> &str {
        match self {
            FooterPart::Date(text) => text,
            FooterPart::Branch(Some(text)) | FooterPart::Cwd(Some(text)) => text,
            FooterPart::Branch(None) => "no branch",
            FooterPart::Cwd(None) => "no cwd",
        }
    }

    fn prefix(&self) -> Option<&'static str> {
        match self {
            FooterPart::Date(_) => None,
            FooterPart::Branch(_) => Some(SESSION_META_BRANCH_ICON),
            FooterPart::Cwd(_) => Some(SESSION_META_CWD_ICON),
        }
    }
}

fn pack_footer_parts(parts: Vec<FooterPart>, width: u16) -> Vec<Line<'static>> {
    let available_width = width as usize;
    if available_width <= SESSION_META_INDENT_WIDTH {
        return Vec::new();
    }
    let cwd_width = cwd_column_width(available_width);
    let all_parts_width = footer_parts_width(&parts, cwd_width);
    if all_parts_width <= available_width {
        return vec![footer_line(parts, available_width, cwd_width)];
    }

    let mut lines = Vec::with_capacity(parts.len());
    let mut current_parts = Vec::new();
    for part in parts {
        let mut candidate_parts = std::mem::take(&mut current_parts);
        candidate_parts.push(part);
        if candidate_parts.len() > 1
            && footer_parts_width(&candidate_parts, cwd_width) > available_width
        {
            let previous_parts = candidate_parts
                .drain(..candidate_parts.len().saturating_sub(1))
                .collect();
            lines.push(footer_line(previous_parts, available_width, cwd_width));
        }
        current_parts = candidate_parts;
    }
    if !current_parts.is_empty() {
        lines.push(footer_line(current_parts, available_width, cwd_width));
    }
    lines
}

fn cwd_column_width(width: usize) -> usize {
    let available = width.saturating_sub(
        SESSION_META_INDENT_WIDTH + SESSION_META_DATE_WIDTH + 2 * SESSION_META_FIELD_GAP_WIDTH,
    );
    (available / 2).clamp(SESSION_META_MIN_CWD_WIDTH, SESSION_META_MAX_CWD_WIDTH)
}

fn footer_parts_width(parts: &[FooterPart], cwd_width: usize) -> usize {
    let content_width: usize = parts
        .iter()
        .enumerate()
        .map(|(idx, part)| footer_part_width(part, idx + 1 < parts.len(), cwd_width))
        .sum();
    SESSION_META_INDENT_WIDTH + content_width
}

fn footer_part_width(part: &FooterPart, padded: bool, cwd_width: usize) -> usize {
    let prefix_width = part.prefix().map_or(0, UnicodeWidthStr::width);
    let prefix_gap_width = usize::from(part.prefix().is_some() && !part.text().is_empty());
    let text_width = UnicodeWidthStr::width(part.text());
    let actual_width = prefix_width + prefix_gap_width + text_width;
    match part {
        FooterPart::Date(_) if padded => SESSION_META_DATE_WIDTH.max(actual_width),
        FooterPart::Cwd(_) if padded => cwd_width,
        _ => actual_width,
    }
}

fn footer_line(parts: Vec<FooterPart>, width: usize, cwd_width: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec!["  ".into()];
    let mut remaining_width = width.saturating_sub(SESSION_META_INDENT_WIDTH);
    let part_count = parts.len();
    for (idx, part) in parts.into_iter().enumerate() {
        if idx > 0 {
            let gap_width = SESSION_META_FIELD_GAP_WIDTH.min(remaining_width);
            if gap_width > 0 {
                spans.push(" ".repeat(gap_width).dim());
                remaining_width = remaining_width.saturating_sub(gap_width);
            }
        }
        let padded = idx + 1 < part_count;
        let target_width = match part {
            FooterPart::Date(_) if padded => Some(SESSION_META_DATE_WIDTH),
            FooterPart::Cwd(_) if padded => Some(cwd_width),
            FooterPart::Date(_) | FooterPart::Branch(_) | FooterPart::Cwd(_) => None,
        };
        let used_width = push_footer_part(&mut spans, part, target_width, remaining_width);
        remaining_width = remaining_width.saturating_sub(used_width);
        if let Some(target_width) = target_width {
            let padding = target_width.saturating_sub(used_width);
            if padding > 0 {
                spans.push(" ".repeat(padding).dim());
                remaining_width = remaining_width.saturating_sub(padding);
            }
        }
    }
    spans.into()
}

fn push_footer_part(
    spans: &mut Vec<Span<'static>>,
    part: FooterPart,
    target_width: Option<usize>,
    available_width: usize,
) -> usize {
    let text = part.text().to_string();
    let Some(prefix) = part.prefix() else {
        let text = truncate_text(&text, available_width);
        let width = UnicodeWidthStr::width(text.as_str());
        spans.push(text.dim());
        return width;
    };

    let prefix_width = UnicodeWidthStr::width(prefix);
    if available_width <= prefix_width {
        let prefix = truncate_text(prefix, available_width);
        let width = UnicodeWidthStr::width(prefix.as_str());
        spans.push(prefix.dim());
        return width;
    }

    spans.push(prefix.dim());
    let mut used_width = prefix_width;
    if !text.is_empty() && used_width < available_width {
        spans.push(" ".dim());
        used_width += 1;
    }
    let text_width = target_width
        .unwrap_or(available_width)
        .saturating_sub(used_width)
        .min(available_width.saturating_sub(used_width));
    let text = truncate_text(&text, text_width);
    let rendered_text_width = UnicodeWidthStr::width(text.as_str());
    match part {
        FooterPart::Branch(None) | FooterPart::Cwd(None) => spans.push(text.dim().italic()),
        _ => spans.push(text.dim()),
    }
    used_width + rendered_text_width
}

fn render_transcript_preview_lines(
    row: &Row,
    state: &PickerState,
    width: u16,
) -> Vec<Line<'static>> {
    let mut details = render_expanded_session_details(row, state, width);
    let Some(thread_id) = row.thread_id else {
        return details;
    };
    let preview_lines = match state.transcript_previews.get(&thread_id) {
        Some(TranscriptPreviewState::Loading) => {
            vec![vec!["  │ ".dim(), "Loading recent transcript...".italic().dim()].into()]
        }
        Some(TranscriptPreviewState::Failed) => vec![
            vec![
                "  │ ".dim(),
                "Could not load transcript preview".italic().cl_red(),
            ]
            .into(),
        ],
        Some(TranscriptPreviewState::Loaded(lines)) => {
            render_conversation_preview_lines(lines, width)
        }
        None => Vec::new(),
    };
    details.extend(preview_lines);
    details
}

fn render_expanded_session_details(
    row: &Row,
    state: &PickerState,
    width: u16,
) -> Vec<Line<'static>> {
    let reference = state.relative_time_reference.unwrap_or_else(Utc::now);
    let session = match (row.thread_name.as_deref(), row.thread_id) {
        (Some(thread_name), Some(thread_id)) => format!("{thread_name} ({thread_id})"),
        (Some(thread_name), None) => thread_name.to_string(),
        (None, Some(thread_id)) => thread_id.to_string(),
        (None, None) => "-".to_string(),
    };
    let directory = row
        .cwd
        .as_ref()
        .map(|path| format_directory_display(path, /*max_width*/ None))
        .unwrap_or_else(|| "-".to_string());
    let branch = row
        .git_branch
        .as_ref()
        .map(|branch| format!("{SESSION_META_BRANCH_ICON} {branch}"))
        .unwrap_or_else(|| format!("{SESSION_META_BRANCH_ICON} no branch"));

    vec![
        expanded_detail_line("Session:", &session, width),
        expanded_time_detail_line("Created:", reference, row.created_at, width),
        expanded_time_detail_line(
            "Updated:",
            reference,
            row.updated_at.or(row.created_at),
            width,
        ),
        expanded_detail_line("Directory:", &directory, width),
        expanded_detail_line("Branch:", &branch, width),
        vec!["  │".dim()].into(),
        vec!["  │ ".dim(), "Conversation:".dim()].into(),
    ]
}

fn render_conversation_preview_lines(
    lines: &[TranscriptPreviewLine],
    width: u16,
) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![
            vec![
                "  └ ".dim(),
                "No transcript preview available".italic().dim(),
            ]
            .into(),
        ];
    }

    let mut rendered = Vec::new();
    for line in lines {
        rendered.extend(render_transcript_content_lines(line, width));
    }
    let rendered_len = rendered.len();
    rendered
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let prefix = if idx + 1 == rendered_len {
                "  └ "
            } else {
                "  │ "
            };
            prefix_transcript_line(prefix, line)
        })
        .collect()
}

fn render_transcript_content_lines(line: &TranscriptPreviewLine, width: u16) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(4) as usize;
    let lines = match line.speaker {
        TranscriptPreviewSpeaker::User => vec![conversation_content_line(
            Line::from(line.text.clone()),
            conversation_user_style(),
        )],
        TranscriptPreviewSpeaker::Assistant => {
            let mut lines = Vec::new();
            append_markdown(
                &line.text, /*width*/ None, /*cwd*/ None, &mut lines,
            );
            for line in &mut lines {
                *line = conversation_content_line(line.clone(), conversation_assistant_style());
            }
            lines
        }
    };
    adaptive_wrap_lines(lines, RtOptions::new(content_width.max(/*other*/ 1)))
}

fn conversation_content_line(mut line: Line<'static>, style: Style) -> Line<'static> {
    line.style = line.style.patch(style);
    for span in &mut line.spans {
        span.style = span.style.patch(style);
    }
    line
}

fn prefix_transcript_line(prefix: &'static str, line: Line<'static>) -> Line<'static> {
    let mut spans = vec![prefix.set_style(transcript_prefix_style(&line))];
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

fn transcript_prefix_style(line: &Line<'_>) -> Style {
    let style = line
        .spans
        .iter()
        .find(|span| !span.content.trim().is_empty())
        .map(|span| line.style.patch(span.style))
        .unwrap_or(line.style);
    connector_style_from_content(style)
}

fn connector_style_from_content(style: Style) -> Style {
    Style {
        fg: style.fg,
        bg: style.bg,
        ..Style::default()
    }
}

fn conversation_assistant_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::Gray)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn conversation_user_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::DarkGray).italic()
    } else {
        Style::default().fg(Color::Gray).italic()
    }
}

fn expanded_detail_line(label: &'static str, value: &str, width: u16) -> Line<'static> {
    const LABEL_WIDTH: usize = 10;
    let prefix_width = 4;
    let gap_width = 2;
    let value_width = (width as usize)
        .saturating_sub(prefix_width + LABEL_WIDTH + gap_width)
        .max(1);
    vec![
        "  │ ".dim(),
        format!("{label:<LABEL_WIDTH$}").dim(),
        "  ".dim(),
        truncate_text(value, value_width).into(),
    ]
    .into()
}

fn expanded_time_detail_line(
    label: &'static str,
    reference: DateTime<Utc>,
    ts: Option<DateTime<Utc>>,
    width: u16,
) -> Line<'static> {
    let Some(ts) = ts else {
        return expanded_detail_line(label, "-", width);
    };
    let value = format!(
        "{} · {}",
        format_relative_time_long(reference, ts),
        format_timestamp(ts)
    );
    expanded_detail_line(label, &value, width)
}

fn format_relative_time(reference: DateTime<Utc>, ts: Option<DateTime<Utc>>) -> String {
    let Some(ts) = ts else {
        return "-".to_string();
    };
    let seconds = (reference - ts).num_seconds().max(0);
    if seconds == 0 {
        return "now".to_string();
    }
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

fn format_relative_time_long(reference: DateTime<Utc>, ts: DateTime<Utc>) -> String {
    let seconds = (reference - ts).num_seconds().max(0);
    if seconds == 0 {
        return "now".to_string();
    }
    if seconds < 60 {
        return plural_time(seconds, "second");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return plural_time(minutes, "minute");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return plural_time(hours, "hour");
    }
    plural_time(hours / 24, "day")
}

fn plural_time(value: i64, unit: &str) -> String {
    if value == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{value} {unit}s ago")
    }
}

fn format_timestamp(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn render_empty_state_line(state: &PickerState) -> Line<'static> {
    if !state.query.is_empty() {
        if state.search_state.is_active()
            || (state.pagination.loading.is_pending() && state.pagination.next_cursor.is_some())
        {
            return vec!["Searching…".italic().dim()].into();
        }
        if state.pagination.reached_scan_cap {
            let msg = format!(
                "Search scanned first {} sessions; more may exist",
                state.pagination.num_scanned_files
            );
            return vec![Span::from(msg).italic().dim()].into();
        }
        return vec!["No results for your search".italic().dim()].into();
    }

    if state.pagination.loading.is_pending() {
        if state.all_rows.is_empty() && state.pagination.num_scanned_files == 0 {
            return vec!["Loading sessions…".italic().dim()].into();
        }
        return vec!["Loading older sessions…".italic().dim()].into();
    }

    vec!["No sessions yet".italic().dim()].into()
}
