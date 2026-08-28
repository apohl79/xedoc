use codex_git_utils::recent_commits;
use core_test_support::skip_if_sandbox;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::process::Command;

async fn create_test_git_repo(temp_dir: &TempDir) -> PathBuf {
    let repo_path = temp_dir.path().join("repo");
    fs::create_dir(&repo_path).expect("Failed to create repo dir");
    let envs = vec![
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ];

    // Initialize git repo
    Command::new("git")
        .envs(envs.clone())
        .args(["init"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("Failed to init git repo");

    // Configure git user (required for commits)
    Command::new("git")
        .envs(envs.clone())
        .args(["config", "user.name", "Test User"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("Failed to set git user name");

    Command::new("git")
        .envs(envs.clone())
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("Failed to set git user email");

    // Create a test file and commit it
    let test_file = repo_path.join("test.txt");
    fs::write(&test_file, "test content").expect("Failed to write test file");

    Command::new("git")
        .envs(envs.clone())
        .args(["add", "."])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("Failed to add files");

    Command::new("git")
        .envs(envs.clone())
        .args(["commit", "-m", "Initial commit"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("Failed to commit");

    repo_path
}

#[tokio::test]
async fn test_recent_commits_non_git_directory_returns_empty() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let entries = recent_commits(temp_dir.path(), /*limit*/ 10).await;
    assert!(entries.is_empty(), "expected no commits outside a git repo");
}

#[tokio::test]
async fn test_recent_commits_orders_and_limits() {
    skip_if_sandbox!();
    use tokio::time::Duration;
    use tokio::time::sleep;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let repo_path = create_test_git_repo(&temp_dir).await;

    // Make three distinct commits with small delays to ensure ordering by timestamp.
    fs::write(repo_path.join("file.txt"), "one").unwrap();
    Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git add");
    Command::new("git")
        .args(["commit", "-m", "first change"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git commit 1");

    sleep(Duration::from_millis(1100)).await;

    fs::write(repo_path.join("file.txt"), "two").unwrap();
    Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git add 2");
    Command::new("git")
        .args(["commit", "-m", "second change"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git commit 2");

    sleep(Duration::from_millis(1100)).await;

    fs::write(repo_path.join("file.txt"), "three").unwrap();
    Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git add 3");
    Command::new("git")
        .args(["commit", "-m", "third change"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git commit 3");

    // Request the latest 3 commits; should be our three changes in reverse time order.
    let entries = recent_commits(&repo_path, /*limit*/ 3).await;
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].subject, "third change");
    assert_eq!(entries[1].subject, "second change");
    assert_eq!(entries[2].subject, "first change");
    // Basic sanity on SHA formatting
    for e in entries {
        assert!(e.sha.len() >= 7 && e.sha.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
