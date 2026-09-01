use super::AnthropicOAuthAuthProvider;
use crate::ANTHROPIC_OAUTH_TOKEN_ENDPOINT;
use crate::AnthropicAccountPool;
use crate::AnthropicOAuthAccount;
use crate::AnthropicOAuthCredential;
use crate::load_anthropic_oauth_credentials;
use crate::persist_anthropic_oauth_credential;
use http::HeaderMap;
use http::HeaderValue;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::sync::Arc;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::method;
use wiremock::matchers::path;
use xedoc_api::AuthProvider;
use xedoc_api::AuthRecoveryAction;
use xedoc_api::AuthRefreshPolicy;
use xedoc_api::RouteAwareAuthHttpTransport;
use xedoc_api::TransportError;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;

fn auth_provider() -> AnthropicOAuthAuthProvider {
    let account = account(
        Path::new("anthropic-user.json"),
        "user@example.com",
        "private-access-token",
    );
    let accounts =
        Arc::new(AnthropicAccountPool::new(vec![account.clone()]).expect("account pool"));
    AnthropicOAuthAuthProvider::new(
        account,
        accounts,
        ANTHROPIC_OAUTH_TOKEN_ENDPOINT.to_string(),
    )
}

fn account(path: &Path, email: &str, access_token: &str) -> AnthropicOAuthAccount {
    AnthropicOAuthAccount {
        credential: AnthropicOAuthCredential {
            access_token: access_token.to_string(),
            refresh_token: format!("{access_token}-refresh"),
            email: email.to_string(),
            expires_at: "2030-01-01T00:00:00.000Z".to_string(),
            account_id: format!("{email}-id"),
            last_refresh_at: None,
        },
        source_path: path.to_path_buf(),
    }
}

fn transport() -> RouteAwareAuthHttpTransport {
    RouteAwareAuthHttpTransport::new(HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault))
}

fn unauthorized() -> TransportError {
    http_error(http::StatusCode::UNAUTHORIZED)
}

fn http_error(status: http::StatusCode) -> TransportError {
    TransportError::Http {
        status,
        url: None,
        headers: None,
        body: None,
    }
}

#[test]
fn adds_oauth_headers_without_replacing_request_betas() {
    let auth = auth_provider();
    let mut headers = HeaderMap::new();
    let _ = headers.insert(
        "anthropic-beta",
        HeaderValue::from_static("thinking-binding-controls-2026-08-01"),
    );

    auth.add_auth_headers(&mut headers);

    assert_eq!(
        headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer private-access-token")
    );
    assert_eq!(
        headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok()),
        Some("thinking-binding-controls-2026-08-01,oauth-2025-04-20")
    );
    assert_eq!(
        headers
            .get("anthropic-dangerous-direct-browser-access")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    assert_eq!(
        headers.get("x-app").and_then(|value| value.to_str().ok()),
        Some("cli")
    );
}

#[test]
fn debug_output_redacts_access_token() {
    let auth = auth_provider();

    let actual = format!("{auth:?}");

    assert!(!actual.contains("private-access-token"));
    assert!(actual.contains("<redacted>"));
}

#[tokio::test]
async fn retryable_account_failures_rotate_to_another_account() {
    let directory = tempfile::tempdir().expect("tempdir");
    for error in [
        http_error(http::StatusCode::UNAUTHORIZED),
        http_error(http::StatusCode::FORBIDDEN),
        http_error(http::StatusCode::TOO_MANY_REQUESTS),
        http_error(http::StatusCode::INTERNAL_SERVER_ERROR),
        TransportError::Timeout,
        TransportError::Network("connection closed".to_string()),
    ] {
        let first = account(
            &directory.path().join("anthropic-first.json"),
            "first@example.com",
            "first-access",
        );
        let second = account(
            &directory.path().join("anthropic-second.json"),
            "second@example.com",
            "second-access",
        );
        let accounts = Arc::new(
            AnthropicAccountPool::new(vec![first.clone(), second.clone()]).expect("account pool"),
        );
        let auth = AnthropicOAuthAuthProvider::new(
            first,
            Arc::clone(&accounts),
            ANTHROPIC_OAUTH_TOKEN_ENDPOINT.to_string(),
        );

        let action = auth
            .recover_from_error(&error, &transport(), AuthRefreshPolicy::AlreadyAttempted)
            .await
            .expect("recover account");

        assert_eq!(
            (action, accounts.select().expect("next account")),
            (AuthRecoveryAction::RetryWithNextCredential, second)
        );
    }
}

#[tokio::test]
async fn non_retryable_client_failure_propagates_without_rotation() {
    let first = account(
        Path::new("anthropic-first.json"),
        "first@example.com",
        "first-access",
    );
    let second = account(
        Path::new("anthropic-second.json"),
        "second@example.com",
        "second-access",
    );
    let accounts =
        Arc::new(AnthropicAccountPool::new(vec![first.clone(), second]).expect("account pool"));
    let auth = AnthropicOAuthAuthProvider::new(
        first.clone(),
        Arc::clone(&accounts),
        ANTHROPIC_OAUTH_TOKEN_ENDPOINT.to_string(),
    );

    let action = auth
        .recover_from_error(
            &http_error(http::StatusCode::BAD_REQUEST),
            &transport(),
            AuthRefreshPolicy::Allowed,
        )
        .await
        .expect("recover account");

    assert_eq!(
        (action, accounts.select().expect("same account")),
        (AuthRecoveryAction::Propagate, first)
    );
}

#[tokio::test]
async fn unauthorized_refreshes_and_persists_the_selected_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_json(serde_json::json!({
            "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            "grant_type": "refresh_token",
            "refresh_token": "old-access-refresh"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 3600,
            "account": {
                "email_address": "user@example.com",
                "uuid": "account-id"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("tempdir");
    let source_path = directory.path().join("anthropic-user.json");
    let account = account(&source_path, "user@example.com", "old-access");
    persist_anthropic_oauth_credential(&source_path, &account.credential)
        .expect("persist initial credential");
    let accounts =
        Arc::new(AnthropicAccountPool::new(vec![account.clone()]).expect("account pool"));
    let auth = AnthropicOAuthAuthProvider::new(
        account,
        accounts,
        format!("{}/v1/oauth/token", server.uri()),
    );
    let mut headers = HeaderMap::new();
    auth.add_auth_headers(&mut headers);

    let action = auth
        .recover_from_error(&unauthorized(), &transport(), AuthRefreshPolicy::Allowed)
        .await
        .expect("recover auth");
    let loaded = load_anthropic_oauth_credentials(directory.path()).expect("reload credentials");
    let refreshed = &loaded.accounts[0].credential;

    assert_eq!(
        (
            action,
            refreshed.access_token.as_str(),
            refreshed.refresh_token.as_str(),
            refreshed.email.as_str(),
            refreshed.account_id.as_str(),
        ),
        (
            AuthRecoveryAction::RetryAfterRefresh,
            "new-access",
            "new-refresh",
            "user@example.com",
            "account-id",
        )
    );
}

#[tokio::test]
async fn terminal_refresh_failure_rotates_to_another_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"code": "refresh_token_reused"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("tempdir");
    let first = account(
        &directory.path().join("anthropic-first.json"),
        "first@example.com",
        "first-access",
    );
    let second = account(
        &directory.path().join("anthropic-second.json"),
        "second@example.com",
        "second-access",
    );
    let accounts = Arc::new(
        AnthropicAccountPool::new(vec![first.clone(), second.clone()]).expect("account pool"),
    );
    let auth = AnthropicOAuthAuthProvider::new(
        first,
        Arc::clone(&accounts),
        format!("{}/v1/oauth/token", server.uri()),
    );
    let mut headers = HeaderMap::new();
    auth.add_auth_headers(&mut headers);

    let action = auth
        .recover_from_error(&unauthorized(), &transport(), AuthRefreshPolicy::Allowed)
        .await
        .expect("recover auth");
    let selected = accounts.select().expect("next account");

    assert_eq!(
        (action, selected),
        (AuthRecoveryAction::RetryWithNextCredential, second)
    );
}

#[tokio::test]
async fn retryable_refresh_failure_uses_three_attempts_before_rotation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(503))
        .expect(3)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("tempdir");
    let first = account(
        &directory.path().join("anthropic-first.json"),
        "first@example.com",
        "first-access",
    );
    let second = account(
        &directory.path().join("anthropic-second.json"),
        "second@example.com",
        "second-access",
    );
    let accounts = Arc::new(
        AnthropicAccountPool::new(vec![first.clone(), second.clone()]).expect("account pool"),
    );
    let auth = AnthropicOAuthAuthProvider::new(
        first,
        Arc::clone(&accounts),
        format!("{}/v1/oauth/token", server.uri()),
    );
    let mut headers = HeaderMap::new();
    auth.add_auth_headers(&mut headers);

    let action = auth
        .recover_from_error(&unauthorized(), &transport(), AuthRefreshPolicy::Allowed)
        .await
        .expect("recover auth");
    let selected = accounts.select().expect("next account");

    assert_eq!(
        (action, selected),
        (AuthRecoveryAction::RetryWithNextCredential, second)
    );
}
