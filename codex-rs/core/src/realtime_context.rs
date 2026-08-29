use crate::session::session::Session;
use codex_thread_store::ListThreadsParams;
use codex_thread_store::SortDirection;
use codex_thread_store::StoredThread;
use codex_thread_store::ThreadSortKey;
use dirs::home_dir;
use tracing::warn;

pub(crate) async fn build_realtime_startup_context(
    sess: &Session,
    budget_tokens: usize,
) -> Option<String> {
    let config = sess.get_config().await;
    let history = sess.clone_history().await;
    let recent_threads = load_recent_threads(sess).await;

    codex_core_realtime_context::build_realtime_startup_context(
        &config.cwd,
        history.raw_items(),
        &recent_threads,
        home_dir(),
        budget_tokens,
    )
    .await
}

async fn load_recent_threads(sess: &Session) -> Vec<StoredThread> {
    match sess
        .services
        .thread_store
        .list_threads(ListThreadsParams {
            page_size: 40,
            cursor: None,
            sort_key: ThreadSortKey::UpdatedAt,
            sort_direction: SortDirection::Desc,
            allowed_sources: Vec::new(),
            model_providers: None,
            cwd_filters: None,
            relation_filter: None,
            archived: false,
            search_term: None,
            use_state_db_only: false,
        })
        .await
    {
        Ok(page) => page.items,
        Err(err) => {
            warn!("failed to load realtime startup threads from thread store: {err}");
            Vec::new()
        }
    }
}
