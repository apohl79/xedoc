// Forbid accidental stdout/stderr writes in the *library* portion of the TUI.
// The standalone `xedoc-tui` binary prints a short help message before the
// alternate‑screen mode starts; that file opts‑out locally via `allow`.
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]
use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;
use crate::legacy_core::config::ConfigTomlLoadResult;
use crate::legacy_core::config::load_config_toml_with_layer_stack;
use crate::legacy_core::config::resolve_oss_provider;
use crate::legacy_core::config::resolve_profile_v2_config_path;
use crate::session_resume::ResolveCwdOutcome;
use crate::session_resume::ResumeCwdContext;
use crate::session_resume::effective_resume_cwd_mode;
use crate::session_resume::resolve_cwd_for_resume_or_fork;
pub use crate::startup_error::LocalStateDbStartupError;
use crate::version::xedoc_cli_version;
use additional_dirs::add_dir_warning_message;
use app::App;
pub use app::AppExitInfo;
pub use app::ExitReason;
use app_server_session::AppServerSession;
use app_server_session::ThreadParamsMode;
use color_eyre::eyre::WrapErr;
use cwd_prompt::CwdPromptAction;
pub use session_archive_commands::DeleteConfirmation;
pub use session_archive_commands::SessionArchiveAction;
pub use session_archive_commands::SessionArchiveCommandOptions;
pub use session_archive_commands::run_session_archive_command;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
pub use token_usage::TokenUsage;
use tracing::error;
use tracing::warn;
use tracing_appender::non_blocking;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use url::Url;
use uuid::Uuid;
use xedoc_app_server_client::AppServerClient;
use xedoc_app_server_client::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
use xedoc_app_server_client::InProcessAppServerClient;
use xedoc_app_server_client::InProcessClientStartArgs;
pub use xedoc_app_server_client::RemoteAppServerEndpoint;
use xedoc_app_server_protocol::Account as AppServerAccount;
use xedoc_app_server_protocol::AskForApproval;
use xedoc_app_server_protocol::ConfigWarningNotification;
use xedoc_app_server_protocol::Thread as AppServerThread;
use xedoc_app_server_protocol::ThreadListCwdFilter;
use xedoc_app_server_protocol::ThreadListParams;
use xedoc_app_server_protocol::ThreadSortKey as AppServerThreadSortKey;
use xedoc_app_server_protocol::ThreadSourceKind;
use xedoc_config::CloudConfigBundleLoader;
use xedoc_config::ConfigLoadError;
use xedoc_config::LoaderOverrides;
use xedoc_config::format_config_error_with_source;
use xedoc_config::types::ResumeCwdMode;
use xedoc_exec_server::EnvironmentManager;
use xedoc_exec_server::ExecServerRuntimePaths;
use xedoc_login::AuthConfig;
use xedoc_login::default_client::originator;
use xedoc_login::default_client::set_default_client_residency_requirement;
use xedoc_login::enforce_login_restrictions;
use xedoc_protocol::ThreadId;
use xedoc_protocol::auth::AuthMode;
use xedoc_protocol::config_types::AltScreenMode;
use xedoc_protocol::config_types::SandboxMode;
use xedoc_rollout::StateDbHandle;
use xedoc_rollout::state_db;
use xedoc_state::log_db;
pub use xedoc_tui_transcript::set_xedoc_cli_version;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_absolute_path::canonicalize_existing_preserving_symlinks;
use xedoc_utils_home_dir::find_xedoc_home;
use xedoc_utils_oss::ensure_oss_provider_ready;
use xedoc_utils_oss::get_default_model_for_oss_provider;

pub(crate) use xedoc_app_server_client::legacy_core;

mod additional_dirs;
mod app;
mod app_backtrack;
mod app_command;
mod app_event;
mod app_event_sender;
mod app_server_approval_conversions;
mod app_server_session;
pub(crate) use app_server_session::connect_remote_app_server;
mod bottom_pane;
mod chatwidget;
pub(crate) use xedoc_tui_render::city_lights;
mod cli;
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod clipboard_paste;
mod config_update;
pub use xedoc_tui_transcript::Terminal;
pub(crate) use xedoc_tui_transcript::custom_terminal;
mod cwd_prompt;
pub(crate) use xedoc_tui_render::diff_render;
pub(crate) use xedoc_tui_transcript::exec_command;
mod external_editor;
mod file_search;
mod goal_display;
mod goal_files;
pub(crate) use xedoc_tui_transcript::history_cell;
mod hooks_rpc;
pub(crate) use xedoc_tui_input::key_hint;
pub(crate) use xedoc_tui_input::keymap;
pub(crate) use xedoc_tui_transcript::inline_visualization;
pub(crate) use xedoc_tui_transcript::insert_history;
pub use xedoc_tui_transcript::insert_history_lines;
mod keymap_setup;
pub use xedoc_tui_transcript::RowBuilder;
mod managed_new_thread_defaults;
pub(crate) use xedoc_tui_markdown as markdown_render;
mod model_catalog;
mod model_migration;
mod multi_agents;
mod notifications;
pub(crate) mod onboarding;
mod oss_selection;
mod pager_overlay;
pub(crate) use xedoc_tui_render::render;
mod resize_reflow_cap;
mod resume_picker;
pub(crate) use resume_picker::resume_source_kinds;
mod selection_list;
mod session_archive_commands;
mod session_log;
mod session_resume;
pub(crate) use xedoc_tui_transcript::session_state;
mod startup_error;
mod startup_hooks_review;
mod status;
pub(crate) use xedoc_tui_render::terminal_hyperlinks;
pub(crate) use xedoc_tui_render::terminal_palette;
pub(crate) use xedoc_tui_render::terminal_probe;
pub(crate) use xedoc_tui_transcript::text_formatting;
mod token_optimizer_stats;
mod token_usage;
mod transcript_reflow;
mod tui;
mod web_ui;
mod web_ui_files;
mod web_ui_html;
pub use xedoc_tui_transcript::UpdateAction;
pub(crate) use xedoc_tui_transcript::update_action;
#[cfg(not(debug_assertions))]
pub use xedoc_tui_transcript::update_action::get_update_action;
mod update_prompt;
#[cfg(not(debug_assertions))]
pub(crate) use xedoc_tui_transcript::update_versions;
mod updates;
#[cfg(any(not(debug_assertions), test))]
mod updates_cache;
pub(crate) use xedoc_tui_transcript::version;
mod workspace_command;

#[cfg(test)]
pub(crate) use xedoc_tui_test_support as test_support;

use crate::onboarding::onboarding_screen::OnboardingScreenArgs;
use crate::onboarding::onboarding_screen::run_onboarding_app;
use crate::startup_hooks_review::StartupHooksReviewOutcome;
use crate::startup_hooks_review::load_startup_hooks_review_entry;
use crate::startup_hooks_review::maybe_run_startup_hooks_review;
use crate::tui::Tui;
pub use cli::Cli;
pub use markdown_render::render_markdown_text;
use xedoc_arg0::Arg0DispatchPaths;
// (tests access modules directly within the crate)

const TUI_LOG_FILE_NAME: &str = "xedoc-tui.log";

#[cfg(unix)]
const AUTO_CONNECT_DAEMON_CONNECT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(50);

#[allow(clippy::too_many_arguments)]
async fn start_embedded_app_server(
    arg0_paths: Arg0DispatchPaths,
    config: Config,
    cli_kv_overrides: Vec<(String, toml::Value)>,
    loader_overrides: LoaderOverrides,
    strict_config: bool,
    cloud_config_bundle: CloudConfigBundleLoader,
    log_db: Option<log_db::LogDbLayer>,
    state_db: Option<StateDbHandle>,
    environment_manager: Arc<EnvironmentManager>,
) -> color_eyre::Result<InProcessAppServerClient> {
    start_embedded_app_server_with(
        arg0_paths,
        config,
        cli_kv_overrides,
        loader_overrides,
        strict_config,
        cloud_config_bundle,
        log_db,
        state_db,
        environment_manager,
        InProcessAppServerClient::start,
    )
    .await
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppServerTarget {
    Embedded,
    LocalDaemon { endpoint: RemoteAppServerEndpoint },
    Remote { endpoint: RemoteAppServerEndpoint },
}

impl AppServerTarget {
    pub(crate) fn supports_thread_disconnect(&self) -> bool {
        matches!(self, Self::LocalDaemon { .. } | Self::Remote { .. })
    }

    pub(crate) fn uses_remote_workspace(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    fn thread_params_mode(&self) -> ThreadParamsMode {
        if self.uses_remote_workspace() {
            ThreadParamsMode::Remote
        } else {
            ThreadParamsMode::Embedded
        }
    }
}

async fn init_state_db_for_app_server_target(
    config: &Config,
    app_server_target: &AppServerTarget,
) -> std::io::Result<Option<StateDbHandle>> {
    match app_server_target {
        AppServerTarget::Embedded => state_db::try_init(config).await.map(Some).map_err(|err| {
            let database_path = xedoc_state::runtime_db_path_for_corruption_error(&err)
                .unwrap_or_else(|| xedoc_state::state_db_path(config.sqlite_home.as_path()));
            std::io::Error::other(LocalStateDbStartupError::new(
                database_path,
                format!("{err:#}"),
            ))
        }),
        AppServerTarget::LocalDaemon { .. } | AppServerTarget::Remote { .. } => {
            Ok(state_db::get_state_db(config).await)
        }
    }
}

// TODO(jif) delete after 22/11/2026.
fn remove_legacy_tui_log_file(xedoc_home: &Path) {
    // Shared append-only TUI logs could grow without bound. Existing processes
    // may still hold the file open, so startup cleanup is best effort.
    let _ = std::fs::remove_file(xedoc_home.join("log").join(TUI_LOG_FILE_NAME));
}

fn remote_addr_has_explicit_port(addr: &str, parsed: &Url) -> bool {
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if parsed.port().is_some() {
        return true;
    }

    let Some((_, rest)) = addr.split_once("://") else {
        return false;
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host_and_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host_and_port)| host_and_port);
    let explicit_default_port = match parsed.scheme() {
        "ws" => 80,
        "wss" => 443,
        _ => return false,
    };
    let expected_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    host_and_port == format!("{expected_host}:{explicit_default_port}")
}

fn websocket_url_supports_auth_token(parsed: &Url) -> bool {
    match (parsed.scheme(), parsed.host()) {
        ("wss", Some(_)) => true,
        ("ws", Some(url::Host::Domain(domain))) => domain.eq_ignore_ascii_case("localhost"),
        ("ws", Some(url::Host::Ipv4(addr))) => addr.is_loopback(),
        ("ws", Some(url::Host::Ipv6(addr))) => addr.is_loopback(),
        _ => false,
    }
}

pub fn resolve_remote_addr(addr: &str) -> color_eyre::Result<RemoteAppServerEndpoint> {
    if let Some(socket_path) = addr.strip_prefix("unix://") {
        let socket_path = if socket_path.is_empty() {
            let xedoc_home = find_xedoc_home().wrap_err("failed to resolve XEDOC_HOME")?;
            xedoc_app_server_client::app_server_control_socket_path(&xedoc_home)
                .map_err(color_eyre::Report::new)?
        } else {
            AbsolutePathBuf::relative_to_current_dir(socket_path)
                .map_err(color_eyre::Report::new)?
        };
        return Ok(RemoteAppServerEndpoint::UnixSocket { socket_path });
    }

    let parsed = match Url::parse(addr) {
        Ok(parsed) => parsed,
        Err(_) => {
            color_eyre::eyre::bail!(
                "invalid remote address `{addr}`; expected `ws://host:port`, `wss://host:port`, `unix://`, or `unix://PATH`"
            );
        }
    };
    if matches!(parsed.scheme(), "ws" | "wss")
        && parsed.host_str().is_some()
        && remote_addr_has_explicit_port(addr, &parsed)
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
    {
        return Ok(RemoteAppServerEndpoint::WebSocket {
            websocket_url: parsed.to_string(),
            auth_token: None,
        });
    }

    color_eyre::eyre::bail!(
        "invalid remote address `{addr}`; expected `ws://host:port`, `wss://host:port`, `unix://`, or `unix://PATH`"
    );
}

pub fn remote_addr_supports_auth_token(endpoint: &RemoteAppServerEndpoint) -> bool {
    match endpoint {
        RemoteAppServerEndpoint::WebSocket { websocket_url, .. } => {
            Url::parse(websocket_url).is_ok_and(|parsed| websocket_url_supports_auth_token(&parsed))
        }
        RemoteAppServerEndpoint::UnixSocket { .. } => false,
    }
}

#[cfg(unix)]
async fn maybe_probe_default_daemon_socket(xedoc_home: &Path) -> Option<AbsolutePathBuf> {
    let socket_path = xedoc_app_server_client::app_server_control_socket_path(xedoc_home).ok()?;
    maybe_probe_daemon_socket(socket_path).await
}

#[cfg(unix)]
async fn maybe_probe_daemon_socket(socket_path: AbsolutePathBuf) -> Option<AbsolutePathBuf> {
    if !socket_path.as_path().try_exists().unwrap_or(false) {
        return None;
    }

    match tokio::time::timeout(
        AUTO_CONNECT_DAEMON_CONNECT_TIMEOUT,
        tokio::net::UnixStream::connect(socket_path.as_path()),
    )
    .await
    {
        Ok(Ok(_stream)) => Some(socket_path),
        Ok(Err(err)) => {
            tracing::debug!(%err, socket_path = %socket_path.display(), "skipping default app-server daemon socket");
            None
        }
        Err(_) => {
            tracing::debug!(
                socket_path = %socket_path.display(),
                timeout_ms = AUTO_CONNECT_DAEMON_CONNECT_TIMEOUT.as_millis(),
                "timed out probing default app-server daemon socket"
            );
            None
        }
    }
}

#[cfg(not(unix))]
async fn maybe_probe_default_daemon_socket(_xedoc_home: &Path) -> Option<AbsolutePathBuf> {
    None
}

#[cfg(not(unix))]
async fn maybe_probe_daemon_socket(_socket_path: AbsolutePathBuf) -> Option<AbsolutePathBuf> {
    None
}

#[allow(clippy::too_many_arguments)]
async fn start_app_server(
    target: &AppServerTarget,
    arg0_paths: Arg0DispatchPaths,
    config: Config,
    cli_kv_overrides: Vec<(String, toml::Value)>,
    loader_overrides: LoaderOverrides,
    strict_config: bool,
    cloud_config_bundle: CloudConfigBundleLoader,
    log_db: Option<log_db::LogDbLayer>,
    state_db: Option<StateDbHandle>,
    environment_manager: Arc<EnvironmentManager>,
) -> color_eyre::Result<AppServerClient> {
    match target {
        AppServerTarget::Embedded => start_embedded_app_server(
            arg0_paths,
            config,
            cli_kv_overrides,
            loader_overrides,
            strict_config,
            cloud_config_bundle,
            log_db,
            state_db,
            environment_manager,
        )
        .await
        .map(AppServerClient::InProcess),
        AppServerTarget::LocalDaemon { endpoint } | AppServerTarget::Remote { endpoint } => {
            connect_remote_app_server(endpoint.clone()).await
        }
    }
}

pub(crate) async fn start_app_server_for_picker(
    config: &Config,
    target: &AppServerTarget,
    state_db: Option<StateDbHandle>,
    environment_manager: Arc<EnvironmentManager>,
) -> color_eyre::Result<AppServerSession> {
    let app_server = start_app_server(
        target,
        Arg0DispatchPaths::default(),
        config.clone(),
        Vec::new(),
        LoaderOverrides::default(),
        /*strict_config*/ false,
        CloudConfigBundleLoader::default(),
        /*log_db*/ None,
        state_db,
        environment_manager,
    )
    .await?;
    Ok(AppServerSession::new(
        app_server,
        target.thread_params_mode(),
    ))
}

#[cfg(test)]
pub(crate) async fn start_embedded_app_server_for_picker(
    config: &Config,
) -> color_eyre::Result<AppServerSession> {
    let state_db = init_state_db_for_app_server_target(config, &AppServerTarget::Embedded).await?;
    start_app_server_for_picker(
        config,
        &AppServerTarget::Embedded,
        state_db,
        Arc::new(EnvironmentManager::default_for_tests()),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn start_embedded_app_server_with<F, Fut>(
    arg0_paths: Arg0DispatchPaths,
    config: Config,
    cli_kv_overrides: Vec<(String, toml::Value)>,
    loader_overrides: LoaderOverrides,
    strict_config: bool,
    cloud_config_bundle: CloudConfigBundleLoader,
    log_db: Option<log_db::LogDbLayer>,
    state_db: Option<StateDbHandle>,
    environment_manager: Arc<EnvironmentManager>,
    start_client: F,
) -> color_eyre::Result<InProcessAppServerClient>
where
    F: FnOnce(InProcessClientStartArgs) -> Fut,
    Fut: Future<Output = std::io::Result<InProcessAppServerClient>>,
{
    let config_warnings = config
        .startup_warnings
        .iter()
        .map(|warning| ConfigWarningNotification {
            summary: warning.clone(),
            details: None,
            path: None,
            range: None,
        })
        .collect();
    let client = start_client(InProcessClientStartArgs {
        arg0_paths,
        config: Arc::new(config),
        cli_overrides: cli_kv_overrides,
        loader_overrides,
        strict_config,
        cloud_config_bundle,
        log_db,
        state_db,
        environment_manager,
        config_warnings,
        session_source: serde_json::from_value(serde_json::json!("cli"))
            .unwrap_or_else(|err| panic!("cli session source should deserialize: {err}")),
        enable_xedoc_api_key_env: false,
        client_name: "xedoc-tui".to_string(),
        client_version: xedoc_cli_version().to_string(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    })
    .await
    .wrap_err("failed to start embedded app server")?;
    Ok(client)
}

async fn shutdown_app_server_if_present(app_server: Option<AppServerSession>) {
    if let Some(app_server) = app_server
        && let Err(err) = app_server.shutdown().await
    {
        warn!(%err, "Failed to shut down temporary embedded app server");
    }
}

fn session_target_from_app_server_thread(
    thread: AppServerThread,
) -> Option<resume_picker::SessionTarget> {
    match ThreadId::from_string(&thread.id) {
        Ok(thread_id) => Some(resume_picker::SessionTarget {
            path: thread.path,
            thread_id,
        }),
        Err(err) => {
            warn!(
                thread_id = thread.id,
                %err,
                "Ignoring app-server thread with invalid thread id during TUI session lookup"
            );
            None
        }
    }
}

async fn lookup_session_target_by_name_with_app_server(
    app_server: &mut AppServerSession,
    name: &str,
) -> color_eyre::Result<Option<resume_picker::SessionTarget>> {
    let mut cursor = None;
    loop {
        let response = app_server
            .thread_list(ThreadListParams {
                cursor: cursor.clone(),
                limit: Some(100),
                sort_key: Some(AppServerThreadSortKey::UpdatedAt),
                sort_direction: None,
                model_providers: None,
                source_kinds: Some(vec![ThreadSourceKind::Cli, ThreadSourceKind::VsCode]),
                archived: Some(false),
                parent_thread_id: None,
                ancestor_thread_id: None,
                cwd: None,
                use_state_db_only: false,
                search_term: Some(name.to_string()),
            })
            .await?;
        if let Some(thread) = response
            .data
            .into_iter()
            .find(|thread| thread.name.as_deref() == Some(name))
        {
            return Ok(session_target_from_app_server_thread(thread));
        }
        if response.next_cursor.is_none() {
            return Ok(None);
        }
        cursor = response.next_cursor;
    }
}

async fn lookup_session_target_with_app_server(
    app_server: &mut AppServerSession,
    id_or_name: &str,
) -> color_eyre::Result<Option<resume_picker::SessionTarget>> {
    if Uuid::parse_str(id_or_name).is_ok() {
        let thread_id = match ThreadId::from_string(id_or_name) {
            Ok(thread_id) => thread_id,
            Err(err) => {
                warn!(
                    session = id_or_name,
                    %err,
                    "Failed to parse session id during TUI lookup"
                );
                return Ok(None);
            }
        };
        return match app_server
            .thread_read(thread_id, /*include_turns*/ false)
            .await
        {
            Ok(thread) => Ok(session_target_from_app_server_thread(thread)),
            Err(err) => {
                warn!(
                    session = id_or_name,
                    %err,
                    "thread/read failed during TUI session lookup"
                );
                Ok(None)
            }
        };
    }

    lookup_session_target_by_name_with_app_server(app_server, id_or_name).await
}

async fn lookup_latest_session_target_with_app_server(
    app_server: &mut AppServerSession,
    cwd_filter: Option<&Path>,
    include_non_interactive: bool,
) -> color_eyre::Result<Option<resume_picker::SessionTarget>> {
    let uses_remote_workspace = app_server.uses_remote_workspace();
    for lookup_mode in [
        LatestSessionLookupMode::StateDbOnly,
        LatestSessionLookupMode::ScanAndRepair,
    ] {
        let response = app_server
            .thread_list(latest_session_lookup_params(
                cwd_filter,
                include_non_interactive,
                lookup_mode,
            ))
            .await?;
        let target = response
            .data
            .into_iter()
            .find_map(session_target_from_app_server_thread);
        if target.as_ref().is_some_and(|target| {
            uses_remote_workspace || target.path.as_deref().is_some_and(std::path::Path::exists)
        }) {
            return Ok(target);
        }
    }
    Ok(None)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LatestSessionLookupMode {
    StateDbOnly,
    ScanAndRepair,
}

fn latest_session_lookup_params(
    cwd_filter: Option<&Path>,
    include_non_interactive: bool,
    lookup_mode: LatestSessionLookupMode,
) -> ThreadListParams {
    ThreadListParams {
        cursor: None,
        limit: Some(1),
        sort_key: Some(AppServerThreadSortKey::UpdatedAt),
        sort_direction: None,
        // `thread/list` treats an empty provider list as "any provider". Sessions record the
        // provider they last ran with, so filtering by the currently configured provider would
        // skip the genuinely most recent session after a cross-provider switch. This matches the
        // session picker, which also lists threads across providers.
        model_providers: Some(Vec::new()),
        source_kinds: Some(resume_source_kinds(include_non_interactive)),
        archived: Some(false),
        parent_thread_id: None,
        ancestor_thread_id: None,
        cwd: cwd_filter.map(|cwd| ThreadListCwdFilter::One(cwd.to_string_lossy().to_string())),
        use_state_db_only: match lookup_mode {
            LatestSessionLookupMode::StateDbOnly => true,
            LatestSessionLookupMode::ScanAndRepair => false,
        },
        search_term: None,
    }
}

fn config_cwd_for_app_server_target(
    cwd: Option<&Path>,
    app_server_target: &AppServerTarget,
    environment_manager: &EnvironmentManager,
) -> std::io::Result<Option<AbsolutePathBuf>> {
    if uses_remote_workspace_or_environment(app_server_target, environment_manager) {
        return Ok(None);
    }

    let cwd = match cwd {
        Some(path) => {
            AbsolutePathBuf::from_absolute_path(canonicalize_existing_preserving_symlinks(path)?)
        }
        None => AbsolutePathBuf::current_dir(),
    }?;
    Ok(Some(cwd))
}

fn uses_remote_workspace_or_environment(
    app_server_target: &AppServerTarget,
    environment_manager: &EnvironmentManager,
) -> bool {
    app_server_target.uses_remote_workspace()
        || environment_manager
            .default_environment()
            .is_some_and(|environment| environment.is_remote())
}

async fn resolve_startup_resume_or_fork_cwd(
    tui: &mut Tui,
    config: &Config,
    state_db: Option<&xedoc_state::StateRuntime>,
    session_selection: &resume_picker::SessionSelection,
    cwd_override: Option<&Path>,
    uses_remote_workspace: bool,
    uses_remote_workspace_or_environment: bool,
) -> color_eyre::Result<ResolveCwdOutcome> {
    let Some((action, target_session)) = (match session_selection {
        resume_picker::SessionSelection::Resume(target_session) => {
            Some((CwdPromptAction::Resume, target_session))
        }
        resume_picker::SessionSelection::Fork(target_session) => {
            Some((CwdPromptAction::Fork, target_session))
        }
        _ => None,
    }) else {
        return Ok(ResolveCwdOutcome::Continue(None));
    };
    let resume_cwd_mode = effective_resume_cwd_mode(config.tui_resume_cwd, cwd_override);
    if uses_remote_workspace_or_environment
        && cwd_override.is_none()
        && matches!(resume_cwd_mode, Some(ResumeCwdMode::Current))
    {
        color_eyre::eyre::bail!(
            "`tui.resume_cwd = \"current\"` requires `--cd` when using a remote workspace"
        );
    }
    if uses_remote_workspace {
        return Ok(ResolveCwdOutcome::Continue(Some(config.cwd.to_path_buf())));
    }

    resolve_cwd_for_resume_or_fork(
        tui,
        config,
        state_db,
        target_session,
        action,
        ResumeCwdContext {
            current_cwd: config.cwd.as_path(),
            remembered_current_cwd: config.cwd.as_path(),
            allow_remember_current: !uses_remote_workspace_or_environment || cwd_override.is_some(),
            mode: resume_cwd_mode,
        },
    )
    .await
}

fn should_load_configured_environments(
    loader_overrides: &LoaderOverrides,
    app_server_target: &AppServerTarget,
) -> bool {
    !loader_overrides.ignore_user_config && !app_server_target.uses_remote_workspace()
}

fn latest_session_cwd_filter<'a>(
    uses_remote_workspace: bool,
    remote_cwd_override: Option<&'a Path>,
    config: &'a Config,
    show_all: bool,
) -> Option<&'a Path> {
    if show_all {
        return None;
    }

    if uses_remote_workspace {
        remote_cwd_override
    } else {
        Some(config.cwd.as_path())
    }
}

fn app_server_target_for_launch(
    explicit_remote_endpoint: Option<RemoteAppServerEndpoint>,
    default_daemon_socket: Option<AbsolutePathBuf>,
    can_reuse_implicit_local_daemon: bool,
) -> AppServerTarget {
    match explicit_remote_endpoint {
        Some(endpoint) => AppServerTarget::Remote { endpoint },
        None if can_reuse_implicit_local_daemon => {
            default_daemon_socket.map_or(AppServerTarget::Embedded, |socket_path| {
                AppServerTarget::LocalDaemon {
                    endpoint: RemoteAppServerEndpoint::UnixSocket { socket_path },
                }
            })
        }
        None => AppServerTarget::Embedded,
    }
}

fn loader_overrides_are_default(loader_overrides: &LoaderOverrides) -> bool {
    let loader_overrides_are_default = loader_overrides.user_config_path.is_none()
        && loader_overrides.user_config_profile.is_none()
        && loader_overrides.managed_config_path.is_none()
        && loader_overrides.system_config_path.is_none()
        && loader_overrides.system_requirements_path.is_none()
        && !loader_overrides.ignore_managed_requirements
        && !loader_overrides.ignore_user_config
        && !loader_overrides.ignore_user_and_project_exec_policy_rules
        && loader_overrides
            .macos_managed_config_requirements_base64
            .is_none();
    #[cfg(target_os = "macos")]
    let loader_overrides_are_default =
        loader_overrides_are_default && loader_overrides.managed_preferences_base64.is_none();
    loader_overrides_are_default
}

fn can_reuse_implicit_local_daemon(
    cli_kv_overrides: &[(String, toml::Value)],
    loader_overrides: &LoaderOverrides,
    strict_config: bool,
    has_non_replayable_launch_overrides: bool,
) -> bool {
    // A reused daemon cannot adopt this invocation's full launch config state.
    cli_kv_overrides.is_empty()
        && loader_overrides_are_default(loader_overrides)
        && !strict_config
        && !has_non_replayable_launch_overrides
}

pub async fn run_main(
    cli: Cli,
    arg0_paths: Arg0DispatchPaths,
    loader_overrides: LoaderOverrides,
    explicit_remote_endpoint: Option<RemoteAppServerEndpoint>,
) -> std::io::Result<AppExitInfo> {
    run_main_with_local_daemon_socket(
        cli,
        arg0_paths,
        loader_overrides,
        explicit_remote_endpoint,
        /*local_daemon_socket*/ None,
    )
    .await
}

pub async fn run_main_with_local_daemon_socket(
    mut cli: Cli,
    arg0_paths: Arg0DispatchPaths,
    loader_overrides: LoaderOverrides,
    explicit_remote_endpoint: Option<RemoteAppServerEndpoint>,
    local_daemon_socket: Option<AbsolutePathBuf>,
) -> std::io::Result<AppExitInfo> {
    let strict_config = cli.strict_config;
    let (sandbox_mode, approval_policy) = if cli.dangerously_bypass_approvals_and_sandbox {
        (
            Some(SandboxMode::DangerFullAccess),
            Some(AskForApproval::Never.to_core()),
        )
    } else {
        (
            cli.sandbox_mode.map(Into::<SandboxMode>::into),
            cli.approval_policy.map(Into::into),
        )
    };

    // Map the legacy --search flag to the canonical web_search mode.
    if cli.web_search {
        cli.config_overrides
            .raw_overrides
            .push("web_search=\"live\"".to_string());
    }

    // When using `--oss`, let the bootstrapper pick the model (defaulting to
    // gpt-oss:20b) and ensure it is present locally. Also, force the built‑in
    let raw_overrides = cli.config_overrides.raw_overrides.clone();
    // `oss` model provider.
    let overrides_cli = xedoc_utils_cli::CliConfigOverrides { raw_overrides };
    let cli_kv_overrides = match overrides_cli.parse_overrides() {
        // Parse `-c` overrides from the CLI.
        Ok(v) => v,
        #[allow(clippy::print_stderr)]
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };

    // we load config.toml here to determine project state.
    #[allow(clippy::print_stderr)]
    let xedoc_home = match find_xedoc_home() {
        Ok(xedoc_home) => xedoc_home.to_path_buf(),
        Err(err) => {
            eprintln!("Error finding xedoc home: {err}");
            std::process::exit(1);
        }
    };

    let mut launch_loader_overrides = loader_overrides.clone();
    if let Some(profile_v2) = cli.config_profile_v2.as_ref() {
        let user_config_path = resolve_profile_v2_config_path(&xedoc_home, profile_v2);
        launch_loader_overrides.user_config_path = Some(user_config_path);
        launch_loader_overrides.user_config_profile = Some(profile_v2.clone());
    }
    let reuse_implicit_local_daemon = can_reuse_implicit_local_daemon(
        &cli_kv_overrides,
        &launch_loader_overrides,
        strict_config,
        cli.bypass_hook_trust,
    );
    let default_daemon = if explicit_remote_endpoint.is_none() && reuse_implicit_local_daemon {
        match local_daemon_socket {
            Some(socket_path) => maybe_probe_daemon_socket(socket_path).await,
            None => maybe_probe_default_daemon_socket(&xedoc_home).await,
        }
    } else {
        None
    };
    let app_server_target = app_server_target_for_launch(
        explicit_remote_endpoint,
        default_daemon,
        reuse_implicit_local_daemon,
    );
    let remote_cwd_override = cli
        .cwd
        .clone()
        .filter(|_| app_server_target.uses_remote_workspace());

    let local_runtime_paths = ExecServerRuntimePaths::from_optional_paths(
        arg0_paths.xedoc_self_exe.clone(),
        arg0_paths.xedoc_linux_sandbox_exe.clone(),
    )?;
    let environment_manager =
        if should_load_configured_environments(&loader_overrides, &app_server_target) {
            EnvironmentManager::from_xedoc_home(xedoc_home.clone(), Some(local_runtime_paths)).await
        } else {
            EnvironmentManager::from_env(Some(local_runtime_paths)).await
        }
        .map(Arc::new)
        .map_err(std::io::Error::other)?;
    let cwd = cli.cwd.clone();
    let config_cwd =
        config_cwd_for_app_server_target(cwd.as_deref(), &app_server_target, &environment_manager)?;
    let mut loader_overrides = loader_overrides;
    if let Some(profile_v2) = cli.config_profile_v2.as_ref() {
        let user_config_path = resolve_profile_v2_config_path(&xedoc_home, profile_v2);
        loader_overrides.user_config_path = Some(user_config_path);
        loader_overrides.user_config_profile = Some(profile_v2.clone());
    }

    let bootstrap_config = load_bootstrap_config_or_exit(
        &xedoc_home,
        config_cwd.as_ref(),
        cli_kv_overrides.clone(),
        loader_overrides.clone(),
        strict_config,
        CloudConfigBundleLoader::default(),
    )
    .await;
    let bootstrap_config_toml = &bootstrap_config.config_toml;

    let cloud_config_bundle = CloudConfigBundleLoader::default();

    let cwd_override = if app_server_target.uses_remote_workspace() {
        None
    } else {
        cwd.clone()
    };

    let mut manually_selected_oss_provider = None;
    let model_provider_override = if cli.oss {
        let resolved = resolve_oss_provider(cli.oss_provider.as_deref(), bootstrap_config_toml);

        if let Some(provider) = resolved {
            Some(provider)
        } else {
            // No provider configured, prompt the user
            let selection = oss_selection::select_oss_provider().await?;
            let provider = selection.provider;
            if provider == "__CANCELLED__" {
                return Err(std::io::Error::other(
                    "OSS provider selection was cancelled by user",
                ));
            }
            if selection.manually_selected {
                manually_selected_oss_provider = Some(provider.clone());
            }
            Some(provider)
        }
    } else {
        None
    };

    // When using `--oss`, let the bootstrapper pick the model based on selected provider
    let model = if let Some(model) = &cli.model {
        Some(model.clone())
    } else if cli.oss {
        // Use the provider from model_provider_override
        model_provider_override
            .as_ref()
            .and_then(|provider_id| get_default_model_for_oss_provider(provider_id))
            .map(std::borrow::ToOwned::to_owned)
    } else {
        None // No model specified, will use the default.
    };

    let additional_dirs = cli.add_dir.clone();

    let overrides = ConfigOverrides {
        model,
        approval_policy,
        sandbox_mode,
        cwd: cwd_override,
        model_provider: model_provider_override.clone(),
        xedoc_self_exe: arg0_paths.xedoc_self_exe.clone(),
        xedoc_linux_sandbox_exe: arg0_paths.xedoc_linux_sandbox_exe.clone(),
        main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe.clone(),
        show_raw_agent_reasoning: cli.oss.then_some(true),
        bypass_hook_trust: cli.bypass_hook_trust.then_some(true),
        additional_writable_roots: additional_dirs,
        ..Default::default()
    };

    let config = load_config_or_exit(
        cli_kv_overrides.clone(),
        overrides.clone(),
        loader_overrides.clone(),
        cloud_config_bundle.clone(),
        strict_config,
    )
    .await;

    remove_legacy_tui_log_file(config.xedoc_home.as_path());

    let otel_originator = originator().value;
    let otel = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        xedoc_app_server_client::build_otel_provider(
            &config,
            xedoc_cli_version(),
            /*service_name_override*/ None,
        )
    })) {
        Ok(Ok(otel)) => otel,
        Ok(Err(e)) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("Could not create otel exporter: {e}");
            }
            None
        }
        Err(_) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("Could not create otel exporter: panicked during initialization");
            }
            None
        }
    };
    if let Some(metrics) = otel.as_ref().and_then(xedoc_otel::OtelProvider::metrics) {
        let _ = xedoc_otel::record_process_start_once(metrics, otel_originator.as_str());
        let telemetry =
            xedoc_rollout::sqlite_telemetry_recorder(metrics.clone(), otel_originator.as_str());
        let _ = xedoc_state::install_process_db_telemetry(telemetry);
    }
    let state_db = init_state_db_for_app_server_target(&config, &app_server_target).await?;
    let config_toml_log_dir_configured = config
        .config_layer_stack
        .effective_config()
        .as_table()
        .is_some_and(|table| table.contains_key("log_dir"));

    set_default_client_residency_requirement(config.enforce_residency.value());

    if let Some(warning) = add_dir_warning_message(
        &cli.add_dir,
        &config.permissions.effective_permission_profile(),
        config.cwd.as_path(),
    ) {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("Error adding directories: {warning}");
            std::process::exit(1);
        }
    }

    if !app_server_target.uses_remote_workspace() {
        let auth_route_config = config.auth_route_config();
        #[allow(clippy::print_stderr)]
        if let Err(err) = enforce_login_restrictions(&AuthConfig {
            xedoc_home: config.xedoc_home.to_path_buf(),
            auth_credentials_store_mode: config.cli_auth_credentials_store_mode,
            keyring_backend_kind: config.auth_keyring_backend_kind(),
            forced_login_method: config.forced_login_method,
            forced_chatgpt_workspace_id: config.forced_chatgpt_workspace_id.clone(),
            chatgpt_base_url: Some(config.chatgpt_base_url.clone()),
            auth_route_config,
        })
        .await
        {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }

    let (tui_file_layer, _tui_file_log_guard) = if config_toml_log_dir_configured {
        let log_dir = config.log_dir.clone();
        std::fs::create_dir_all(&log_dir)?;
        let mut log_file_opts = OpenOptions::new();
        log_file_opts.create(true).append(true);

        // Ensure the file is only readable and writable by the current user.
        // Doing the equivalent to `chmod 600` on Windows is quite a bit more
        // code and requires the Windows API crates.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            log_file_opts.mode(0o600);
        }

        let log_file = log_file_opts.open(log_dir.join(TUI_LOG_FILE_NAME))?;
        let (non_blocking, guard) = non_blocking(log_file);
        let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("xedoc_core=info,xedoc_tui=info,xedoc_rmcp_client=info")
        });
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking)
            .with_target(true)
            .with_ansi(false)
            .with_span_events(
                tracing_subscriber::fmt::format::FmtSpan::NEW
                    | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
            )
            .with_filter(env_filter);
        (Some(file_layer), Some(guard))
    } else {
        (None, None)
    };

    if cli.oss && model_provider_override.is_some() {
        // We're in the oss section, so provider_id should be Some
        // Let's handle None case gracefully though just in case
        let provider_id = match model_provider_override.as_ref() {
            Some(id) => id,
            None => {
                error!("OSS provider unexpectedly not set when oss flag is used");
                return Err(std::io::Error::other(
                    "OSS provider not set but oss flag was used",
                ));
            }
        };
        ensure_oss_provider_ready(provider_id, &config).await?;
    }

    let otel_logger_layer = otel.as_ref().and_then(|o| o.logger_layer());

    let otel_tracing_layer = otel.as_ref().and_then(|o| o.tracing_layer());

    let log_db = state_db.clone().map(log_db::start);
    let log_db_layer = log_db
        .clone()
        .map(|layer| layer.with_filter(log_db::default_filter()));

    let _ = tracing_subscriber::registry()
        .with(tui_file_layer)
        .with(log_db_layer)
        .with(otel_logger_layer)
        .with(otel_tracing_layer)
        .try_init();

    run_ratatui_app(
        cli,
        arg0_paths,
        loader_overrides,
        strict_config,
        app_server_target,
        remote_cwd_override,
        config,
        manually_selected_oss_provider,
        overrides,
        cli_kv_overrides,
        cloud_config_bundle,
        log_db,
        state_db,
        environment_manager,
    )
    .await
    .map_err(|err| std::io::Error::other(err.to_string()))
}

#[allow(clippy::too_many_arguments)]
async fn run_ratatui_app(
    cli: Cli,
    arg0_paths: Arg0DispatchPaths,
    loader_overrides: LoaderOverrides,
    strict_config: bool,
    app_server_target: AppServerTarget,
    remote_cwd_override: Option<PathBuf>,
    initial_config: Config,
    manually_selected_oss_provider: Option<String>,
    overrides: ConfigOverrides,
    cli_kv_overrides: Vec<(String, toml::Value)>,
    cloud_config_bundle: CloudConfigBundleLoader,
    log_db: Option<log_db::LogDbLayer>,
    state_db: Option<StateDbHandle>,
    environment_manager: Arc<EnvironmentManager>,
) -> color_eyre::Result<AppExitInfo> {
    let uses_remote_workspace = app_server_target.uses_remote_workspace();
    color_eyre::install()?;

    // Forward panic reports through tracing so they appear in the UI status
    // line, but do not swallow the default/color-eyre panic handler.
    // Chain to the previous hook so users still get a rich panic report
    // (including backtraces) after we restore the terminal.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        prev_hook(info);
    }));
    let mut initialized_terminal = tui::init()?;
    initialized_terminal.terminal.clear()?;

    let mut tui = Tui::new(
        initialized_terminal.terminal,
        initialized_terminal.enhanced_keys_supported,
        initialized_terminal.stderr_guard,
    );
    let mut terminal_restore_guard = TerminalRestoreGuard::new();

    #[cfg(not(debug_assertions))]
    {
        use crate::update_prompt::UpdatePromptOutcome;

        let skip_update_prompt = cli.prompt.as_ref().is_some_and(|prompt| !prompt.is_empty());
        if !skip_update_prompt {
            match update_prompt::run_update_prompt_if_needed(&mut tui, &initial_config).await? {
                UpdatePromptOutcome::Continue => {}
                UpdatePromptOutcome::RunUpdate(action) => {
                    terminal_restore_guard.restore()?;
                    return Ok(AppExitInfo {
                        token_usage: crate::token_usage::TokenUsage::default(),
                        thread_id: None,
                        resume_hint: None,
                        update_action: Some(action),
                        exit_reason: ExitReason::UserRequested,
                    });
                }
            }
        }
    }

    // Initialize high-fidelity session event logging if enabled.
    session_log::maybe_init(&initial_config);

    let app_server_session = match start_app_server(
        &app_server_target,
        arg0_paths.clone(),
        initial_config.clone(),
        cli_kv_overrides.clone(),
        loader_overrides.clone(),
        strict_config,
        cloud_config_bundle.clone(),
        log_db.clone(),
        state_db.clone(),
        environment_manager.clone(),
    )
    .await
    {
        Ok(app_server) => AppServerSession::new(app_server, app_server_target.thread_params_mode()),
        Err(err) => {
            terminal_restore_guard.restore_silently();
            session_log::log_session_end();
            return Err(err);
        }
    }
    .with_remote_cwd_override(remote_cwd_override.clone());
    if let Some(provider) = manually_selected_oss_provider.as_deref()
        && let Err(err) = config_update::write_config_batch(
            app_server_session.request_handle(),
            vec![config_update::build_oss_provider_edit(provider)],
        )
        .await
    {
        warn!(
            %err,
            provider,
            "Failed to persist selected OSS provider preference"
        );
    }
    let mut app_server = Some(app_server_session);

    let should_show_trust_screen_flag =
        !uses_remote_workspace && should_show_trust_screen(&initial_config);
    let login_status = {
        let Some(app_server_session) = app_server.as_mut() else {
            unreachable!("app server should exist before the login status read");
        };
        get_login_status(app_server_session).await?
    };
    let should_show_onboarding = should_show_trust_screen_flag;

    let config = if should_show_onboarding {
        let onboarding_result = run_onboarding_app(
            OnboardingScreenArgs {
                show_trust_screen: should_show_trust_screen_flag,
                app_server_request_handle: app_server
                    .as_ref()
                    .map(AppServerSession::request_handle),
                config: initial_config.clone(),
            },
            &mut tui,
        )
        .await?;
        if onboarding_result.should_exit {
            shutdown_app_server_if_present(app_server.take()).await;
            terminal_restore_guard.restore_silently();
            session_log::log_session_end();
            let _ = tui.terminal.clear();
            return Ok(AppExitInfo {
                token_usage: crate::token_usage::TokenUsage::default(),
                thread_id: None,
                resume_hint: None,
                update_action: None,
                exit_reason: ExitReason::UserRequested,
            });
        }
        // Reload config after an explicit trust decision so current process
        // state reflects the persisted project configuration.
        if onboarding_result.directory_trust_persisted {
            load_config_or_exit(
                cli_kv_overrides.clone(),
                overrides.clone(),
                loader_overrides.clone(),
                cloud_config_bundle.clone(),
                strict_config,
            )
            .await
        } else {
            initial_config
        }
    } else {
        initial_config
    };

    let mut missing_session_exit = |id_str: &str, action: &str| {
        error!("Error finding conversation path: {id_str}");
        terminal_restore_guard.restore_silently();
        session_log::log_session_end();
        let _ = tui.terminal.clear();
        Ok(AppExitInfo {
            token_usage: crate::token_usage::TokenUsage::default(),
            thread_id: None,
            resume_hint: None,
            update_action: None,
            exit_reason: ExitReason::Fatal(format!(
                "No saved session found with ID {id_str}. Run `xedoc {action}` without an ID to choose from existing sessions."
            )),
        })
    };

    let use_fork = cli.fork_picker || cli.fork_last || cli.fork_session_id.is_some();
    let session_selection = if use_fork {
        if let Some(id_str) = cli.fork_session_id.as_deref() {
            let Some(startup_app_server) = app_server.as_mut() else {
                unreachable!("app server should be initialized for --fork <id>");
            };
            match lookup_session_target_with_app_server(startup_app_server, id_str).await? {
                Some(target_session) => resume_picker::SessionSelection::Fork(target_session),
                None => {
                    shutdown_app_server_if_present(app_server.take()).await;
                    return missing_session_exit(id_str, "fork");
                }
            }
        } else if cli.fork_last {
            let filter_cwd = latest_session_cwd_filter(
                uses_remote_workspace,
                remote_cwd_override.as_deref(),
                &config,
                cli.fork_show_all,
            );
            let Some(app_server) = app_server.as_mut() else {
                unreachable!("app server should be initialized for --fork --last");
            };
            match lookup_latest_session_target_with_app_server(
                app_server, filter_cwd, /*include_non_interactive*/ false,
            )
            .await?
            {
                Some(target_session) => resume_picker::SessionSelection::Fork(target_session),
                None => resume_picker::SessionSelection::StartFresh,
            }
        } else if cli.fork_picker {
            let Some(app_server) = app_server.take() else {
                unreachable!("app server should be initialized for --fork picker");
            };
            match resume_picker::run_fork_picker_with_app_server(
                &mut tui,
                &config,
                cli.fork_show_all,
                app_server,
            )
            .await?
            {
                resume_picker::SessionSelection::Exit => {
                    terminal_restore_guard.restore_silently();
                    session_log::log_session_end();
                    return Ok(AppExitInfo {
                        token_usage: crate::token_usage::TokenUsage::default(),
                        thread_id: None,
                        resume_hint: None,
                        update_action: None,
                        exit_reason: ExitReason::UserRequested,
                    });
                }
                other => other,
            }
        } else {
            resume_picker::SessionSelection::StartFresh
        }
    } else if let Some(id_str) = cli.resume_session_id.as_deref() {
        let Some(startup_app_server) = app_server.as_mut() else {
            unreachable!("app server should be initialized for --resume <id>");
        };
        match lookup_session_target_with_app_server(startup_app_server, id_str).await? {
            Some(target_session) => resume_picker::SessionSelection::Resume(target_session),
            None => {
                shutdown_app_server_if_present(app_server.take()).await;
                return missing_session_exit(id_str, "resume");
            }
        }
    } else if cli.resume_last {
        let filter_cwd = latest_session_cwd_filter(
            uses_remote_workspace,
            remote_cwd_override.as_deref(),
            &config,
            cli.resume_show_all,
        );
        let Some(app_server) = app_server.as_mut() else {
            unreachable!("app server should be initialized for --resume --last");
        };
        match lookup_latest_session_target_with_app_server(
            app_server,
            filter_cwd,
            cli.resume_include_non_interactive,
        )
        .await?
        {
            Some(target_session) => resume_picker::SessionSelection::Resume(target_session),
            None => resume_picker::SessionSelection::StartFresh,
        }
    } else if cli.resume_picker {
        let Some(app_server) = app_server.take() else {
            unreachable!("app server should be initialized for --resume picker");
        };
        match resume_picker::run_resume_picker_with_app_server(
            &mut tui,
            &config,
            cli.resume_show_all,
            cli.resume_include_non_interactive,
            app_server,
        )
        .await?
        {
            resume_picker::SessionSelection::Exit => {
                terminal_restore_guard.restore_silently();
                session_log::log_session_end();
                return Ok(AppExitInfo {
                    token_usage: crate::token_usage::TokenUsage::default(),
                    thread_id: None,
                    resume_hint: None,
                    update_action: None,
                    exit_reason: ExitReason::UserRequested,
                });
            }
            other => other,
        }
    } else {
        resume_picker::SessionSelection::StartFresh
    };

    let current_cwd = config.cwd.clone();
    let fallback_cwd = match resolve_startup_resume_or_fork_cwd(
        &mut tui,
        &config,
        state_db.as_deref(),
        &session_selection,
        cli.cwd.as_deref(),
        uses_remote_workspace,
        uses_remote_workspace_or_environment(&app_server_target, &environment_manager),
    )
    .await
    {
        Ok(ResolveCwdOutcome::Continue(cwd)) => cwd,
        Ok(ResolveCwdOutcome::Exit) => {
            terminal_restore_guard.restore_silently();
            session_log::log_session_end();
            return Ok(AppExitInfo {
                token_usage: crate::token_usage::TokenUsage::default(),
                thread_id: None,
                resume_hint: None,
                update_action: None,
                exit_reason: ExitReason::UserRequested,
            });
        }
        Err(err) => {
            terminal_restore_guard.restore_silently();
            session_log::log_session_end();
            return Err(err);
        }
    };

    let picker_cancelled_without_selection = matches!(
        session_selection,
        resume_picker::SessionSelection::StartFresh
    ) && (cli.resume_picker || cli.fork_picker);

    let mut config = match &session_selection {
        resume_picker::SessionSelection::Resume(_) | resume_picker::SessionSelection::Fork(_) => {
            load_config_or_exit_with_fallback_cwd(
                cli_kv_overrides.clone(),
                overrides.clone(),
                loader_overrides.clone(),
                cloud_config_bundle.clone(),
                strict_config,
                fallback_cwd,
            )
            .await
        }
        resume_picker::SessionSelection::StartFresh if picker_cancelled_without_selection => {
            load_config_or_exit(
                cli_kv_overrides.clone(),
                overrides.clone(),
                loader_overrides.clone(),
                cloud_config_bundle.clone(),
                strict_config,
            )
            .await
        }
        _ => config,
    };

    // Configure syntax highlighting theme from the final config — onboarding
    // and resume/fork can both reload config with a different tui_theme, so
    // this must happen after the last possible reload.
    if let Some(w) = crate::render::highlight::set_theme_override(
        config.tui_theme.clone(),
        find_xedoc_home().ok().map(AbsolutePathBuf::into_path_buf),
    ) {
        config.startup_warnings.push(w);
    }

    set_default_client_residency_requirement(config.enforce_residency.value());
    let should_show_trust_screen = should_show_trust_screen(&config);
    let Cli {
        prompt,
        shared,
        no_alt_screen,
        ..
    } = cli;
    let images = shared.into_inner().images;

    let use_alt_screen = determine_alt_screen_mode(no_alt_screen, config.tui_alternate_screen);
    tui.set_alt_screen_enabled(use_alt_screen);
    let mut app_server = match app_server {
        Some(app_server) => app_server,
        None => match start_app_server(
            &app_server_target,
            arg0_paths,
            config.clone(),
            cli_kv_overrides.clone(),
            loader_overrides.clone(),
            strict_config,
            cloud_config_bundle.clone(),
            log_db.clone(),
            state_db.clone(),
            environment_manager.clone(),
        )
        .await
        {
            Ok(app_server) => {
                AppServerSession::new(app_server, app_server_target.thread_params_mode())
                    .with_remote_cwd_override(remote_cwd_override.clone())
            }
            Err(err) => {
                terminal_restore_guard.restore_silently();
                session_log::log_session_end();
                return Err(err);
            }
        },
    };

    // Persistent app-server resumes may attach to an already-running thread,
    // where resume config overrides are ignored.
    let is_persistent_resume = !matches!(&app_server_target, AppServerTarget::Embedded)
        && matches!(
            &session_selection,
            resume_picker::SessionSelection::Resume(_)
        );
    let bypass_hook_trust_for_startup_review = config.bypass_hook_trust && !is_persistent_resume;
    let hooks_request_handle = app_server.request_handle();
    let hooks_cwd = config.cwd.to_path_buf();
    let startup_prefetch_started_at = Instant::now();
    tracing::info!(
        session_selection = ?session_selection,
        app_server_target = ?app_server_target,
        "tui bootstrap prefetch started"
    );
    let (startup_bootstrap, startup_hooks_entry) = tokio::join!(
        app_server.bootstrap(&config),
        load_startup_hooks_review_entry(hooks_request_handle, hooks_cwd),
    );
    let startup_bootstrap = Some(startup_bootstrap?);
    let startup_elapsed_before_app = startup_prefetch_started_at.elapsed();
    tracing::info!(
        duration_ms = %startup_elapsed_before_app.as_millis(),
        "tui bootstrap prefetch completed"
    );
    let startup_hooks_browser = match maybe_run_startup_hooks_review(
        &mut app_server,
        &mut tui,
        &config,
        bypass_hook_trust_for_startup_review,
        startup_hooks_entry,
    )
    .await?
    {
        StartupHooksReviewOutcome::Continue => None,
        StartupHooksReviewOutcome::OpenHooksBrowser(data) => Some(data),
    };

    let app_result = App::run(
        &mut tui,
        app_server,
        config,
        current_cwd.to_path_buf(),
        cli_kv_overrides.clone(),
        overrides.clone(),
        loader_overrides.clone(),
        cloud_config_bundle,
        prompt,
        images,
        session_selection,
        should_show_trust_screen, // Proxy to: is it a first run in this directory?
        login_status == LoginStatus::NotAuthenticated,
        app_server_target,
        state_db,
        environment_manager,
        startup_elapsed_before_app,
        startup_bootstrap,
        startup_hooks_browser,
        cli.gui_hosted,
    )
    .await;

    terminal_restore_guard.restore_silently();
    // Mark the end of the recorded session.
    session_log::log_session_end();
    // ignore error when collecting usage – report underlying error instead
    app_result
}

#[expect(
    clippy::print_stderr,
    reason = "TUI should no longer be displayed, so we can write to stderr."
)]
fn restore() {
    if let Err(err) = tui::restore_after_exit() {
        eprintln!(
            "failed to restore terminal. Run `reset` or restart your terminal to recover: {err}"
        );
    }
}

struct TerminalRestoreGuard {
    active: bool,
}

impl TerminalRestoreGuard {
    fn new() -> Self {
        Self { active: true }
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    fn restore(&mut self) -> color_eyre::Result<()> {
        if self.active {
            crate::tui::restore_after_exit()?;
            self.active = false;
        }
        Ok(())
    }

    fn restore_silently(&mut self) {
        if self.active {
            restore();
            self.active = false;
        }
    }
}

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        self.restore_silently();
    }
}

/// Determine whether to use the terminal's alternate screen buffer.
///
/// - If `--no-alt-screen` is explicitly passed, always disable alternate screen
/// - Otherwise, respect the `tui.alternate_screen` config setting:
///   - `always`: Use alternate screen
///   - `never`: Inline mode only, preserves scrollback
///   - `auto` (default): Use alternate screen
fn determine_alt_screen_mode(no_alt_screen: bool, tui_alternate_screen: AltScreenMode) -> bool {
    if no_alt_screen {
        return false;
    }

    tui_alternate_screen != AltScreenMode::Never
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStatus {
    AuthMode(AuthMode),
    OtherProvider,
    NotAuthenticated,
}

/// Determines the user's authentication mode using a lightweight account read
/// rather than a full `bootstrap`, avoiding the model-list fetch and
/// rate-limit round-trip that `bootstrap` would trigger.
async fn get_login_status(app_server: &mut AppServerSession) -> color_eyre::Result<LoginStatus> {
    let account = app_server.read_account().await?;
    let login_status = match account.account {
        Some(AppServerAccount::ApiKey {}) => LoginStatus::AuthMode(AuthMode::ApiKey),
        Some(AppServerAccount::Chatgpt { .. }) => LoginStatus::AuthMode(AuthMode::Chatgpt),
        Some(AppServerAccount::AmazonBedrock { .. }) => LoginStatus::OtherProvider,
        None => {
            let model_manager = app_server.read_model_manager().await?;
            if model_manager.providers.iter().any(|provider| {
                provider.id != "openai"
                    && (provider.api_key_configured || provider.oauth_configured)
            }) {
                LoginStatus::OtherProvider
            } else {
                LoginStatus::NotAuthenticated
            }
        }
    };
    Ok(login_status)
}

async fn load_config_or_exit(
    cli_kv_overrides: Vec<(String, toml::Value)>,
    overrides: ConfigOverrides,
    loader_overrides: LoaderOverrides,
    cloud_config_bundle: CloudConfigBundleLoader,
    strict_config: bool,
) -> Config {
    load_config_or_exit_with_fallback_cwd(
        cli_kv_overrides,
        overrides,
        loader_overrides,
        cloud_config_bundle,
        strict_config,
        /*fallback_cwd*/ None,
    )
    .await
}

async fn load_config_or_exit_with_fallback_cwd(
    cli_kv_overrides: Vec<(String, toml::Value)>,
    overrides: ConfigOverrides,
    loader_overrides: LoaderOverrides,
    cloud_config_bundle: CloudConfigBundleLoader,
    strict_config: bool,
    fallback_cwd: Option<PathBuf>,
) -> Config {
    #[allow(clippy::print_stderr)]
    match ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .loader_overrides(loader_overrides)
        .strict_config(strict_config)
        .cloud_config_bundle(cloud_config_bundle)
        .fallback_cwd(fallback_cwd)
        .build()
        .await
    {
        Ok(config) => config,
        Err(err) => {
            eprintln!("Error loading configuration: {err}");
            std::process::exit(1);
        }
    }
}

#[allow(clippy::print_stderr)]
async fn load_bootstrap_config_or_exit(
    xedoc_home: &Path,
    cwd: Option<&AbsolutePathBuf>,
    cli_kv_overrides: Vec<(String, xedoc_config::TomlValue)>,
    loader_overrides: LoaderOverrides,
    strict_config: bool,
    cloud_config_bundle: CloudConfigBundleLoader,
) -> ConfigTomlLoadResult {
    match load_config_toml_with_layer_stack(
        xedoc_home,
        cwd,
        cli_kv_overrides,
        xedoc_config::ConfigLoadOptions {
            loader_overrides,
            strict_config,
            cloud_config_bundle,
        },
    )
    .await
    {
        Ok(config_toml) => config_toml,
        Err(err) => {
            let config_error = err
                .get_ref()
                .and_then(|err| err.downcast_ref::<ConfigLoadError>())
                .map(ConfigLoadError::config_error);
            if let Some(config_error) = config_error {
                eprintln!(
                    "Error loading config.toml:\n{}",
                    format_config_error_with_source(config_error)
                );
            } else {
                eprintln!("Error loading config.toml: {err}");
            }
            std::process::exit(1);
        }
    }
}

/// Determine if the user has decided whether to trust the current directory.
fn should_show_trust_screen(config: &Config) -> bool {
    config.active_project.trust_level.is_none()
}
