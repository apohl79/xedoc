use chrono::Utc;
use xedoc_rollout::find_thread_path_by_id_str;

use super::LocalThreadStore;
use super::helpers::matching_rollout_file_name;
use super::helpers::scoped_rollout_path;
use crate::ArchiveThreadParams;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

pub(super) async fn archive_thread(
    store: &LocalThreadStore,
    params: ArchiveThreadParams,
) -> ThreadStoreResult<()> {
    let thread_id = params.thread_id;
    let state_db_ctx = store.state_db().await;
    let rollout_path = find_thread_path_by_id_str(
        store.config.xedoc_home.as_path(),
        &thread_id.to_string(),
        state_db_ctx.as_deref(),
    )
    .await
    .map_err(|err| ThreadStoreError::InvalidRequest {
        message: format!("failed to locate thread id {thread_id}: {err}"),
    })?
    .ok_or_else(|| ThreadStoreError::InvalidRequest {
        message: format!("no rollout found for thread id {thread_id}"),
    })?;

    let canonical_rollout_path = scoped_rollout_path(
        store.config.xedoc_home.join(xedoc_rollout::SESSIONS_SUBDIR),
        rollout_path.as_path(),
        "sessions",
    )?;
    let file_name = matching_rollout_file_name(
        canonical_rollout_path.as_path(),
        thread_id,
        rollout_path.as_path(),
    )?;

    let archive_folder = store
        .config
        .xedoc_home
        .join(xedoc_rollout::ARCHIVED_SESSIONS_SUBDIR);
    std::fs::create_dir_all(&archive_folder).map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to archive thread: {err}"),
    })?;
    let archived_path = archive_folder.join(&file_name);
    std::fs::rename(&canonical_rollout_path, &archived_path).map_err(|err| {
        ThreadStoreError::Internal {
            message: format!("failed to archive thread: {err}"),
        }
    })?;

    if let Some(ctx) = state_db_ctx {
        let _ = ctx
            .mark_archived(thread_id, archived_path.as_path(), Utc::now())
            .await;
    }
    Ok(())
}
