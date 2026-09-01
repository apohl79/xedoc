use http::HeaderMap;
use http::HeaderValue;
use std::fmt;
use xedoc_api::AuthProvider;

const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";

#[derive(Clone)]
pub struct AnthropicOAuthAuthProvider {
    access_token: String,
}

impl AnthropicOAuthAuthProvider {
    pub fn new(access_token: String) -> Self {
        Self { access_token }
    }
}

impl fmt::Debug for AnthropicOAuthAuthProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicOAuthAuthProvider")
            .field("access_token", &"<redacted>")
            .finish()
    }
}

impl AuthProvider for AnthropicOAuthAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", self.access_token)) {
            let _ = headers.insert(http::header::AUTHORIZATION, value);
        }
        merge_beta_header(headers, ANTHROPIC_OAUTH_BETA);
        let _ = headers.insert(
            "anthropic-dangerous-direct-browser-access",
            HeaderValue::from_static("true"),
        );
        let _ = headers.insert("x-app", HeaderValue::from_static("cli"));
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
