//! File completion for browser notes, including paths outside the session cwd.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use xedoc_app_server_protocol::FuzzyFileSearchMatchType;
use xedoc_app_server_protocol::FuzzyFileSearchResult;
use xedoc_utils_absolute_path::AbsolutePathBuf;

pub(super) async fn search(
    query: String,
    cwd: PathBuf,
    cancellation: Arc<AtomicBool>,
) -> Vec<FuzzyFileSearchResult> {
    let explicit_path = matches!(query.as_str(), "~" | "." | "..")
        || ["~/", "./", "../", r"~\", r".\", r"..\"]
            .iter()
            .any(|prefix| query.starts_with(prefix))
        || Path::new(&query).is_absolute();
    if !explicit_path {
        return crate::fuzzy_file_search::run_fuzzy_file_search(
            query,
            vec![cwd.to_string_lossy().into_owned()],
            cancellation,
        )
        .await;
    }

    tokio::task::spawn_blocking(move || {
        let (display_prefix, entry_prefix) =
            if query == "~" || matches!(query.rsplit(['/', '\\']).next(), Some("." | "..")) {
                (format!("{query}/"), String::new())
            } else {
                let split = query
                    .rfind(['/', '\\'])
                    .map(|index| index + 1)
                    .unwrap_or(/*default*/ 0);
                (query[..split].to_string(), query[split..].to_string())
            };
        let directory = AbsolutePathBuf::resolve_path_against_base(&display_prefix, cwd);
        let entries = match std::fs::read_dir(directory.as_path()) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::debug!("browser file completion failed for {directory:?}: {error}");
                return Vec::new();
            }
        };
        let mut matches = Vec::new();
        for entry in entries {
            if cancellation.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let Ok(entry) = entry else { continue };
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !file_name.starts_with(&entry_prefix)
                || (entry_prefix.is_empty() && file_name.starts_with('.'))
            {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(entry.path()).or_else(|_| entry.metadata()) else {
                continue;
            };
            matches.push(FuzzyFileSearchResult {
                root: directory.as_path().to_string_lossy().into_owned(),
                path: format!("{display_prefix}{file_name}"),
                match_type: if metadata.is_dir() {
                    FuzzyFileSearchMatchType::Directory
                } else {
                    FuzzyFileSearchMatchType::File
                },
                file_name,
                score: 1,
                indices: None,
            });
        }
        matches.sort_by(|left, right| left.path.cmp(&right.path));
        matches.truncate(/*len*/ 50);
        matches
    })
    .await
    .unwrap_or_default()
}
