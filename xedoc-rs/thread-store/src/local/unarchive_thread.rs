use xedoc_rollout::find_archived_thread_path_by_id_str;
use xedoc_rollout::read_thread_item_from_rollout;
use xedoc_rollout::rollout_date_parts;

use super::LocalThreadStore;
use super::helpers::matching_rollout_file_name;
use super::helpers::scoped_rollout_path;
use super::helpers::stored_thread_from_rollout_item;
use super::helpers::touch_modified_time;
use crate::ArchiveThreadParams;
use crate::StoredThread;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

pub(super) async fn unarchive_thread(
    store: &LocalThreadStore,
    params: ArchiveThreadParams,
) -> ThreadStoreResult<StoredThread> {
    let thread_id = params.thread_id;
    let state_db_ctx = store.state_db().await;
    let archived_path = find_archived_thread_path_by_id_str(
        store.config.xedoc_home.as_path(),
        &thread_id.to_string(),
        state_db_ctx.as_deref(),
    )
    .await
    .map_err(|err| ThreadStoreError::InvalidRequest {
        message: format!("failed to locate archived thread id {thread_id}: {err}"),
    })?
    .ok_or_else(|| ThreadStoreError::InvalidRequest {
        message: format!("no archived rollout found for thread id {thread_id}"),
    })?;

    let canonical_archived_path = scoped_rollout_path(
        store
            .config
            .xedoc_home
            .join(xedoc_rollout::ARCHIVED_SESSIONS_SUBDIR),
        archived_path.as_path(),
        "archived",
    )?;
    let file_name = matching_rollout_file_name(
        canonical_archived_path.as_path(),
        thread_id,
        archived_path.as_path(),
    )?;
    let Some((year, month, day)) = rollout_date_parts(&file_name) else {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout path `{}` missing filename timestamp",
                archived_path.display()
            ),
        });
    };

    let dest_dir = store
        .config
        .xedoc_home
        .join(xedoc_rollout::SESSIONS_SUBDIR)
        .join(year)
        .join(month)
        .join(day);
    std::fs::create_dir_all(&dest_dir).map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to unarchive thread: {err}"),
    })?;
    let restored_path = dest_dir.join(&file_name);
    std::fs::rename(&canonical_archived_path, &restored_path).map_err(|err| {
        ThreadStoreError::Internal {
            message: format!("failed to unarchive thread: {err}"),
        }
    })?;
    touch_modified_time(restored_path.as_path()).map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to update unarchived thread timestamp: {err}"),
    })?;

    if let Some(ctx) = state_db_ctx {
        let _ = ctx
            .mark_unarchived(thread_id, restored_path.as_path())
            .await;
    }

    let item = read_thread_item_from_rollout(restored_path.clone())
        .await
        .ok_or_else(|| ThreadStoreError::Internal {
            message: format!(
                "failed to read unarchived thread {}",
                restored_path.display()
            ),
        })?;
    stored_thread_from_rollout_item(
        item,
        /*archived*/ false,
        store.config.default_model_provider_id.as_str(),
    )
    .ok_or_else(|| ThreadStoreError::Internal {
        message: format!(
            "failed to read unarchived thread id from {}",
            restored_path.display()
        ),
    })
}
