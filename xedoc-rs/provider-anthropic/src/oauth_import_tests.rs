use std::fs;

use pretty_assertions::assert_eq;
use serde_json::json;

use super::import_anthropic_oauth_credentials;
use crate::AnthropicCredentialLoad;
use crate::AnthropicOAuthAccount;
use crate::AnthropicOAuthCredential;
use crate::load_anthropic_oauth_credentials;

fn legacy_credential(email: &str, token: &str) -> String {
    json!({
        "access_token": token,
        "refresh_token": "refresh-token",
        "email": email,
        "expired": "2030-01-01T00:00:00.000Z",
        "account_uuid": "account-id",
        "type": "claude"
    })
    .to_string()
}

#[test]
fn imports_legacy_directory_as_canonical_credentials() {
    let source = tempfile::tempdir().expect("source directory");
    let destination = tempfile::tempdir().expect("destination directory");
    fs::write(
        source.path().join("claude-user@example.com.json"),
        legacy_credential("user@example.com", "access-token"),
    )
    .expect("write source credential");
    fs::write(source.path().join("google-user@example.com.json"), "{}")
        .expect("write unrelated credential");
    fs::write(
        source
            .path()
            .join("claude-anthropic-api-key@anthropic_api_key.json"),
        json!({
            "access_token": "api-key",
            "refresh_token": "",
            "email": "anthropic-api-key@anthropic_api_key",
            "expired": "9999-12-31T23:59:59.999Z",
            "account_uuid": "anthropic-api-key",
            "type": "claude"
        })
        .to_string(),
    )
    .expect("write legacy API-key credential");

    let imported = import_anthropic_oauth_credentials(source.path(), destination.path())
        .expect("import credentials");
    let loaded =
        load_anthropic_oauth_credentials(destination.path()).expect("load imported credentials");

    assert_eq!(
        (imported, loaded),
        (
            1,
            AnthropicCredentialLoad {
                accounts: vec![AnthropicOAuthAccount {
                    credential: AnthropicOAuthCredential {
                        access_token: "access-token".to_string(),
                        refresh_token: "refresh-token".to_string(),
                        email: "user@example.com".to_string(),
                        expires_at: "2030-01-01T00:00:00.000Z".to_string(),
                        account_id: "account-id".to_string(),
                        last_refresh_at: None,
                    },
                    source_path: destination.path().join("anthropic-user@example.com.json"),
                }],
                failures: Vec::new(),
            },
        )
    );
}

#[test]
fn rejects_an_empty_source_directory() {
    let source = tempfile::tempdir().expect("source directory");
    let destination = tempfile::tempdir().expect("destination directory");

    let error = import_anthropic_oauth_credentials(source.path(), destination.path())
        .expect_err("reject empty directory");

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn rejects_source_accounts_with_colliding_sanitized_names() {
    let source = tempfile::tempdir().expect("source directory");
    let destination = tempfile::tempdir().expect("destination directory");
    fs::write(
        source.path().join("claude-first.json"),
        legacy_credential("user+label@example.com", "first-access"),
    )
    .expect("write first source credential");
    fs::write(
        source.path().join("claude-second.json"),
        legacy_credential("user_label@example.com", "second-access"),
    )
    .expect("write second source credential");

    let error = import_anthropic_oauth_credentials(source.path(), destination.path())
        .expect_err("reject source collision");

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn imports_single_credential_with_an_arbitrary_filename() {
    let source = tempfile::tempdir().expect("source directory");
    let destination = tempfile::tempdir().expect("destination directory");
    let source_path = source.path().join("credential.json");
    fs::write(
        &source_path,
        legacy_credential("user+label@example.com", "access-token"),
    )
    .expect("write source credential");

    let imported = import_anthropic_oauth_credentials(&source_path, destination.path())
        .expect("import credential");
    let files = fs::read_dir(destination.path())
        .expect("read destination")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect::<Vec<_>>();

    assert_eq!(
        (imported, files),
        (
            1,
            vec![std::ffi::OsString::from(
                "anthropic-user_label@example.com.json"
            )],
        )
    );
}

#[test]
fn rejects_a_sanitized_filename_collision() {
    let source = tempfile::tempdir().expect("source directory");
    let destination = tempfile::tempdir().expect("destination directory");
    let source_path = source.path().join("credential.json");
    fs::write(
        &source_path,
        legacy_credential("user+label@example.com", "new-access"),
    )
    .expect("write source credential");
    fs::write(
        destination
            .path()
            .join("anthropic-user_label@example.com.json"),
        legacy_credential("user_label@example.com", "existing-access"),
    )
    .expect("write destination credential");

    let error = import_anthropic_oauth_credentials(&source_path, destination.path())
        .expect_err("reject collision");

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
}

#[cfg(unix)]
#[test]
fn persists_credentials_with_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let source = tempfile::tempdir().expect("source directory");
    let destination_root = tempfile::tempdir().expect("destination root");
    let destination = destination_root.path().join("accounts");
    let source_path = source.path().join("credential.json");
    fs::write(
        &source_path,
        legacy_credential("user@example.com", "access-token"),
    )
    .expect("write source credential");

    import_anthropic_oauth_credentials(&source_path, &destination).expect("import credential");
    let directory_mode = fs::metadata(&destination)
        .expect("destination metadata")
        .permissions()
        .mode()
        & 0o777;
    let file_mode = fs::metadata(destination.join("anthropic-user@example.com.json"))
        .expect("credential metadata")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!((directory_mode, file_mode), (0o700, 0o600));
}
