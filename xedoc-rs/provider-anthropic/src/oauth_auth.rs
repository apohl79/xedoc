use http::HeaderMap;
use http::HeaderValue;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use xedoc_api::AuthHttpTransport;
use xedoc_api::AuthProvider;
use xedoc_api::AuthRecoveryAction;
use xedoc_api::AuthRecoveryFuture;
use xedoc_api::AuthRecoveryIdentity;
use xedoc_api::AuthRefreshPolicy;
use xedoc_api::TransportError;

use crate::AnthropicAccountFailureKind;
use crate::AnthropicAccountPool;
use crate::AnthropicOAuthAccount;
use crate::oauth_refresh::AnthropicRefreshOutcome;
use crate::oauth_refresh::refresh_anthropic_oauth_account;

const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";

pub struct AnthropicOAuthAuthProvider {
    account_path: PathBuf,
    accounts: Arc<AnthropicAccountPool>,
    refresh_endpoint: String,
    applied_access_token: Mutex<Option<String>>,
}

impl AnthropicOAuthAuthProvider {
    pub fn new(
        account: AnthropicOAuthAccount,
        accounts: Arc<AnthropicAccountPool>,
        refresh_endpoint: String,
    ) -> Self {
        Self {
            account_path: account.source_path,
            accounts,
            refresh_endpoint,
            applied_access_token: Mutex::new(None),
        }
    }
}

impl fmt::Debug for AnthropicOAuthAuthProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicOAuthAuthProvider")
            .field("account_path", &"<redacted>")
            .field("refresh_endpoint", &"<redacted>")
            .finish()
    }
}

impl AuthProvider for AnthropicOAuthAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        let Some(credential) = self.accounts.credential(&self.account_path) else {
            return;
        };
        *self
            .applied_access_token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(credential.access_token.clone());
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", credential.access_token)) {
            let _ = headers.insert(http::header::AUTHORIZATION, value);
        }
        merge_beta_header(headers, ANTHROPIC_OAUTH_BETA);
        let _ = headers.insert(
            "anthropic-dangerous-direct-browser-access",
            HeaderValue::from_static("true"),
        );
        let _ = headers.insert("x-app", HeaderValue::from_static("cli"));
    }

    fn recovery_identity(&self) -> Option<AuthRecoveryIdentity> {
        Some(AuthRecoveryIdentity::new(
            self.account_path.to_string_lossy().into_owned(),
        ))
    }

    fn record_success(&self) {
        let _ = self.accounts.record_success(&self.account_path);
    }

    fn recover_from_error<'a>(
        &'a self,
        error: &'a TransportError,
        transport: &'a dyn AuthHttpTransport,
        refresh_policy: AuthRefreshPolicy,
    ) -> AuthRecoveryFuture<'a> {
        Box::pin(async move {
            match failure_kind(error) {
                Some(AnthropicAccountFailureKind::Auth)
                    if matches!(refresh_policy, AuthRefreshPolicy::Allowed) =>
                {
                    let applied_access_token = self
                        .applied_access_token
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    match refresh_anthropic_oauth_account(
                        &self.accounts,
                        &self.account_path,
                        applied_access_token.as_deref(),
                        &self.refresh_endpoint,
                        transport,
                    )
                    .await?
                    {
                        AnthropicRefreshOutcome::Refreshed => {
                            Ok(AuthRecoveryAction::RetryAfterRefresh)
                        }
                        AnthropicRefreshOutcome::Terminal => {
                            Ok(self.rotate(AnthropicAccountFailureKind::Auth))
                        }
                        AnthropicRefreshOutcome::Unavailable(kind) => Ok(self.rotate(kind)),
                    }
                }
                Some(kind) => Ok(self.rotate(kind)),
                None => Ok(AuthRecoveryAction::Propagate),
            }
        })
    }
}

impl AnthropicOAuthAuthProvider {
    fn rotate(&self, kind: AnthropicAccountFailureKind) -> AuthRecoveryAction {
        let _ = self.accounts.record_failure(&self.account_path, kind);
        if self.accounts.has_available_account() {
            AuthRecoveryAction::RetryWithNextCredential
        } else {
            AuthRecoveryAction::Propagate
        }
    }
}

fn failure_kind(error: &TransportError) -> Option<AnthropicAccountFailureKind> {
    match error {
        TransportError::Http { status, .. } if *status == http::StatusCode::UNAUTHORIZED => {
            Some(AnthropicAccountFailureKind::Auth)
        }
        TransportError::Http { status, .. } if *status == http::StatusCode::FORBIDDEN => {
            Some(AnthropicAccountFailureKind::Forbidden)
        }
        TransportError::Http { status, .. } if *status == http::StatusCode::TOO_MANY_REQUESTS => {
            Some(AnthropicAccountFailureKind::RateLimit)
        }
        TransportError::Http { status, .. } if status.is_server_error() => {
            Some(AnthropicAccountFailureKind::Server)
        }
        TransportError::Timeout | TransportError::Network(_) => {
            Some(AnthropicAccountFailureKind::Network)
        }
        TransportError::Http { .. } | TransportError::RetryLimit | TransportError::Build(_) => None,
    }
}

fn merge_beta_header(headers: &mut HeaderMap, beta: &str) {
    let existing = headers
        .get("anthropic-beta")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if existing.split(',').any(|value| value.trim() == beta) {
        return;
    }
    let value = if existing.is_empty() {
        beta.to_string()
    } else {
        format!("{existing},{beta}")
    };
    if let Ok(value) = HeaderValue::from_str(&value) {
        let _ = headers.insert("anthropic-beta", value);
    }
}

#[cfg(test)]
#[path = "oauth_auth_tests.rs"]
mod tests;
