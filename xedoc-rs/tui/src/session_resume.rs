//! Resolve saved-session state needed before resuming or forking a thread.
//!
//! The app-server API owns normal thread lifecycle data. This module coordinates
//! the TUI-specific cwd prompt and falls back to local rollout metadata only
//! before the app server has resumed the selected thread.

use std::io;
use std::path::Path;
use std::path::PathBuf;

use crate::cwd_prompt;
use crate::cwd_prompt::CwdPromptAction;
use crate::cwd_prompt::CwdPromptOutcome;
use crate::legacy_core::config::Config;
use crate::resume_picker::SessionTarget;
use crate::tui::Tui;
use serde::Deserialize;
use serde_json::Value;
use xedoc_config::types::ResumeCwdMode;
use xedoc_protocol::ThreadId;
use xedoc_rollout::open_rollout_line_reader;
use xedoc_state::StateRuntime;
use xedoc_utils_path as path_utils;

#[derive(Default)]
struct RolloutResumeState {
    thread_id: Option<ThreadId>,
    cwd: Option<PathBuf>,
    model: Option<String>,
}

#[derive(Deserialize)]
struct SessionMetadata {
    id: ThreadId,
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct TurnContextResumeState {
    cwd: PathBuf,
    model: String,
}

#[derive(Deserialize)]
struct RawRecord {
    #[serde(rename = "type")]
    item_type: String,
    payload: Option<Value>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResolveCwdOutcome {
    Continue(Option<PathBuf>),
    Exit,
}

pub(crate) struct ResumeCwdContext<'path> {
    pub(crate) current_cwd: &'path Path,
    pub(crate) remembered_current_cwd: &'path Path,
    pub(crate) allow_remember_current: bool,
    pub(crate) mode: Option<ResumeCwdMode>,
}

pub(crate) fn effective_resume_cwd_mode(
    configured_mode: Option<ResumeCwdMode>,
    cwd_override: Option<&Path>,
) -> Option<ResumeCwdMode> {
    if cwd_override.is_some() {
        Some(ResumeCwdMode::Current)
    } else {
        configured_mode
    }
}

pub(crate) async fn read_session_model(
    state_db_ctx: Option<&StateRuntime>,
    thread_id: ThreadId,
    path: Option<&Path>,
) -> Option<String> {
    if let Some(state_db_ctx) = state_db_ctx
        && let Ok(Some(metadata)) = state_db_ctx.get_thread(thread_id).await
        && let Some(model) = metadata.model
    {
        return Some(model);
    }

    let path = path?;
    read_rollout_resume_state(path)
        .await
        .ok()
        .and_then(|state| state.model)
}

pub(crate) async fn resolve_cwd_for_resume_or_fork(
    tui: &mut Tui,
    config: &Config,
    state_db_ctx: Option<&StateRuntime>,
    target_session: &SessionTarget,
    action: CwdPromptAction,
    cwd_context: ResumeCwdContext<'_>,
) -> color_eyre::Result<ResolveCwdOutcome> {
    if matches!(cwd_context.mode, Some(ResumeCwdMode::Current)) {
        return Ok(ResolveCwdOutcome::Continue(Some(
            cwd_context.remembered_current_cwd.to_path_buf(),
        )));
    }
    let Some(history_cwd) = read_session_cwd(
        state_db_ctx,
        target_session.thread_id,
        target_session.path.as_deref(),
    )
    .await
    else {
        if matches!(cwd_context.mode, Some(ResumeCwdMode::Session)) {
            color_eyre::eyre::bail!(
                "failed to determine the working directory recorded for the selected session"
            );
        }
        return Ok(ResolveCwdOutcome::Continue(None));
    };
    match cwd_context.mode {
        Some(ResumeCwdMode::Session) => {
            return Ok(ResolveCwdOutcome::Continue(Some(history_cwd)));
        }
        Some(ResumeCwdMode::Current) | None => {}
    }
    if cwds_differ(cwd_context.current_cwd, &history_cwd) {
        let selection_outcome = cwd_prompt::run_cwd_selection_prompt(
            tui,
            config,
            action,
            cwd_context.current_cwd,
            &history_cwd,
            cwd_context.remembered_current_cwd,
            cwd_context.allow_remember_current,
        )
        .await?;
        return Ok(match selection_outcome {
            CwdPromptOutcome::Selection(selection) => ResolveCwdOutcome::Continue(Some(
                selection
                    .selected_cwd(
                        cwd_context.current_cwd,
                        &history_cwd,
                        cwd_context.remembered_current_cwd,
                    )
                    .to_path_buf(),
            )),
            CwdPromptOutcome::Exit => ResolveCwdOutcome::Exit,
        });
    }
    Ok(ResolveCwdOutcome::Continue(Some(history_cwd)))
}

async fn read_session_cwd(
    state_db_ctx: Option<&StateRuntime>,
    thread_id: ThreadId,
    path: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(state_db_ctx) = state_db_ctx
        && let Ok(Some(metadata)) = state_db_ctx.get_thread(thread_id).await
    {
        return Some(metadata.cwd);
    }

    let path = path?;
    match read_rollout_resume_state(path).await {
        Ok(state) => state.cwd,
        Err(err) => {
            let rollout_path = path.display().to_string();
            tracing::warn!(
                %rollout_path,
                %err,
                "Failed to read session metadata from rollout"
            );
            None
        }
    }
}

pub(crate) fn cwds_differ(current_cwd: &Path, session_cwd: &Path) -> bool {
    !path_utils::paths_match_after_normalization(current_cwd, session_cwd)
}

async fn read_rollout_resume_state(path: &Path) -> io::Result<RolloutResumeState> {
    let mut reader = open_rollout_line_reader(path).await?;
    let mut state = RolloutResumeState::default();
    let mut saw_record = false;

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<RawRecord>(trimmed) else {
            continue;
        };
        saw_record = true;
        let Some(payload) = record.payload else {
            continue;
        };

        match record.item_type.as_str() {
            "session_meta" if state.thread_id.is_none() => {
                if let Ok(metadata) = serde_json::from_value::<SessionMetadata>(payload) {
                    state.thread_id = Some(metadata.id);
                    state.cwd.get_or_insert(metadata.cwd);
                }
            }
            "turn_context" => {
                if let Ok(turn_context) = serde_json::from_value::<TurnContextResumeState>(payload)
                {
                    state.cwd = Some(turn_context.cwd);
                    state.model = Some(turn_context.model);
                }
            }
            _ => {}
        }
    }

    if saw_record {
        Ok(state)
    } else {
        Err(io::Error::other(format!(
            "rollout at {} is empty",
            path.display()
        )))
    }
}
