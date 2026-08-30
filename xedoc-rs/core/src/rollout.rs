pub use xedoc_rollout::ARCHIVED_SESSIONS_SUBDIR;
pub use xedoc_rollout::Cursor;
pub use xedoc_rollout::INTERACTIVE_SESSION_SOURCES;
pub use xedoc_rollout::RolloutRecorder;
pub use xedoc_rollout::RolloutRecorderParams;
pub use xedoc_rollout::SESSIONS_SUBDIR;
pub use xedoc_rollout::SessionMeta;
pub use xedoc_rollout::SortDirection;
pub use xedoc_rollout::ThreadItem;
pub use xedoc_rollout::ThreadSortKey;
pub use xedoc_rollout::ThreadsPage;
pub use xedoc_rollout::append_thread_name;
pub use xedoc_rollout::find_archived_thread_path_by_id_str;
#[deprecated(note = "use find_thread_path_by_id_str")]
pub use xedoc_rollout::find_conversation_path_by_id_str;
pub use xedoc_rollout::find_thread_meta_by_name_str;
pub use xedoc_rollout::find_thread_name_by_id;
pub use xedoc_rollout::find_thread_names_by_ids;
pub use xedoc_rollout::find_thread_path_by_id_str;
pub use xedoc_rollout::parse_cursor;
pub use xedoc_rollout::read_head_for_summary;
pub use xedoc_rollout::read_session_meta_line;
pub use xedoc_rollout::rollout_date_parts;

#[cfg(test)]
pub(crate) mod recorder {
    pub use xedoc_rollout::RolloutRecorder;
}

pub(crate) use crate::session_rollout_init_error::map_session_init_error;

pub(crate) mod truncation {
    pub(crate) use crate::thread_rollout_truncation::*;
}
