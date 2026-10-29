use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use tracing::warn;
use xedoc_protocol::ThreadId;
use xedoc_protocol::protocol::GitInfo;
use xedoc_protocol::protocol::RolloutItem;
use xedoc_protocol::protocol::SessionSource;
use xedoc_protocol::protocol::ThreadHistoryMode;
use xedoc_rollout::ARCHIVED_SESSIONS_SUBDIR;
use xedoc_rollout::append_rollout_item_to_path;
use xedoc_rollout::append_thread_name;
use xedoc_rollout::append_thread_name_with_source;
use xedoc_rollout::find_archived_thread_path_by_id_str;
use xedoc_rollout::find_thread_path_by_id_str;
use xedoc_rollout::read_session_meta_line;
use xedoc_state::ThreadMetadataBuilder;
use xedoc_state::ThreadTitleSource;

use super::LocalThreadStore;
use super::helpers::git_info_from_parts;
use super::helpers::permission_profile_to_metadata_value;
use super::live_writer;
use crate::GitInfoPatch;
use crate::ReadThreadParams;
use crate::StoredThread;
use crate::ThreadMetadataPatch;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use crate::UpdateThreadMetadataParams;
use crate::local::read_thread;

struct ResolvedRolloutPath {
    path: PathBuf,
    archived: bool,
}

pub(super) async fn update_thread_metadata(
    store: &LocalThreadStore,
    params: UpdateThreadMetadataParams,
) -> ThreadStoreResult<StoredThread> {
    let thread_id = params.thread_id;
    let patch = params.patch;
    if patch.is_empty() {
        return read_thread::read_thread(
            store,
            ReadThreadParams {
                thread_id,
                include_archived: params.include_archived,
                include_history: false,
            },
        )
        .await;
    }

    let requires_rollout_compat = requires_rollout_compatibility_update(&patch);
    let has_explicit_metadata = patch.name.is_some() || requires_rollout_compat;
    let history_mode = if has_explicit_metadata {
        Some(
            read_thread::read_thread(
                store,
                ReadThreadParams {
                    thread_id,
                    include_archived: params.include_archived,
                    include_history: false,
                },
            )
            .await?
            .history_mode,
        )
    } else {
        None
    };
    let paginated = matches!(history_mode, Some(ThreadHistoryMode::Paginated));
    let require_sqlite_write = sqlite_write_failure_should_block(&patch) || paginated;
    let mut updated = apply_metadata_update(
        store,
        thread_id,
        patch.clone(),
        params.include_archived,
        require_sqlite_write,
        history_mode,
    )
    .await?;
    if paginated
        && requires_rollout_compat
        && let Some(git_info) = patch.git_info.as_ref()
    {
        // The generic upsert preserves non-null Git fields for rollout reconciliation. Apply the
        // explicit patch afterward so clears are written to SQLite too.
        let Some(state_db) = store.state_db().await else {
            return Err(ThreadStoreError::Internal {
                message: format!("sqlite state db unavailable for thread {thread_id}"),
            });
        };
        apply_thread_git_info_patch(state_db.as_ref(), thread_id, git_info).await?;
        updated = read_thread::read_thread(
            store,
            ReadThreadParams {
                thread_id,
                include_archived: params.include_archived,
                include_history: false,
            },
        )
        .await?;
    }
    if paginated {
        // Paginated metadata lives in SQLite. Keep the name index update, then stop before the
        // legacy SessionMeta compatibility path below.
        if let Some(name) = patch.name.as_ref()
            && let Err(err) = append_thread_name(
                store.config.xedoc_home.as_path(),
                thread_id,
                name.as_deref().unwrap_or_default(),
            )
            .await
        {
            warn!("failed to index paginated thread name for {thread_id}: {err}");
        }
        return Ok(updated);
    }
    let needs_rollout_compat = requires_rollout_compat || patch.name.is_some();
    if !needs_rollout_compat {
        return Ok(updated);
    }

    if live_writer::rollout_path(store, thread_id).await.is_ok() {
        live_writer::persist_thread(store, thread_id).await?;
    }
    let mut resolved_rollout_path =
        resolve_rollout_path(store, thread_id, params.include_archived).await?;
    let name = patch.name;
    let title_source = patch.title_source;
    let git_info = patch.git_info;

    let state_db_ctx = store.state_db().await;
    xedoc_rollout::state_db::reconcile_rollout(
        state_db_ctx.as_deref(),
        resolved_rollout_path.path.as_path(),
        store.config.default_model_provider_id.as_str(),
        /*builder*/ None,
        &[],
        /*archived_only*/ resolved_rollout_path.archived.then_some(true),
    )
    .await;

    if let Some(name) = name {
        apply_thread_name(
            store,
            thread_id,
            name.unwrap_or_default(),
            title_source.unwrap_or(ThreadTitleSource::Manual),
        )
        .await?;
    }

    let resolved_git_info = match git_info {
        Some(git_info) => {
            let Some(state_db) = store.state_db().await else {
                return Err(ThreadStoreError::Internal {
                    message: format!("sqlite state db unavailable for thread {thread_id}"),
                });
            };
            let metadata =
                state_db
                    .get_thread(thread_id)
                    .await
                    .map_err(|err| ThreadStoreError::Internal {
                        message: format!(
                            "failed to read git metadata for thread {thread_id}: {err}"
                        ),
                    })?;
            let Some(metadata) = metadata else {
                return Err(ThreadStoreError::Internal {
                    message: format!("thread metadata unavailable before git update: {thread_id}"),
                });
            };
            let existing_git_info = git_info_from_parts(
                metadata.git_sha,
                metadata.git_branch,
                metadata.git_origin_url,
            );
            Some(resolve_git_info_patch(existing_git_info, git_info))
        }
        None => None,
    };
    if let Some((sha, branch, origin_url)) = resolved_git_info.as_ref() {
        apply_thread_git_info_to_rollout(
            resolved_rollout_path.path.as_path(),
            thread_id,
            sha,
            branch,
            origin_url,
        )
        .await?;
        refresh_resolved_rollout_path(&mut resolved_rollout_path).await;
        apply_thread_git_info(store, thread_id, sha, branch, origin_url).await?;
    }

    let mut thread = match read_thread::read_thread(
        store,
        ReadThreadParams {
            thread_id,
            include_archived: params.include_archived,
            include_history: false,
        },
    )
    .await
    {
        Ok(thread) => thread,
        Err(_) => {
            read_thread::read_thread_by_rollout_path(
                store,
                resolved_rollout_path.path,
                params.include_archived,
                /*include_history*/ false,
            )
            .await?
        }
    };
    if let Some((sha, branch, origin_url)) = resolved_git_info {
        thread.git_info = git_info_from_parts(sha, branch, origin_url);
    }
    Ok(thread)
}

async fn refresh_resolved_rollout_path(resolved: &mut ResolvedRolloutPath) {
    if let Some(path) = xedoc_rollout::existing_rollout_path(resolved.path.as_path()).await {
        resolved.path = path;
    }
}

async fn apply_metadata_update(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    patch: ThreadMetadataPatch,
    include_archived: bool,
    require_sqlite_write: bool,
    history_mode: Option<ThreadHistoryMode>,
) -> ThreadStoreResult<StoredThread> {
    let live_rollout_path = live_writer::rollout_path(store, thread_id).await.ok();
    let mut rollout_path = patch.rollout_path.clone().or(live_rollout_path);
    let mut rollout_path_archived = rollout_path
        .as_deref()
        .is_some_and(|path| rollout_path_is_archived(store, path));
    let state_db = store.state_db().await;
    let sqlite_write_result: ThreadStoreResult<()> = if let Some(state_db) = state_db.as_ref() {
        let patch = patch.clone();
        async {
            let existing =
                state_db
                    .get_thread(thread_id)
                    .await
                    .map_err(|err| ThreadStoreError::Internal {
                        message: format!("failed to read thread metadata for {thread_id}: {err}"),
                    })?;
            let advance_recency_at = patch.advance_recency_at;
            if existing.is_none() && rollout_path.is_none() {
                let resolved = resolve_rollout_path(store, thread_id, include_archived).await?;
                rollout_path_archived = resolved.archived;
                rollout_path = Some(resolved.path);
            }
            let mut metadata = match existing.clone() {
                Some(metadata) => metadata,
                None => {
                    let rollout_path =
                        rollout_path
                            .as_deref()
                            .ok_or_else(|| ThreadStoreError::Internal {
                                message: format!(
                                    "thread metadata missing rollout path for {thread_id}"
                                ),
                            })?;
                    metadata_for_missing_sqlite_row(
                        store,
                        thread_id,
                        rollout_path,
                        rollout_path_archived,
                        &patch,
                    )
                    .await?
                }
            };
            if let Some(rollout_path) = rollout_path {
                metadata.rollout_path = rollout_path;
            }
            if let Some(history_mode) = history_mode {
                // The read above gets the canonical mode from the rollout. Persist it before an
                // explicit paginated patch makes SQLite metadata authoritative.
                metadata.history_mode = history_mode;
            }
            if let Some(preview) = patch.preview {
                metadata.preview = Some(preview);
            }
            if let Some(name) = patch.name.as_ref() {
                metadata.name = name.clone();
                if metadata.history_mode != ThreadHistoryMode::Paginated {
                    metadata.title = name.clone().unwrap_or_default();
                    metadata.title_source = patch.title_source.unwrap_or(ThreadTitleSource::Manual);
                }
            }
            if let Some(title) = patch.title {
                metadata.title = title;
                metadata.title_source = patch.title_source.unwrap_or(ThreadTitleSource::Derived);
            } else if let Some(title_source) = patch.title_source {
                metadata.title_source = title_source;
            }
            if let Some(model_provider) = patch.model_provider {
                metadata.model_provider = model_provider;
            }
            if let Some(model) = patch.model {
                metadata.model = Some(model);
            }
            if let Some(reasoning_effort) = patch.reasoning_effort {
                metadata.reasoning_effort = reasoning_effort;
            }
            if let Some(created_at) = patch.created_at {
                metadata.created_at = created_at;
            }
            if let Some(updated_at) = patch.updated_at {
                metadata.updated_at = updated_at;
            }
            if existing.is_none()
                && let Some(recency_at) = advance_recency_at
            {
                metadata.recency_at = recency_at;
            }
            if let Some(source) = patch.source {
                metadata.source = enum_to_string(&source);
            }
            if let Some(thread_source) = patch.thread_source {
                metadata.thread_source = thread_source;
            }
            if let Some(agent_nickname) = patch.agent_nickname {
                metadata.agent_nickname = agent_nickname;
            }
            if let Some(agent_role) = patch.agent_role {
                metadata.agent_role = agent_role;
            }
            if let Some(agent_path) = patch.agent_path {
                metadata.agent_path = agent_path;
            }
            if let Some(cwd) = patch.cwd {
                metadata.cwd = normalize_cwd(cwd);
            }
            if let Some(cli_version) = patch.cli_version {
                metadata.cli_version = cli_version;
            }
            if let Some(approval_mode) = patch.approval_mode {
                metadata.approval_mode = enum_to_string(&approval_mode);
            }
            if let Some(permission_profile) = patch.permission_profile {
                metadata.sandbox_policy = permission_profile_to_metadata_value(&permission_profile);
            }
            if let Some(token_usage) = patch.token_usage {
                metadata.tokens_used = token_usage.total_tokens.max(0);
            }
            if let Some(first_user_message) = patch.first_user_message {
                metadata.first_user_message = Some(first_user_message);
            }
            if let Some(git_info) = patch.git_info {
                let existing_git_info = git_info_from_parts(
                    metadata.git_sha.clone(),
                    metadata.git_branch.clone(),
                    metadata.git_origin_url.clone(),
                );
                let (sha, branch, origin_url) = resolve_git_info_patch(existing_git_info, git_info);
                metadata.git_sha = sha;
                metadata.git_branch = branch;
                metadata.git_origin_url = origin_url;
            }
            state_db
                .upsert_thread(&metadata)
                .await
                .map_err(|err| ThreadStoreError::Internal {
                    message: format!("failed to update thread metadata for {thread_id}: {err}"),
                })?;
            if let Some(name) = patch.name.as_ref() {
                let history_mode = history_mode.ok_or_else(|| ThreadStoreError::Internal {
                    message: format!(
                        "thread history mode unavailable before name update: {thread_id}"
                    ),
                })?;
                let updated = match history_mode {
                    ThreadHistoryMode::Legacy => {
                        state_db
                            .update_thread_title(thread_id, name.as_deref().unwrap_or_default())
                            .await
                    }
                    ThreadHistoryMode::Paginated => {
                        state_db
                            .update_thread_name(thread_id, name.as_deref())
                            .await
                    }
                }
                .map_err(|err| ThreadStoreError::Internal {
                    message: format!("failed to set thread name: {err}"),
                })?;
                if !updated {
                    return Err(ThreadStoreError::Internal {
                        message: format!(
                            "thread metadata unavailable before name update: {thread_id}"
                        ),
                    });
                }
            }
            if existing.is_some()
                && let Some(recency_at) = advance_recency_at
            {
                state_db
                    .touch_thread_recency_at(thread_id, recency_at)
                    .await
                    .map_err(|err| ThreadStoreError::Internal {
                        message: format!(
                            "failed to advance thread recency_at for {thread_id}: {err}"
                        ),
                    })?;
            }
            Ok(())
        }
        .await
    } else if require_sqlite_write {
        Err(ThreadStoreError::Internal {
            message: format!("sqlite state db unavailable for thread {thread_id}"),
        })
    } else {
        Ok(())
    };
    match sqlite_write_result {
        Ok(()) => {}
        Err(err) if require_sqlite_write || !sqlite_write_error_is_best_effort(&err) => {
            return Err(err);
        }
        Err(err) => {
            warn!("state db update_thread_metadata failed for {thread_id}: {err}");
        }
    }

    read_thread::read_thread(
        store,
        ReadThreadParams {
            thread_id,
            include_archived,
            include_history: false,
        },
    )
    .await
}

async fn metadata_for_missing_sqlite_row(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    rollout_path: &Path,
    rollout_path_archived: bool,
    patch: &ThreadMetadataPatch,
) -> ThreadStoreResult<xedoc_state::ThreadMetadata> {
    let created_at = patch
        .created_at
        .or(patch.updated_at)
        .unwrap_or_else(Utc::now);
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        rollout_path.to_path_buf(),
        created_at,
        patch.source.clone().unwrap_or(SessionSource::Unknown),
    );
    builder.model_provider = patch.model_provider.clone();
    builder.history_mode = canonical_history_mode(store, thread_id, rollout_path).await?;
    builder.thread_source = patch.thread_source.clone().flatten();
    builder.agent_nickname = patch.agent_nickname.clone().flatten();
    builder.agent_role = patch.agent_role.clone().flatten();
    builder.agent_path = patch.agent_path.clone().flatten();
    builder.cwd = patch.cwd.clone().map(normalize_cwd).unwrap_or_default();
    builder.cli_version = patch.cli_version.clone();
    let mut metadata = builder.build(store.config.default_model_provider_id.as_str());
    if rollout_path_archived {
        metadata.archived_at = Some(metadata.updated_at);
    }
    Ok(metadata)
}

async fn canonical_history_mode(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    rollout_path: &Path,
) -> ThreadStoreResult<ThreadHistoryMode> {
    let session_meta = match read_session_meta_line(rollout_path).await {
        Ok(session_meta) => session_meta,
        Err(err) => {
            if xedoc_rollout::existing_rollout_path(rollout_path)
                .await
                .is_none()
                && let Some(history_mode) = store
                    .live_recorders
                    .lock()
                    .await
                    .get(&thread_id)
                    .map(|entry| entry.history_mode)
            {
                // The live writer retains the canonical mode selected before its deferred
                // SessionMeta reaches JSONL.
                return Ok(history_mode);
            }
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "failed to read canonical session metadata for {thread_id}: {err}"
                ),
            });
        }
    };
    if session_meta.meta.id != thread_id {
        return Err(ThreadStoreError::Internal {
            message: format!(
                "failed to rebuild thread metadata: rollout session metadata id mismatch: expected {thread_id}, found {}",
                session_meta.meta.id
            ),
        });
    }
    Ok(session_meta.meta.history_mode)
}

fn requires_rollout_compatibility_update(patch: &ThreadMetadataPatch) -> bool {
    if patch.git_info.is_none() {
        return false;
    }
    !has_observed_metadata_facts(patch)
}

fn sqlite_write_failure_should_block(patch: &ThreadMetadataPatch) -> bool {
    // Before live metadata sync moved above the rollout writer, SQLite sync failures for
    // transcript-derived metadata and thread names were log-only. Keep that
    // failure isolation so a corrupted optional state DB does not make JSONL transcript durability
    // look broken. Explicit git-only updates still require SQLite because partial git patches need
    // the existing SQLite value to preserve unspecified fields.
    patch.git_info.is_some() && !has_observed_metadata_facts(patch)
}

fn sqlite_write_error_is_best_effort(err: &ThreadStoreError) -> bool {
    matches!(err, ThreadStoreError::Internal { .. })
}

fn has_observed_metadata_facts(patch: &ThreadMetadataPatch) -> bool {
    patch.rollout_path.is_some()
        || patch.title_source.is_some()
        || patch.preview.is_some()
        || patch.title.is_some()
        || patch.model_provider.is_some()
        || patch.model.is_some()
        || patch.reasoning_effort.is_some()
        || patch.created_at.is_some()
        || patch.source.is_some()
        || patch.thread_source.is_some()
        || patch.agent_nickname.is_some()
        || patch.agent_role.is_some()
        || patch.agent_path.is_some()
        || patch.cwd.is_some()
        || patch.cli_version.is_some()
        || patch.approval_mode.is_some()
        || patch.permission_profile.is_some()
        || patch.token_usage.is_some()
        || patch.first_user_message.is_some()
}

fn enum_to_string<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(value)) => value,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

fn normalize_cwd(cwd: PathBuf) -> PathBuf {
    xedoc_utils_path::normalize_for_path_comparison(cwd.as_path()).unwrap_or(cwd)
}

async fn apply_thread_git_info_patch(
    state_db: &xedoc_state::StateRuntime,
    thread_id: ThreadId,
    git_info: &GitInfoPatch,
) -> ThreadStoreResult<()> {
    let updated = state_db
        .update_thread_git_info(
            thread_id,
            git_info.sha.as_ref().map(|sha| sha.as_deref()),
            git_info.branch.as_ref().map(|branch| branch.as_deref()),
            git_info
                .origin_url
                .as_ref()
                .map(|origin_url| origin_url.as_deref()),
        )
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to update git metadata for thread {thread_id}: {err}"),
        })?;
    if updated {
        Ok(())
    } else {
        Err(ThreadStoreError::Internal {
            message: format!("thread metadata unavailable before git update: {thread_id}"),
        })
    }
}

async fn apply_thread_git_info(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    sha: &Option<String>,
    branch: &Option<String>,
    origin_url: &Option<String>,
) -> ThreadStoreResult<()> {
    let Some(state_db) = store.state_db().await else {
        return Err(ThreadStoreError::Internal {
            message: format!("sqlite state db unavailable for thread {thread_id}"),
        });
    };
    let updated = state_db
        .update_thread_git_info(
            thread_id,
            Some(sha.as_deref()),
            Some(branch.as_deref()),
            Some(origin_url.as_deref()),
        )
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to update git metadata for thread {thread_id}: {err}"),
        })?;
    if updated {
        Ok(())
    } else {
        Err(ThreadStoreError::Internal {
            message: format!("thread metadata disappeared before update completed: {thread_id}"),
        })
    }
}

fn resolve_git_info_patch(
    existing: Option<GitInfo>,
    git_info: GitInfoPatch,
) -> (Option<String>, Option<String>, Option<String>) {
    let (existing_sha, existing_branch, existing_origin_url) = match existing {
        Some(info) => (
            info.commit_hash.map(|sha| sha.0),
            info.branch,
            info.repository_url,
        ),
        None => (None, None, None),
    };
    let sha = git_info.sha.unwrap_or(existing_sha);
    let branch = git_info.branch.unwrap_or(existing_branch);
    let origin_url = git_info.origin_url.unwrap_or(existing_origin_url);
    (sha, branch, origin_url)
}

async fn apply_thread_git_info_to_rollout(
    rollout_path: &Path,
    thread_id: ThreadId,
    sha: &Option<String>,
    branch: &Option<String>,
    origin_url: &Option<String>,
) -> ThreadStoreResult<()> {
    let mut session_meta =
        read_session_meta_line(rollout_path)
            .await
            .map_err(|err| ThreadStoreError::Internal {
                message: format!("failed to set thread git metadata: {err}"),
            })?;
    if session_meta.meta.id != thread_id {
        return Err(ThreadStoreError::Internal {
            message: format!(
                "failed to set thread git metadata: rollout session metadata id mismatch: expected {thread_id}, found {}",
                session_meta.meta.id
            ),
        });
    }

    session_meta.git = Some(GitInfo {
        commit_hash: sha.as_deref().map(xedoc_git_utils::GitSha::new),
        branch: branch.clone(),
        repository_url: origin_url.clone(),
    });
    append_rollout_item_to_path(rollout_path, &RolloutItem::SessionMeta(session_meta))
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to set thread git metadata: {err}"),
        })
}

async fn apply_thread_name(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    name: String,
    title_source: ThreadTitleSource,
) -> ThreadStoreResult<()> {
    if let Some(state_db) = store.state_db().await {
        let updated = state_db
            .update_thread_title_with_source(thread_id, &name, title_source)
            .await
            .map_err(|err| ThreadStoreError::Internal {
                message: format!("failed to set thread name: {err}"),
            })?;
        if !updated {
            return Err(ThreadStoreError::Internal {
                message: format!("thread metadata unavailable before name update: {thread_id}"),
            });
        }
    }

    append_thread_name_with_source(
        store.config.xedoc_home.as_path(),
        thread_id,
        &name,
        Some(title_source),
    )
    .await
    .map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to index thread name: {err}"),
    })
}

async fn resolve_rollout_path(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    include_archived: bool,
) -> ThreadStoreResult<ResolvedRolloutPath> {
    if let Ok(path) = live_writer::rollout_path(store, thread_id).await {
        let archived = rollout_path_is_archived(store, path.as_path());
        return Ok(ResolvedRolloutPath { path, archived });
    }

    let state_db_ctx = store.state_db().await;
    let active_path = find_thread_path_by_id_str(
        store.config.xedoc_home.as_path(),
        &thread_id.to_string(),
        state_db_ctx.as_deref(),
    )
    .await
    .map_err(|err| ThreadStoreError::InvalidRequest {
        message: format!("failed to locate thread id {thread_id}: {err}"),
    })?;
    if let Some(path) = active_path {
        return Ok(ResolvedRolloutPath {
            path,
            archived: false,
        });
    }
    if !include_archived {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!("thread not found: {thread_id}"),
        });
    }
    find_archived_thread_path_by_id_str(
        store.config.xedoc_home.as_path(),
        &thread_id.to_string(),
        state_db_ctx.as_deref(),
    )
    .await
    .map_err(|err| ThreadStoreError::InvalidRequest {
        message: format!("failed to locate archived thread id {thread_id}: {err}"),
    })?
    .map(|path| ResolvedRolloutPath {
        path,
        archived: true,
    })
    .ok_or_else(|| ThreadStoreError::InvalidRequest {
        message: format!("thread not found: {thread_id}"),
    })
}

fn rollout_path_is_archived(store: &LocalThreadStore, path: &Path) -> bool {
    path.starts_with(store.config.xedoc_home.join(ARCHIVED_SESSIONS_SUBDIR))
}
