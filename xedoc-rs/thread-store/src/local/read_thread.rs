use chrono::DateTime;
use chrono::Utc;
use xedoc_protocol::models::PermissionProfile;
use xedoc_protocol::protocol::AskForApproval;
use xedoc_protocol::protocol::SessionMetaLine;
use xedoc_protocol::protocol::SessionSource;
use xedoc_protocol::protocol::ThreadHistoryMode;
use xedoc_rollout::RolloutRecorder;
use xedoc_rollout::find_archived_thread_path_by_id_str;
use xedoc_rollout::find_thread_name_by_id;
use xedoc_rollout::find_thread_name_record_by_id;
use xedoc_rollout::find_thread_path_by_id_str;
use xedoc_rollout::read_session_meta_line;
use xedoc_rollout::read_thread_item_from_rollout;
use xedoc_state::ThreadMetadata;

use super::LocalThreadStore;
use super::helpers::distinct_thread_metadata_title;
use super::helpers::git_info_from_parts;
use super::helpers::permission_profile_from_metadata_value;
use super::helpers::rollout_path_is_archived;
use super::helpers::set_thread_name;
use super::helpers::set_thread_name_from_title;
use super::helpers::sqlite_thread_name;
use super::helpers::stored_thread_from_rollout_item;
use super::live_writer;
use crate::ReadThreadParams;
use crate::StoredThread;
use crate::StoredThreadHistory;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use crate::error::reject_paginated_history_mode;

pub(super) async fn read_thread(
    store: &LocalThreadStore,
    params: ReadThreadParams,
) -> ThreadStoreResult<StoredThread> {
    let thread_id = params.thread_id;
    if let Some(metadata) = read_sqlite_metadata(store, thread_id).await
        && (params.include_archived
            || (metadata.archived_at.is_none()
                && !rollout_path_is_archived(
                    store.config.xedoc_home.as_path(),
                    metadata.rollout_path.as_path(),
                )))
        && (!params.include_history
            || sqlite_rollout_path_can_load_history_for_thread(
                store,
                &metadata.rollout_path,
                thread_id,
            )
            .await)
    {
        let metadata_sandbox_policy = metadata.sandbox_policy.clone();
        let mut thread = stored_thread_from_sqlite_metadata(store, metadata).await?;
        if !params.include_history
            && let Some(rollout_path) = thread.rollout_path.clone()
            && let Ok(mut rollout_thread) = read_thread_from_rollout_path(store, rollout_path).await
            && rollout_thread.thread_id == thread_id
            && (params.include_archived || rollout_thread.archived_at.is_none())
            && !rollout_thread.preview.is_empty()
        {
            rollout_thread.recency_at = thread.recency_at;
            if thread.name.is_some() {
                rollout_thread.name = thread.name;
            }
            rollout_thread.git_info = thread.git_info;
            rollout_thread.permission_profile = permission_profile_from_metadata_value(
                &metadata_sandbox_policy,
                rollout_thread.cwd.as_path(),
            );
            thread = rollout_thread;
        }
        reject_paginated_history(&thread, params.include_history)?;
        attach_history_if_requested(&mut thread, params.include_history).await?;
        return Ok(thread);
    }

    let path = resolve_rollout_path(store, thread_id, params.include_archived)
        .await?
        .ok_or_else(|| ThreadStoreError::InvalidRequest {
            message: format!("no rollout found for thread id {thread_id}"),
        })?;

    let mut thread = read_thread_from_rollout_path(store, path).await?;
    if !params.include_archived && thread.archived_at.is_some() {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!("thread {} is archived", thread.thread_id),
        });
    }
    reject_paginated_history(&thread, params.include_history)?;
    attach_history_if_requested(&mut thread, params.include_history).await?;
    Ok(thread)
}

async fn sqlite_rollout_path_can_load_history_for_thread(
    store: &LocalThreadStore,
    path: &std::path::Path,
    thread_id: xedoc_protocol::ThreadId,
) -> bool {
    if xedoc_rollout::existing_rollout_path(path).await.is_none() {
        return false;
    }
    // SQLite metadata can outlive a moved/recreated rollout path. When history is
    // requested, verify the path still resolves to the requested thread before
    // trusting it as the source replay.
    read_thread_from_rollout_path(store, path.to_path_buf())
        .await
        .is_ok_and(|thread| thread.thread_id == thread_id)
}

pub(super) async fn read_thread_by_rollout_path(
    store: &LocalThreadStore,
    rollout_path: std::path::PathBuf,
    include_archived: bool,
    include_history: bool,
) -> ThreadStoreResult<StoredThread> {
    let path = resolve_requested_rollout_path(store, rollout_path).await?;
    let mut thread = read_thread_from_rollout_path(store, path).await?;
    if !include_archived && thread.archived_at.is_some() {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!("thread {} is archived", thread.thread_id),
        });
    }
    if let Some(metadata) = read_sqlite_metadata(store, thread.thread_id).await {
        if thread.history_mode == ThreadHistoryMode::Paginated {
            thread.name = sqlite_thread_name(&metadata);
        }
        thread.recency_at = metadata.recency_at;
        thread.git_info = if thread.history_mode == ThreadHistoryMode::Paginated {
            // A paginated rollout only has the initial Git tuple. Do not turn an explicit SQLite
            // clear back into the stale rollout value while reading by path.
            git_info_from_parts(
                metadata.git_sha,
                metadata.git_branch,
                metadata.git_origin_url,
            )
        } else {
            let (fallback_sha, fallback_branch, fallback_origin_url) = match thread.git_info.take()
            {
                Some(info) => (
                    info.commit_hash.map(|sha| sha.0),
                    info.branch,
                    info.repository_url,
                ),
                None => (None, None, None),
            };
            git_info_from_parts(
                metadata.git_sha.or(fallback_sha),
                metadata.git_branch.or(fallback_branch),
                metadata.git_origin_url.or(fallback_origin_url),
            )
        };
    }
    reject_paginated_history(&thread, include_history)?;
    attach_history_if_requested(&mut thread, include_history).await?;
    Ok(thread)
}

fn reject_paginated_history(thread: &StoredThread, include_history: bool) -> ThreadStoreResult<()> {
    if include_history {
        reject_paginated_history_mode(thread.history_mode)?;
    }
    Ok(())
}

async fn resolve_requested_rollout_path(
    store: &LocalThreadStore,
    rollout_path: std::path::PathBuf,
) -> ThreadStoreResult<std::path::PathBuf> {
    let path = if rollout_path.is_relative() {
        store.config.xedoc_home.join(rollout_path)
    } else {
        rollout_path
    };
    match tokio::fs::metadata(path.as_path()).await {
        Ok(metadata) if metadata.is_dir() => {
            return Err(ThreadStoreError::InvalidRequest {
                message: format!(
                    "failed to resolve rollout path `{}`: path is a directory",
                    path.display()
                ),
            });
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(ThreadStoreError::InvalidRequest {
                message: format!(
                    "failed to resolve rollout path `{}`: path is not a file",
                    path.display()
                ),
            });
        }
        _ => {}
    }
    let Some(path) = xedoc_rollout::existing_rollout_path(path.as_path()).await else {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "failed to resolve rollout path `{}`: file does not exist",
                path.display()
            ),
        });
    };
    std::fs::canonicalize(path.as_path()).map_err(|err| ThreadStoreError::InvalidRequest {
        message: format!("failed to resolve rollout path `{}`: {err}", path.display()),
    })
}

async fn attach_history_if_requested(
    thread: &mut StoredThread,
    include_history: bool,
) -> ThreadStoreResult<()> {
    if !include_history {
        return Ok(());
    }
    let thread_id = thread.thread_id;
    let Some(path) = thread.rollout_path.clone() else {
        return Err(ThreadStoreError::Internal {
            message: format!("failed to load thread history for thread {thread_id}"),
        });
    };
    let items = load_history_items(&path).await?;
    thread.history = Some(StoredThreadHistory { thread_id, items });
    Ok(())
}

pub(super) async fn resolve_rollout_path(
    store: &LocalThreadStore,
    thread_id: xedoc_protocol::ThreadId,
    include_archived: bool,
) -> ThreadStoreResult<Option<std::path::PathBuf>> {
    if let Ok(path) = live_writer::rollout_path(store, thread_id).await
        && xedoc_rollout::existing_rollout_path(path.as_path())
            .await
            .is_some()
        && (include_archived || !rollout_path_is_archived(store.config.xedoc_home.as_path(), &path))
    {
        return Ok(Some(path));
    }

    let state_db_ctx = store.state_db().await;
    if include_archived {
        match find_thread_path_by_id_str(
            store.config.xedoc_home.as_path(),
            &thread_id.to_string(),
            state_db_ctx.as_deref(),
        )
        .await
        .map_err(|err| ThreadStoreError::InvalidRequest {
            message: format!("failed to locate thread id {thread_id}: {err}"),
        })? {
            Some(path) => Ok(Some(path)),
            None => find_archived_thread_path_by_id_str(
                store.config.xedoc_home.as_path(),
                &thread_id.to_string(),
                state_db_ctx.as_deref(),
            )
            .await
            .map_err(|err| ThreadStoreError::InvalidRequest {
                message: format!("failed to locate archived thread id {thread_id}: {err}"),
            }),
        }
    } else {
        find_thread_path_by_id_str(
            store.config.xedoc_home.as_path(),
            &thread_id.to_string(),
            state_db_ctx.as_deref(),
        )
        .await
        .map_err(|err| ThreadStoreError::InvalidRequest {
            message: format!("failed to locate thread id {thread_id}: {err}"),
        })
    }
}

async fn read_thread_from_rollout_path(
    store: &LocalThreadStore,
    path: std::path::PathBuf,
) -> ThreadStoreResult<StoredThread> {
    let Some(item) = read_thread_item_from_rollout(path.clone()).await else {
        return stored_thread_from_session_meta(store, path).await;
    };
    let archived = rollout_path_is_archived(store.config.xedoc_home.as_path(), path.as_path());
    let mut thread = stored_thread_from_rollout_item(
        item,
        archived,
        store.config.default_model_provider_id.as_str(),
    )
    .ok_or_else(|| ThreadStoreError::Internal {
        message: format!("failed to read thread id from {}", path.display()),
    })?;
    thread.rollout_path = Some(xedoc_rollout::plain_rollout_path(path.as_path()));
    let meta_line = read_required_session_meta_line(path.as_path()).await?;
    thread.forked_from_id = meta_line.meta.forked_from_id;
    thread.parent_thread_id = meta_line.meta.parent_thread_id;
    thread.history_mode = meta_line.meta.history_mode;
    if let Some(model_provider) = meta_line
        .meta
        .model_provider
        .filter(|provider| !provider.is_empty())
    {
        thread.model_provider = model_provider;
    }
    if let Ok(Some((title, title_source))) =
        find_thread_name_record_by_id(store.config.xedoc_home.as_path(), &thread.thread_id).await
    {
        set_thread_name_from_title(&mut thread, title, title_source);
    } else if thread.history_mode == ThreadHistoryMode::Legacy
        && let Ok(Some(name)) =
            find_thread_name_by_id(store.config.xedoc_home.as_path(), &thread.thread_id).await
        && !name.trim().is_empty()
    {
        set_thread_name(&mut thread, name);
    }
    Ok(thread)
}

pub(super) async fn load_history_items(
    path: &std::path::Path,
) -> ThreadStoreResult<Vec<xedoc_protocol::protocol::RolloutItem>> {
    let (items, _, _) = RolloutRecorder::load_rollout_items(path)
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to load thread history {}: {err}", path.display()),
        })?;
    Ok(items)
}

async fn read_sqlite_metadata(
    store: &LocalThreadStore,
    thread_id: xedoc_protocol::ThreadId,
) -> Option<ThreadMetadata> {
    let runtime = store.state_db().await?;
    runtime.get_thread(thread_id).await.ok().flatten()
}

async fn stored_thread_from_sqlite_metadata(
    store: &LocalThreadStore,
    metadata: ThreadMetadata,
) -> ThreadStoreResult<StoredThread> {
    let (_, title_source) = match distinct_thread_metadata_title(&metadata) {
        Some((title, title_source)) => (Some(title), Some(title_source)),
        None => {
            match find_thread_name_record_by_id(store.config.xedoc_home.as_path(), &metadata.id)
                .await
                .ok()
                .flatten()
            {
                Some((title, title_source)) if !title.trim().is_empty() => {
                    (Some(title), title_source)
                }
                Some(_) | None => (None, None),
            }
        }
    };
    let session_meta = match read_required_session_meta_line(metadata.rollout_path.as_path()).await
    {
        Ok(meta_line) => Some(meta_line.meta),
        Err(_)
            if xedoc_rollout::existing_rollout_path(metadata.rollout_path.as_path())
                .await
                .is_none() =>
        {
            None
        }
        Err(err) => {
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "failed to read session metadata {}: {err}",
                    metadata.rollout_path.display()
                ),
            });
        }
    };
    let rollout_path = xedoc_rollout::plain_rollout_path(metadata.rollout_path.as_path());
    let forked_from_id = session_meta.as_ref().and_then(|meta| meta.forked_from_id);
    let parent_thread_id = session_meta.as_ref().and_then(|meta| meta.parent_thread_id);
    let history_mode = session_meta
        .as_ref()
        .map(|meta| meta.history_mode)
        .unwrap_or(metadata.history_mode);
    let name = thread_name_from_metadata(store, &metadata, history_mode).await;
    let preview = metadata
        .preview
        .clone()
        .or_else(|| metadata.first_user_message.clone())
        .unwrap_or_default();
    let permission_profile =
        permission_profile_from_metadata_value(&metadata.sandbox_policy, metadata.cwd.as_path());
    Ok(StoredThread {
        thread_id: metadata.id,
        extra_config: None,
        rollout_path: Some(rollout_path),
        forked_from_id,
        parent_thread_id,
        preview,
        name,
        title_source,
        model_provider: if metadata.model_provider.is_empty() {
            store.config.default_model_provider_id.clone()
        } else {
            metadata.model_provider
        },
        model: metadata.model,
        reasoning_effort: metadata.reasoning_effort,
        created_at: metadata.created_at,
        updated_at: metadata.updated_at,
        recency_at: metadata.recency_at,
        archived_at: metadata.archived_at,
        cwd: metadata.cwd,
        cli_version: metadata.cli_version,
        source: parse_session_source(&metadata.source),
        history_mode,
        thread_source: metadata.thread_source,
        agent_nickname: metadata.agent_nickname,
        agent_role: metadata.agent_role,
        agent_path: metadata.agent_path,
        git_info: git_info_from_parts(
            metadata.git_sha,
            metadata.git_branch,
            metadata.git_origin_url,
        ),
        approval_mode: parse_or_default(&metadata.approval_mode, AskForApproval::OnRequest),
        permission_profile,
        token_usage: None,
        first_user_message: metadata.first_user_message,
        history: None,
    })
}

async fn thread_name_from_metadata(
    store: &LocalThreadStore,
    metadata: &ThreadMetadata,
    history_mode: ThreadHistoryMode,
) -> Option<String> {
    match history_mode {
        ThreadHistoryMode::Paginated => sqlite_thread_name(metadata),
        ThreadHistoryMode::Legacy => {
            if let Some(title) = distinct_thread_metadata_title(metadata) {
                Some(title.0)
            } else {
                find_thread_name_by_id(store.config.xedoc_home.as_path(), &metadata.id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|name| !name.trim().is_empty())
            }
        }
    }
}

async fn stored_thread_from_session_meta(
    store: &LocalThreadStore,
    path: std::path::PathBuf,
) -> ThreadStoreResult<StoredThread> {
    let meta_line = read_required_session_meta_line(path.as_path()).await?;
    let archived = rollout_path_is_archived(store.config.xedoc_home.as_path(), path.as_path());
    Ok(stored_thread_from_meta_line(
        store, meta_line, path, archived,
    ))
}

async fn read_required_session_meta_line(
    path: &std::path::Path,
) -> ThreadStoreResult<SessionMetaLine> {
    read_session_meta_line(path)
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to read session metadata {}: {err}", path.display()),
        })
}

fn stored_thread_from_meta_line(
    store: &LocalThreadStore,
    meta_line: SessionMetaLine,
    path: std::path::PathBuf,
    archived: bool,
) -> StoredThread {
    let created_at = parse_rfc3339_non_optional(&meta_line.meta.timestamp).unwrap_or_else(Utc::now);
    let updated_at = std::fs::metadata(path.as_path())
        .ok()
        .and_then(|meta| meta.modified().ok())
        .map(DateTime::<Utc>::from)
        .unwrap_or(created_at);
    let rollout_path = xedoc_rollout::plain_rollout_path(path.as_path());
    StoredThread {
        thread_id: meta_line.meta.id,
        extra_config: None,
        rollout_path: Some(rollout_path),
        forked_from_id: meta_line.meta.forked_from_id,
        parent_thread_id: meta_line.meta.parent_thread_id,
        preview: String::new(),
        name: None,
        title_source: None,
        model_provider: meta_line
            .meta
            .model_provider
            .filter(|provider| !provider.is_empty())
            .unwrap_or_else(|| store.config.default_model_provider_id.clone()),
        model: None,
        reasoning_effort: None,
        created_at,
        updated_at,
        recency_at: updated_at,
        archived_at: archived.then_some(updated_at),
        cwd: meta_line.meta.cwd,
        cli_version: meta_line.meta.cli_version,
        source: meta_line.meta.source,
        history_mode: meta_line.meta.history_mode,
        thread_source: meta_line.meta.thread_source,
        agent_nickname: meta_line.meta.agent_nickname,
        agent_role: meta_line.meta.agent_role,
        agent_path: meta_line.meta.agent_path,
        git_info: meta_line.git,
        approval_mode: AskForApproval::OnRequest,
        permission_profile: PermissionProfile::read_only(),
        token_usage: None,
        first_user_message: None,
        history: None,
    }
}

fn parse_session_source(source: &str) -> SessionSource {
    serde_json::from_str(source)
        .or_else(|_| serde_json::from_value(serde_json::Value::String(source.to_string())))
        .unwrap_or(SessionSource::Unknown)
}

fn parse_or_default<T>(value: &str, default: T) -> T
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_str(value)
        .or_else(|_| serde_json::from_value(serde_json::Value::String(value.to_string())))
        .unwrap_or(default)
}

fn parse_rfc3339_non_optional(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}
