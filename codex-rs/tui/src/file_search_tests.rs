use super::*;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::unbounded_channel;

fn recv_file_search_result(
    rx: &mut UnboundedReceiver<AppEvent>,
) -> (String, Vec<file_search::FileMatch>) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match rx.try_recv() {
            Ok(AppEvent::FileSearchResult { query, matches }) => return (query, matches),
            Ok(event) => panic!("expected FileSearchResult, got {event:?}"),
            Err(TryRecvError::Empty) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("did not receive FileSearchResult: {err}"),
        }
    }
}

#[test]
fn disk_completion_for_tilde_uses_home_as_root() {
    let cwd = PathBuf::from("/workspace/project");
    let home = PathBuf::from("/home/alice");

    let request = resolve_file_search_request("~", &cwd, Some(&home));

    assert_eq!(
        request,
        FileSearchRequest::DiskCompletion {
            original_query: "~".to_string(),
            directory: home,
            entry_prefix: String::new(),
            display_prefix: "~/".to_string(),
        }
    );
}

#[test]
fn disk_completion_for_tilde_subpath_uses_home_subdirectory() {
    let cwd = PathBuf::from("/workspace/project");
    let home = PathBuf::from("/home/alice");

    let request = resolve_file_search_request("~/src/co", &cwd, Some(&home));

    assert_eq!(
        request,
        FileSearchRequest::DiskCompletion {
            original_query: "~/src/co".to_string(),
            directory: PathBuf::from("/home/alice/src"),
            entry_prefix: "co".to_string(),
            display_prefix: "~/src/".to_string(),
        }
    );
}

#[test]
fn disk_completion_for_relative_parent_uses_cwd_parent() {
    let cwd = PathBuf::from("/workspace/project/crate");

    let request = resolve_file_search_request("../fo", &cwd, None);

    assert_eq!(
        request,
        FileSearchRequest::DiskCompletion {
            original_query: "../fo".to_string(),
            directory: PathBuf::from("/workspace/project"),
            entry_prefix: "fo".to_string(),
            display_prefix: "../".to_string(),
        }
    );
}

#[test]
fn disk_completion_for_relative_parent_directory_keeps_trailing_prefix() {
    let cwd = PathBuf::from("/workspace/project/crate");

    let request = resolve_file_search_request("../../", &cwd, None);

    assert_eq!(
        request,
        FileSearchRequest::DiskCompletion {
            original_query: "../../".to_string(),
            directory: PathBuf::from("/workspace"),
            entry_prefix: String::new(),
            display_prefix: "../../".to_string(),
        }
    );
}

#[test]
fn bare_query_uses_project_fuzzy_search() {
    let cwd = PathBuf::from("/workspace/project");

    let request = resolve_file_search_request("foo", &cwd, None);

    assert_eq!(request, FileSearchRequest::ProjectFuzzy("foo".to_string()));
}

#[test]
fn disk_completion_for_absolute_path_uses_absolute_parent() {
    let cwd = PathBuf::from("/workspace/project");

    let request = resolve_file_search_request("/var/lo", &cwd, None);

    assert_eq!(
        request,
        FileSearchRequest::DiskCompletion {
            original_query: "/var/lo".to_string(),
            directory: PathBuf::from("/var"),
            entry_prefix: "lo".to_string(),
            display_prefix: "/var/".to_string(),
        }
    );
}

#[test]
fn disk_completion_for_current_directory_uses_cwd() {
    let cwd = PathBuf::from("/workspace/project");

    let request = resolve_file_search_request("./src", &cwd, None);

    assert_eq!(
        request,
        FileSearchRequest::DiskCompletion {
            original_query: "./src".to_string(),
            directory: PathBuf::from("/workspace/project"),
            entry_prefix: "src".to_string(),
            display_prefix: "./".to_string(),
        }
    );
}

#[test]
fn disk_completion_matches_direct_children_with_display_prefix() {
    let temp = tempdir().expect("create temp dir");
    fs::write(temp.path().join("alpha.txt"), "").expect("write alpha");
    fs::create_dir(temp.path().join("alpine")).expect("create alpine");
    fs::write(temp.path().join("beta.txt"), "").expect("write beta");

    let matches = collect_disk_completion_matches(temp.path(), "al", "~/", /*limit*/ 20)
        .expect("collect matches");

    assert_eq!(
        matches
            .iter()
            .map(|file_match| (&file_match.path, file_match.match_type))
            .collect::<Vec<_>>(),
        vec![
            (&PathBuf::from("~/alpha.txt"), file_search::MatchType::File),
            (
                &PathBuf::from("~/alpine"),
                file_search::MatchType::Directory
            ),
        ]
    );
}

#[test]
fn disk_completion_hides_dotfiles_unless_prefix_starts_with_dot() {
    let temp = tempdir().expect("create temp dir");
    fs::write(temp.path().join(".hidden"), "").expect("write hidden");
    fs::write(temp.path().join("visible"), "").expect("write visible");

    let visible_matches = collect_disk_completion_matches(temp.path(), "", "~/", /*limit*/ 20)
        .expect("collect visible matches");
    let hidden_matches = collect_disk_completion_matches(temp.path(), ".", "~/", /*limit*/ 20)
        .expect("collect hidden matches");

    assert_eq!(
        visible_matches
            .iter()
            .map(|file_match| &file_match.path)
            .collect::<Vec<_>>(),
        vec![&PathBuf::from("~/visible")]
    );
    assert_eq!(
        hidden_matches
            .iter()
            .map(|file_match| &file_match.path)
            .collect::<Vec<_>>(),
        vec![&PathBuf::from("~/.hidden")]
    );
}

#[cfg(unix)]
#[test]
fn disk_completion_follows_symlinked_directories() {
    let temp = tempdir().expect("create temp dir");
    fs::create_dir(temp.path().join("target")).expect("create target dir");
    std::os::unix::fs::symlink(temp.path().join("target"), temp.path().join("linked"))
        .expect("create symlinked dir");

    let matches = collect_disk_completion_matches(temp.path(), "li", "~/", /*limit*/ 20)
        .expect("collect matches");

    assert_eq!(
        matches
            .iter()
            .map(|file_match| (&file_match.path, file_match.match_type))
            .collect::<Vec<_>>(),
        vec![(
            &PathBuf::from("~/linked"),
            file_search::MatchType::Directory
        )]
    );
}

#[test]
fn manager_disk_completion_emits_matches_for_absolute_query() {
    let temp = tempdir().expect("create temp dir");
    fs::write(temp.path().join("alpha.txt"), "").expect("write alpha");
    let query = temp.path().join("al").to_string_lossy().to_string();
    let (tx_raw, mut rx) = unbounded_channel();
    let manager = FileSearchManager::new(temp.path().to_path_buf(), AppEventSender::new(tx_raw));

    manager.on_user_query(query.clone());

    let (actual_query, matches) = recv_file_search_result(&mut rx);
    assert_eq!(actual_query, query);
    assert_eq!(
        matches
            .iter()
            .map(|file_match| &file_match.path)
            .collect::<Vec<_>>(),
        vec![&temp.path().join("alpha.txt")]
    );
}

#[test]
fn manager_disk_completion_sends_empty_result_for_missing_directory() {
    let temp = tempdir().expect("create temp dir");
    let query = temp
        .path()
        .join("missing")
        .join("al")
        .to_string_lossy()
        .to_string();
    let (tx_raw, mut rx) = unbounded_channel();
    let manager = FileSearchManager::new(temp.path().to_path_buf(), AppEventSender::new(tx_raw));

    manager.on_user_query(query.clone());

    let (actual_query, matches) = recv_file_search_result(&mut rx);
    assert_eq!(actual_query, query);
    assert_eq!(matches, Vec::new());
}

#[test]
fn stale_disk_completion_results_are_not_sent_after_query_changes() {
    let (tx_raw, mut rx) = unbounded_channel();
    let sender = AppEventSender::new(tx_raw);
    let state = Arc::new(Mutex::new(SearchState {
        latest_query: "~/al".to_string(),
        session: None,
        session_token: 1,
    }));
    let matches = vec![file_search::FileMatch {
        score: 1,
        path: PathBuf::from("~/alpha.txt"),
        match_type: file_search::MatchType::File,
        root: PathBuf::from("/home/alice"),
        indices: None,
    }];

    send_disk_completion_result_if_current(
        &state,
        &sender,
        /*session_token*/ 1,
        "~/al".to_string(),
        matches.clone(),
    );
    let event = rx.try_recv().expect("current result");
    assert!(matches!(
        event,
        AppEvent::FileSearchResult {
            query,
            matches: event_matches
        } if query == "~/al" && event_matches == matches
    ));

    {
        let mut st = state.lock().expect("lock state");
        st.latest_query = "other".to_string();
        st.session_token = 2;
    }
    send_disk_completion_result_if_current(
        &state,
        &sender,
        /*session_token*/ 1,
        "~/al".to_string(),
        matches,
    );

    assert!(rx.try_recv().is_err());
}
