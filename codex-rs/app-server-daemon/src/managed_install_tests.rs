use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::executable_identity_from_bytes;
use super::parse_codex_version;
use super::resolved_managed_codex_bin;

#[tokio::test]
async fn resolves_managed_codex_symlink_to_release_binary() {
    let temp_dir = TempDir::new().expect("temp dir");
    let release_dir = temp_dir.path().join("releases").join("1.2.3").join("bin");
    tokio::fs::create_dir_all(&release_dir)
        .await
        .expect("create release bin directory");
    let release_bin = release_dir.join("codex");
    tokio::fs::write(&release_bin, "release")
        .await
        .expect("write release binary");
    let expected_release_bin = tokio::fs::canonicalize(&release_bin)
        .await
        .expect("resolve release binary");
    let current_bin = temp_dir.path().join("current-codex");
    std::os::unix::fs::symlink(&release_bin, &current_bin).expect("create current symlink");

    assert_eq!(
        resolved_managed_codex_bin(&current_bin)
            .await
            .expect("resolve managed binary"),
        expected_release_bin
    );
}

#[tokio::test]
async fn resolving_missing_managed_codex_reports_input_path() {
    let temp_dir = TempDir::new().expect("temp dir");
    let missing_bin = temp_dir.path().join("missing-codex");

    let err = resolved_managed_codex_bin(&missing_bin)
        .await
        .expect_err("missing managed binary");

    assert!(
        err.to_string().contains(&missing_bin.display().to_string()),
        "error should identify the unresolved binary: {err:#}"
    );
}

#[test]
fn parses_codex_cli_version_output() {
    assert_eq!(
        parse_codex_version("codex 1.2.3\n").expect("version"),
        "1.2.3"
    );
}

#[test]
fn rejects_malformed_codex_cli_version_output() {
    assert!(parse_codex_version("codex\n").is_err());
}

#[test]
fn executable_identity_uses_binary_contents() {
    let old = executable_identity_from_bytes(b"old");
    let same = executable_identity_from_bytes(b"old");
    let new = executable_identity_from_bytes(b"new");

    assert_eq!(old, same);
    assert_ne!(old, new);
}
