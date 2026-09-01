use chrono::SecondsFormat;
use chrono::Utc;
use http::HeaderValue;
use http::Method;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use xedoc_api::AuthHttpError;
use xedoc_api::AuthHttpTransport;
use xedoc_api::AuthRecoveryError;
use xedoc_client::EncodedJsonBody;
use xedoc_client::Request;
use xedoc_client::RequestBody;

use crate::AnthropicAccountFailureKind;
use crate::AnthropicAccountPool;
use crate::AnthropicOAuthCredential;
use crate::persist_anthropic_oauth_credential;

pub const ANTHROPIC_OAUTH_TOKEN_ENDPOINT: &str = "https://api.anthropic.com/v1/oauth/token";

const ANTHROPIC_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const REFRESH_GRANT_TYPE: &str = "refresh_token";
const MAX_REFRESH_ATTEMPTS: u32 = 3;

pub(crate) enum AnthropicRefreshOutcome {
    Refreshed,
    Terminal,
    Unavailable(AnthropicAccountFailureKind),
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    account: Option<RefreshAccount>,
}

#[derive(Deserialize)]
struct RefreshAccount {
    email_address: Option<String>,
    uuid: Option<String>,
}

enum RefreshAttemptFailure {
    Retryable(AnthropicAccountFailureKind),
    Terminal,
}

pub(crate) async fn refresh_anthropic_oauth_account(
    pool: &Arc<AnthropicAccountPool>,
    source_path: &Path,
    applied_access_token: Option<&str>,
    endpoint: &str,
    transport: &dyn AuthHttpTransport,
) -> Result<AnthropicRefreshOutcome, AuthRecoveryError> {
    let refresh_lock = pool
        .refresh_lock(source_path)
        .ok_or(AuthRecoveryError::ReauthorizationRequired)?;
    let _guard = refresh_lock.lock_owned().await;
    let credential = pool
        .credential(source_path)
        .ok_or(AuthRecoveryError::ReauthorizationRequired)?;
    if applied_access_token.is_some_and(|token| token != credential.access_token) {
        return Ok(AnthropicRefreshOutcome::Refreshed);
    }

    let refreshed = match refresh_with_retry(&credential, endpoint, transport).await? {
        Ok(refreshed) => refreshed,
        Err(outcome) => return Ok(outcome),
    };
    persist_anthropic_oauth_credential(source_path, &refreshed)
        .map_err(|_| AuthRecoveryError::CredentialStore)?;
    if !pool.update_credential(source_path, refreshed) {
        return Err(AuthRecoveryError::ReauthorizationRequired);
    }
    let _ = pool.record_success(source_path);
    Ok(AnthropicRefreshOutcome::Refreshed)
}

async fn refresh_with_retry(
    credential: &AnthropicOAuthCredential,
    endpoint: &str,
    transport: &dyn AuthHttpTransport,
) -> Result<Result<AnthropicOAuthCredential, AnthropicRefreshOutcome>, AuthRecoveryError> {
    let mut last_failure = AnthropicAccountFailureKind::Network;
    for attempt in 1..=MAX_REFRESH_ATTEMPTS {
        match refresh_once(credential, endpoint, transport).await? {
            Ok(refreshed) => return Ok(Ok(refreshed)),
            Err(RefreshAttemptFailure::Terminal) => {
                return Ok(Err(AnthropicRefreshOutcome::Terminal));
            }
            Err(RefreshAttemptFailure::Retryable(kind)) => last_failure = kind,
        }
        if attempt < MAX_REFRESH_ATTEMPTS {
            tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
        }
    }
    Ok(Err(AnthropicRefreshOutcome::Unavailable(last_failure)))
}

async fn refresh_once(
    credential: &AnthropicOAuthCredential,
    endpoint: &str,
    transport: &dyn AuthHttpTransport,
) -> Result<Result<AnthropicOAuthCredential, RefreshAttemptFailure>, AuthRecoveryError> {
    let request = build_refresh_request(endpoint, &credential.refresh_token)?;
    let response = match transport.execute(request).await {
        Ok(response) => response,
        Err(AuthHttpError::Build) => return Err(AuthRecoveryError::RequestBuild),
        Err(AuthHttpError::Network | AuthHttpError::Timeout) => {
            return Ok(Err(RefreshAttemptFailure::Retryable(
                AnthropicAccountFailureKind::Network,
            )));
        }
    };
    if response.status().is_success() {
        let response: RefreshResponse = serde_json::from_slice(response.body())
            .map_err(|_| AuthRecoveryError::InvalidTokenResponse)?;
        return refreshed_credential(credential, response).map(Ok);
    }
    if terminal_refresh_error(response.body()) {
        return Ok(Err(RefreshAttemptFailure::Terminal));
    }
    Ok(Err(RefreshAttemptFailure::Retryable(
        failure_kind_for_status(response.status()),
    )))
}

fn build_refresh_request(
    endpoint: &str,
    refresh_token: &str,
) -> Result<Request, AuthRecoveryError> {
    let body = EncodedJsonBody::encode(&RefreshRequest {
        client_id: ANTHROPIC_OAUTH_CLIENT_ID,
        grant_type: REFRESH_GRANT_TYPE,
        refresh_token,
    })
    .map_err(|_| AuthRecoveryError::RequestBuild)?;
    let mut request = Request::new(Method::POST, endpoint.to_string());
    request.headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    request.body = Some(RequestBody::EncodedJson(body));
    request
        .into_prepared()
        .map_err(|_| AuthRecoveryError::RequestBuild)
}

fn refreshed_credential(
    current: &AnthropicOAuthCredential,
    response: RefreshResponse,
) -> Result<AnthropicOAuthCredential, AuthRecoveryError> {
    if response.access_token.trim().is_empty()
        || response.refresh_token.trim().is_empty()
        || response.expires_in < 0
    {
        return Err(AuthRecoveryError::InvalidTokenResponse);
    }
    let now = Utc::now();
    let account = response.account;
    Ok(AnthropicOAuthCredential {
        access_token: response.access_token,
        refresh_token: response.refresh_token,
        email: account
            .as_ref()
            .and_then(|account| account.email_address.clone())
            .filter(|email| !email.trim().is_empty())
            .unwrap_or_else(|| current.email.clone()),
        expires_at: (now + chrono::Duration::seconds(response.expires_in))
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        account_id: account
            .and_then(|account| account.uuid)
            .filter(|account_id| !account_id.trim().is_empty())
            .unwrap_or_else(|| current.account_id.clone()),
        last_refresh_at: Some(now.to_rfc3339_opts(SecondsFormat::Millis, true)),
    })
}

fn terminal_refresh_error(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let code = value
        .get("error")
        .and_then(|error| {
            error
                .as_str()
                .or_else(|| error.get("code").and_then(serde_json::Value::as_str))
        })
        .or_else(|| value.get("code").and_then(serde_json::Value::as_str));
    matches!(
        code.map(str::to_ascii_lowercase).as_deref(),
        Some("refresh_token_expired" | "refresh_token_reused" | "refresh_token_invalidated")
    )
}

fn failure_kind_for_status(status: http::StatusCode) -> AnthropicAccountFailureKind {
    if status == http::StatusCode::TOO_MANY_REQUESTS {
        AnthropicAccountFailureKind::RateLimit
    } else if status.is_server_error() {
        AnthropicAccountFailureKind::Server
    } else {
        AnthropicAccountFailureKind::Auth
    }
}
