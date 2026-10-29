use std::collections::HashMap;

use xedoc_rollout::RolloutConfig;
use xedoc_rollout::RolloutRecorder;
use xedoc_rollout::parse_cursor;

use super::LocalThreadStore;
use super::helpers::resolve_thread_names;
use super::helpers::set_thread_name;
use super::helpers::stored_thread_from_rollout_item;
use crate::ListThreadsParams;
use crate::SortDirection;
use crate::ThreadPage;
use crate::ThreadRelationFilter;
use crate::ThreadSortKey;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

pub(super) async fn list_threads(
    store: &LocalThreadStore,
    params: ListThreadsParams,
) -> ThreadStoreResult<ThreadPage> {
    let cursor = params
        .cursor
        .as_deref()
        .map(|cursor| {
            parse_cursor(cursor).ok_or_else(|| ThreadStoreError::InvalidRequest {
                message: format!("invalid cursor: {cursor}"),
            })
        })
        .transpose()?;
    let sort_key = match params.sort_key {
        ThreadSortKey::CreatedAt => xedoc_rollout::ThreadSortKey::CreatedAt,
        ThreadSortKey::UpdatedAt => xedoc_rollout::ThreadSortKey::UpdatedAt,
        ThreadSortKey::RecencyAt => xedoc_rollout::ThreadSortKey::RecencyAt,
    };
    let sort_direction = match params.sort_direction {
        SortDirection::Asc => xedoc_rollout::SortDirection::Asc,
        SortDirection::Desc => xedoc_rollout::SortDirection::Desc,
    };
    let state_db = store.state_db().await;
    let rollout_config = RolloutConfig {
        xedoc_home: store.config.xedoc_home.clone(),
        sqlite_home: store.config.sqlite_home.clone(),
        cwd: store.config.xedoc_home.clone(),
        model_provider_id: store.config.default_model_provider_id.clone(),
    };
    let page = list_rollout_threads(
        state_db,
        &rollout_config,
        store.config.default_model_provider_id.as_str(),
        &params,
        cursor.as_ref(),
        sort_key,
        sort_direction,
    )
    .await?;

    let next_cursor = page
        .next_cursor
        .as_ref()
        .and_then(|cursor| serde_json::to_value(cursor).ok())
        .and_then(|value| value.as_str().map(str::to_owned));
    let mut items = page
        .items
        .into_iter()
        .filter_map(|item| {
            stored_thread_from_rollout_item(
                item,
                params.archived,
                store.config.default_model_provider_id.as_str(),
            )
        })
        .collect::<Vec<_>>();

    let thread_history_modes = items
        .iter()
        .map(|thread| (thread.thread_id, thread.history_mode))
        .collect::<HashMap<_, _>>();
    let names = resolve_thread_names(store, &thread_history_modes).await;
    for thread in &mut items {
        if let Some(name) = names.get(&thread.thread_id).cloned() {
            set_thread_name(thread, name);
        }
    }

    Ok(ThreadPage { items, next_cursor })
}

pub(super) async fn list_rollout_threads(
    state_db: Option<xedoc_rollout::StateDbHandle>,
    config: &RolloutConfig,
    default_model_provider_id: &str,
    params: &ListThreadsParams,
    cursor: Option<&xedoc_rollout::Cursor>,
    sort_key: xedoc_rollout::ThreadSortKey,
    sort_direction: xedoc_rollout::SortDirection,
) -> ThreadStoreResult<xedoc_rollout::ThreadsPage> {
    if let Some(relation_filter) = params.relation_filter {
        let relation_filter = match relation_filter {
            ThreadRelationFilter::DirectChildrenOf(parent_thread_id) => {
                xedoc_state::ThreadRelationFilter::DirectChildrenOf(parent_thread_id)
            }
            ThreadRelationFilter::DescendantsOf(ancestor_thread_id) => {
                xedoc_state::ThreadRelationFilter::DescendantsOf(ancestor_thread_id)
            }
        };
        let page = xedoc_rollout::state_db::list_threads_db(
            state_db.as_deref(),
            config.xedoc_home.as_path(),
            params.page_size,
            cursor,
            sort_key,
            sort_direction,
            params.allowed_sources.as_slice(),
            params.model_providers.as_deref(),
            params.cwd_filters.as_deref(),
            Some(relation_filter),
            params.archived,
            params.search_term.as_deref(),
        )
        .await
        .ok_or_else(|| ThreadStoreError::Internal {
            message: "state DB unavailable for relationship-filtered thread listing".to_string(),
        })?;
        return Ok(page.into());
    }

    let page = if params.use_state_db_only && params.archived {
        RolloutRecorder::list_archived_threads_from_state_db(
            state_db,
            config,
            params.page_size,
            cursor,
            sort_key,
            sort_direction,
            params.allowed_sources.as_slice(),
            params.model_providers.as_deref(),
            params.cwd_filters.as_deref(),
            default_model_provider_id,
            params.search_term.as_deref(),
        )
        .await
    } else if params.use_state_db_only {
        RolloutRecorder::list_threads_from_state_db(
            state_db,
            config,
            params.page_size,
            cursor,
            sort_key,
            sort_direction,
            params.allowed_sources.as_slice(),
            params.model_providers.as_deref(),
            params.cwd_filters.as_deref(),
            default_model_provider_id,
            params.search_term.as_deref(),
        )
        .await
    } else if params.archived {
        RolloutRecorder::list_archived_threads(
            state_db,
            config,
            params.page_size,
            cursor,
            sort_key,
            sort_direction,
            params.allowed_sources.as_slice(),
            params.model_providers.as_deref(),
            params.cwd_filters.as_deref(),
            default_model_provider_id,
            params.search_term.as_deref(),
        )
        .await
    } else {
        RolloutRecorder::list_threads(
            state_db,
            config,
            params.page_size,
            cursor,
            sort_key,
            sort_direction,
            params.allowed_sources.as_slice(),
            params.model_providers.as_deref(),
            params.cwd_filters.as_deref(),
            default_model_provider_id,
            params.search_term.as_deref(),
        )
        .await
    };
    page.map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to list threads: {err}"),
    })
}
