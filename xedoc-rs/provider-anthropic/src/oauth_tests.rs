use super::AnthropicAccountFailureKind;
use super::AnthropicAccountPool;
use super::AnthropicAccountPoolError;
use super::AnthropicAccountUnavailable;
use super::AnthropicCredentialLoad;
use super::AnthropicOAuthAccount;
use super::AnthropicOAuthCredential;
use super::load_anthropic_oauth_credentials;
use super::persist_anthropic_oauth_credential;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;

fn credential(email: &str, token: &str) -> AnthropicOAuthCredential {
    AnthropicOAuthCredential {
        access_token: token.to_string(),
        refresh_token: format!("{token}-refresh"),
        email: email.to_string(),
        expires_at: "2030-01-01T00:00:00.000Z".to_string(),
        account_id: format!("{email}-id"),
        last_refresh_at: Some("2029-12-01T00:00:00.000Z".to_string()),
    }
}

fn account(directory: &Path, email: &str, token: &str) -> AnthropicOAuthAccount {
    AnthropicOAuthAccount {
        credential: credential(email, token),
        source_path: directory.join(format!("anthropic-{email}.json")),
    }
}

#[test]
fn loads_canonical_and_legacy_credentials_in_filename_order() {
    let directory = tempfile::tempdir().expect("tempdir");
    fs::write(
        directory.path().join("claude-b@example.com.json"),
        serde_json::json!({
            "access_token": "legacy-access",
            "refresh_token": "legacy-refresh",
            "email": "b@example.com",
            "expired": "2030-02-01T00:00:00.000Z",
            "account_uuid": "legacy-id",
            "last_refresh": "2029-12-02T00:00:00.000Z",
            "type": "claude"
        })
        .to_string(),
    )
    .expect("write legacy credential");
    fs::write(
        directory.path().join("anthropic-a@example.com.json"),
        serde_json::json!({
            "access_token": "canonical-access",
            "refresh_token": "canonical-refresh",
            "email": "a@example.com",
            "expires_at": "2030-01-01T00:00:00.000Z",
            "account_id": "canonical-id",
            "last_refresh_at": "2029-12-01T00:00:00.000Z",
            "type": "anthropic"
        })
        .to_string(),
    )
    .expect("write canonical credential");
    fs::write(
        directory.path().join("deepseek-ignored.json"),
        r#"{"access_token":"not-anthropic"}"#,
    )
    .expect("write unrelated credential");

    let actual = load_anthropic_oauth_credentials(directory.path()).expect("load credentials");

    assert_eq!(
        actual,
        AnthropicCredentialLoad {
            accounts: vec![
                AnthropicOAuthAccount {
                    credential: AnthropicOAuthCredential {
                        access_token: "canonical-access".to_string(),
                        refresh_token: "canonical-refresh".to_string(),
                        email: "a@example.com".to_string(),
                        expires_at: "2030-01-01T00:00:00.000Z".to_string(),
                        account_id: "canonical-id".to_string(),
                        last_refresh_at: Some("2029-12-01T00:00:00.000Z".to_string()),
                    },
                    source_path: directory.path().join("anthropic-a@example.com.json"),
                },
                AnthropicOAuthAccount {
                    credential: AnthropicOAuthCredential {
                        access_token: "legacy-access".to_string(),
                        refresh_token: "legacy-refresh".to_string(),
                        email: "b@example.com".to_string(),
                        expires_at: "2030-02-01T00:00:00.000Z".to_string(),
                        account_id: "legacy-id".to_string(),
                        last_refresh_at: Some("2029-12-02T00:00:00.000Z".to_string()),
                    },
                    source_path: directory.path().join("claude-b@example.com.json"),
                },
            ],
            failures: vec![],
        }
    );
}

#[test]
fn reports_invalid_credentials_without_discarding_valid_accounts() {
    let directory = tempfile::tempdir().expect("tempdir");
    let invalid_path = directory.path().join("claude-invalid.json");
    fs::write(&invalid_path, r#"{"access_token":"secret"}"#).expect("write invalid credential");
    fs::write(
        directory.path().join("claude-valid.json"),
        serde_json::json!({
            "access_token": "valid-access",
            "refresh_token": "valid-refresh",
            "email": "valid@example.com",
            "expired": "2030-01-01T00:00:00.000Z",
            "type": "claude"
        })
        .to_string(),
    )
    .expect("write valid credential");

    let actual = load_anthropic_oauth_credentials(directory.path()).expect("load credentials");

    assert_eq!(
        actual.accounts,
        vec![AnthropicOAuthAccount {
            credential: AnthropicOAuthCredential {
                access_token: "valid-access".to_string(),
                refresh_token: "valid-refresh".to_string(),
                email: "valid@example.com".to_string(),
                expires_at: "2030-01-01T00:00:00.000Z".to_string(),
                account_id: "valid@example.com".to_string(),
                last_refresh_at: None,
            },
            source_path: directory.path().join("claude-valid.json"),
        }]
    );
    assert_eq!(
        actual
            .failures
            .iter()
            .map(|failure| (
                failure.path.clone(),
                failure.message.contains("missing field `refresh_token`")
            ))
            .collect::<Vec<_>>(),
        vec![(invalid_path, true)]
    );
}

#[test]
fn credential_debug_output_redacts_secrets_and_identity() {
    let credential = credential("user@example.com", "private-access");

    let actual = format!("{credential:?}");

    assert_eq!(
        [
            actual.contains("private-access"),
            actual.contains("private-access-refresh"),
            actual.contains("user@example.com"),
        ],
        [false, false, false]
    );
}

#[test]
fn rejects_duplicate_account_emails() {
    let directory = tempfile::tempdir().expect("tempdir");
    let actual = AnthropicAccountPool::new(vec![
        account(directory.path(), "same@example.com", "first"),
        AnthropicOAuthAccount {
            credential: credential("same@example.com", "second"),
            source_path: directory.path().join("anthropic-second.json"),
        },
    ])
    .expect_err("duplicate email");

    assert_eq!(actual, AnthropicAccountPoolError::DuplicateAccount);
}

#[test]
fn selection_stays_sticky_until_the_account_fails() {
    let directory = tempfile::tempdir().expect("tempdir");
    let first = account(directory.path(), "first@example.com", "first");
    let second = account(directory.path(), "second@example.com", "second");
    let pool = AnthropicAccountPool::new(vec![first.clone(), second]).expect("account pool");

    let actual = vec![
        pool.select().expect("first selection"),
        pool.select().expect("second selection"),
        pool.select().expect("third selection"),
    ];

    assert_eq!(actual, vec![first.clone(), first.clone(), first]);
}

#[test]
fn a_rate_limited_account_fails_over_to_the_next_account() {
    let directory = tempfile::tempdir().expect("tempdir");
    let first = account(directory.path(), "first@example.com", "first");
    let second = account(directory.path(), "second@example.com", "second");
    let pool =
        AnthropicAccountPool::new(vec![first.clone(), second.clone()]).expect("account pool");
    assert_eq!(pool.select().expect("first selection"), first);

    assert!(pool.record_failure(
        &directory.path().join("anthropic-first@example.com.json"),
        AnthropicAccountFailureKind::RateLimit
    ));

    assert_eq!(pool.select().expect("failover selection"), second);
}

#[test]
fn success_restores_account_health() {
    let directory = tempfile::tempdir().expect("tempdir");
    let first = account(directory.path(), "first@example.com", "first");
    let source_path = directory.path().join("anthropic-first@example.com.json");
    let pool = AnthropicAccountPool::new(vec![first.clone()]).expect("account pool");
    assert_eq!(pool.select().expect("initial selection"), first);
    assert!(pool.record_failure(&source_path, AnthropicAccountFailureKind::RateLimit));
    assert!(pool.select().is_err());

    assert!(pool.record_success(&source_path));

    assert_eq!(pool.select().expect("restored selection"), first);
}

#[test]
fn all_unavailable_accounts_prefer_a_recoverable_failure() {
    let directory = tempfile::tempdir().expect("tempdir");
    let pool = AnthropicAccountPool::new(vec![
        account(directory.path(), "auth@example.com", "auth"),
        account(directory.path(), "limited@example.com", "limited"),
    ])
    .expect("account pool");
    assert!(pool.record_failure(
        &directory.path().join("anthropic-auth@example.com.json"),
        AnthropicAccountFailureKind::Auth
    ));
    assert!(pool.record_failure(
        &directory.path().join("anthropic-limited@example.com.json"),
        AnthropicAccountFailureKind::RateLimit
    ));

    let actual = pool.select().expect_err("all accounts unavailable");

    assert_eq!(
        actual.failure_kind,
        Some(AnthropicAccountFailureKind::RateLimit)
    );
    assert!(actual.retry_after.is_some());
}

#[test]
fn persists_refreshed_credentials_in_canonical_format() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("claude-user@example.com.json");
    let expected = credential("user@example.com", "new-access");

    persist_anthropic_oauth_credential(&path, &expected).expect("persist credential");
    let actual = load_anthropic_oauth_credentials(directory.path()).expect("reload credential");

    assert_eq!(
        actual,
        AnthropicCredentialLoad {
            accounts: vec![AnthropicOAuthAccount {
                credential: expected,
                source_path: path,
            }],
            failures: vec![],
        }
    );
}

#[test]
fn an_empty_pool_reports_that_no_account_is_configured() {
    let pool = AnthropicAccountPool::new(vec![]).expect("empty account pool");

    let actual = pool.select().expect_err("no account");

    assert_eq!(
        actual,
        AnthropicAccountUnavailable {
            failure_kind: None,
            retry_after: None,
        }
    );
}
